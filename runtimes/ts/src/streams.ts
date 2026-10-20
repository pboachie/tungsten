// SPDX-License-Identifier: Apache-2.0
/**
 * `EventStream`: what `ClientCore.stream` returns. It is the async iterable
 * of {@link StreamItem}s it always was, plus helpers that consume it:
 * {@link EventStream.on} (dispatch by event name), {@link EventStream.collect},
 * {@link EventStream.reduce}, {@link EventStream.first} and
 * {@link EventStream.cancel}. A stream is read once; every helper closes the
 * connection when it returns.
 */
import type { Diagnostic, StreamItem } from "./types.js";
import { canonicalJson, hasOwn } from "./util.js";
import { diagnostic } from "./envelope.js";

/** One delivered event. */
export interface StreamEvent<T> {
  value: T;
  /** The `event` field, `message` when the server named none. */
  event: string;
  /** The last event id the stream has set so far, or null. */
  id: string | null;
  /** The last `retry` value (milliseconds) the stream has set so far, or null. */
  retry: number | null;
}

/** What a helper resolves to: the result and the number of reconnects the
 * stream made, or the failure that ended the stream with what was gathered
 * before it as `partial`. */
export type StreamResult<R, P = R> =
  | { ok: true; value: R; reconnects: number }
  | { ok: false; error: Diagnostic; partial: P; reconnects: number };

/** A handler of {@link EventStream.on}: receives the decoded `data` and the event. */
export type EventHandler<T> = (value: T, event: StreamEvent<T>) => void | Promise<void>;

/** The key of {@link EventStream.on} handlers that receives every event no
 * other handler took. */
export const ANY_EVENT = "*";

/** Optional collection budgets; an unset budget defaults to 4 MiB or 60 seconds. */
export interface CollectOptions {
  maxBytes?: number;
  maxMs?: number;
}

export class EventStream<T> implements AsyncIterable<StreamItem<T>> {
  readonly #source: AsyncGenerator<StreamItem<T>>;
  readonly #abort: () => void;
  #cancelled = false;
  readonly #operation: string;
  readonly #mutation: boolean;

  /** @param source the items of the stream
   * @param abort aborts the request the source reads (closes the connection) */
  constructor(source: AsyncGenerator<StreamItem<T>>, abort: () => void, operation = "<stream>", mutation = false) {
    this.#source = source;
    this.#abort = abort;
    this.#operation = operation;
    this.#mutation = mutation;
  }

  /** Whether {@link cancel} was called. */
  get cancelled(): boolean {
    return this.#cancelled;
  }

  /** Close the connection and end the stream: a pending or later read ends
   * without a failure item. Safe to call more than once. */
  cancel(): void {
    if (this.#cancelled) return;
    this.#cancelled = true;
    this.#abort();
    this.#source.return(undefined).catch(() => undefined);
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<StreamItem<T>> {
    try {
      for (;;) {
        const next = await this.#source.next();
        if (next.done === true || this.#cancelled) return;
        yield next.value;
      }
    } finally {
      this.#abort();
      await this.#source.return(undefined).catch(() => undefined);
    }
  }

  /** Read the stream to its end and call the handler named like each event
   * (`handlers.message_delta` for `event: message_delta`; events without a
   * name are `message`), else `handlers["*"]`. A handler that throws or
   * rejects ends the stream (the connection is closed) and the error
   * propagates. Resolves to the number of events delivered. */
  async on(handlers: Record<string, EventHandler<T> | undefined>): Promise<StreamResult<number, number>> {
    let count = 0;
    let reconnects = 0;
    for await (const item of this) {
      if (!item.ok) return { ok: false, error: item.error, partial: count, reconnects };
      reconnects = item.meta.reconnects;
      const handler = hasOwn(handlers, item.event) ? handlers[item.event] : handlers[ANY_EVENT];
      count += 1;
      await handler?.(item.value, { value: item.value, event: item.event, id: item.id, retry: item.retry });
    }
    return { ok: true, value: count, reconnects };
  }

  /** Read the stream to its end and fold the events into one value
   * (concatenating text deltas, say). `partial` of a failure is the value
   * folded so far. */
  async reduce<A>(fold: (accumulator: A, value: T, event: StreamEvent<T>) => A, initial: A): Promise<StreamResult<A>> {
    let accumulator = initial;
    let reconnects = 0;
    for await (const item of this) {
      if (!item.ok) return { ok: false, error: item.error, partial: accumulator, reconnects };
      reconnects = item.meta.reconnects;
      accumulator = fold(accumulator, item.value, { value: item.value, event: item.event, id: item.id, retry: item.retry });
    }
    return { ok: true, value: accumulator, reconnects };
  }

  /** Read the stream to its end into a list of the events' `data`. Always
   * bounded: with no options (or either budget unset) it defaults to 4 MiB of
   * canonical-JSON event data and 60 seconds. Use `reduce`, `on` or iteration
   * to process long or large streams incrementally. */
  async collect(options?: CollectOptions): Promise<StreamResult<T[]>> {
    const maximum = options?.maxBytes ?? 4 * 1024 * 1024;
    const milliseconds = options?.maxMs ?? 60_000;
    const values: T[] = [];
    let reconnects = 0;
    const failure = (what: string, invalid = false): StreamResult<T[]> => ({
      ok: false, partial: values, reconnects,
      error: diagnostic(this.#operation, invalid ? "VALIDATION_FAILED" : "UNEXPECTED_RESPONSE", {
        failed_parameter: "events", expected: what, retryable: "never",
        remediation: invalid ? "Collection budgets must be positive safe integers; nothing was read." :
          `The stream was abandoned after ${values.length} events (${what}; collect maxBytes and maxMs).${this.#mutation ? " The call may have taken effect; inspect API state before retrying." : ""} The events collected so far precede this error.`,
      }),
    });
    if (options === null || ![maximum, milliseconds].every(value => Number.isSafeInteger(value) && value > 0)) {
      this.cancel();
      return failure("positive safe integer collection budgets", true);
    }
    const deadline = performance.now() + milliseconds;
    let bytes = 0;
    const expired = Symbol("collection deadline");
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      for (;;) {
        const remaining = deadline - performance.now();
        if (remaining <= 0) return failure(`at most ${milliseconds} ms to collect the events`);
        const step = await Promise.race([
          this.#source.next(),
          new Promise<typeof expired>(resolve => {
            const wake = (): void => {
              const left = deadline - performance.now();
              if (left <= 0) resolve(expired);
              else timer = setTimeout(wake, Math.min(left, 2_147_483_647));
            };
            wake();
          }),
        ]);
        clearTimeout(timer);
        timer = undefined;
        if (step === expired) return failure(`at most ${milliseconds} ms to collect the events`);
        if (step.done === true || this.#cancelled) return { ok: true, value: values, reconnects };
        const item = step.value;
        if (!item.ok) return { ok: false, error: item.error, partial: values, reconnects };
        reconnects = item.meta.reconnects;
        try {
          bytes += new TextEncoder().encode(canonicalJson(item.value)).byteLength;
        } catch {
          return failure("JSON-serializable events");
        }
        if (performance.now() >= deadline) return failure(`at most ${milliseconds} ms to collect the events`);
        if (bytes > maximum) return failure(`at most ${maximum} bytes of events`);
        values.push(item.value);
      }
    } finally {
      clearTimeout(timer);
      this.cancel();
    }
  }

  /** The first event for which `predicate` holds (any event without one),
   * then close the stream; `value` is null when the stream ends without
   * one. */
  async first(predicate?: (value: T, event: StreamEvent<T>) => boolean): Promise<StreamResult<StreamEvent<T> | null, null>> {
    let reconnects = 0;
    for await (const item of this) {
      if (!item.ok) return { ok: false, error: item.error, partial: null, reconnects };
      reconnects = item.meta.reconnects;
      const event: StreamEvent<T> = { value: item.value, event: item.event, id: item.id, retry: item.retry };
      if (predicate === undefined || predicate(item.value, event)) return { ok: true, value: event, reconnects };
    }
    return { ok: true, value: null, reconnects };
  }
}
