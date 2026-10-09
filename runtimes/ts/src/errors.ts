// SPDX-License-Identifier: Apache-2.0
/**
 * Throwing ergonomics for people (planning/05: "a throwing client variant
 * is exposed for human ergonomics, implemented in the runtime"). Agents
 * should keep branching on `Result.ok`.
 */
import type { Diagnostic, Result } from "./types.js";
import { isRecord } from "./util.js";

/** An error carrying the diagnostic envelope of a failed call. */
export class TungstenError extends Error {
  /** The envelope, exactly as the non-throwing API returns it. */
  readonly diagnostic: Diagnostic;
  /** The failed result's `partial`: what the call or macro already
   * produced (it can hold values the API shows only once). */
  readonly partial: unknown;

  constructor(diagnostic: Diagnostic, partial?: unknown) {
    super(`${diagnostic.category} (${diagnostic.operation}): ${diagnostic.remediation}`);
    this.name = "TungstenError";
    this.diagnostic = diagnostic;
    this.partial = partial;
  }

  /** Serializes to the envelope, so logging the error logs the envelope
   * (never `partial`, which can hold secrets). */
  toJSON(): Diagnostic {
    return this.diagnostic;
  }
}

/** The value of a successful result; throws {@link TungstenError} otherwise. */
export function unwrap<T>(result: Result<T>): T {
  if (result.ok) return result.value;
  throw new TungstenError(result.error, result.partial);
}

function isResult(value: unknown): value is Result<unknown> {
  return isRecord(value) && typeof value.ok === "boolean" && (value.ok ? "value" in value : isRecord(value.error));
}

type Unwrapped<R> = [R] extends [Result<infer V>] ? V : R;

type ThrowingReturn<R> =
  R extends Promise<infer P> ? Promise<Unwrapped<P>> : R extends AsyncIterable<infer Y> ? AsyncIterable<Unwrapped<Y>> : R;

type ThrowingCallable<T> = T extends (...args: infer A) => infer R ? (...args: A) => ThrowingReturn<R> : unknown;

/** `T` with every method returning `Promise<Result<V>>` turned into one
 * returning `Promise<V>` (throwing {@link TungstenError}), and every
 * `AsyncIterable<Result<V>>` into `AsyncIterable<V>`, recursively through
 * nested resources. */
export type Throwing<T> = T extends object ? ThrowingCallable<T> & { readonly [K in keyof T]: Throwing<T[K]> } : T;

function throwingIterable(iterable: AsyncIterable<unknown>): AsyncIterable<unknown> {
  return {
    async *[Symbol.asyncIterator]() {
      for await (const item of iterable) yield isResult(item) ? unwrap(item) : item;
    },
  };
}

function convert(value: unknown): unknown {
  if (value instanceof Promise) return value.then((resolved) => (isResult(resolved) ? unwrap(resolved) : resolved));
  if (isRecord(value) && typeof (value as { [Symbol.asyncIterator]?: unknown })[Symbol.asyncIterator] === "function") {
    return throwingIterable(value as unknown as AsyncIterable<unknown>);
  }
  return value;
}

/**
 * A view of a generated client (or any object of resources) whose methods
 * throw {@link TungstenError} instead of returning `{ok: false}`:
 * `throwing(client).webhooks.list()` resolves to the body.
 */
export function throwing<T extends object>(target: T): Throwing<T> {
  const proxies = new WeakMap<object, object>();
  const originals = new WeakMap<object, object>();
  const wrap = (value: object): object => {
    const cached = proxies.get(value);
    if (cached) return cached;
    const proxy = new Proxy(value, {
      get(obj, prop, receiver) {
        const inner: unknown = Reflect.get(obj, prop, receiver === proxy ? obj : receiver);
        if (typeof inner === "function" && prop === "constructor") return inner;
        return (typeof inner === "object" && inner !== null) || typeof inner === "function" ? wrap(inner as object) : inner;
      },
      apply(fn, thisArg: unknown, args: unknown[]) {
        const self = typeof thisArg === "object" && thisArg !== null ? (originals.get(thisArg) ?? thisArg) : thisArg;
        return convert(Reflect.apply(fn as (...a: unknown[]) => unknown, self, args));
      },
    });
    proxies.set(value, proxy);
    originals.set(proxy, value);
    return proxy;
  };
  return wrap(target) as Throwing<T>;
}
