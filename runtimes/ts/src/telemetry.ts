// SPDX-License-Identifier: Apache-2.0
/**
 * Optional, local telemetry for calls made through {@link ClientCore}.
 *
 * Off by default: nothing is recorded or sent unless `ClientOptions.telemetry`
 * names an observer. Events describe one logical operation, each attempt it
 * sends and the phases around them (authentication, scheduler queueing, retry
 * backoff). They carry only bounded values: the API name and operation id
 * from the compiled descriptor, enumerated phases, outcomes, categories and
 * reasons, HTTP status codes, attempt numbers and durations. URLs, headers,
 * bodies, argument values, credentials, confirmation tokens, tenant labels
 * and error text are never part of an event, so there is nothing to redact.
 *
 * The TypeScript, Python and Rust runtimes emit the same events with the same
 * field names and meanings.
 *
 * @module
 */
import type { Category } from "./types.js";

/** Version of the event contract; incremented for incompatible changes. */
export const TELEMETRY_SCHEMA_VERSION = 1;

/** What an event marks. `phase` events are emitted when a phase ends. */
export type TelemetryEventName = "operation.start" | "operation.end" | "attempt.start" | "attempt.end" | "phase";

/** `auth`: resolving credentials (including a token fetch or refresh).
 * `queue`: waiting for request scheduler capacity. `retry_backoff`: the wait
 * before a permitted retry. */
export type TelemetryPhase = "auth" | "queue" | "retry_backoff";

/** How an operation, attempt or phase ended. `outcome_unknown`: a mutation
 * may have taken effect (its category is `OUTCOME_UNKNOWN`), which takes
 * precedence over `deadline_exceeded`. */
export type TelemetryOutcome = "ok" | "error" | "cancelled" | "deadline_exceeded" | "outcome_unknown";

/** Why a `queue` phase ended: `admitted`, the scheduler's rejection (`full`,
 * `timeout`, `paused`, `closed`), the call's deadline, or cancellation. */
export type TelemetryQueueReason = "admitted" | "full" | "timeout" | "paused" | "closed" | "deadline" | "cancelled";

/** The public entry point that started the operation. */
export type TelemetryEntry = "call";

/** One telemetry event. Fields that do not apply to an event are null. */
export interface TelemetryEvent {
  schema_version: typeof TELEMETRY_SCHEMA_VERSION;
  name: TelemetryEventName;
  /** The API name of the compiled descriptor. */
  api: string;
  /** The operation id of the compiled descriptor. */
  operation: string;
  entry: TelemetryEntry;
  /** Correlates the events of one operation; increases per client from 1.
   * An identifier, not a label: do not use it as a metric dimension. */
  call_id: number;
  /** Whether the operation may change server state. */
  mutation: boolean;
  /** The attempt number within the operation (from 1), for attempt events
   * and for the `queue` and `retry_backoff` phases. */
  attempt: number | null;
  phase: TelemetryPhase | null;
  /** Milliseconds, for `.end` and `phase` events. */
  duration_ms: number | null;
  outcome: TelemetryOutcome | null;
  /** The diagnostic category of a failed operation, attempt or phase. */
  category: Category | null;
  /** The HTTP status of an attempt that received a response. */
  http_status: number | null;
  /** For a `queue` phase. */
  reason: TelemetryQueueReason | null;
  /** Attempts sent, on `operation.end`. */
  attempts: number | null;
}

/** Receives telemetry events. It is called synchronously on the request
 * path and never awaited: keep it fast and hand slow work elsewhere (a
 * bounded buffer such as {@link MemoryTelemetryCollector}). An observer that
 * throws or rejects never changes the call. */
export type TelemetryObserver = ((event: TelemetryEvent) => void) | { onEvent(event: TelemetryEvent): void };

const DEFAULT_CAPACITY = 1024;
const MAX_CAPACITY = 65536;

/** An observer keeping the most recent events in memory, for tests,
 * debugging and exporters that drain it periodically. Holds at most
 * `capacity` events (default 1024, at most 65536); when full, the oldest
 * event is dropped and counted in {@link dropped}. */
export class MemoryTelemetryCollector {
  readonly capacity: number;
  #events: TelemetryEvent[] = [];
  #dropped = 0;

  constructor(capacity: number = DEFAULT_CAPACITY) {
    this.capacity = typeof capacity === "number" && Number.isFinite(capacity)
      ? Math.min(MAX_CAPACITY, Math.max(1, Math.floor(capacity)))
      : DEFAULT_CAPACITY;
  }

  onEvent(event: TelemetryEvent): void {
    if (this.#events.length >= this.capacity) {
      this.#events.shift();
      this.#dropped += 1;
    }
    this.#events.push(event);
  }

  /** The buffered events, oldest first. */
  events(): TelemetryEvent[] {
    return [...this.#events];
  }

  /** Events dropped because the buffer was full. */
  get dropped(): number {
    return this.#dropped;
  }

  /** Remove and return the buffered events, oldest first. */
  drain(): TelemetryEvent[] {
    const events = this.#events;
    this.#events = [];
    return events;
  }

  /** Drop the buffered events and reset the drop count. */
  clear(): void {
    this.#events = [];
    this.#dropped = 0;
  }
}

const CATEGORIES: ReadonlySet<string> = new Set<Category>([
  "VALIDATION_FAILED", "MALFORMED_REQUEST", "REQUEST_TOO_LARGE", "AUTH_FAILED", "NOT_FOUND", "CONFLICT",
  "PRECONDITION_FAILED", "RATE_LIMITED", "UPSTREAM_UNAVAILABLE", "OUTCOME_UNKNOWN", "TRANSPORT_FAILED",
  "CONFIRMATION_REQUIRED", "UNEXPECTED_RESPONSE",
]);

/** The fields of a failure the classification reads. */
export interface FailureFields {
  category?: unknown;
  code?: unknown;
}

/** How a result ended, for `outcome`. */
export function telemetryOutcome(error: FailureFields | null, cancelled: boolean): TelemetryOutcome {
  if (error === null) return "ok";
  if (error.category === "OUTCOME_UNKNOWN") return "outcome_unknown";
  if (cancelled) return "cancelled";
  if (error.code === "DEADLINE_EXCEEDED") return "deadline_exceeded";
  return "error";
}

/** The category of a failure; none for a cancelled one. */
function categoryOf(error: FailureFields | null, outcome: TelemetryOutcome): Category | null {
  return error !== null && outcome !== "cancelled" && typeof error.category === "string" && CATEGORIES.has(error.category) ? (error.category as Category) : null;
}

function statusOf(status: unknown): number | null {
  return typeof status === "number" && Number.isInteger(status) && status >= 100 && status <= 599 ? status : null;
}

function millis(start: number): number {
  return Math.max(0, Math.round((performance.now() - start) * 1000) / 1000);
}

/** Per-client telemetry state: the observer and the call id counter. */
export class Telemetry {
  readonly #observer: TelemetryObserver;
  #next = 0;

  private constructor(observer: TelemetryObserver) {
    this.#observer = observer;
  }

  /** Null when `observer` is not an observer (telemetry stays off). */
  static of(observer: unknown): Telemetry | null {
    if (typeof observer === "function") return new Telemetry(observer as TelemetryObserver);
    if (typeof observer === "object" && observer !== null && typeof (observer as { onEvent?: unknown }).onEvent === "function") {
      return new Telemetry(observer as TelemetryObserver);
    }
    return null;
  }

  /** Start an operation and emit `operation.start`. */
  start(api: string, operation: string, entry: TelemetryEntry, mutation: boolean): TelemetrySpan {
    this.#next += 1;
    const span = new TelemetrySpan(this, api, operation, entry, this.#next, mutation);
    span.emit("operation.start", {});
    return span;
  }

  deliver(event: TelemetryEvent): void {
    try {
      const observer = this.#observer;
      const returned: unknown = typeof observer === "function" ? observer(event) : observer.onEvent(event);
      if (returned instanceof Promise) returned.catch(() => undefined);
    } catch {
      // A failing observer never changes the call.
    }
  }
}

/** The events of one logical operation. */
export class TelemetrySpan {
  readonly #telemetry: Telemetry;
  readonly #api: string;
  readonly #operation: string;
  readonly #entry: TelemetryEntry;
  readonly #callId: number;
  readonly #mutation: boolean;
  readonly #start = performance.now();
  #attempts = 0;
  #ended = false;

  constructor(telemetry: Telemetry, api: string, operation: string, entry: TelemetryEntry, callId: number, mutation: boolean) {
    this.#telemetry = telemetry;
    this.#api = api;
    this.#operation = operation;
    this.#entry = entry;
    this.#callId = callId;
    this.#mutation = mutation;
  }

  emit(name: TelemetryEventName, fields: Partial<TelemetryEvent>): void {
    this.#telemetry.deliver({
      schema_version: TELEMETRY_SCHEMA_VERSION,
      name,
      api: this.#api,
      operation: this.#operation,
      entry: this.#entry,
      call_id: this.#callId,
      mutation: this.#mutation,
      attempt: null,
      phase: null,
      duration_ms: null,
      outcome: null,
      category: null,
      http_status: null,
      reason: null,
      attempts: null,
      ...fields,
    });
  }

  /** The number of the attempt about to be sent, counted across the whole
   * operation (an OAuth re-send after a rejected token continues it). */
  nextAttempt(): number {
    this.#attempts += 1;
    return this.#attempts;
  }

  /** The attempt number most recently started. */
  get attempt(): number {
    return this.#attempts;
  }

  phase(phase: TelemetryPhase, start: number, error: FailureFields | null, cancelled: boolean, extra: Partial<TelemetryEvent> = {}): void {
    this.emit("phase", {
      phase,
      duration_ms: millis(start),
      outcome: telemetryOutcome(error, cancelled),
      category: categoryOf(error, telemetryOutcome(error, cancelled)),
      ...extra,
    });
  }

  attemptStart(attempt: number): number {
    this.emit("attempt.start", { attempt });
    return performance.now();
  }

  attemptEnd(attempt: number, start: number, error: FailureFields | null, cancelled: boolean, status: unknown): void {
    this.emit("attempt.end", {
      attempt,
      duration_ms: millis(start),
      outcome: telemetryOutcome(error, cancelled),
      category: categoryOf(error, telemetryOutcome(error, cancelled)),
      http_status: statusOf(status),
    });
  }

  /** Emit `operation.end` once. */
  end(error: FailureFields | null, cancelled: boolean, status: unknown): void {
    if (this.#ended) return;
    this.#ended = true;
    this.emit("operation.end", {
      duration_ms: millis(this.#start),
      outcome: telemetryOutcome(error, cancelled),
      category: categoryOf(error, telemetryOutcome(error, cancelled)),
      http_status: statusOf(status),
      attempts: this.#attempts,
    });
  }
}

/** The part of an OpenTelemetry `Span` the adapter uses. */
export interface OtelSpanLike {
  setAttribute(key: string, value: string | number | boolean): unknown;
  addEvent(name: string, attributes?: Record<string, string | number | boolean>): unknown;
  setStatus(status: { code: number }): unknown;
  end(): unknown;
}

/** The part of an OpenTelemetry `Tracer` the adapter uses (`startSpan`). */
export interface OtelTracerLike {
  startSpan(name: string, options?: { attributes?: Record<string, string | number | boolean>; kind?: number }): OtelSpanLike;
}

/** Options of {@link openTelemetryObserver}. */
export interface OpenTelemetryObserverOptions {
  /** Operations whose spans are open at once; when exceeded, the oldest is
   * ended and dropped. Default 10000. */
  maxOpenSpans?: number;
}

const OTEL_STATUS_ERROR = 2;
const OTEL_KIND_CLIENT = 2;

/** An observer recording each operation as one OpenTelemetry client span
 * named after the operation id, with attempts and phases as span events
 * and the outcome as attributes (`tungsten.outcome`, `tungsten.category`,
 * `http.response.status_code`, `tungsten.attempts`); a failed or cancelled
 * operation sets the span's status to error. It accepts any object with the
 * `Tracer.startSpan` shape of `@opentelemetry/api`, which this runtime does
 * not depend on; spans start in the tracer's active context. Attributes
 * hold only the bounded event fields. */
export function openTelemetryObserver(tracer: OtelTracerLike, options: OpenTelemetryObserverOptions = {}): (event: TelemetryEvent) => void {
  const limit = typeof options.maxOpenSpans === "number" && Number.isFinite(options.maxOpenSpans) ? Math.max(1, Math.floor(options.maxOpenSpans)) : 10000;
  const open = new Map<number, OtelSpanLike>();
  return (event: TelemetryEvent): void => {
    if (event.name === "operation.start") {
      if (open.size >= limit) {
        const [oldest, span] = open.entries().next().value as [number, OtelSpanLike];
        open.delete(oldest);
        span.end();
      }
      open.set(event.call_id, tracer.startSpan(event.operation, {
        kind: OTEL_KIND_CLIENT,
        attributes: { "tungsten.api": event.api, "tungsten.operation": event.operation, "tungsten.entry": event.entry, "tungsten.mutation": event.mutation },
      }));
      return;
    }
    const span = open.get(event.call_id);
    if (span === undefined) return;
    const attributes = otelAttributes(event);
    if (event.name !== "operation.end") {
      span.addEvent(event.name === "phase" ? `tungsten.phase.${event.phase ?? "unknown"}` : `tungsten.${event.name}`, attributes);
      return;
    }
    open.delete(event.call_id);
    for (const [key, value] of Object.entries(attributes)) span.setAttribute(key, value);
    if (event.outcome !== "ok") span.setStatus({ code: OTEL_STATUS_ERROR });
    span.end();
  };
}

function otelAttributes(event: TelemetryEvent): Record<string, string | number | boolean> {
  const attributes: Record<string, string | number | boolean> = {};
  if (event.attempt !== null) attributes["tungsten.attempt"] = event.attempt;
  if (event.duration_ms !== null) attributes["tungsten.duration_ms"] = event.duration_ms;
  if (event.outcome !== null) attributes["tungsten.outcome"] = event.outcome;
  if (event.category !== null) attributes["tungsten.category"] = event.category;
  if (event.http_status !== null) attributes["http.response.status_code"] = event.http_status;
  if (event.reason !== null) attributes["tungsten.queue.reason"] = event.reason;
  if (event.attempts !== null) attributes["tungsten.attempts"] = event.attempts;
  return attributes;
}
