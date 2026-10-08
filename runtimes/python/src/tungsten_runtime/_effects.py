# SPDX-License-Identifier: Apache-2.0
"""The I/O a flow asks its driver to perform.

Every behaviour of the runtime (validation, auth, idempotency, retries,
classification, previews, verification, pagination, macros) is written once,
as generator functions ("flows") that ``yield`` an effect whenever they need
I/O and receive its result. ``_drivers`` runs flows on ``httpx.Client``
(``ClientCore``) or ``httpx.AsyncClient`` (``AsyncClientCore``), so the two
cores differ only in how they perform these effects.
"""

from __future__ import annotations

from collections.abc import Callable, Generator, Mapping
from dataclasses import dataclass
from typing import Literal, cast

from .types import HttpMethod


@dataclass(frozen=True, slots=True)
class AttemptRequest:
    """One HTTP attempt: sent once, never following redirects."""

    url: str
    method: HttpMethod
    headers: Mapping[str, str]
    body: bytes | None
    timeout_ms: float


@dataclass(frozen=True, slots=True)
class Answered:
    """A response arrived; ``body`` is None when reading it failed."""

    status: int
    headers: Mapping[str, str]
    body: bytes | None
    #: Why the body could not be read: the deadline or a broken connection.
    body_failure: Literal["timeout", "broken"] | None


@dataclass(frozen=True, slots=True)
class NotSent:
    """The request cannot have reached the server (DNS, refused, TLS, connect timeout)."""

    detail: str


@dataclass(frozen=True, slots=True)
class Lost:
    """Sent or possibly sent, then the connection failed without a response."""

    detail: str


@dataclass(frozen=True, slots=True)
class TimedOut:
    """No response within the attempt's deadline, after the request was (possibly) sent."""


type AttemptOutcome = Answered | NotSent | Lost | TimedOut


@dataclass(frozen=True, slots=True)
class Send:
    request: AttemptRequest


@dataclass(frozen=True, slots=True)
class Sleep:
    ms: float


@dataclass(frozen=True, slots=True)
class StoreGet:
    store: object
    scope: str
    logical_id: str


@dataclass(frozen=True, slots=True)
class StorePut:
    store: object
    scope: str
    logical_id: str
    key: str


@dataclass(frozen=True, slots=True)
class Observe:
    """Call an observer (middleware hook, ``on_diagnostic``). Its failures and
    its return value are ignored; an awaitable result is awaited by the async
    driver."""

    fn: Callable[..., object]
    args: tuple[object, ...]


@dataclass(frozen=True, slots=True)
class Shared:
    """Run ``flow()`` once for every concurrent request of the same ``key``
    (single-flight); the result or exception is shared."""

    key: str
    flow: Callable[[], Flow[str]]


@dataclass(frozen=True, slots=True)
class Emit:
    """Hand one item to the caller of an iterating flow (``pages``)."""

    item: object


type Effect = Send | Sleep | StoreGet | StorePut | Observe | Shared | Emit

type Flow[T] = Generator[Effect, object, T]


def send(request: AttemptRequest) -> Flow[AttemptOutcome]:
    outcome = yield Send(request)
    return cast(AttemptOutcome, outcome)


def sleep(ms: float) -> Flow[None]:
    yield Sleep(ms)


def store_get(store: object, scope: str, logical_id: str) -> Flow[object]:
    found = yield StoreGet(store, scope, logical_id)
    return found


def store_put(store: object, scope: str, logical_id: str, key: str) -> Flow[None]:
    yield StorePut(store, scope, logical_id, key)


def observe(fn: object, *args: object) -> Flow[None]:
    if callable(fn):
        yield Observe(fn, args)


def shared(key: str, flow: Callable[[], Flow[str]]) -> Flow[str]:
    value = yield Shared(key, flow)
    return cast(str, value)


def emit(item: object) -> Flow[None]:
    yield Emit(item)
