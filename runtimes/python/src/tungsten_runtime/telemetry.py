# SPDX-License-Identifier: Apache-2.0
"""Optional, local telemetry for calls made through the client core.

Off by default: nothing is recorded or sent unless ``ClientOptions.telemetry``
names an observer. Events describe one logical operation, each attempt it
sends and the phases around them (authentication, scheduler queueing, retry
backoff). They carry only bounded values: the API name and operation id from
the compiled descriptor, enumerated phases, outcomes, categories and reasons,
HTTP status codes, attempt numbers and durations. URLs, headers, bodies,
argument values, credentials, confirmation tokens, tenant labels and error
text are never part of an event, so there is nothing to redact.

The TypeScript, Python and Rust runtimes emit the same events with the same
field names and meanings.
"""

from __future__ import annotations

import itertools
import math
import threading
import time
from collections import deque
from collections.abc import Callable, Mapping
from typing import Literal, Protocol, TypedDict, cast, runtime_checkable

#: Version of the event contract; incremented for incompatible changes.
TELEMETRY_SCHEMA_VERSION: Literal[1] = 1

#: What an event marks. ``phase`` events are emitted when a phase ends.
TelemetryEventName = Literal["operation.start", "operation.end", "attempt.start", "attempt.end", "phase"]
#: ``auth``: resolving credentials (including a token fetch or refresh).
#: ``queue``: waiting for request scheduler capacity. ``retry_backoff``: the
#: wait before a permitted retry.
TelemetryPhase = Literal["auth", "queue", "retry_backoff"]
#: How an operation, attempt or phase ended. ``outcome_unknown``: a mutation
#: may have taken effect (its category is ``OUTCOME_UNKNOWN``), which takes
#: precedence over ``deadline_exceeded``.
TelemetryOutcome = Literal["ok", "error", "cancelled", "deadline_exceeded", "outcome_unknown"]
#: Why a ``queue`` phase ended: ``admitted``, the scheduler's rejection
#: (``full``, ``timeout``, ``paused``, ``closed``), the call's deadline, or
#: cancellation.
TelemetryQueueReason = Literal["admitted", "full", "timeout", "paused", "closed", "deadline", "cancelled"]
#: The public entry point that started the operation. One ``pages`` operation
#: covers every page it fetched, one ``poll`` operation every read it made,
#: one ``stream`` operation its connection and every reconnect; their attempts
#: are numbered across the whole operation.
TelemetryEntry = Literal["call", "pages", "poll", "preview", "stream"]


class TelemetryEvent(TypedDict):
    """One telemetry event. Fields that do not apply to an event are None."""

    schema_version: Literal[1]
    name: TelemetryEventName
    #: The API name of the compiled descriptor.
    api: str
    #: The operation id of the compiled descriptor.
    operation: str
    entry: TelemetryEntry
    #: Correlates the events of one operation; increases per client from 1.
    #: An identifier, not a label: do not use it as a metric dimension.
    call_id: int
    #: Whether the operation may change server state.
    mutation: bool
    #: The attempt number within the operation (from 1), for attempt events
    #: and for the ``queue`` and ``retry_backoff`` phases.
    attempt: int | None
    phase: TelemetryPhase | None
    #: Milliseconds, for ``.end`` and ``phase`` events.
    duration_ms: float | None
    outcome: TelemetryOutcome | None
    #: The diagnostic category of a failed operation, attempt or phase.
    category: str | None
    #: The HTTP status of an attempt that received a response, and of a
    #: ``call`` operation's final answer.
    http_status: int | None
    #: For a ``queue`` phase.
    reason: TelemetryQueueReason | None
    #: Attempts sent, on ``operation.end``.
    attempts: int | None


@runtime_checkable
class TelemetrySink(Protocol):
    def on_event(self, event: TelemetryEvent) -> None: ...


#: Receives telemetry events: a callable or an object with ``on_event``. It is
#: called synchronously on the request path (in the async client too) and
#: never awaited: keep it fast and hand slow work elsewhere (a bounded buffer
#: such as ``MemoryTelemetryCollector``). An observer that raises never
#: changes the call; a coroutine it returns is closed without running.
TelemetryObserver = Callable[[TelemetryEvent], object] | TelemetrySink

_DEFAULT_CAPACITY = 1024
_MAX_CAPACITY = 65536


class MemoryTelemetryCollector:
    """An observer keeping the most recent events in memory, for tests,
    debugging and exporters that drain it periodically. Holds at most
    ``capacity`` events (default 1024, at most 65536); when full, the oldest
    event is dropped and counted in ``dropped``. Thread-safe."""

    def __init__(self, capacity: int = _DEFAULT_CAPACITY) -> None:
        value = cast(object, capacity)
        if isinstance(value, int | float) and not isinstance(value, bool) and math.isfinite(value):
            self.capacity = min(_MAX_CAPACITY, max(1, int(value)))
        else:
            self.capacity = _DEFAULT_CAPACITY
        self._events: deque[TelemetryEvent] = deque()
        self._dropped = 0
        self._lock = threading.Lock()

    def on_event(self, event: TelemetryEvent) -> None:
        with self._lock:
            if len(self._events) >= self.capacity:
                self._events.popleft()
                self._dropped += 1
            self._events.append(event)

    def events(self) -> list[TelemetryEvent]:
        """The buffered events, oldest first."""
        with self._lock:
            return list(self._events)

    @property
    def dropped(self) -> int:
        """Events dropped because the buffer was full."""
        return self._dropped

    def drain(self) -> list[TelemetryEvent]:
        """Remove and return the buffered events, oldest first."""
        with self._lock:
            events = list(self._events)
            self._events.clear()
            return events

    def clear(self) -> None:
        """Drop the buffered events and reset the drop count."""
        with self._lock:
            self._events.clear()
            self._dropped = 0


_CATEGORIES = frozenset(
    {
        "VALIDATION_FAILED",
        "MALFORMED_REQUEST",
        "REQUEST_TOO_LARGE",
        "AUTH_FAILED",
        "NOT_FOUND",
        "CONFLICT",
        "PRECONDITION_FAILED",
        "RATE_LIMITED",
        "UPSTREAM_UNAVAILABLE",
        "OUTCOME_UNKNOWN",
        "TRANSPORT_FAILED",
        "CONFIRMATION_REQUIRED",
        "UNEXPECTED_RESPONSE",
    }
)


def telemetry_outcome(error: Mapping[str, object] | None, cancelled: bool) -> TelemetryOutcome:
    """How a result ended, for ``outcome``."""
    if error is None:
        return "ok"
    if error.get("category") == "OUTCOME_UNKNOWN":
        return "outcome_unknown"
    if cancelled:
        return "cancelled"
    if error.get("code") == "DEADLINE_EXCEEDED":
        return "deadline_exceeded"
    return "error"


def _category(error: Mapping[str, object] | None, outcome: TelemetryOutcome) -> str | None:
    """The category of a failure; none for a cancelled one."""
    category = None if error is None or outcome == "cancelled" else error.get("category")
    return category if isinstance(category, str) and category in _CATEGORIES else None


def _status(status: object) -> int | None:
    if isinstance(status, int) and not isinstance(status, bool) and 100 <= status <= 599:
        return status
    return None


def _now() -> float:
    return time.monotonic() * 1000


def _millis(start: float) -> float:
    return max(0.0, round(_now() - start, 3))


class Telemetry:
    """Per-client telemetry state: the observer and the call id counter."""

    def __init__(self, observer: Callable[[TelemetryEvent], object]) -> None:
        self._observer = observer
        self._ids = itertools.count(1)

    @staticmethod
    def of(observer: object) -> Telemetry | None:
        """None when ``observer`` is not an observer (telemetry stays off)."""
        method = getattr(observer, "on_event", None)
        if callable(method):
            return Telemetry(cast(Callable[[TelemetryEvent], object], method))
        if callable(observer):
            return Telemetry(cast(Callable[[TelemetryEvent], object], observer))
        return None

    def start(self, api: str, operation: str, entry: TelemetryEntry, mutation: bool) -> TelemetrySpan:
        """Start an operation and emit ``operation.start``."""
        span = TelemetrySpan(self, api, operation, entry, next(self._ids), mutation)
        span.emit("operation.start")
        return span

    def deliver(self, event: TelemetryEvent) -> None:
        try:
            returned = self._observer(event)
            close = getattr(returned, "close", None)
            if returned is not None and hasattr(returned, "__await__") and callable(close):
                close()
        except Exception:
            pass


class TelemetrySpan:
    """The events of one logical operation."""

    def __init__(
        self,
        telemetry: Telemetry,
        api: str,
        operation: str,
        entry: TelemetryEntry,
        call_id: int,
        mutation: bool,
    ) -> None:
        self._telemetry = telemetry
        self._api = api
        self._operation = operation
        self._entry: TelemetryEntry = entry
        self._call_id = call_id
        self._mutation = mutation
        self._start = _now()
        self._attempts = 0
        self._ended = False
        self._cancelled = False
        self._attempt_start = self._start

    def emit(
        self,
        name: TelemetryEventName,
        *,
        attempt: int | None = None,
        phase: TelemetryPhase | None = None,
        duration_ms: float | None = None,
        outcome: TelemetryOutcome | None = None,
        category: str | None = None,
        http_status: int | None = None,
        reason: TelemetryQueueReason | None = None,
        attempts: int | None = None,
    ) -> None:
        if self._cancelled and name != "operation.end":
            return
        self._telemetry.deliver(
            {
                "schema_version": TELEMETRY_SCHEMA_VERSION,
                "name": name,
                "api": self._api,
                "operation": self._operation,
                "entry": self._entry,
                "call_id": self._call_id,
                "mutation": self._mutation,
                "attempt": attempt,
                "phase": phase,
                "duration_ms": duration_ms,
                "outcome": outcome,
                "category": category,
                "http_status": http_status,
                "reason": reason,
                "attempts": attempts,
            }
        )

    def cancel(self) -> None:
        """The caller cancelled the operation while an attempt was in flight:
        report this attempt and the operation as cancelled, and nothing the
        runtime does to finish the operation without I/O."""
        if not self._cancelled and self._attempts > 0:
            self.emit(
                "attempt.end",
                attempt=self._attempts,
                duration_ms=_millis(self._attempt_start),
                outcome="cancelled",
            )
        self._cancelled = True

    def next_attempt(self) -> int:
        """The number of the attempt about to be sent, counted across the
        whole operation (an OAuth re-send after a rejected token continues it)."""
        self._attempts += 1
        return self._attempts

    @property
    def attempt(self) -> int:
        """The attempt number most recently started."""
        return self._attempts

    def phase(
        self,
        phase: TelemetryPhase,
        start: float,
        error: Mapping[str, object] | None,
        cancelled: bool,
        *,
        attempt: int | None = None,
        reason: TelemetryQueueReason | None = None,
    ) -> None:
        self.emit(
            "phase",
            phase=phase,
            duration_ms=_millis(start),
            outcome=(outcome := telemetry_outcome(error, cancelled)),
            category=_category(error, outcome),
            attempt=attempt,
            reason=reason,
        )

    def attempt_start(self, attempt: int) -> float:
        self.emit("attempt.start", attempt=attempt)
        self._attempt_start = _now()
        return self._attempt_start

    def attempt_end(
        self, attempt: int, start: float, error: Mapping[str, object] | None, cancelled: bool, status: object
    ) -> None:
        self.emit(
            "attempt.end",
            attempt=attempt,
            duration_ms=_millis(start),
            outcome=(outcome := telemetry_outcome(error, cancelled)),
            category=_category(error, outcome),
            http_status=_status(status),
        )

    def end(self, error: Mapping[str, object] | None, cancelled: bool, status: object) -> None:
        """Emit ``operation.end`` once."""
        if self._ended:
            return
        self._ended = True
        self.emit(
            "operation.end",
            duration_ms=_millis(self._start),
            outcome=(outcome := telemetry_outcome(error, cancelled or self._cancelled)),
            category=_category(error, outcome),
            http_status=_status(status),
            attempts=self._attempts,
        )


_OTEL_KIND_CLIENT_NAME = "CLIENT"


def open_telemetry_observer(tracer: object, max_open_spans: int = 10000) -> Callable[[TelemetryEvent], None]:
    """An observer recording each operation as one OpenTelemetry client span
    named after the operation id, with attempts and phases as span events and
    the outcome as attributes (``tungsten.outcome``, ``tungsten.category``,
    ``http.response.status_code``, ``tungsten.attempts``); a failed or
    cancelled operation sets the span's status to error. ``tracer`` is any
    object with the ``Tracer.start_span`` shape of ``opentelemetry-api``,
    which this runtime does not depend on (when it is installed, its
    ``SpanKind`` and ``Status`` are used); spans start in the tracer's
    current context. At most ``max_open_spans`` operations are open at once
    (default 10000); beyond that the oldest is ended and dropped. Attributes
    hold only the bounded event fields. Thread-safe."""
    limit = max(1, int(max_open_spans))
    open_spans: dict[int, object] = {}
    lock = threading.Lock()
    kind: object = None
    error_status: object = None
    try:
        from opentelemetry.trace import SpanKind, Status, StatusCode  # pyright: ignore[reportMissingImports]

        kind = SpanKind.CLIENT  # pyright: ignore[reportUnknownMemberType]
        error_status = Status(StatusCode.ERROR)  # pyright: ignore[reportUnknownMemberType]
    except Exception:  # the API is optional
        pass

    def start(name: str, attributes: dict[str, str | int | float | bool]) -> object:
        start_span = getattr(tracer, "start_span")  # noqa: B009
        if kind is not None:
            return start_span(name, kind=kind, attributes=attributes)
        return start_span(name, attributes=attributes)

    def observe(event: TelemetryEvent) -> None:
        call_id = event["call_id"]
        if event["name"] == "operation.start":
            span = start(
                event["operation"],
                {
                    "tungsten.api": event["api"],
                    "tungsten.operation": event["operation"],
                    "tungsten.entry": event["entry"],
                    "tungsten.mutation": event["mutation"],
                },
            )
            evicted: object = None
            with lock:
                if len(open_spans) >= limit:
                    oldest = next(iter(open_spans))
                    evicted = open_spans.pop(oldest)
                open_spans[call_id] = span
            if evicted is not None:
                getattr(evicted, "end")()  # noqa: B009
            return
        with lock:
            span = (
                open_spans.pop(call_id, None) if event["name"] == "operation.end" else open_spans.get(call_id)
            )
        if span is None:
            return
        attributes = _otel_attributes(event)
        if event["name"] != "operation.end":
            name = (
                f"tungsten.phase.{event['phase']}"
                if event["name"] == "phase"
                else f"tungsten.{event['name']}"
            )
            getattr(span, "add_event")(name, attributes)  # noqa: B009
            return
        set_attribute = getattr(span, "set_attribute")  # noqa: B009
        for key, value in attributes.items():
            set_attribute(key, value)
        if event["outcome"] != "ok" and error_status is not None:
            getattr(span, "set_status")(error_status)  # noqa: B009
        getattr(span, "end")()  # noqa: B009

    return observe


def _otel_attributes(event: TelemetryEvent) -> dict[str, str | int | float | bool]:
    attributes: dict[str, str | int | float | bool] = {}
    fields: tuple[tuple[str, str], ...] = (
        ("attempt", "tungsten.attempt"),
        ("duration_ms", "tungsten.duration_ms"),
        ("outcome", "tungsten.outcome"),
        ("category", "tungsten.category"),
        ("http_status", "http.response.status_code"),
        ("reason", "tungsten.queue.reason"),
        ("attempts", "tungsten.attempts"),
    )
    for field, key in fields:
        value = event.get(field)
        if isinstance(value, str | int | float | bool):
            attributes[key] = value
    return attributes


def now_ms() -> float:
    """The monotonic clock phases are timed with, in milliseconds."""
    return _now()


__all__ = [
    "TELEMETRY_SCHEMA_VERSION",
    "MemoryTelemetryCollector",
    "TelemetryEntry",
    "TelemetryEvent",
    "TelemetryEventName",
    "TelemetryObserver",
    "TelemetryOutcome",
    "TelemetryPhase",
    "TelemetryQueueReason",
    "TelemetrySink",
    "open_telemetry_observer",
]
