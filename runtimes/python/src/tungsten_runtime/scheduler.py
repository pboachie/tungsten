# SPDX-License-Identifier: Apache-2.0
"""Bounded request admission shared by the calls of one or more clients.

A :class:`RequestScheduler` keeps, per scope (a tenant label plus, by
default, the request origin), a count of requests in flight, a bounded
first-in-first-out queue of requests waiting for capacity, and a shared
rate-limit pause. Ordinary calls and event streams are counted apart, so
long-lived streams cannot starve ordinary calls. Nothing is coordinated
across processes.

One scheduler may serve threads (``ClientCore``) and event loops
(``AsyncClientCore``) at once.
"""

from __future__ import annotations

import asyncio
import contextlib
import math
import random as _random
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Literal, cast
from urllib.parse import urlsplit

__all__ = ["Permit", "Rejection", "RequestScheduler", "SchedulerOptions", "SchedulerStats"]

Kind = Literal["call", "stream"]
_KINDS: tuple[Kind, Kind] = ("call", "stream")
#: The pause after a ``RATE_LIMITED`` answer that names no wait.
_DEFAULT_PAUSE_MS = 1000.0
_MAX_PAUSE_MS = 3_600_000.0
#: Scopes whose retry budget is remembered; the least recently used is
#: forgotten (and starts again from the reserve) beyond this.
_MAX_BUDGETS = 1024


@dataclass(frozen=True, slots=True)
class SchedulerOptions:
    """Limits of a :class:`RequestScheduler`. Every limit applies per scope,
    except the optional ``max_active_total`` and ``max_streams_total``."""

    #: Ordinary requests in flight at once. A positive integer (at most 10000).
    max_active: int
    #: Ordinary requests, and separately event streams, that may wait for
    #: capacity; one more is rejected at once with ``QUEUE_FULL``. 0 never waits.
    max_queued: int = 100
    #: Event streams open at once. Default ``max_active``.
    max_streams: int | None = None
    #: Longest wait for capacity, in milliseconds; a longer wait fails with
    #: ``QUEUE_TIMEOUT`` and nothing is sent. The call's own deadline and task
    #: cancellation also end the wait.
    max_queue_ms: int = 30_000
    #: ``"origin"``: one scope per tenant and request origin. ``"scheduler"``:
    #: one scope per tenant, whatever the origin.
    scope: Literal["origin", "scheduler"] = "origin"
    #: Retries per call a scope may spend over time, shared by every caller of
    #: the scope (0 to 1). Each call adds this much to the scope's retry budget
    #: and each retry spends 1; a retry the budget cannot pay for is not made and
    #: the call returns its last failure. None (default): retries are not budgeted.
    retry_ratio: float | None = None
    #: Retries a scope's budget holds before any call added to it, and the base
    #: of its cap (``retry_reserve + 100 * retry_ratio``).
    retry_reserve: int = 10
    #: Ordinary requests in flight at once across every scope, on top of each
    #: scope's ``max_active``. Scopes waiting for this capacity take turns, one
    #: request each, so a busy scope cannot starve a quiet one. None (default):
    #: no cap across scopes. A positive integer (at most 1000000).
    max_active_total: int | None = None
    #: Event streams open at once across every scope, on top of each scope's
    #: ``max_streams``. None (default): no cap across scopes.
    max_streams_total: int | None = None


@dataclass(frozen=True, slots=True)
class Rejection:
    """Why a request was not admitted; it must not be sent."""

    reason: Literal["full", "timeout", "paused", "closed", "aborted"]
    #: With ``paused``: how long the shared pause still lasts.
    retry_after_ms: int | None = None


@dataclass(frozen=True, slots=True)
class SchedulerStats:
    """Counts across every scope, for telemetry and load checks."""

    active: int
    streams: int
    queued: int
    scopes: int


def _integer(value: object, fallback: int | None, low: int, high: int, name: str) -> int:
    if value is None and fallback is not None:
        return fallback
    if not isinstance(value, int) or isinstance(value, bool) or not low <= value <= high:
        raise ValueError(f"{name}: expected an integer from {low} to {high}, got {value!r}")
    return value


def _now() -> float:
    return time.monotonic() * 1000


class _Waiter:
    __slots__ = ("kind", "permit", "random", "rejection", "wake")

    def __init__(self, kind: Kind, wake: Callable[[], None], random: Callable[[], float]) -> None:
        self.kind: Kind = kind
        self.random = random
        self.permit: Permit | None = None
        self.rejection: Rejection | None = None
        self.wake = wake


class _Pool:
    __slots__ = ("active", "paused_until", "probing", "queue")

    def __init__(self) -> None:
        self.active: dict[Kind, int] = {"call": 0, "stream": 0}
        self.queue: dict[Kind, list[_Waiter]] = {"call": [], "stream": []}
        self.paused_until = 0.0
        self.probing = False

    def busy(self) -> int:
        return sum(self.active.values()) + sum(len(q) for q in self.queue.values())


class Permit:
    """Capacity held by one admitted request."""

    __slots__ = ("_release", "_released", "_settle", "_settled")

    def __init__(self, settle: Callable[[float | None, bool], None], release: Callable[[], None]) -> None:
        self._settle = settle
        self._release = release
        self._settled = False
        self._released = False

    def settle(self, rate_limited_ms: float | None, *, limited: bool) -> None:
        """Record the answer: with ``limited``, the wait a ``RATE_LIMITED``
        answer asks for (None when it named none). The first call counts."""
        if not self._settled:
            self._settled = True
            self._settle(rate_limited_ms, limited)

    def release(self) -> None:
        """Free the capacity. Safe to call more than once."""
        if not self._released:
            self._released = True
            self._release()


class RequestScheduler:
    """Admits requests within the configured bounds. Pass one to
    ``ClientOptions.scheduler`` of several clients to share capacity and
    rate-limit pauses between them; give each client a ``tenant`` to keep
    their accounting apart."""

    def __init__(self, options: SchedulerOptions) -> None:
        if not isinstance(cast(object, options), SchedulerOptions):
            raise TypeError("RequestScheduler takes SchedulerOptions")
        self.max_active = _integer(options.max_active, None, 1, 10_000, "max_active")
        self.max_queued = _integer(options.max_queued, 100, 0, 1_000_000, "max_queued")
        self.max_streams = _integer(options.max_streams, self.max_active, 1, 10_000, "max_streams")
        self.max_queue_ms = _integer(options.max_queue_ms, 30_000, 0, int(_MAX_PAUSE_MS), "max_queue_ms")
        if options.scope not in ("origin", "scheduler"):
            raise ValueError(f'scope must be "origin" or "scheduler", got {options.scope!r}')
        self.scope: Literal["origin", "scheduler"] = options.scope
        ratio = cast(object, options.retry_ratio)
        if ratio is not None and (
            not isinstance(ratio, int | float)
            or isinstance(ratio, bool)
            or not math.isfinite(ratio)
            or not 0 <= ratio <= 1
        ):
            raise ValueError(f"retry_ratio must be a number from 0 to 1, got {ratio!r}")
        self.retry_ratio: float | None = None if ratio is None else float(ratio)
        self.retry_reserve = _integer(options.retry_reserve, 10, 0, 10_000, "retry_reserve")
        self._budgets: dict[str, float] = {}
        self.max_active_total: int | None = (
            None
            if options.max_active_total is None
            else _integer(options.max_active_total, None, 1, 1_000_000, "max_active_total")
        )
        self.max_streams_total: int | None = (
            None
            if options.max_streams_total is None
            else _integer(options.max_streams_total, None, 1, 1_000_000, "max_streams_total")
        )
        self._total: dict[Kind, int] = {"call": 0, "stream": 0}
        #: The scope served last under a cap across scopes, per kind.
        self._served: dict[Kind, str | None] = {"call": None, "stream": None}
        self._lock = threading.Lock()
        self._pools: dict[str, _Pool] = {}
        self._idle = threading.Condition(self._lock)
        self._closed = False

    def scope_key(self, url: str, tenant: str | None) -> str:
        """The scope of a request to ``url`` made for ``tenant``."""
        origin = ""
        if self.scope == "origin":
            try:
                parts = urlsplit(url)
                host = parts.hostname or ""
                port = parts.port
                scheme = parts.scheme.lower()
                if ":" in host:
                    host = f"[{host}]"
                default = {"http": 80, "https": 443}.get(scheme)
                origin = f"{scheme}://{host}" + ("" if port is None or port == default else f":{port}")
            except ValueError:
                origin = ""
        return f"{tenant or ''}\n{origin}"

    # ------------------------------------------------------------ retry budget

    def record_call(self, key: str) -> None:
        """Add one call's share to the retry budget of ``key``."""
        ratio = self.retry_ratio
        if ratio is None:
            return
        with self._lock:
            cap = self.retry_reserve + 100 * ratio
            self._set_budget(key, min(cap, self._budgets.get(key, float(self.retry_reserve)) + ratio))

    def allow_retry(self, key: str) -> bool:
        """Spend one retry from the budget of ``key``; False when it cannot pay."""
        if self.retry_ratio is None:
            return True
        with self._lock:
            balance = self._budgets.get(key, float(self.retry_reserve))
            if balance < 1:
                return False
            self._set_budget(key, balance - 1)
            return True

    def _set_budget(self, key: str, balance: float) -> None:
        self._budgets.pop(key, None)
        self._budgets[key] = balance
        if len(self._budgets) > _MAX_BUDGETS:
            del self._budgets[next(iter(self._budgets))]

    # ------------------------------------------------------------ admission

    def _try(
        self, key: str, kind: Kind, budget_ms: float, random: Callable[[], float]
    ) -> Permit | Rejection | _Pool:
        """Admit at once, refuse at once, or return the pool to wait in."""
        if self._closed:
            return Rejection("closed")
        pool = self._pools.get(key)
        if pool is None:
            pool = self._pools[key] = _Pool()
        paused = max(0.0, pool.paused_until - _now())
        if (
            paused == 0
            and not pool.queue[kind]
            and pool.active[kind] < self._limit(pool, kind)
            and self._room(kind)
        ):
            return self._grant(key, pool, kind, random)
        if paused > budget_ms:
            self._forget(key, pool)
            return Rejection("paused", math.ceil(paused))
        if len(pool.queue[kind]) >= self.max_queued or budget_ms <= 0:
            full = len(pool.queue[kind]) >= self.max_queued
            self._forget(key, pool)
            return Rejection("full" if full else "timeout")
        return pool

    def _budget(self, deadline_ms: float | None) -> float:
        budget = float(self.max_queue_ms)
        if deadline_ms is not None:
            budget = min(budget, max(0.0, deadline_ms - _now()))
        return budget

    def acquire(
        self,
        key: str,
        kind: Kind,
        deadline_ms: float | None = None,
        random: Callable[[], float] = _random.random,
    ) -> Permit | Rejection:
        """Wait (blocking this thread) for capacity in ``key``.
        ``deadline_ms`` is an absolute ``time.monotonic()`` time in
        milliseconds that also ends the wait."""
        budget = self._budget(deadline_ms)
        until = _now() + budget
        event = threading.Event()
        with self._lock:
            found = self._try(key, kind, budget, random)
            if not isinstance(found, _Pool):
                return found
            waiter = _Waiter(kind, event.set, random)
            found.queue[kind].append(waiter)
        while True:
            with self._lock:
                outcome = self._check(key, found, waiter, until)
                if outcome is not None:
                    return outcome
                event.clear()
                wait = self._wait_ms(found, until)
            event.wait(wait / 1000)

    async def acquire_async(
        self,
        key: str,
        kind: Kind,
        deadline_ms: float | None = None,
        random: Callable[[], float] = _random.random,
    ) -> Permit | Rejection:
        """Wait (without blocking the event loop) for capacity in ``key``.
        Cancelling the task removes the request from the queue."""
        loop = asyncio.get_running_loop()
        budget = self._budget(deadline_ms)
        until = _now() + budget
        holder: list[asyncio.Future[None]] = [loop.create_future()]

        def wake() -> None:
            def resolve() -> None:
                if not holder[0].done():
                    holder[0].set_result(None)

            # A closed loop took its waiter with it.
            with contextlib.suppress(RuntimeError):
                loop.call_soon_threadsafe(resolve)

        with self._lock:
            found = self._try(key, kind, budget, random)
            if not isinstance(found, _Pool):
                return found
            waiter = _Waiter(kind, wake, random)
            found.queue[kind].append(waiter)
        try:
            while True:
                with self._lock:
                    outcome = self._check(key, found, waiter, until)
                    if outcome is not None:
                        return outcome
                    holder[0] = loop.create_future()
                    wait = self._wait_ms(found, until)
                with contextlib.suppress(TimeoutError, asyncio.TimeoutError):
                    await asyncio.wait_for(asyncio.shield(holder[0]), wait / 1000)
        except BaseException:
            with self._lock:
                self._withdraw(key, found, waiter)
            if waiter.permit is not None:
                waiter.permit.release()
            raise

    def _check(self, key: str, pool: _Pool, waiter: _Waiter, until: float) -> Permit | Rejection | None:
        """Under the lock: the waiter's outcome once it has one."""
        self._pump(key, pool)
        if waiter.permit is not None:
            return waiter.permit
        if waiter.rejection is not None:
            return waiter.rejection
        if _now() >= until:
            self._withdraw(key, pool, waiter)
            return Rejection("timeout")
        return None

    def _wait_ms(self, pool: _Pool, until: float) -> float:
        now = _now()
        wait = until - now
        if pool.paused_until > now:
            wait = min(wait, pool.paused_until - now)
        return max(1.0, wait)

    def _withdraw(self, key: str, pool: _Pool, waiter: _Waiter) -> None:
        queue = pool.queue[waiter.kind]
        if waiter in queue:
            queue.remove(waiter)
        self._pump(key, pool)
        self._forget(key, pool)

    # ------------------------------------------------------------ shutdown

    def close(self) -> None:
        """Reject every waiting request and every later one; requests in
        flight keep their capacity until they finish."""
        with self._lock:
            self._closed = True
            for key, pool in list(self._pools.items()):
                for kind in _KINDS:
                    for waiter in pool.queue[kind]:
                        waiter.rejection = Rejection("closed")
                        waiter.wake()
                    pool.queue[kind].clear()
                self._forget(key, pool)

    def drained(self, timeout_ms: float | None = None) -> bool:
        """Block until no request is in flight or waiting; False when
        ``timeout_ms`` passed first."""
        with self._idle:
            return self._idle.wait_for(
                lambda: self._busy() == 0, None if timeout_ms is None else timeout_ms / 1000
            )

    async def drained_async(self) -> None:
        """Wait until no request is in flight or waiting."""
        while True:
            with self._lock:
                if self._busy() == 0:
                    return
            await asyncio.sleep(0.01)

    def stats(self) -> SchedulerStats:
        with self._lock:
            pools = list(self._pools.values())
            return SchedulerStats(
                active=sum(p.active["call"] for p in pools),
                streams=sum(p.active["stream"] for p in pools),
                queued=sum(len(p.queue["call"]) + len(p.queue["stream"]) for p in pools),
                scopes=len(pools),
            )

    # ------------------------------------------------------------ internals

    def _busy(self) -> int:
        return sum(pool.busy() for pool in self._pools.values())

    def _limit(self, pool: _Pool, kind: Kind) -> int:
        # After a rate-limit pause, one request per kind probes the quota
        # until an answer that is not rate limited arrives.
        if pool.probing:
            return 1
        return self.max_active if kind == "call" else self.max_streams

    def _room(self, kind: Kind) -> bool:
        """Under the lock: whether the cap across scopes, if any, leaves room
        for one more."""
        cap = self.max_active_total if kind == "call" else self.max_streams_total
        return cap is None or self._total[kind] < cap

    def _grant(self, key: str, pool: _Pool, kind: Kind, random: Callable[[], float]) -> Permit:
        pool.active[kind] += 1
        self._total[kind] += 1
        self._served[kind] = key

        def settle(rate_limited_ms: float | None, limited: bool) -> None:
            with self._lock:
                if not limited:
                    pool.probing = False
                    return
                wait = min(
                    _MAX_PAUSE_MS, max(0.0, _DEFAULT_PAUSE_MS if rate_limited_ms is None else rate_limited_ms)
                )
                try:
                    r = float(random())
                except Exception:
                    r = 0.5
                r = min(1.0, max(0.0, r)) if math.isfinite(r) else 0.5
                # A little spread keeps clients that share a quota from all
                # resuming at the same instant.
                spread = min(1000.0, wait * 0.1) * r
                pool.paused_until = max(pool.paused_until, _now() + wait + spread)
                pool.probing = True
                # Waiters recompute how long to sleep: until the pause ends.
                for queue in pool.queue.values():
                    for waiter in queue:
                        waiter.wake()

        def release() -> None:
            with self._lock:
                pool.active[kind] -= 1
                self._total[kind] -= 1
                self._pump(key, pool)
                self._forget(key, pool)

        return Permit(settle, release)

    def _pump(self, key: str, pool: _Pool) -> None:
        """Under the lock: admit waiting requests in order while capacity allows."""
        if self.max_active_total is not None or self.max_streams_total is not None:
            self._pump_all()
            return
        if pool.paused_until > _now():
            return
        for kind in _KINDS:
            queue = pool.queue[kind]
            while queue and pool.active[kind] < self._limit(pool, kind):
                waiter = queue.pop(0)
                waiter.permit = self._grant(key, pool, kind, waiter.random)
                waiter.wake()

    def _pump_all(self) -> None:
        """Under the lock, with a cap across scopes: admit waiting requests
        while capacity allows, one per scope in turn (scopes in key order,
        starting after the one served last), each scope's own queue first in,
        first out."""
        now = _now()
        for kind in _KINDS:
            keys = sorted(
                key for key, pool in self._pools.items() if pool.queue[kind] and pool.paused_until <= now
            )
            if not keys:
                continue
            last = self._served[kind]
            start = 0 if last is None else next((i for i, key in enumerate(keys) if key > last), 0)
            order = keys[start:] + keys[:start]
            progress = True
            while progress and self._room(kind):
                progress = False
                for key in order:
                    if not self._room(kind):
                        break
                    pool = self._pools.get(key)
                    if pool is None or not pool.queue[kind] or pool.active[kind] >= self._limit(pool, kind):
                        continue
                    waiter = pool.queue[kind].pop(0)
                    waiter.permit = self._grant(key, pool, kind, waiter.random)
                    waiter.wake()
                    progress = True

    def _forget(self, key: str, pool: _Pool) -> None:
        """Under the lock: drop an idle scope once its pause has passed."""
        if pool.busy() > 0:
            return
        if pool.paused_until <= _now() and self._pools.get(key) is pool:
            del self._pools[key]
        if self._busy() == 0:
            self._idle.notify_all()
