# SPDX-License-Identifier: Apache-2.0
"""Run flows (``_effects``): the synchronous driver performs effects with
``httpx.Client`` and ``time.sleep``, the asynchronous one with
``httpx.AsyncClient`` and ``asyncio.sleep``. One HTTP attempt has a
deadline covering connect, send and the body; redirects are never followed
by httpx; failures are classified by whether the request can have reached
the server."""

from __future__ import annotations

import asyncio
import errno
import inspect
import socket
import ssl
import time
from collections.abc import AsyncIterator, Awaitable, Iterator
from dataclasses import dataclass
from http.cookiejar import CookieJar, DefaultCookiePolicy
from typing import Any, Literal, cast

import httpx

from ._effects import (
    Answered,
    AttemptOutcome,
    AttemptRequest,
    Effect,
    Emit,
    Flow,
    Lost,
    NotSent,
    Observe,
    Send,
    Shared,
    Sleep,
    StoreGet,
    StorePut,
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
    if isinstance(error, httpx.ConnectTimeout | httpx.PoolTimeout):
        return NotSent(_detail(error) if len(_codes(error)) > 0 else "connect timeout")
    if isinstance(error, httpx.TimeoutException):
        return TimedOut()
    chain = _chain(error)
    if isinstance(error, _NOT_SENT) or any(
        isinstance(e, socket.gaierror | ssl.SSLError)
        or (isinstance(e, OSError) and e.errno in _NOT_SENT_ERRNOS)
        for e in chain
    ):
        return NotSent(_detail(error))
    return Lost(_detail(error))


def _request(client: httpx.Client | httpx.AsyncClient, req: AttemptRequest, seconds: float) -> httpx.Request:
    headers = [(name.encode("latin-1"), value.encode("latin-1")) for name, value in req.headers.items()]
    return client.build_request(
        req.method, req.url, headers=headers, content=req.body, timeout=httpx.Timeout(seconds)
    )


def _headers(response: httpx.Response) -> dict[str, str]:
    return {name.lower(): ", ".join(response.headers.get_list(name)) for name in response.headers}


def _deadline(req: AttemptRequest) -> tuple[float, float]:
    seconds = max(1.0, req.timeout_ms) / 1000
    return seconds, time.monotonic() + seconds


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


# ------------------------------------------------------------------ sync


class SyncDriver:
    """Performs effects with ``httpx.Client``."""

    __slots__ = ("http",)

    def __init__(self, http: httpx.Client) -> None:
        self.http = http

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

    def iterate(self, flow: Flow[None]) -> Iterator[object]:
        """The items a flow emits, in order."""
        sent: object = None
        thrown: Exception | None = None
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
        finally:
            flow.close()

    def _perform_safely(self, effect: Effect) -> tuple[object, Exception | None]:
        try:
            return self._perform(effect), None
        except Exception as error:
            return None, error

    def _perform(self, effect: Effect) -> object:
        if isinstance(effect, Send):
            return self._attempt(effect.request)
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
        if isinstance(effect, Shared):
            return self.run(effect.flow())
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

    __slots__ = ("_inflight", "http")

    def __init__(self, http: httpx.AsyncClient) -> None:
        self.http = http
        self._inflight: dict[str, asyncio.Future[str]] = {}

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
            sent, thrown = await self._perform_safely(effect)

    async def iterate(self, flow: Flow[None]) -> AsyncIterator[object]:
        """The items a flow emits, in order."""
        sent: object = None
        thrown: Exception | None = None
        try:
            while True:
                effect = _advance(flow, sent, thrown)
                if isinstance(effect, _Finished):
                    return
                if isinstance(effect, Emit):
                    sent, thrown = None, None
                    yield effect.item
                    continue
                sent, thrown = await self._perform_safely(effect)
        finally:
            flow.close()

    async def _perform_safely(self, effect: Effect) -> tuple[object, Exception | None]:
        try:
            return await self._perform(effect), None
        except Exception as error:
            return None, error

    async def _perform(self, effect: Effect) -> object:
        if isinstance(effect, Send):
            return await self._attempt(effect.request)
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
