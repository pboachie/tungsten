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
import { hasOwn } from "./util.js";

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

export class EventStream<T> implements AsyncIterable<StreamItem<T>> {
  readonly #source: AsyncGenerator<StreamItem<T>>;
  readonly #abort: () => void;
  #cancelled = false;

  /** @param source the items of the stream
   * @param abort aborts the request the source reads (closes the connection) */
  constructor(source: AsyncGenerator<StreamItem<T>>, abort: () => void) {
    this.#source = source;
    this.#abort = abort;
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

  /** Read the stream to its end into a list of the events' `data`. */
  collect(): Promise<StreamResult<T[]>> {
    return this.reduce<T[]>((all, value) => {
      all.push(value);
      return all;
    }, []);
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
