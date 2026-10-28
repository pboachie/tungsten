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

/** Limits of a {@link RequestScheduler}. Every limit applies per scope,
 * except the optional `maxActiveTotal` and `maxStreamsTotal`. */
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
  /** Retries per call a scope may spend over time, shared by every caller
   * of the scope (0 to 1). Each call adds this much to the scope's retry
   * budget and each retry spends 1; a retry the budget cannot pay for is not
   * made and the call returns its last failure. Default: retries are not
   * budgeted. */
  retryRatio?: number;
  /** Retries a scope's budget holds before any call added to it, and the
   * base of its cap (`retryReserve + 100 × retryRatio`). Default 10. */
  retryReserve?: number;
  /** Ordinary requests in flight at once across every scope, on top of
   * each scope's `maxActive`. Scopes waiting for this capacity take turns,
   * one request each, so a busy scope cannot starve a quiet one. Default:
   * no cap across scopes. A positive integer (at most 1000000). */
  maxActiveTotal?: number;
  /** Event streams open at once across every scope, on top of each
   * scope's `maxStreams`. Default: no cap across scopes. */
  maxStreamsTotal?: number;
  /** Receives every admission event, synchronously; see
   * {@link SchedulerEvent}. Its exceptions are ignored and it must not wait.
   * Default: none. */
  onEvent?: (event: SchedulerEvent) => void;
}

/**
 * One admission event (contract version 1). Events name no scope, tenant,
 * URL or payload, so they carry no secret and few distinct values; read
 * {@link RequestScheduler.stats} for counts. Durations are whole
 * milliseconds; a field that does not apply is null.
 */
export interface SchedulerEvent {
  version: 1;
  /** `admitted`: a request got capacity. `rejected`: it did not and was
   * not sent. `released`: a request gave its capacity back. `paused`: a
   * `RATE_LIMITED` answer paused its scope. `retry_denied`: the retry
   * budget refused a retry. */
  event: "admitted" | "rejected" | "released" | "paused" | "retry_denied";
  /** Ordinary call or event stream; null for `paused`. */
  kind: "call" | "stream" | null;
  /** `admitted`, `rejected`: how long the request waited for capacity. */
  wait_ms: number | null;
  /** `released`: how long the request held its capacity. */
  held_ms: number | null;
  /** `paused`: how long the scope pauses; `rejected` with reason `paused`:
   * how long the pause still lasts. */
  pause_ms: number | null;
  /** `rejected`: why. */
  reason: "full" | "timeout" | "paused" | "closed" | "aborted" | null;
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
}

/** Scopes whose retry budget is remembered; the least recently used is
 * forgotten (and starts again from the reserve) beyond this. */
const MAX_BUDGETS = 1024;

/** The pause after a `RATE_LIMITED` answer that names no wait. */
const DEFAULT_PAUSE_MS = 1000;
const MAX_PAUSE_MS = 3_600_000;

function integer(value: unknown, fallback: number, min: number, max: number): number {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== "number" || !Number.isInteger(value) || value < min || value > max) {
    throw new RangeError(`expected an integer from ${min} to ${max}, got ${String(value)}`);
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
  readonly retryRatio: number | null;
  readonly retryReserve: number;
  readonly maxActiveTotal: number | null;
  readonly maxStreamsTotal: number | null;
  readonly #total: Record<Kind, number> = { call: 0, stream: 0 };
  /** The scope served last under a cap across scopes, per kind. */
  readonly #served: Record<Kind, string | null> = { call: null, stream: null };
  readonly #pools = new Map<string, Pool>();
  readonly #budgets = new Map<string, number>();
  readonly #idle: Array<() => void> = [];
  readonly #onEvent: ((event: SchedulerEvent) => void) | null;
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
    const ratio = options.retryRatio;
    if (ratio !== undefined && ratio !== null && (typeof ratio !== "number" || !Number.isFinite(ratio) || ratio < 0 || ratio > 1)) {
      throw new RangeError(`retryRatio must be a number from 0 to 1, got ${String(ratio)}`);
    }
    this.retryRatio = ratio ?? null;
    this.retryReserve = integer(options.retryReserve, 10, 0, 10_000);
    this.maxActiveTotal = options.maxActiveTotal === undefined || options.maxActiveTotal === null ? null : integer(options.maxActiveTotal, 0, 1, 1_000_000);
    this.maxStreamsTotal = options.maxStreamsTotal === undefined || options.maxStreamsTotal === null ? null : integer(options.maxStreamsTotal, 0, 1, 1_000_000);
    if (options.onEvent !== undefined && options.onEvent !== null && typeof options.onEvent !== "function") {
      throw new TypeError("onEvent must be a function");
    }
    this.#onEvent = options.onEvent ?? null;
  }

  #emit(event: SchedulerEvent["event"], kind: Kind | null, fields: Partial<Pick<SchedulerEvent, "wait_ms" | "held_ms" | "pause_ms" | "reason">> = {}): void {
    const observer = this.#onEvent;
    if (observer === null) return;
    const ms = (value: number | null | undefined): number | null => (value === null || value === undefined ? null : Math.max(0, Math.round(value)));
    try {
      observer({
        version: 1,
        event,
        kind,
        wait_ms: ms(fields.wait_ms),
        held_ms: ms(fields.held_ms),
        pause_ms: ms(fields.pause_ms),
        reason: fields.reason ?? null,
      });
    } catch {
      // Observers never change admission.
    }
  }

  /** Add one call's share to the retry budget of `key`. */
  recordCall(key: string): void {
    if (this.retryRatio === null) return;
    const cap = this.retryReserve + 100 * this.retryRatio;
    this.#setBudget(key, Math.min(cap, this.#budget(key) + this.retryRatio));
  }

  /** Spend one retry from the budget of `key`; false when it cannot pay. */
  allowRetry(key: string): boolean {
    if (this.retryRatio === null) return true;
    const balance = this.#budget(key);
    if (balance < 1) {
      this.#emit("retry_denied", "call");
      return false;
    }
    this.#setBudget(key, balance - 1);
    return true;
  }

  #budget(key: string): number {
    return this.#budgets.get(key) ?? this.retryReserve;
  }

  #setBudget(key: string, balance: number): void {
    this.#budgets.delete(key);
    this.#budgets.set(key, balance);
    if (this.#budgets.size > MAX_BUDGETS) {
      const oldest = this.#budgets.keys().next().value;
      if (oldest !== undefined) this.#budgets.delete(oldest);
    }
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
   * `performance.now()` time that also ends the wait.
   */
  acquire(key: string, kind: Kind, signal?: AbortSignal, deadline?: number, random: () => number = Math.random): Promise<Permit | Rejection> {
    const outcome = this.#acquire(key, kind, signal, deadline, random);
    if (this.#onEvent === null) return outcome;
    const started = performance.now();
    return outcome.then((admitted) => {
      const wait_ms = performance.now() - started;
      if ("release" in admitted) this.#emit("admitted", kind, { wait_ms });
      else this.#emit("rejected", kind, { wait_ms, reason: admitted.reason, pause_ms: admitted.reason === "paused" ? admitted.retryAfterMs : null });
      return admitted;
    });
  }

  #acquire(key: string, kind: Kind, signal: AbortSignal | undefined, deadline: number | undefined, random: () => number): Promise<Permit | Rejection> {
    if (this.#closed) return Promise.resolve({ reason: "closed" });
    if (signal?.aborted) return Promise.resolve({ reason: "aborted" });
    const pool = this.#pool(key);
    const now = performance.now();
    // The wait ends at an absolute time: the deadline itself when it comes first.
    const until = Math.min(now + this.maxQueueMs, deadline ?? Infinity);
    const budget = Math.max(0, until - now);
    const paused = Math.max(0, pool.pausedUntil - now);
    if (paused === 0 && pool.queue[kind].length === 0 && pool.active[kind] < this.#limit(pool, kind) && this.#room(kind)) {
      return Promise.resolve(this.#grant(key, pool, kind, random));
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
        random,
        grant: (permit) => { finish(); resolve(permit); },
        reject: (rejection) => { finish(); resolve(rejection); this.#forget(key, pool); this.#pump(key, pool); },
      };
      const onAbort = (): void => waiter.reject({ reason: "aborted" });
      // A timer can fire just before the clock reaches `until`; the wait ends only once it has.
      const expire = (): void => {
        const left = until - performance.now();
        if (left > 0) timer = setTimeout(expire, left);
        else waiter.reject({ reason: "timeout" });
      };
      if (Number.isFinite(budget)) timer = setTimeout(expire, budget);
      signal?.addEventListener("abort", onAbort, { once: true });
      pool.queue[kind].push(waiter);
      this.#pump(key, pool);
    });
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
      pool = { active: { call: 0, stream: 0 }, queue: { call: [], stream: [] }, pausedUntil: 0, probing: false, timer: null };
      this.#pools.set(key, pool);
    }
    return pool;
  }

  /** After a rate-limit pause, one request per kind probes the quota until
   * an answer that is not rate limited arrives. */
  #limit(pool: Pool, kind: Kind): number {
    if (pool.probing) return 1;
    return kind === "call" ? this.maxActive : this.maxStreams;
  }

  /** Whether the cap across scopes, if any, leaves room for one more. */
  #room(kind: Kind): boolean {
    const cap = kind === "call" ? this.maxActiveTotal : this.maxStreamsTotal;
    return cap === null || this.#total[kind] < cap;
  }

  #grant(key: string, pool: Pool, kind: Kind, random: () => number): Permit {
    pool.active[kind] += 1;
    this.#total[kind] += 1;
    this.#served[kind] = key;
    const granted = performance.now();
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
        this.#emit("paused", null, { pause_ms: wait + spread });
        this.#pump(key, pool);
      },
      release: () => {
        if (released) return;
        released = true;
        pool.active[kind] -= 1;
        this.#total[kind] -= 1;
        this.#emit("released", kind, { held_ms: performance.now() - granted });
        this.#pump(key, pool);
        this.#forget(key, pool);
      },
    };
  }

  /** Admit waiting requests in order while capacity allows. */
  #pump(key: string, pool: Pool): void {
    if (this.maxActiveTotal !== null || this.maxStreamsTotal !== null) {
      this.#pumpAll();
      return;
    }
    if (!this.#ready(key, pool)) return;
    for (const kind of ["call", "stream"] as const) {
      const queue = pool.queue[kind];
      while (queue.length > 0 && pool.active[kind] < this.#limit(pool, kind)) {
        const waiter = queue[0] as Waiter;
        waiter.grant(this.#grant(key, pool, kind, waiter.random));
      }
    }
  }

  /** Whether the pool's rate-limit pause has passed; while it lasts, a
   * timer pumps the pool when it ends. */
  #ready(key: string, pool: Pool): boolean {
    const wait = pool.pausedUntil - performance.now();
    if (wait <= 0) return true;
    if (pool.timer === null && (pool.queue.call.length > 0 || pool.queue.stream.length > 0)) {
      pool.timer = setTimeout(() => {
        pool.timer = null;
        this.#pump(key, pool);
      }, wait);
    }
    return false;
  }

  /** Under a cap across scopes: admit waiting requests while capacity
   * allows, one per scope in turn (scopes in key order, starting after the
   * one served last), each scope's own queue first in, first out. */
  #pumpAll(): void {
    for (const kind of ["call", "stream"] as const) {
      const keys = [...this.#pools]
        .filter(([key, pool]) => pool.queue[kind].length > 0 && this.#ready(key, pool))
        .map(([key]) => key)
        .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
      if (keys.length === 0) continue;
      const last = this.#served[kind];
      const start = last === null ? 0 : Math.max(0, keys.findIndex((key) => key > last));
      const order = [...keys.slice(start), ...keys.slice(0, start)];
      for (let progress = true; progress && this.#room(kind); ) {
        progress = false;
        for (const key of order) {
          if (!this.#room(kind)) break;
          const pool = this.#pools.get(key);
          if (pool === undefined || pool.queue[kind].length === 0 || pool.active[kind] >= this.#limit(pool, kind)) continue;
          const waiter = pool.queue[kind][0] as Waiter;
          waiter.grant(this.#grant(key, pool, kind, waiter.random));
          progress = true;
        }
      }
    }
  }

  #idleNow(): boolean {
    const stats = this.stats();
    return stats.active + stats.streams + stats.queued === 0;
  }

  /** Drop an idle scope once its pause has passed; wake `drained()`. */
  #forget(key: string, pool: Pool): void {
    const busy = pool.active.call + pool.active.stream + pool.queue.call.length + pool.queue.stream.length;
    if (busy > 0) return;
    if (pool.pausedUntil <= performance.now()) {
      if (pool.timer !== null) clearTimeout(pool.timer);
      this.#pools.delete(key);
    }
    if (this.#idle.length > 0 && this.#idleNow()) for (const resolve of this.#idle.splice(0)) resolve();
  }
}
