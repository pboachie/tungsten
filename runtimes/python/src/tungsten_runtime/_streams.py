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
from collections.abc import AsyncIterator, Awaitable, Callable, Iterator, Mapping
from dataclasses import dataclass
from typing import Any, cast

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


class EventStream[T]:
    """The events of a stream, for a synchronous client: iterate it for
    ``StreamEvent``s then at most one final ``Err``, or use a helper."""

    __slots__ = ("_active", "_cancelled", "_close", "_items", "_lock", "_stop")

    def __init__(
        self, items: Iterator[StreamEvent[T] | Err], close: Callable[[], None], stop: threading.Event
    ) -> None:
        self._items = items
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

    def collect(self) -> StreamResult[list[T]]:
        """Read the stream to its end into a list of the events' ``data``."""

        def add(all_values: list[T], value: T, _: StreamEvent[T]) -> list[T]:
            all_values.append(value)
            return all_values

        return self.reduce(add, [])

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

    __slots__ = ("_active", "_cancel_event", "_cancelled", "_items", "_stop")

    def __init__(self, items: AsyncIterator[StreamEvent[T] | Err], stop: threading.Event) -> None:
        self._items = items
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
        waiter = asyncio.ensure_future(wake.wait())
        try:
            while not self._cancelled:
                step = asyncio.ensure_future(anext(self._items))
                await asyncio.wait({step, waiter}, return_when=asyncio.FIRST_COMPLETED)
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
            waiter.cancel()
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

    async def collect(self) -> StreamResult[list[T]]:
        """See ``EventStream.collect``."""

        def add(all_values: list[T], value: T, _: StreamEvent[T]) -> list[T]:
            all_values.append(value)
            return all_values

        return await self.reduce(add, [])

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
