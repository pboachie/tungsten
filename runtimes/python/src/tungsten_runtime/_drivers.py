# SPDX-License-Identifier: Apache-2.0
"""Run flows (``_effects``): the synchronous driver performs effects with
``httpx.Client`` and ``time.sleep``, the asynchronous one with
``httpx.AsyncClient`` and ``asyncio.sleep``. One HTTP attempt has a
deadline covering native socket I/O and the body: the synchronous transport
bounds every socket operation by it on the caller's thread (no helper
thread), the asynchronous one cancels the attempt. Redirects are never
followed by httpx; failures are classified by whether the request can have
reached the server. Synchronous custom transports remain on the caller's
thread with httpx's per-phase timeouts: their callbacks must cooperate with
timeouts, since Python cannot safely interrupt arbitrary blocking code. Late
custom results are rejected only after the work and cleanup finish. Async
cancellation likewise waits for cleanup; non-cooperative callbacks and OS DNS
resolution are not hard real-time guarantees."""

from __future__ import annotations

import asyncio
import contextlib
import errno
import inspect
import socket
import ssl
import sys
import threading
import time
import weakref
from collections.abc import AsyncIterator, Awaitable, Callable, Iterable, Iterator
from dataclasses import dataclass, replace
from http.cookiejar import CookieJar, DefaultCookiePolicy
from typing import Any, Literal, TypeVar, cast

import httpcore
import httpx

from ._effects import (
    Acquire,
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
from .scheduler import RequestScheduler

T = TypeVar("T")


if sys.version_info >= (3, 11):
    from asyncio import timeout as _timeout
else:
    from async_timeout import timeout as _timeout


def _oversized_length(response: httpx.Response, maximum: int | None) -> bool:
    value = response.headers.get("content-length")
    if maximum is None or value is None or not value.isascii() or not value.isdigit():
        return False
    digits = value.lstrip("0") or "0"
    bound = str(maximum)
    return len(digits) > len(bound) or (len(digits) == len(bound) and digits > bound)


def _no_cookies() -> CookieJar:
    """A jar that keeps nothing: ``Set-Cookie`` answers never reach later
    requests; cookies are only what the auth profile and the call say."""
    return CookieJar(policy=DefaultCookiePolicy(allowed_domains=[]))


def sync_http(transport: object) -> httpx.Client:
    return _DeadlineClient(
        transport=transport if isinstance(transport, httpx.BaseTransport) else None,
        follow_redirects=False,
        cookies=_no_cookies(),
        timeout=None,
    )


class _DeadlineClient(httpx.Client):
    """Keep httpx's proxy/NO_PROXY routing, with deadlines on every route."""

    def _init_transport(self, *args: Any, **kwargs: Any) -> httpx.BaseTransport:
        # HTTPX 0.27 passes app=None and orders its factory parameters
        # differently from 0.28. Keep upstream ownership of custom transports.
        if kwargs.get("transport") is not None or kwargs.get("app") is not None:
            return super()._init_transport(*args, **kwargs)
        kwargs.pop("transport", None)
        kwargs.pop("app", None)
        return _DeadlineTransport(*args, **kwargs)

    def _init_proxy_transport(self, proxy: httpx.Proxy, *args: Any, **kwargs: Any) -> httpx.BaseTransport:
        return _DeadlineTransport(*args, proxy=proxy, **kwargs)


async def _before_deadline(work: Awaitable[T], deadline: float | None) -> T:
    if deadline is None:
        return await work
    try:
        async with _timeout(max(0.0, deadline - time.monotonic())):
            return await work
    except (TimeoutError, asyncio.TimeoutError):
        raise httpx.ReadTimeout("The attempt deadline expired") from None


class _Deadline(threading.local):
    """The attempt deadline (``time.monotonic()``) of the request the current
    thread is sending or reading, or None. A request runs on one thread, from
    its first socket write to the end of its body, so the socket layer below
    finds its deadline here."""

    until: float | None = None


_DEADLINE = _Deadline()


def _bounded(timeout: float | None, expired: type[httpcore.TimeoutException]) -> float | None:
    """A socket operation's timeout: httpx's phase timeout, cut to the time
    left before the attempt deadline; it raises once that has passed."""
    until = _DEADLINE.until
    if until is None:
        return timeout
    left = until - time.monotonic()
    if left <= 0:
        raise expired("The attempt deadline expired")
    return left if timeout is None or left < timeout else timeout


class _Sockets:
    """The open sockets of one transport, so closing it can end the reads
    other threads are blocked in. Shared by the backend and its streams, which
    therefore never reference the transport: a dropped transport is collected
    and its finalizer closes the pool."""

    __slots__ = ("_lock", "_streams")

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._streams: set[_DeadlineStream] = set()

    def track(self, stream: _DeadlineStream) -> None:
        with self._lock:
            self._streams.add(stream)

    def forget(self, stream: _DeadlineStream) -> None:
        with self._lock:
            self._streams.discard(stream)

    def interrupt(self) -> None:
        with self._lock:
            streams = list(self._streams)
        for stream in streams:
            stream.interrupt()

    def __len__(self) -> int:
        with self._lock:
            return len(self._streams)


class _DeadlineStream(httpcore.NetworkStream):
    """A socket whose every operation ends by the attempt deadline."""

    __slots__ = ("_inner", "_sockets")

    def __init__(self, inner: httpcore.NetworkStream, sockets: _Sockets) -> None:
        self._inner = inner
        self._sockets = sockets
        sockets.track(self)

    def read(self, max_bytes: int, timeout: float | None = None) -> bytes:
        return self._inner.read(max_bytes, _bounded(timeout, httpcore.ReadTimeout))

    def write(self, buffer: bytes, timeout: float | None = None) -> None:
        self._inner.write(buffer, _bounded(timeout, httpcore.WriteTimeout))

    def close(self) -> None:
        self._sockets.forget(self)
        self._inner.close()

    def start_tls(
        self,
        ssl_context: ssl.SSLContext,
        server_hostname: str | None = None,
        timeout: float | None = None,
    ) -> httpcore.NetworkStream:
        try:
            bounded = _bounded(timeout, httpcore.ConnectTimeout)
            upgraded = self._inner.start_tls(ssl_context, server_hostname, bounded)
        except BaseException:
            # The pool does not own this stream until TLS succeeds. Release it
            # even when the deadline expires before the handshake starts.
            with contextlib.suppress(Exception):
                self.close()
            raise
        self._sockets.forget(self)
        return _DeadlineStream(upgraded, self._sockets)

    def get_extra_info(self, info: str) -> Any:
        return self._inner.get_extra_info(info)

    def interrupt(self) -> None:
        """End an operation another thread is blocked in: shut the socket
        down (a socket closed from another thread does not wake it)."""
        sock = self._inner.get_extra_info("socket")
        if isinstance(sock, socket.socket):
            with contextlib.suppress(OSError):
                sock.shutdown(socket.SHUT_RDWR)


class _DeadlineBackend(httpcore.NetworkBackend):
    """httpcore's own blocking sockets, each wrapped to honour the deadline."""

    __slots__ = ("_inner", "_sockets")

    def __init__(self, sockets: _Sockets) -> None:
        self._inner = httpcore.SyncBackend()
        self._sockets = sockets

    def connect_tcp(
        self,
        host: str,
        port: int,
        timeout: float | None = None,
        local_address: str | None = None,
        socket_options: Iterable[tuple[Any, ...]] | None = None,
    ) -> httpcore.NetworkStream:
        timeout = _bounded(timeout, httpcore.ConnectTimeout)
        stream = self._inner.connect_tcp(host, port, timeout, local_address, socket_options)
        return _DeadlineStream(stream, self._sockets)

    def connect_unix_socket(
        self,
        path: str,
        timeout: float | None = None,
        socket_options: Iterable[tuple[Any, ...]] | None = None,
    ) -> httpcore.NetworkStream:
        timeout = _bounded(timeout, httpcore.ConnectTimeout)
        stream = self._inner.connect_unix_socket(path, timeout, socket_options)
        return _DeadlineStream(stream, self._sockets)

    def sleep(self, seconds: float) -> None:
        self._inner.sleep(seconds)


class _DeadlineTransport(httpx.HTTPTransport):
    """httpx's synchronous transport (its connection pool, TLS defaults and
    error mapping) whose sockets honour an attempt deadline: every socket
    operation of a request carrying ``tungsten_deadline`` waits at most until
    that instant, through the connect, TLS, write, headers and body phases,
    whatever the per-phase timeouts allow. Nothing runs on another thread: a
    blocked call spends its time in a socket wait, and that is where the
    deadline ends it. Closing the transport shuts down the sockets other
    threads are blocked in, so they return at once."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self._sockets = _Sockets()
        # Preserve httpx's direct, HTTP(S) or SOCKS pool, TLS configuration and
        # limits. No connection exists yet; every new socket uses this backend.
        # httpx exposes no backend hook. Keep this intentional httpcore
        # integration exception local; assignment types remain checked.
        backend = _DeadlineBackend(self._sockets)
        self._pool._network_backend = backend  # pyright: ignore[reportPrivateUsage]
        # Closes the pool of a transport that is dropped (its client never
        # closed), so its sockets are released cleanly; close() detaches it.
        self._finalizer = weakref.finalize(self, _close_pool, self._pool)

    def handle_request(self, request: httpx.Request) -> httpx.Response:
        deadline = cast(float | None, request.extensions.get("tungsten_deadline"))
        local = _DEADLINE
        local.until = deadline
        try:
            response = super().handle_request(request)
        finally:
            local.until = None
        if deadline is None:
            return response
        if (
            request.extensions.get("tungsten_stream")
            and 200 <= response.status_code <= 299
            and _is_event_stream(response)
        ):
            return response  # After SSE headers, the configured idle timeout applies.
        response.stream = _DeadlineBody(response.stream, deadline)
        return response

    def close(self) -> None:
        self._sockets.interrupt()
        self._finalizer.detach()
        super().close()


def _close_pool(pool: httpcore.ConnectionPool) -> None:
    with contextlib.suppress(Exception):
        pool.close()


class _DeadlineBody(httpx.SyncByteStream):
    """The body of a response, read under its request's deadline."""

    __slots__ = ("_deadline", "_inner")

    def __init__(self, inner: httpx.SyncByteStream | httpx.AsyncByteStream, deadline: float) -> None:
        self._inner = inner
        self._deadline = deadline

    def __iter__(self) -> Iterator[bytes]:
        local = _DEADLINE
        chunks = iter(cast(httpx.SyncByteStream, self._inner))
        while True:
            local.until = self._deadline
            try:
                chunk = next(chunks)
            except StopIteration:
                return
            finally:
                local.until = None
            yield chunk

    def close(self) -> None:
        close = getattr(self._inner, "close", None)
        if callable(close):
            close()


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
    """Headers by lower-case name; repeated names joined with ``, ``."""
    out: dict[str, str] = {}
    for name, value in response.headers.multi_items():
        out[name] = value if name not in out else f"{out[name]}, {value}"
    return out


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


def _earliest(first: float | None, second: float | None) -> float | None:
    return second if first is None else first if second is None else min(first, second)


def _closed(stream: SyncStream | _AsyncStream) -> None:
    on_close, stream.on_close = stream.on_close, None
    if on_close is not None:
        on_close()


class SyncStream:
    """The unread body of an event stream on ``httpx.Client``. Each read waits
    at most the attempt timeout (httpx's read timeout), so an idle stream
    ends with a timeout."""

    __slots__ = ("_chunks", "on_close", "response")

    def __init__(self, response: httpx.Response) -> None:
        self.response = response
        self._chunks = response.iter_bytes()
        #: Runs once when the stream is closed (frees scheduler capacity).
        self.on_close: Callable[[], None] | None = None

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
        network = self.response.extensions.get("network_stream")
        if isinstance(network, _DeadlineStream):
            network.interrupt()
            return
        sock = network.get_extra_info("socket") if network is not None else None
        if sock is not None:
            with contextlib.suppress(OSError):
                sock.shutdown(socket.SHUT_RDWR)

    def close(self) -> None:
        try:
            self.response.close()
        finally:
            _closed(self)


class _AsyncStream:
    """The unread body of an event stream on ``httpx.AsyncClient``."""

    __slots__ = ("_chunks", "on_close", "response")

    def __init__(self, response: httpx.Response) -> None:
        self.response = response
        self._chunks = response.aiter_bytes()
        #: Runs once when the stream is closed (frees scheduler capacity).
        self.on_close: Callable[[], None] | None = None

    async def read(self) -> ReadOutcome:
        try:
            return Chunk(await anext(self._chunks))
        except StopAsyncIteration:
            return StreamEnded()
        except Exception as error:
            return _read_failure(error)

    async def aclose(self) -> None:
        try:
            await self.response.aclose()
        finally:
            _closed(self)


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

    def run(self, flow: Flow[T]) -> T:
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
        if isinstance(effect, Bounded) and isinstance(effect.effect, Acquire):
            inner = effect.effect
            return cast(RequestScheduler, inner.scheduler).acquire(
                inner.key, inner.kind, _earliest(inner.deadline_ms, effect.deadline_ms), inner.random
            )
        if isinstance(effect, Acquire):
            return cast(RequestScheduler, effect.scheduler).acquire(
                effect.key, effect.kind, effect.deadline_ms, effect.random
            )
        if isinstance(effect, Bounded):
            if isinstance(effect.effect, Send):
                request = effect.effect.request
                limit = (
                    effect.deadline_ms
                    if request.deadline_ms is None
                    else min(request.deadline_ms, effect.deadline_ms)
                )
                effect = replace(effect, effect=Send(replace(request, deadline_ms=limit)))
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
                return NotSent("deadline expired before send")
            response = self.http.send(request, stream=True)
        except Exception as error:
            return _send_failure(error)
        if time.monotonic() >= deadline:
            response.close()
            return TimedOut()
        if req.stream and 200 <= response.status_code <= 299 and _is_event_stream(response):
            return Streaming(response.status_code, _headers(response), SyncStream(response))
        maximum = req.max_response_bytes
        try:
            chunks: list[bytes] = []
            failure: Literal["timeout", "broken", "oversized"] | None = None
            try:
                size = 0
                if (
                    req.method != "HEAD"
                    and response.status_code not in (204, 205, 304)
                    and _oversized_length(response, maximum)
                ):
                    failure = "oversized"
                else:
                    for chunk in response.iter_bytes():
                        size += len(chunk)
                        if maximum is not None and size > maximum:
                            failure = "oversized"
                            break
                        chunks.append(chunk)
                        if time.monotonic() > deadline:
                            failure = "timeout"
                            break
            except httpx.TimeoutException:
                failure = "timeout"
            except Exception:
                failure = "broken"
            if time.monotonic() >= deadline and failure != "oversized":
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

    async def run(self, flow: Flow[T]) -> T:
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
        if isinstance(effect, Bounded) and isinstance(effect.effect, Acquire):
            inner = effect.effect
            return await cast(RequestScheduler, inner.scheduler).acquire_async(
                inner.key, inner.kind, _earliest(inner.deadline_ms, effect.deadline_ms), inner.random
            )
        if isinstance(effect, Acquire):
            return await cast(RequestScheduler, effect.scheduler).acquire_async(
                effect.key, effect.kind, effect.deadline_ms, effect.random
            )
        if isinstance(effect, Bounded):
            if isinstance(effect.effect, Send):
                request = effect.effect.request
                limit = (
                    effect.deadline_ms
                    if request.deadline_ms is None
                    else min(request.deadline_ms, effect.deadline_ms)
                )
                effect = replace(effect, effect=Send(replace(request, deadline_ms=limit)))
            remaining = effect.deadline_ms / 1000 - time.monotonic()
            if remaining <= 0:
                raise PollExpired()
            try:
                async with _timeout(remaining):
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
            except (TimeoutError, asyncio.TimeoutError):
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
                return NotSent("deadline expired before send")
            response = await _before_deadline(self.http.send(request, stream=True), deadline)
        except Exception as error:
            return _send_failure(error)
        if time.monotonic() >= deadline:
            await response.aclose()
            return TimedOut()
        if req.stream and 200 <= response.status_code <= 299 and _is_event_stream(response):
            return Streaming(response.status_code, _headers(response), _AsyncStream(response))
        maximum = req.max_response_bytes
        try:
            chunks: list[bytes] = []
            failure: Literal["timeout", "broken", "oversized"] | None = None

            async def read_body() -> Literal["oversized"] | None:
                if (
                    req.method != "HEAD"
                    and response.status_code not in (204, 205, 304)
                    and _oversized_length(response, maximum)
                ):
                    return "oversized"
                size = 0
                async for chunk in response.aiter_bytes():
                    size += len(chunk)
                    if maximum is not None and size > maximum:
                        return "oversized"
                    chunks.append(chunk)
                return None

            try:
                failure = await _before_deadline(read_body(), deadline)
            except httpx.TimeoutException:
                failure = "timeout"
            except Exception:
                failure = "broken"
            if time.monotonic() >= deadline and failure != "oversized":
                failure = "timeout"
            return Answered(
                response.status_code, _headers(response), None if failure else b"".join(chunks), failure
            )
        finally:
            await response.aclose()
