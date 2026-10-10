# SPDX-License-Identifier: Apache-2.0
"""Run flows (``_effects``): the synchronous driver performs effects with
``httpx.Client`` and ``time.sleep``, the asynchronous one with
``httpx.AsyncClient`` and ``asyncio.sleep``. One HTTP attempt has a
deadline covering connect, send and the body; redirects are never followed
by httpx; failures are classified by whether the request can have reached
the server."""

from __future__ import annotations

import asyncio
import contextlib
import errno
import inspect
import socket
import ssl
import threading
import time
from collections.abc import AsyncIterator, Awaitable, Callable, Iterator
from dataclasses import dataclass
from http.cookiejar import CookieJar, DefaultCookiePolicy
from typing import Any, Literal, cast

import httpx

from ._effects import (
    Answered,
    AttemptOutcome,
    AttemptRequest,
    Chunk,
    CloseStream,
    Effect,
    Emit,
    Flow,
    Invoke,
    Lost,
    NotSent,
    Observe,
    ReadChunk,
    ReadOutcome,
    Send,
    Shared,
    Sleep,
    StoreGet,
    StorePut,
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
        transport=transport if isinstance(transport, httpx.BaseTransport) else None,
        follow_redirects=False,
        cookies=_no_cookies(),
        timeout=None,
    )


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


def _request(client: httpx.Client | httpx.AsyncClient, req: AttemptRequest, seconds: float) -> httpx.Request:
    headers = [(name.encode("latin-1"), value.encode("latin-1")) for name, value in req.headers.items()]
    # A stream waits at most the idle timeout for each read (httpx's read timeout).
    idle = max(1.0, req.idle_ms) / 1000 if req.stream and req.idle_ms is not None else seconds
    return client.build_request(
        req.method,
        req.url,
        headers=headers,
        content=req.body,
        timeout=httpx.Timeout(seconds, read=idle),
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


def _store_call(effect: StoreGet | StorePut) -> object:
    store = cast(Any, effect.store)
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
        if isinstance(effect, Emit):
            if isinstance(effect.item, Err):
                failure = effect.item.error
        elif isinstance(effect, Send):
            sent = Lost("cancelled")
        elif isinstance(effect, ReadChunk):
            sent = StreamFailed("lost")
        elif isinstance(effect, StoreGet | StorePut | Observe):
            try:
                result = _store_call(effect) if not isinstance(effect, Observe) else effect.fn(*effect.args)
            except Exception:
                result = None
            if inspect.isawaitable(result):
                _discard(result)
                result = None
            sent = None if isinstance(effect, Observe) else result
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

    def _perform(self, effect: Effect) -> object:
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
        if isinstance(effect, StoreGet | StorePut):
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
        try:
            request = _request(self.http, req, seconds)
        except Exception as error:
            return NotSent(_detail(error))
        try:
            response = self.http.send(request, stream=True)
        except Exception as error:
            return _send_failure(error)
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
            if isinstance(effect, Send):
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
        if isinstance(effect, StoreGet | StorePut):
            result = _store_call(effect)
            if inspect.isawaitable(result):
                return await cast(Awaitable[object], result)
            return result
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
        try:
            request = _request(self.http, req, seconds)
        except Exception as error:
            return NotSent(_detail(error))
        try:
            response = await self.http.send(request, stream=True)
        except Exception as error:
            return _send_failure(error)
        if req.stream and 200 <= response.status_code <= 299 and _is_event_stream(response):
            return Streaming(response.status_code, _headers(response), _AsyncStream(response))
        try:
            chunks: list[bytes] = []
            failure: Literal["timeout", "broken"] | None = None
            try:
                async for chunk in response.aiter_bytes():
                    chunks.append(chunk)
                    if time.monotonic() > deadline:
                        failure = "timeout"
                        break
            except httpx.TimeoutException:
                failure = "timeout"
            except Exception:
                failure = "broken"
            return Answered(
                response.status_code, _headers(response), None if failure else b"".join(chunks), failure
            )
        finally:
            await response.aclose()
