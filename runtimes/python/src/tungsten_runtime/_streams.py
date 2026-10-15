# SPDX-License-Identifier: Apache-2.0
"""``EventStream`` and ``AsyncEventStream``: what ``ClientCore.stream`` and
``AsyncClientCore.stream`` return. They iterate like the generators they
replace and add helpers that consume the stream: ``on`` (dispatch by event
name), ``collect``, ``reduce``, ``first`` and ``cancel``. A stream is read
once; every helper closes the connection when it returns."""

from __future__ import annotations

import asyncio
import contextlib
import threading
import time
from collections.abc import AsyncIterator, Awaitable, Callable, Iterator, Mapping
from dataclasses import dataclass
from queue import Empty, Full, Queue
from typing import Any, cast

from ._envelope import diagnostic
from ._json import canonical_json
from .types import Diagnostic, Err, StreamEvent

#: The key of ``on`` handlers that receives every event no other handler took.
ANY_EVENT = "*"


@dataclass(frozen=True, slots=True)
class StreamResult[R]:
    """What a helper returns: ``value`` and the number of reconnects the
    stream made, or the failure that ended the stream with what was
    gathered before it as ``partial``. ``reconnects`` is that of the last
    event delivered."""

    ok: bool
    reconnects: int
    value: R | None = None
    error: Diagnostic | None = None
    partial: R | None = None


def _collection_error(
    operation: str, mutation: bool, what: str, kept: int, *, invalid: bool = False
) -> Diagnostic:
    after = " The call may have taken effect; inspect API state before retrying." if mutation else ""
    return diagnostic(
        operation,
        "VALIDATION_FAILED" if invalid else "UNEXPECTED_RESPONSE",
        failed_parameter="events",
        expected=what,
        retryable="never",
        remediation=(
            "Collection budgets must be positive safe integers; nothing was read."
            if invalid
            else f"The stream was abandoned after {kept} events ({what}; collect max_bytes and max_ms).{after} The events collected so far precede this error."
        ),
    )


def _collection_size(value: object) -> int:
    dump = getattr(value, "model_dump", None)
    plain: object = dump(mode="json", by_alias=True) if callable(dump) else value
    return len(canonical_json(plain).encode("utf-8"))


def _collection_limits(max_bytes: int | None, max_ms: int | None) -> tuple[int, int] | None:
    if max_bytes is None and max_ms is None:
        return None
    return (4 * 1024 * 1024 if max_bytes is None else max_bytes, 60_000 if max_ms is None else max_ms)


class EventStream[T]:
    """The events of a stream, for a synchronous client: iterate it for
    ``StreamEvent``s then at most one final ``Err``, or use a helper."""

    __slots__ = ("_active", "_cancelled", "_close", "_items", "_lock", "_mutation", "_operation", "_stop")

    def __init__(
        self,
        items: Iterator[StreamEvent[T] | Err],
        close: Callable[[], None],
        stop: threading.Event,
        operation: str = "<stream>",
        mutation: bool = False,
    ) -> None:
        self._items = items
        self._operation = operation
        self._mutation = mutation
        self._close = close
        self._stop = stop
        self._cancelled = False
        self._lock = threading.Lock()
        self._active: Iterator[StreamEvent[T] | Err] | None = None

    @property
    def cancelled(self) -> bool:
        """Whether ``cancel`` was called."""
        return self._cancelled

    def cancel(self) -> None:
        """Close the connection and end the stream: a pending or later read
        ends without a failure item. Safe to call from another thread (it
        ends a blocked read) and more than once."""
        with self._lock:
            if self._cancelled:
                return
            self._cancelled = True
        self._stop.set()
        self._close()

    def close(self) -> None:
        """Release the connection and finish the iteration (``cancel`` for
        the connection alone)."""
        self.cancel()
        active = self._active
        if active is not None:
            with contextlib.suppress(ValueError):
                cast(Any, active).close()

    def __iter__(self) -> Iterator[StreamEvent[T] | Err]:
        self._active = self._iterate()
        return self._active

    def _iterate(self) -> Iterator[StreamEvent[T] | Err]:
        try:
            for item in self._items:
                if self._cancelled:
                    return
                yield item
        finally:
            self._close()
            with contextlib.suppress(ValueError):
                cast(Any, self._items).close()

    def on(self, handlers: Mapping[str, Callable[[T, StreamEvent[T]], object]]) -> StreamResult[int]:
        """Read the stream to its end and call the handler named like each
        event (``handlers["message_delta"]`` for ``event: message_delta``;
        events without a name are ``message``), else ``handlers["*"]``. A
        handler that raises ends the stream (the connection is closed) and
        the exception propagates. ``value`` is the number of events
        delivered."""
        count = 0
        reconnects = 0
        try:
            for item in self:
                if isinstance(item, Err):
                    return StreamResult(False, reconnects, error=item.error, partial=count)
                reconnects = item.meta.reconnects
                handler = handlers.get(item.event, handlers.get(ANY_EVENT))
                count += 1
                if handler is not None:
                    handler(item.value, item)
        finally:
            self.close()
        return StreamResult(True, reconnects, value=count)

    def reduce[A](self, fold: Callable[[A, T, StreamEvent[T]], A], initial: A) -> StreamResult[A]:
        """Read the stream to its end and fold the events into one value
        (concatenating text deltas, say). ``partial`` of a failure is the
        value folded so far."""
        accumulator = initial
        reconnects = 0
        try:
            for item in self:
                if isinstance(item, Err):
                    return StreamResult(False, reconnects, error=item.error, partial=accumulator)
                reconnects = item.meta.reconnects
                accumulator = fold(accumulator, item.value, item)
        finally:
            self.close()
        return StreamResult(True, reconnects, value=accumulator)

    def collect(self, *, max_bytes: int | None = None, max_ms: int | None = None) -> StreamResult[list[T]]:
        """Collect events. With either budget set, the other defaults to 4 MiB
        or 60 seconds. No arguments preserves unbounded collection. A budget
        failure retains only the admitted events as ``partial``."""
        limits = _collection_limits(max_bytes, max_ms)
        if limits is None:

            def add(all_values: list[T], value: T, _: StreamEvent[T]) -> list[T]:
                all_values.append(value)
                return all_values

            return self.reduce(add, [])
        maximum, milliseconds = limits
        if any(type(v) is not int or v < 1 or v > 9_007_199_254_740_991 for v in limits):
            self.close()
            return StreamResult(
                False,
                0,
                error=_collection_error(
                    self._operation,
                    self._mutation,
                    "positive safe integer collection budgets",
                    0,
                    invalid=True,
                ),
                partial=[],
            )
        deadline = time.monotonic() + milliseconds / 1000
        queue: Queue[object] = Queue(maxsize=1)
        stop = threading.Event()

        def put(item: object) -> None:
            while not stop.is_set():
                try:
                    queue.put(item, timeout=0.01)
                    return
                except Full:
                    continue

        def pull() -> None:
            try:
                for item in self:
                    if stop.is_set():
                        break
                    put(item)
            except Exception as error:
                put(error)
            finally:
                put(None)

        threading.Thread(target=pull, daemon=True).start()
        values: list[T] = []
        size, reconnects = 0, 0
        what = f"at most {milliseconds} ms to collect the events"
        try:
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                try:
                    received = queue.get(timeout=min(remaining, threading.TIMEOUT_MAX))
                except Empty:
                    continue
                if received is None:
                    return StreamResult(True, reconnects, value=values)
                if isinstance(received, Exception):
                    raise received
                if isinstance(received, Err):
                    return StreamResult(False, reconnects, error=received.error, partial=values)
                item = cast(StreamEvent[T], received)
                reconnects = item.meta.reconnects
                try:
                    size += _collection_size(item.value)
                except Exception:
                    what = "JSON-serializable events"
                    break
                if time.monotonic() >= deadline:
                    break
                if size > maximum:
                    what = f"at most {maximum} bytes of events"
                    break
                values.append(item.value)
        finally:
            stop.set()
            self.close()
        return StreamResult(
            False,
            reconnects,
            error=_collection_error(self._operation, self._mutation, what, len(values)),
            partial=values,
        )

    def first(
        self, predicate: Callable[[T, StreamEvent[T]], bool] | None = None
    ) -> StreamResult[StreamEvent[T] | None]:
        """The first event for which ``predicate`` holds (any event without
        one), then close the stream; ``value`` is None when the stream ends
        without one."""
        reconnects = 0
        try:
            for item in self:
                if isinstance(item, Err):
                    return StreamResult(False, reconnects, error=item.error)
                reconnects = item.meta.reconnects
                if predicate is None or predicate(item.value, item):
                    return StreamResult(True, reconnects, value=item)
        finally:
            self.close()
        return StreamResult(True, reconnects)


class AsyncEventStream[T]:
    """The events of a stream, for an asynchronous client: ``async for`` over
    it, or ``await`` a helper."""

    __slots__ = ("_active", "_cancel_event", "_cancelled", "_items", "_mutation", "_operation", "_stop")

    def __init__(
        self,
        items: AsyncIterator[StreamEvent[T] | Err],
        stop: threading.Event,
        operation: str = "<stream>",
        mutation: bool = False,
    ) -> None:
        self._items = items
        self._operation = operation
        self._mutation = mutation
        self._stop = stop
        self._cancelled = False
        self._cancel_event: asyncio.Event | None = None
        self._active: AsyncIterator[StreamEvent[T] | Err] | None = None

    @property
    def cancelled(self) -> bool:
        """Whether ``cancel`` was called."""
        return self._cancelled

    def cancel(self) -> None:
        """End the stream: a read in progress in another task is abandoned
        and the connection closed, and no failure item is delivered. Safe to
        call more than once."""
        self._cancelled = True
        self._stop.set()
        if self._cancel_event is not None:
            self._cancel_event.set()

    async def aclose(self) -> None:
        """Release the connection and finish the iteration; call it when no
        task is reading the stream (``cancel`` abandons a read in progress)."""
        self.cancel()
        if self._active is not None:
            await cast(Any, self._active).aclose()
        await cast(Any, self._items).aclose()

    def __aiter__(self) -> AsyncIterator[StreamEvent[T] | Err]:
        self._active = self._iterate()
        return self._active

    async def _iterate(self) -> AsyncIterator[StreamEvent[T] | Err]:
        wake = self._cancel_event = asyncio.Event()
        if self._cancelled:
            wake.set()
        try:
            while not self._cancelled:
                step = asyncio.ensure_future(anext(self._items))
                waiter = asyncio.ensure_future(wake.wait())
                try:
                    await asyncio.wait({step, waiter}, return_when=asyncio.FIRST_COMPLETED)
                except BaseException:
                    step.cancel()
                    with contextlib.suppress(asyncio.CancelledError, Exception):
                        await step
                    raise
                finally:
                    waiter.cancel()
                if not step.done():
                    step.cancel()
                    with contextlib.suppress(asyncio.CancelledError, StopAsyncIteration):
                        await step
                    return
                try:
                    item = step.result()
                except StopAsyncIteration:
                    return
                if self._cancelled:
                    return
                yield item
        finally:
            await cast(Any, self._items).aclose()

    async def on(
        self, handlers: Mapping[str, Callable[[T, StreamEvent[T]], object | Awaitable[object]]]
    ) -> StreamResult[int]:
        """See ``EventStream.on``; a handler may be a coroutine function."""
        count = 0
        reconnects = 0
        try:
            async for item in self:
                if isinstance(item, Err):
                    return StreamResult(False, reconnects, error=item.error, partial=count)
                reconnects = item.meta.reconnects
                handler = handlers.get(item.event, handlers.get(ANY_EVENT))
                count += 1
                if handler is not None:
                    outcome = handler(item.value, item)
                    if asyncio.iscoroutine(outcome):
                        await outcome
        finally:
            await self.aclose()
        return StreamResult(True, reconnects, value=count)

    async def reduce[A](self, fold: Callable[[A, T, StreamEvent[T]], A], initial: A) -> StreamResult[A]:
        """See ``EventStream.reduce``."""
        accumulator = initial
        reconnects = 0
        try:
            async for item in self:
                if isinstance(item, Err):
                    return StreamResult(False, reconnects, error=item.error, partial=accumulator)
                reconnects = item.meta.reconnects
                accumulator = fold(accumulator, item.value, item)
        finally:
            await self.aclose()
        return StreamResult(True, reconnects, value=accumulator)

    async def collect(
        self, *, max_bytes: int | None = None, max_ms: int | None = None
    ) -> StreamResult[list[T]]:
        """See ``EventStream.collect``."""
        limits = _collection_limits(max_bytes, max_ms)
        if limits is None:

            def add(all_values: list[T], value: T, _: StreamEvent[T]) -> list[T]:
                all_values.append(value)
                return all_values

            return await self.reduce(add, [])
        maximum, milliseconds = limits
        if any(type(v) is not int or v < 1 or v > 9_007_199_254_740_991 for v in limits):
            await self.aclose()
            return StreamResult(
                False,
                0,
                error=_collection_error(
                    self._operation,
                    self._mutation,
                    "positive safe integer collection budgets",
                    0,
                    invalid=True,
                ),
                partial=[],
            )
        deadline = time.monotonic() + milliseconds / 1000
        values: list[T] = []
        size, reconnects = 0, 0
        what = f"at most {milliseconds} ms to collect the events"
        iterator = self.__aiter__()
        try:
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                try:
                    item = await asyncio.wait_for(anext(iterator), remaining)
                except StopAsyncIteration:
                    return StreamResult(True, reconnects, value=values)
                except TimeoutError:
                    break
                if isinstance(item, Err):
                    return StreamResult(False, reconnects, error=item.error, partial=values)
                reconnects = item.meta.reconnects
                try:
                    size += _collection_size(item.value)
                except Exception:
                    what = "JSON-serializable events"
                    break
                if time.monotonic() >= deadline:
                    break
                if size > maximum:
                    what = f"at most {maximum} bytes of events"
                    break
                values.append(item.value)
        finally:
            await self.aclose()
        return StreamResult(
            False,
            reconnects,
            error=_collection_error(self._operation, self._mutation, what, len(values)),
            partial=values,
        )

    async def first(
        self, predicate: Callable[[T, StreamEvent[T]], bool] | None = None
    ) -> StreamResult[StreamEvent[T] | None]:
        """See ``EventStream.first``."""
        reconnects = 0
        try:
            async for item in self:
                if isinstance(item, Err):
                    return StreamResult(False, reconnects, error=item.error)
                reconnects = item.meta.reconnects
                if predicate is None or predicate(item.value, item):
                    return StreamResult(True, reconnects, value=item)
        finally:
            await self.aclose()
        return StreamResult(True, reconnects)
