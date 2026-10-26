// SPDX-License-Identifier: Apache-2.0
/**
 * Bounded request admission shared by the calls of one or more clients.
 *
 * A {@link RequestScheduler} keeps, per scope (a tenant label plus, by
 * default, the request origin), a count of requests in flight, a bounded
 * first-in-first-out queue of requests waiting for capacity, and a shared
 * rate-limit pause. Ordinary calls and event streams are counted apart, so
 * long-lived streams cannot starve ordinary calls. Nothing is coordinated
 * across processes.
 *
 * @module
 */

/** Limits of a {@link RequestScheduler}. Every limit applies per scope. */
export interface SchedulerOptions {
  /** Ordinary requests in flight at once. A positive integer (at most 10000). */
  maxActive: number;
  /** Ordinary requests, and separately event streams, that may wait for
   * capacity; one more is rejected at once with `QUEUE_FULL`. 0 never
   * waits. Default 100. */
  maxQueued?: number;
  /** Event streams open at once. Default `maxActive`. */
  maxStreams?: number;
  /** Longest wait for capacity, in milliseconds; a longer wait fails with
   * `QUEUE_TIMEOUT` and nothing is sent. The call's own deadline and
   * AbortSignal also end the wait. Default 30000. */
  maxQueueMs?: number;
  /** `"origin"` (default): one scope per tenant and request origin.
   * `"scheduler"`: one scope per tenant, whatever the origin. */
  scope?: "origin" | "scheduler";
  /** Retries shared by every call of one scope. Without it, only each
   * call's own retry policy limits retries. */
  retryBudget?: RetryBudgetOptions;
}

/**
 * A token bucket of retries shared by the callers of one scope, so that a
 * failing upstream sees a bounded share of retries instead of every
 * caller's full retry policy at once. A scope starts with `burst` retries;
 * each first attempt admitted earns `ratio` more and every second earns
 * `minPerSecond`, up to `burst`. A retry spends one; without one, the call
 * stops retrying and returns its last failure as is.
 */
export interface RetryBudgetOptions {
  /** Retries earned per first attempt admitted, from 0 to 1. Default 0.1. */
  ratio?: number;
  /** Retries earned per second whatever the traffic, from 0 to 10000.
   * Default 1. */
  minPerSecond?: number;
  /** The most retries saved up, and the starting balance; an integer from
   * 1 to 1000000. Default 10. */
  burst?: number;
}

/** Why a request was not admitted. */
export type Rejection =
  | { reason: "full" | "timeout" | "aborted" | "closed" }
  | { reason: "paused"; retryAfterMs: number };

/** Capacity held by one admitted request. */
export interface Permit {
  /** Record the answer: the wait a `RATE_LIMITED` answer asks for (null
   * when it named none), or `undefined` for any other answer. The first
   * call counts; later calls are ignored. */
  settle(rateLimitedMs: number | null | undefined): void;
  /** Free the capacity. Safe to call more than once. */
  release(): void;
}

/** Counts across every scope, for telemetry and load checks. */
export interface SchedulerStats {
  active: number;
  streams: number;
  queued: number;
  scopes: number;
}

type Kind = "call" | "stream";

interface Waiter {
  kind: Kind;
  retry: boolean;
  random: () => number;
  grant: (permit: Permit) => void;
  reject: (rejection: Rejection) => void;
}

interface Pool {
  active: Record<Kind, number>;
  queue: Record<Kind, Waiter[]>;
  pausedUntil: number;
  probing: boolean;
  timer: ReturnType<typeof setTimeout> | null;
  /** Retries left in the shared budget, as of `refilledAt`. */
  tokens: number;
  refilledAt: number;
}

interface Budget {
  ratio: number;
  minPerSecond: number;
  burst: number;
}

/** The pause after a `RATE_LIMITED` answer that names no wait. */
const DEFAULT_PAUSE_MS = 1000;
const MAX_PAUSE_MS = 3_600_000;
/** Slack for the sums of fractional retry earnings. */
const EPSILON = 1e-9;

function integer(value: unknown, fallback: number, min: number, max: number): number {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== "number" || !Number.isInteger(value) || value < min || value > max) {
    throw new RangeError(`expected an integer from ${min} to ${max}, got ${String(value)}`);
  }
  return value;
}

function finite(value: unknown, fallback: number, min: number, max: number): number {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== "number" || !Number.isFinite(value) || value < min || value > max) {
    throw new RangeError(`expected a number from ${min} to ${max}, got ${String(value)}`);
  }
  return value;
}

/**
 * Admits requests within the configured bounds. Pass one to
 * `ClientOptions.scheduler` of several clients to share capacity and
 * rate-limit pauses between them; give each client a `tenant` to keep
 * their accounting apart.
 */
export class RequestScheduler {
  readonly maxActive: number;
  readonly maxQueued: number;
  readonly maxStreams: number;
  readonly maxQueueMs: number;
  readonly scope: "origin" | "scheduler";
  readonly #budget: Budget | null;
  readonly #pools = new Map<string, Pool>();
  readonly #idle: Array<() => void> = [];
  #closed = false;

  constructor(options: SchedulerOptions) {
    this.maxActive = integer(options.maxActive, 0, 1, 10_000);
    this.maxQueued = integer(options.maxQueued, 100, 0, 1_000_000);
    this.maxStreams = integer(options.maxStreams, this.maxActive, 1, 10_000);
    this.maxQueueMs = integer(options.maxQueueMs, 30_000, 0, MAX_PAUSE_MS);
    if (options.scope !== undefined && options.scope !== "origin" && options.scope !== "scheduler") {
      throw new RangeError(`scope must be "origin" or "scheduler", got ${String(options.scope)}`);
    }
    this.scope = options.scope ?? "origin";
    const budget = options.retryBudget;
    this.#budget = budget === undefined || budget === null ? null : {
      ratio: finite(budget.ratio, 0.1, 0, 1),
      minPerSecond: finite(budget.minPerSecond, 1, 0, 10_000),
      burst: integer(budget.burst, 10, 1, 1_000_000),
    };
  }

  /** The scope of a request to `url` made for `tenant`. */
  scopeKey(url: string, tenant: string | undefined): string {
    let origin = "";
    if (this.scope === "origin") {
      try {
        origin = new URL(url).origin;
      } catch {
        origin = "";
      }
    }
    return `${tenant ?? ""}\n${origin}`;
  }

  /**
   * Wait for capacity in `key`. Resolves to a permit, or to why none was
   * given; a rejected request must not be sent. `deadline` is an absolute
   * `performance.now()` time that also ends the wait. `retry` marks a
   * repeat of an earlier attempt, which earns the retry budget nothing.
   */
  acquire(key: string, kind: Kind, signal?: AbortSignal, deadline?: number, random: () => number = Math.random, retry = false): Promise<Permit | Rejection> {
    if (this.#closed) return Promise.resolve({ reason: "closed" });
    if (signal?.aborted) return Promise.resolve({ reason: "aborted" });
    const pool = this.#pool(key);
    const now = performance.now();
    const budget = Math.min(this.maxQueueMs, deadline === undefined ? Infinity : Math.max(0, deadline - now));
    const paused = Math.max(0, pool.pausedUntil - now);
    if (paused === 0 && pool.queue[kind].length === 0 && pool.active[kind] < this.#limit(pool, kind)) {
      return Promise.resolve(this.#grant(key, pool, kind, random, retry));
    }
    if (paused > budget) {
      this.#forget(key, pool);
      return Promise.resolve({ reason: "paused", retryAfterMs: Math.ceil(paused) });
    }
    if (pool.queue[kind].length >= this.maxQueued || budget <= 0) {
      this.#forget(key, pool);
      return Promise.resolve({ reason: pool.queue[kind].length >= this.maxQueued ? "full" : "timeout" });
    }
    return new Promise((resolve) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const finish = (): void => {
        if (timer !== undefined) clearTimeout(timer);
        signal?.removeEventListener("abort", onAbort);
        const queue = pool.queue[kind];
        const index = queue.indexOf(waiter);
        if (index >= 0) queue.splice(index, 1);
      };
      const waiter: Waiter = {
        kind,
        retry,
        random,
        grant: (permit) => { finish(); resolve(permit); },
        reject: (rejection) => { finish(); resolve(rejection); this.#forget(key, pool); this.#pump(key, pool); },
      };
      const onAbort = (): void => waiter.reject({ reason: "aborted" });
      if (Number.isFinite(budget)) timer = setTimeout(() => waiter.reject({ reason: "timeout" }), budget);
      signal?.addEventListener("abort", onAbort, { once: true });
      pool.queue[kind].push(waiter);
      this.#pump(key, pool);
    });
  }

  /** Take one retry from the shared budget of `key`. False when the
   * budget is spent: the caller must not retry. Always true without a
   * retry budget. */
  spendRetry(key: string): boolean {
    const budget = this.#budget;
    if (budget === null) return true;
    const pool = this.#pool(key);
    this.#refill(pool, budget);
    if (pool.tokens < 1 - EPSILON) return false;
    pool.tokens = Math.max(0, pool.tokens - 1);
    return true;
  }

  /** Reject every waiting request and every later one; requests in
   * flight keep their capacity until they finish. */
  close(): void {
    this.#closed = true;
    for (const [key, pool] of [...this.#pools]) {
      for (const kind of ["call", "stream"] as const) {
        for (const waiter of [...pool.queue[kind]]) waiter.reject({ reason: "closed" });
      }
      this.#forget(key, pool);
    }
  }

  /** Resolves once no request is in flight or waiting. */
  drained(): Promise<void> {
    if (this.#idleNow()) return Promise.resolve();
    return new Promise((resolve) => this.#idle.push(resolve));
  }

  stats(): SchedulerStats {
    const stats = { active: 0, streams: 0, queued: 0, scopes: this.#pools.size };
    for (const pool of this.#pools.values()) {
      stats.active += pool.active.call;
      stats.streams += pool.active.stream;
      stats.queued += pool.queue.call.length + pool.queue.stream.length;
    }
    return stats;
  }

  #pool(key: string): Pool {
    let pool = this.#pools.get(key);
    if (pool === undefined) {
      // Scopes kept only for their unfinished pause or spent retry budget
      // are dropped once neither matters any more.
      for (const [other, idle] of this.#pools) {
        if (!this.#disposable(idle)) continue;
        if (idle.timer !== null) clearTimeout(idle.timer);
        this.#pools.delete(other);
      }
      pool = {
        active: { call: 0, stream: 0 }, queue: { call: [], stream: [] }, pausedUntil: 0, probing: false, timer: null,
        tokens: this.#budget?.burst ?? 0, refilledAt: performance.now(),
      };
      this.#pools.set(key, pool);
    }
    return pool;
  }

  /** Add what the budget earned per second since it was last refilled. */
  #refill(pool: Pool, budget: Budget): void {
    const now = performance.now();
    pool.tokens = Math.min(budget.burst, pool.tokens + (Math.max(0, now - pool.refilledAt) / 1000) * budget.minPerSecond);
    pool.refilledAt = now;
  }

  /** An idle scope whose pause has passed and whose retry budget is full
   * holds nothing worth keeping. */
  #disposable(pool: Pool): boolean {
    if (pool.active.call + pool.active.stream + pool.queue.call.length + pool.queue.stream.length > 0) return false;
    if (pool.pausedUntil > performance.now()) return false;
    const budget = this.#budget;
    if (budget === null) return true;
    this.#refill(pool, budget);
    return pool.tokens >= budget.burst - EPSILON;
  }

  /** After a rate-limit pause, one request per kind probes the quota until
   * an answer that is not rate limited arrives. */
  #limit(pool: Pool, kind: Kind): number {
    if (pool.probing) return 1;
    return kind === "call" ? this.maxActive : this.maxStreams;
  }

  #grant(key: string, pool: Pool, kind: Kind, random: () => number, retry: boolean): Permit {
    pool.active[kind] += 1;
    const budget = this.#budget;
    if (budget !== null && !retry) {
      this.#refill(pool, budget);
      pool.tokens = Math.min(budget.burst, pool.tokens + budget.ratio);
    }
    let settled = false;
    let released = false;
    return {
      settle: (rateLimitedMs) => {
        if (settled) return;
        settled = true;
        if (rateLimitedMs === undefined) {
          pool.probing = false;
          return;
        }
        const wait = Math.min(MAX_PAUSE_MS, Math.max(0, rateLimitedMs ?? DEFAULT_PAUSE_MS));
        // A little spread keeps clients that share a quota from all
        // resuming at the same instant.
        const r = random();
        const spread = Math.min(1000, wait * 0.1) * (Number.isFinite(r) ? Math.min(1, Math.max(0, r)) : 0.5);
        pool.pausedUntil = Math.max(pool.pausedUntil, performance.now() + wait + spread);
        pool.probing = true;
        this.#pump(key, pool);
      },
      release: () => {
        if (released) return;
        released = true;
        pool.active[kind] -= 1;
        this.#pump(key, pool);
        this.#forget(key, pool);
      },
    };
  }

  /** Admit waiting requests in order while capacity allows. */
  #pump(key: string, pool: Pool): void {
    const wait = pool.pausedUntil - performance.now();
    if (wait > 0) {
      if (pool.timer === null && (pool.queue.call.length > 0 || pool.queue.stream.length > 0)) {
        pool.timer = setTimeout(() => {
          pool.timer = null;
          this.#pump(key, pool);
        }, wait);
      }
      return;
    }
    for (const kind of ["call", "stream"] as const) {
      const queue = pool.queue[kind];
      while (queue.length > 0 && pool.active[kind] < this.#limit(pool, kind)) {
        const waiter = queue[0] as Waiter;
        waiter.grant(this.#grant(key, pool, kind, waiter.random, waiter.retry));
      }
    }
  }

  #idleNow(): boolean {
    const stats = this.stats();
    return stats.active + stats.streams + stats.queued === 0;
  }

  /** Drop an idle scope once its pause has passed and its retry budget
   * is full again; wake `drained()`. */
  #forget(key: string, pool: Pool): void {
    const busy = pool.active.call + pool.active.stream + pool.queue.call.length + pool.queue.stream.length;
    if (busy > 0) return;
    if (this.#disposable(pool)) {
      if (pool.timer !== null) clearTimeout(pool.timer);
      this.#pools.delete(key);
    }
    if (this.#idle.length > 0 && this.#idleNow()) for (const resolve of this.#idle.splice(0)) resolve();
  }
}
