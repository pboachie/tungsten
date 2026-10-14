# SPDX-License-Identifier: Apache-2.0
"""Run flows (``_effects``): the synchronous driver performs effects with
``httpx.Client`` and ``time.sleep``, the asynchronous one with
``httpx.AsyncClient`` and ``asyncio.sleep``. One HTTP attempt has a
deadline covering native socket I/O and the body; redirects are never followed
by httpx; failures are classified by whether the request can have reached
the server. Synchronous custom transports remain on the caller's thread:
their callbacks must cooperate with timeouts, since Python cannot safely
interrupt arbitrary blocking code. Late custom results are rejected only
after the work and cleanup finish. Async cancellation likewise waits for
cleanup; non-cooperative callbacks and OS DNS resolution are not hard
real-time guarantees."""

from __future__ import annotations

import asyncio
import contextlib
import errno
import inspect
import socket
import ssl
import threading
import time
import weakref
from collections.abc import AsyncIterator, Awaitable, Callable, Coroutine, Iterator
from concurrent.futures import CancelledError
from dataclasses import dataclass, replace
from http.cookiejar import CookieJar, DefaultCookiePolicy
from typing import Any, Literal, cast

import httpx

from ._effects import (
    Answered,
    AttemptOutcome,
    AttemptRequest,
    Bounded,
    Chunk,
    CloseStream,
    Effect,
    Emit,
    Flow,
    Invoke,
    Lost,
    NotSent,
    Observe,
    PollExpired,
    ReadChunk,
    ReadOutcome,
    Send,
    Shared,
    Sleep,
    StoreGet,
    StorePut,
    StoreReserve,
    StreamEnded,
    StreamFailed,
    Streaming,
    TimedOut,
)


def _no_cookies() -> CookieJar:
    """A jar that keeps nothing: ``Set-Cookie`` answers never reach later
    requests; cookies are only what the auth profile and the call say."""
    return CookieJar(policy=DefaultCookiePolicy(allowed_domains=[]))


def sync_http(transport: object) -> httpx.Client:
    return httpx.Client(
        transport=transport if isinstance(transport, httpx.BaseTransport) else _NativeSyncTransport(),
        follow_redirects=False,
        cookies=_no_cookies(),
        timeout=None,
    )


async def _before_deadline[T](work: Awaitable[T], deadline: float | None) -> T:
    if deadline is None:
        return await work
    try:
        async with asyncio.timeout(max(0.0, deadline - time.monotonic())):
            return await work
    except TimeoutError:
        raise httpx.ReadTimeout("The attempt deadline expired") from None


class _NativeSyncTransport(httpx.BaseTransport):
    """Public HTTPX transport bridge: cancellable native I/O for sync callers.

    One owned loop/client retains connection pooling and HTTPX's proxy/TLS
    defaults. A caller waits for cancellation and response cleanup, not just a
    timer notification. Custom sync transports are not moved to another thread:
    arbitrary blocking Python callbacks cannot safely be forcibly interrupted.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread: threading.Thread | None = None
        self._client: httpx.AsyncClient | None = None
        # Stops the loop thread and closes the pool of a transport that is
        # dropped (its client never closed); close() detaches it.
        self._finalizer: weakref.finalize[..., Any] | None = None
        self._closed = False
        self._stopped = threading.Event()
        self._responses: set[httpx.Response] = set()
        self._response_closers: dict[httpx.Response, asyncio.Task[None]] = {}

    def _start(self) -> asyncio.AbstractEventLoop:
        # Caller holds _lock through submission, so close cannot stop the loop
        # between accepting a coroutine and scheduling it.
        if self._closed:
            raise RuntimeError("The transport is closed")
        if self._loop is None:
            loop = asyncio.new_event_loop()
            self._loop = loop
            # The pool belongs to the loop; the finalizer references only
            # the loop, its thread and the pool, never the transport.
            self._client = async_http(None)

            def serve() -> None:
                asyncio.set_event_loop(loop)
                try:
                    loop.run_forever()
                finally:
                    loop.run_until_complete(loop.shutdown_asyncgens())
                    loop.run_until_complete(loop.shutdown_default_executor())
                    loop.close()

            self._thread = threading.Thread(target=serve, name="tungsten-http", daemon=True)
            self._thread.start()
            self._finalizer = weakref.finalize(self, _abandon_loop, loop, self._thread, self._client)
        return self._loop

    def _on_loop(self) -> bool:
        return self._thread is not None and threading.current_thread() is self._thread

    def run[T](self, work: Coroutine[Any, Any, T]) -> T:
        if self._on_loop():
            work.close()
            raise RuntimeError("a synchronous call cannot run on the transport's own loop thread")
        try:
            with self._lock:
                pending = asyncio.run_coroutine_threadsafe(work, self._start())
        except Exception:
            work.close()
            raise
        return pending.result()

    def cleanup(self, work: Coroutine[Any, Any, None]) -> None:
        if self._on_loop():
            # A response finalized on the loop thread (garbage collection can
            # run there) cannot wait for the loop; shutdown joins its cleanup.
            with self._lock:
                if self._closed or self._loop is None:
                    work.close()
                else:
                    self._loop.create_task(work)
            return
        with self._lock:
            if self._closed:
                work.close()
                pending = None
            else:
                pending = asyncio.run_coroutine_threadsafe(work, self._start())
        if pending is not None:
            try:
                pending.result()
                return
            except CancelledError:
                pass  # Shutdown has taken ownership of response cleanup.
        self._stopped.wait()

    async def close_response(self, response: httpx.Response) -> None:
        closing = self._response_closers.get(response)
        if closing is None:
            closing = asyncio.create_task(response.aclose())
            self._response_closers[response] = closing
        # Once response cleanup starts, shutdown must join it, not cancel it.
        await asyncio.shield(closing)
        self._responses.discard(response)
        self._response_closers.pop(response, None)

    def notify(self, callback: Callable[[], None]) -> None:
        with self._lock:
            if not self._closed and self._loop is not None:
                self._loop.call_soon_threadsafe(callback)

    async def _send(self, request: httpx.Request, deadline: float | None) -> httpx.Response:
        if self._client is None:
            raise RuntimeError("The transport is closed")
        # A fresh Request avoids consuming an outer sync-only request stream
        # on the async loop. Headers, URL and timeout extensions stay exact.
        inner = httpx.Request(
            request.method,
            request.url,
            headers=request.headers,
            content=request.content,
            extensions=request.extensions,
        )
        response = await _before_deadline(self._client.send(inner, stream=True), deadline)
        self._responses.add(response)
        return response

    def handle_request(self, request: httpx.Request) -> httpx.Response:
        request.read()
        deadline = cast(float | None, request.extensions.get("tungsten_deadline"))
        response = self.run(self._send(request, deadline))
        if (
            request.extensions.get("tungsten_stream")
            and 200 <= response.status_code <= 299
            and _is_event_stream(response)
        ):
            deadline = None  # After SSE headers, the configured idle timeout applies.
        body = _NativeSyncBody(self, response, deadline)
        return httpx.Response(
            response.status_code,
            headers=response.headers,
            stream=body,
            extensions={**response.extensions, "tungsten_interrupt": body.interrupt},
        )

    async def _shutdown(self) -> None:
        current = asyncio.current_task()
        tasks = [
            task
            for task in asyncio.all_tasks()
            if task is not current and task not in self._response_closers.values()
        ]
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        for response in list(self._responses):
            await self.close_response(response)
        await asyncio.gather(*self._response_closers.values())
        self._response_closers.clear()
        if self._client is not None:
            await self._client.aclose()

    def close(self) -> None:
        with self._lock:
            first = not self._closed
            self._closed = True
            loop, thread = self._loop, self._thread
            if self._finalizer is not None:
                self._finalizer.detach()
        if not first:
            self._stopped.wait()
            return
        try:
            if loop is not None and thread is not None:
                try:
                    asyncio.run_coroutine_threadsafe(self._shutdown(), loop).result()
                finally:
                    loop.call_soon_threadsafe(loop.stop)
                    thread.join()
        finally:
            self._stopped.set()


def _abandon_loop(
    loop: asyncio.AbstractEventLoop, thread: threading.Thread, client: httpx.AsyncClient
) -> None:
    """Release a dropped transport's loop thread: cancel its work, close the
    pool, stop the loop (whose thread then closes it). Runs when the
    transport is garbage collected without close(), or at interpreter exit;
    it waits for the thread unless it runs on that thread itself."""

    async def release() -> None:
        current = asyncio.current_task()
        tasks = [task for task in asyncio.all_tasks() if task is not current]
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        await client.aclose()

    def stop(_: object) -> None:
        with contextlib.suppress(RuntimeError):
            loop.call_soon_threadsafe(loop.stop)

    try:
        asyncio.run_coroutine_threadsafe(release(), loop).add_done_callback(stop)
    except RuntimeError:
        return  # The loop is already closed.
    if threading.current_thread() is not thread:
        thread.join(5)


class _NativeSyncBody(httpx.SyncByteStream):
    def __init__(self, transport: _NativeSyncTransport, response: httpx.Response, deadline: float | None):
        self._transport = transport
        self._response = response
        self._deadline = deadline
        self._chunks = response.aiter_raw()
        self._reading: asyncio.Task[Any] | None = None
        self._interrupted = threading.Event()

    async def _next(self) -> bytes:
        if self._interrupted.is_set():
            raise httpx.ReadError("The response was interrupted")
        self._reading = asyncio.current_task()
        try:
            return await _before_deadline(anext(self._chunks), self._deadline)
        except asyncio.CancelledError:
            raise httpx.ReadError("The response was interrupted") from None
        finally:
            self._reading = None

    def __iter__(self) -> Iterator[bytes]:
        while True:
            try:
                yield self._transport.run(self._next())
            except StopAsyncIteration:
                return

    def interrupt(self) -> None:
        self._interrupted.set()

        def cancel() -> None:
            if self._reading is not None:
                self._reading.cancel()

        self._transport.notify(cancel)

    async def _close(self) -> None:
        reading = self._reading
        if reading is not None:
            reading.cancel()
            await asyncio.gather(reading, return_exceptions=True)
        await self._transport.close_response(self._response)

    def close(self) -> None:
        self._transport.cleanup(self._close())


def async_http(transport: object) -> httpx.AsyncClient:
    return httpx.AsyncClient(
        transport=transport if isinstance(transport, httpx.AsyncBaseTransport) else None,
        follow_redirects=False,
        cookies=_no_cookies(),
        timeout=None,
    )


# ----------------------------------------------------- attempt outcomes

#: httpx errors raised before anything was written to the connection.
_NOT_SENT = (
    httpx.ConnectError,
    httpx.ConnectTimeout,
    httpx.PoolTimeout,
    httpx.UnsupportedProtocol,
    httpx.ProxyError,
)
#: Errnos of a failed ``connect()``; they mean nothing was sent only when
#: a transport raises them bare (httpx's own transports report the connect
#: phase as ``ConnectError``).
_NOT_SENT_ERRNOS = frozenset(
    getattr(errno, name)
    for name in ("ECONNREFUSED", "EHOSTUNREACH", "ENETUNREACH", "ENETDOWN", "EHOSTDOWN", "EADDRNOTAVAIL")
    if hasattr(errno, name)
)


def _chain(error: BaseException) -> list[BaseException]:
    """The exception and its causes and contexts, breadth first."""
    out: list[BaseException] = []
    pending: list[BaseException] = [error]
    while len(pending) > 0 and len(out) < 16:
        current = pending.pop(0)
        if any(current is seen for seen in out):
            continue
        out.append(current)
        pending.extend(e for e in (current.__cause__, current.__context__) if e is not None)
    return out


def _codes(error: BaseException) -> list[str]:
    codes: list[str] = []
    for e in _chain(error):
        if isinstance(e, socket.gaierror):
            codes.append("ENOTFOUND")
        elif isinstance(e, ssl.SSLCertVerificationError):
            codes.append("CERT_VERIFY_FAILED")
        elif isinstance(e, ssl.SSLError):
            codes.append("TLS_ERROR")
        elif isinstance(e, OSError) and e.errno is not None:
            codes.append(errno.errorcode.get(e.errno, f"E{e.errno}"))
    return list(dict.fromkeys(codes))


def _detail(error: BaseException) -> str:
    """A short, secret-free description (error codes, else the class name)."""
    codes = _codes(error)
    return ", ".join(codes) if len(codes) > 0 else type(error).__name__


def _send_failure(error: Exception) -> AttemptOutcome:
    """Classify a failed attempt by the phase it failed in. Only errors of
    the connect phase mean nothing was sent: httpx's connect errors, and a
    bare DNS, certificate or connect errno from another transport. Any
    other httpx transport error (read, write, protocol) can follow a
    request the server already read, whatever TLS alert or errno it wraps,
    so it is lost."""
    if isinstance(error, httpx.ConnectTimeout | httpx.PoolTimeout):
        return NotSent(_detail(error) if len(_codes(error)) > 0 else "connect timeout")
    if isinstance(error, httpx.TimeoutException):
        return TimedOut()
    if isinstance(error, _NOT_SENT):
        return NotSent(_detail(error))
    if isinstance(error, httpx.HTTPError):
        return Lost(_detail(error))
    if isinstance(error, socket.gaierror | ssl.SSLCertVerificationError) or (
        isinstance(error, OSError) and not isinstance(error, ssl.SSLError) and error.errno in _NOT_SENT_ERRNOS
    ):
        return NotSent(_detail(error))
    return Lost(_detail(error))


def _request(
    client: httpx.Client | httpx.AsyncClient, req: AttemptRequest, seconds: float, deadline: float
) -> httpx.Request:
    headers = [(name.encode("latin-1"), value.encode("latin-1")) for name, value in req.headers.items()]
    # A stream waits at most the idle timeout for each read (httpx's read timeout).
    idle = max(1.0, req.idle_ms) / 1000 if req.stream and req.idle_ms is not None else seconds
    return client.build_request(
        req.method,
        req.url,
        headers=headers,
        content=req.body,
        timeout=httpx.Timeout(seconds, read=idle),
        extensions={"tungsten_deadline": deadline, "tungsten_stream": req.stream},
    )


def _headers(response: httpx.Response) -> dict[str, str]:
    return {name.lower(): ", ".join(response.headers.get_list(name)) for name in response.headers}


def _deadline(req: AttemptRequest) -> tuple[float, float]:
    seconds = max(1.0, req.timeout_ms) / 1000
    return seconds, time.monotonic() + seconds


def _is_event_stream(response: httpx.Response) -> bool:
    media = response.headers.get("content-type", "").split(";")[0].strip().lower()
    return media == "text/event-stream"


def _read_failure(error: Exception) -> StreamFailed:
    return StreamFailed("timeout" if isinstance(error, httpx.TimeoutException) else "lost")


def _store_call(effect: StoreGet | StorePut | StoreReserve) -> object:
    store = cast(Any, effect.store)
    if isinstance(effect, StoreReserve):
        from .idempotency import reserve_key

        return reserve_key(store, effect.scope, effect.logical_id, effect.key)
    if isinstance(effect, StoreGet):
        return store.get(effect.scope, effect.logical_id)
    return store.put(effect.scope, effect.logical_id, effect.key)


def _discard(awaitable: object) -> None:
    close = getattr(awaitable, "close", None)
    if callable(close):
        close()


# ------------------------------------------------------------- stepping


@dataclass(frozen=True, slots=True)
class _Finished:
    value: object


def _advance(flow: Flow[Any], sent: object, thrown: Exception | None) -> Effect | _Finished:
    try:
        return flow.throw(thrown) if thrown is not None else flow.send(sent)
    except StopIteration as stop:
        return _Finished(stop.value)


def _drain(flow: Flow[Any], outcome: AttemptOutcome) -> object:
    """Finish a flow without I/O after its request's task was cancelled:
    every send is lost, sleeps are skipped, observers and stores are called
    without waiting. The envelope of the last failure the flow produced, or
    None."""
    from .types import Err

    failure: object = None
    sent: object = outcome
    thrown: Exception | None = None
    for _ in range(10_000):
        try:
            effect = _advance(flow, sent, thrown)
        except Exception:
            return failure
        sent, thrown = None, None
        if isinstance(effect, _Finished):
            return effect.value.error if isinstance(effect.value, Err) else failure
        if isinstance(effect, Bounded):
            effect = cast(Effect, effect.effect)
        if isinstance(effect, Emit):
            if isinstance(effect.item, Err):
                failure = effect.item.error
        elif isinstance(effect, Send):
            sent = Lost("cancelled")
        elif isinstance(effect, ReadChunk):
            sent = StreamFailed("lost")
        elif isinstance(effect, StoreGet | StorePut | StoreReserve):
            thrown = RuntimeError("the call was cancelled")
        elif isinstance(effect, Observe):
            try:
                result = effect.fn(*effect.args)
            except Exception:
                result = None
            if inspect.isawaitable(result):
                _discard(result)
                result = None
            sent = None
        elif isinstance(effect, Invoke | CloseStream):
            sent = None
        elif isinstance(effect, Shared):
            thrown = RuntimeError("the call was cancelled")
    flow.close()
    return failure


async def _quietly(awaitable: Awaitable[object]) -> None:
    with contextlib.suppress(Exception):
        await awaitable


#: Observer tasks started by a cancelled call, kept until they finish.
_REPORTS: set[asyncio.Future[None]] = set()


class SyncStream:
    """The unread body of an event stream on ``httpx.Client``. Each read waits
    at most the attempt timeout (httpx's read timeout), so an idle stream
    ends with a timeout."""

    __slots__ = ("_chunks", "response")

    def __init__(self, response: httpx.Response) -> None:
        self.response = response
        self._chunks = response.iter_bytes()

    def read(self) -> ReadOutcome:
        try:
            return Chunk(next(self._chunks))
        except StopIteration:
            return StreamEnded()
        except Exception as error:
            return _read_failure(error)

    def interrupt(self) -> None:
        """End a read another thread is blocked in: shut the socket down (a
        response closed from another thread does not wake it)."""
        interrupt = self.response.extensions.get("tungsten_interrupt")
        if callable(interrupt):
            interrupt()
            return
        network = self.response.extensions.get("network_stream")
        sock = network.get_extra_info("socket") if network is not None else None
        if sock is not None:
            with contextlib.suppress(OSError):
                sock.shutdown(socket.SHUT_RDWR)

    def close(self) -> None:
        self.response.close()


class _AsyncStream:
    """The unread body of an event stream on ``httpx.AsyncClient``."""

    __slots__ = ("_chunks", "response")

    def __init__(self, response: httpx.Response) -> None:
        self.response = response
        self._chunks = response.aiter_bytes()

    async def read(self) -> ReadOutcome:
        try:
            return Chunk(await anext(self._chunks))
        except StopAsyncIteration:
            return StreamEnded()
        except Exception as error:
            return _read_failure(error)

    async def aclose(self) -> None:
        await self.response.aclose()


# ------------------------------------------------------------------ sync


class _Flight:
    """One run of a shared flow, awaited by the threads that asked for it."""

    __slots__ = ("done", "error", "value")

    def __init__(self) -> None:
        self.done = threading.Event()
        self.error: Exception | None = None
        self.value = ""


class SyncDriver:
    """Performs effects with ``httpx.Client``."""

    __slots__ = ("_inflight", "_lock", "http")

    def __init__(self, http: httpx.Client) -> None:
        self.http = http
        self._lock = threading.Lock()
        self._inflight: dict[str, _Flight] = {}

    def _shared(self, effect: Shared) -> str:
        """Single flight: concurrent requests (threads) of one key share one run."""
        with self._lock:
            flight = self._inflight.get(effect.key)
            leader = flight is None
            if flight is None:
                flight = _Flight()
                self._inflight[effect.key] = flight
        if leader:
            try:
                flight.value = self.run(effect.flow())
            except Exception as error:
                flight.error = error
            finally:
                with self._lock:
                    self._inflight.pop(effect.key, None)
                flight.done.set()
        else:
            flight.done.wait()
        if flight.error is not None:
            raise flight.error
        return flight.value

    def run[T](self, flow: Flow[T]) -> T:
        """Run a flow that never emits to its result."""
        sent: object = None
        thrown: Exception | None = None
        while True:
            effect = _advance(flow, sent, thrown)
            if isinstance(effect, _Finished):
                return cast(T, effect.value)
            if isinstance(effect, Emit):
                flow.close()
                raise RuntimeError("a flow emitted an item outside an iteration")
            sent, thrown = self._perform_safely(effect)

    def iterate(self, flow: Flow[None], track: list[SyncStream] | None = None) -> Iterator[object]:
        """The items a flow emits, in order. Event streams the flow opened
        are closed when the iteration ends, however it ends; with ``track``
        they are also appended to it as they open, so another thread can
        close them to end a blocked read."""
        sent: object = None
        thrown: Exception | None = None
        opened: list[SyncStream] = track if track is not None else []
        try:
            while True:
                effect = _advance(flow, sent, thrown)
                if isinstance(effect, _Finished):
                    return
                if isinstance(effect, Emit):
                    sent, thrown = None, None
                    yield effect.item
                    continue
                sent, thrown = self._perform_safely(effect)
                if isinstance(sent, Streaming):
                    opened.append(cast(SyncStream, sent.stream))
        finally:
            flow.close()
            for stream in opened:
                stream.close()

    def _perform_safely(self, effect: Effect) -> tuple[object, Exception | None]:
        try:
            return self._perform(effect), None
        except Exception as error:
            return None, error

    _poll_workers = threading.BoundedSemaphore(16)

    def _perform(self, effect: Effect) -> object:
        if isinstance(effect, Bounded):
            if isinstance(effect.effect, Send):
                effect = replace(
                    effect, effect=Send(replace(effect.effect.request, deadline_ms=effect.deadline_ms))
                )
            remaining = effect.deadline_ms / 1000 - time.monotonic()
            if remaining <= 0:
                raise PollExpired()
            if isinstance(effect.effect, Sleep):
                # A wait needs no worker: it runs here for at most the time
                # left, so an expiring poll never holds a worker permit.
                delay = max(0.0, effect.effect.ms) / 1000
                time.sleep(min(delay, remaining))
                if delay >= remaining:
                    raise PollExpired()
                return None
            # Workers run only what can block for an unbounded time: the
            # attempt and the caller's callbacks (stores, hooks, token sources).
            if not self._poll_workers.acquire(timeout=remaining):
                raise PollExpired()
            completed = threading.Event()
            bounded_result: list[object] = []

            def perform() -> None:
                try:
                    if time.monotonic() * 1000 >= effect.deadline_ms:
                        bounded_result.extend((None, PollExpired()))
                    else:
                        bounded_result.extend(self._perform_safely(cast(Effect, effect.effect)))
                finally:
                    self._poll_workers.release()
                    completed.set()

            # An abandoned synchronous effect cannot resume the calling flow.
            # In-flight network work may finish, so mutations remain unknown.
            threading.Thread(target=perform, daemon=True).start()
            if not completed.wait(max(0, effect.deadline_ms / 1000 - time.monotonic())):
                if isinstance(effect.effect, Send):
                    return TimedOut()
                raise PollExpired()
            value, error = bounded_result
            if isinstance(error, Exception):
                raise error
            return value
        if isinstance(effect, Send):
            return self._attempt(effect.request)
        if isinstance(effect, ReadChunk):
            return cast(SyncStream, effect.stream).read()
        if isinstance(effect, CloseStream):
            cast(SyncStream, effect.stream).close()
            return None
        if isinstance(effect, Sleep):
            time.sleep(max(0.0, effect.ms) / 1000)
            return None
        if isinstance(effect, StoreGet | StorePut | StoreReserve):
            result = _store_call(effect)
            if inspect.isawaitable(result):
                _discard(result)
                raise TypeError("the idempotency store is asynchronous; use it with AsyncClientCore")
            return result
        if isinstance(effect, Observe):
            try:
                result = effect.fn(*effect.args)
                if inspect.isawaitable(result):
                    _discard(result)
            except Exception:
                pass  # observers never change the call
            return None
        if isinstance(effect, Invoke):
            result = effect.fn(*effect.args)
            if inspect.isawaitable(result):
                _discard(result)
                raise TypeError("the token store is asynchronous; use it with AsyncClientCore")
            return result
        if isinstance(effect, Shared):
            return self._shared(effect)
        raise TypeError(f"unknown effect {type(effect).__name__}")

    def _attempt(self, req: AttemptRequest) -> AttemptOutcome:
        seconds, deadline = _deadline(req)
        if req.deadline_ms is not None:
            deadline = min(deadline, req.deadline_ms / 1000)
            seconds = max(0.001, min(seconds, deadline - time.monotonic()))
        try:
            request = _request(self.http, req, seconds, deadline)
        except Exception as error:
            return NotSent(_detail(error))
        try:
            if time.monotonic() >= deadline:
                return NotSent("polling deadline expired before send")
            response = self.http.send(request, stream=True)
        except Exception as error:
            return _send_failure(error)
        if time.monotonic() >= deadline:
            response.close()
            return TimedOut()
        if req.stream and 200 <= response.status_code <= 299 and _is_event_stream(response):
            return Streaming(response.status_code, _headers(response), SyncStream(response))
        try:
            chunks: list[bytes] = []
            failure: Literal["timeout", "broken"] | None = None
            try:
                for chunk in response.iter_bytes():
                    chunks.append(chunk)
                    if time.monotonic() > deadline:
                        failure = "timeout"
                        break
            except httpx.TimeoutException:
                failure = "timeout"
            except Exception:
                failure = "broken"
            if time.monotonic() >= deadline:
                failure = "timeout"
            return Answered(
                response.status_code, _headers(response), None if failure else b"".join(chunks), failure
            )
        finally:
            response.close()


# ----------------------------------------------------------------- async


class AsyncDriver:
    """Performs effects with ``httpx.AsyncClient``; observers and stores may
    be plain or return awaitables."""

    __slots__ = ("_inflight", "http", "on_cancelled")

    def __init__(
        self, http: httpx.AsyncClient, on_cancelled: Callable[[object], object] | None = None
    ) -> None:
        self.http = http
        self._inflight: dict[str, asyncio.Future[str]] = {}
        #: Receives the envelope of a call cancelled while a request was in
        #: flight (``ClientOptions.on_diagnostic``).
        self.on_cancelled = on_cancelled

    async def run[T](self, flow: Flow[T]) -> T:
        """Run a flow that never emits to its result."""
        sent: object = None
        thrown: Exception | None = None
        while True:
            effect = _advance(flow, sent, thrown)
            if isinstance(effect, _Finished):
                return cast(T, effect.value)
            if isinstance(effect, Emit):
                flow.close()
                raise RuntimeError("a flow emitted an item outside an iteration")
            sent, thrown = await self._perform_cancellable(flow, effect)

    async def iterate(self, flow: Flow[None]) -> AsyncIterator[object]:
        """The items a flow emits, in order. Event streams the flow opened
        are closed when the iteration ends, however it ends."""
        sent: object = None
        thrown: Exception | None = None
        opened: list[_AsyncStream] = []
        try:
            while True:
                effect = _advance(flow, sent, thrown)
                if isinstance(effect, _Finished):
                    return
                if isinstance(effect, Emit):
                    sent, thrown = None, None
                    yield effect.item
                    continue
                sent, thrown = await self._perform_cancellable(flow, effect)
                if isinstance(sent, Streaming):
                    opened.append(cast(_AsyncStream, sent.stream))
        finally:
            flow.close()
            for stream in opened:
                await stream.aclose()

    async def _perform_cancellable(self, flow: Flow[Any], effect: Effect) -> tuple[object, Exception | None]:
        """Perform an effect; when the task is cancelled while a request is
        in flight (``asyncio.timeout``, ``wait_for``), the request may have
        reached the server: the flow is finished without I/O as if the
        connection was lost, its envelope (``OUTCOME_UNKNOWN`` for a
        mutation) goes to ``on_cancelled``, and the cancellation propagates."""
        try:
            return await self._perform_safely(effect)
        except asyncio.CancelledError:
            effective = effect.effect if isinstance(effect, Bounded) else effect
            if isinstance(effective, Send):
                self._report(_drain(flow, Lost("cancelled")))
            flow.close()
            raise

    def _report(self, error: object) -> None:
        if error is None or self.on_cancelled is None:
            return
        try:
            result = self.on_cancelled(error)
        except Exception:
            return
        if inspect.isawaitable(result):
            task = asyncio.ensure_future(_quietly(cast(Awaitable[object], result)))
            _REPORTS.add(task)
            task.add_done_callback(_REPORTS.discard)

    async def _perform_safely(self, effect: Effect) -> tuple[object, Exception | None]:
        try:
            return await self._perform(effect), None
        except Exception as error:
            return None, error

    async def _perform(self, effect: Effect) -> object:
        if isinstance(effect, Bounded):
            if isinstance(effect.effect, Send):
                effect = replace(
                    effect, effect=Send(replace(effect.effect.request, deadline_ms=effect.deadline_ms))
                )
            remaining = effect.deadline_ms / 1000 - time.monotonic()
            if remaining <= 0:
                raise PollExpired()
            try:
                async with asyncio.timeout(remaining):
                    inner = effect.effect
                    if isinstance(inner, Invoke | Observe):
                        try:
                            value = await asyncio.to_thread(inner.fn, *inner.args)
                            if inspect.isawaitable(value):
                                value = await cast(Awaitable[object], value)
                            return None if isinstance(inner, Observe) else value
                        except Exception:
                            if isinstance(inner, Observe):
                                return None
                            raise
                    return await self._perform(cast(Effect, inner))
            except TimeoutError:
                if isinstance(effect.effect, Send):
                    return TimedOut()
                raise PollExpired() from None
        if isinstance(effect, Send):
            return await self._attempt(effect.request)
        if isinstance(effect, ReadChunk):
            return await cast(_AsyncStream, effect.stream).read()
        if isinstance(effect, CloseStream):
            await cast(_AsyncStream, effect.stream).aclose()
            return None
        if isinstance(effect, Sleep):
            await asyncio.sleep(max(0.0, effect.ms) / 1000)
            return None
        if isinstance(effect, StoreGet | StorePut | StoreReserve):
            from .idempotency import async_reserve_key, async_store_method

            if isinstance(effect, StoreReserve):
                return await async_reserve_key(effect.store, effect.scope, effect.logical_id, effect.key)
            method = getattr(effect.store, "get" if isinstance(effect, StoreGet) else "put")
            args = (
                (effect.scope, effect.logical_id)
                if isinstance(effect, StoreGet)
                else (effect.scope, effect.logical_id, effect.key)
            )
            return await async_store_method(method, *args)
        if isinstance(effect, Observe):
            try:
                result = effect.fn(*effect.args)
                if inspect.isawaitable(result):
                    await cast(Awaitable[object], result)
            except Exception:
                pass  # observers never change the call
            return None
        if isinstance(effect, Invoke):
            result = effect.fn(*effect.args)
            if inspect.isawaitable(result):
                return await cast(Awaitable[object], result)
            return result
        if isinstance(effect, Shared):
            return await self._shared(effect)
        raise TypeError(f"unknown effect {type(effect).__name__}")

    async def _shared(self, effect: Shared) -> str:
        """Single flight: concurrent requests of one key share one run."""
        running = self._inflight.get(effect.key)
        if running is None:
            running = asyncio.ensure_future(self.run(effect.flow()))
            self._inflight[effect.key] = running
            key = effect.key

            def forget(_: asyncio.Future[str]) -> None:
                self._inflight.pop(key, None)

            running.add_done_callback(forget)
        return await asyncio.shield(running)

    async def _attempt(self, req: AttemptRequest) -> AttemptOutcome:
        seconds, deadline = _deadline(req)
        if req.deadline_ms is not None:
            deadline = min(deadline, req.deadline_ms / 1000)
            seconds = max(0.001, min(seconds, deadline - time.monotonic()))
        return await self._attempt_until(req, seconds, deadline)

    async def _attempt_until(self, req: AttemptRequest, seconds: float, deadline: float) -> AttemptOutcome:
        try:
            request = _request(self.http, req, seconds, deadline)
        except Exception as error:
            return NotSent(_detail(error))
        try:
            if time.monotonic() >= deadline:
                return NotSent("polling deadline expired before send")
            response = await _before_deadline(self.http.send(request, stream=True), deadline)
        except Exception as error:
            return _send_failure(error)
        if time.monotonic() >= deadline:
            await response.aclose()
            return TimedOut()
        if req.stream and 200 <= response.status_code <= 299 and _is_event_stream(response):
            return Streaming(response.status_code, _headers(response), _AsyncStream(response))
        try:
            chunks: list[bytes] = []
            failure: Literal["timeout", "broken"] | None = None

            async def read_body() -> None:
                async for chunk in response.aiter_bytes():
                    chunks.append(chunk)

            try:
                await _before_deadline(read_body(), deadline)
            except httpx.TimeoutException:
                failure = "timeout"
            except Exception:
                failure = "broken"
            if time.monotonic() >= deadline:
                failure = "timeout"
            return Answered(
                response.status_code, _headers(response), None if failure else b"".join(chunks), failure
            )
        finally:
            await response.aclose()
