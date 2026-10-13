// SPDX-License-Identifier: Apache-2.0
/// <reference types="node" />
/**
 * `@elysra/tungsten-runtime/node`: adapters that need Node APIs (also available in
 * Bun and Deno's Node compatibility). The core entry point never imports
 * this module.
 */
import { randomBytes } from "node:crypto";
import { link, mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { hostname } from "node:os";
import { dirname, resolve } from "node:path";

import type { IdempotencyStore } from "./types.js";

interface StoreFile {
  version: 1;
  keys: Record<string, Record<string, string>>;
}

function isStoreFile(value: unknown): value is StoreFile {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Record<string, unknown>;
  if (record.version !== 1 || typeof record.keys !== "object" || record.keys === null) return false;
  return Object.values(record.keys).every(
    (scope) => typeof scope === "object" && scope !== null && Object.values(scope).every((k) => typeof k === "string"),
  );
}

function sorted(keys: StoreFile["keys"]): StoreFile["keys"] {
  const out: StoreFile["keys"] = {};
  const define = (target: object, key: string, value: unknown): void => {
    Object.defineProperty(target, key, { value, enumerable: true, writable: true, configurable: true });
  };
  for (const scope of Object.keys(keys).sort()) {
    const inner: Record<string, string> = {};
    const entries = keys[scope] ?? {};
    for (const id of Object.keys(entries).sort()) define(inner, id, entries[id]);
    define(out, scope, inner);
  }
  return out;
}

/*
 * The cross-process lease of a FileIdempotencyStore (`<path>.lock`), the same
 * file in every runtime: created exclusively, it records its owner
 * `{"version": 1, "host", "pid", "token"}`. A busy lease is waited for up to
 * LEASE_WAIT_MS. It is reclaimed only from an owner that is provably dead
 * (same host, another process, and that process no longer runs); any other
 * owner (alive, on another host, or unknown) makes `reserve` refuse after the
 * wait. The holder re-checks its token just before it commits the store file,
 * so a writer whose lease was taken over never overwrites the new owner's key.
 */

/** How long `reserve` waits for another writer's lease. */
const LEASE_WAIT_MS = 2000;

interface LeaseOwner {
  host: string;
  pid: number;
  token: string;
}

const pause = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));
const codeOf = (error: unknown): unknown => (error as { code?: unknown }).code;

/** The owner of `lease`: null when it is gone, undefined when it cannot be
 * read or parsed (still being written, or not a lease). */
async function readOwner(lease: string): Promise<LeaseOwner | null | undefined> {
  let text: string;
  try {
    text = await readFile(lease, "utf8");
  } catch (error) {
    return codeOf(error) === "ENOENT" ? null : undefined;
  }
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (typeof value !== "object" || value === null) return undefined;
  const { version, host, pid, token } = value as Record<string, unknown>;
  if (version !== 1 || typeof host !== "string" || !host || !Number.isSafeInteger(pid) || (pid as number) <= 0 || typeof token !== "string" || !token) return undefined;
  return { host, pid: pid as number, token };
}

/** Whether the owner's process is known to have ended. Windows is never
 * sure, so a lease there is never reclaimed. */
function provablyDead(owner: LeaseOwner): boolean {
  if (owner.host !== hostname() || owner.pid === process.pid || process.platform === "win32") return false;
  try {
    process.kill(owner.pid, 0);
    return false;
  } catch (error) {
    return codeOf(error) === "ESRCH";
  }
}

/** Takes the lease and returns its token. */
async function acquireLease(lease: string): Promise<string> {
  const token = randomBytes(16).toString("hex");
  const mine = `${JSON.stringify({ version: 1, host: hostname(), pid: process.pid, token })}\n`;
  const deadline = Date.now() + LEASE_WAIT_MS;
  for (let delay = 5; ; ) {
    try {
      const handle = await open(lease, "wx", 0o600);
      try {
        await handle.writeFile(mine);
      } catch (error) {
        await handle.close();
        await rm(lease, { force: true });
        throw error;
      }
      await handle.close();
      return token;
    } catch (error) {
      if (codeOf(error) !== "EEXIST") throw error;
    }
    const owner = await readOwner(lease);
    let retryNow = owner === null;
    if (owner && provablyDead(owner)) retryNow = await reclaimLease(lease, owner, token);
    const left = deadline - Date.now();
    if (left <= 0) {
      throw new Error(
        owner
          ? `the idempotency store is locked by another writer (${lease}, process ${owner.pid} on ${owner.host}); try again, or remove the lease if that process no longer runs`
          : `the idempotency store is locked by another writer (${lease}); try again`,
      );
    }
    if (!retryNow) {
      await pause(Math.min(delay, left));
      delay = Math.min(delay * 2, 100);
    }
  }
}

/** Removes a dead owner's lease. It moves the lease aside first: if what it
 * moved is not the dead owner's (a live writer took the lease in between), it
 * puts it back. Returns whether to retry at once. */
async function reclaimLease(lease: string, dead: LeaseOwner, token: string): Promise<boolean> {
  const aside = `${lease}.${token}.stale`;
  try {
    await rename(lease, aside);
  } catch (error) {
    return codeOf(error) === "ENOENT";
  }
  try {
    const moved = await readOwner(aside);
    if (moved && moved.token === dead.token) return true;
    await link(aside, lease).catch(() => undefined);
    return false;
  } finally {
    await rm(aside, { force: true }).catch(() => undefined);
  }
}

async function ownsLease(lease: string, token: string): Promise<boolean> {
  return (await readOwner(lease))?.token === token;
}

/** Reservations of one store file, serialized within the process (by
 * absolute path); the lease serializes them across processes. */
const fileQueues = new Map<string, Promise<unknown>>();

/**
 * An {@link IdempotencyStore} persisted to one JSON file, so `auto` keys
 * survive a restart: a call retried after a crash reuses its key. Writes
 * are serialized within the process and atomic (temporary file and
 * rename); the file is created with mode 0600 because keys are never
 * meant to be shown. A file that exists but cannot be parsed makes `get`
 * and `put` reject, and the runtime then refuses the call rather than
 * issue a second key for the same intent. `reserve` also holds the
 * cross-process lease `<path>.lock`: it waits up to 2 s for another writer
 * and reclaims the lease only from an owner whose process provably ended.
 */
export class FileIdempotencyStore implements IdempotencyStore {
  readonly path: string;
  #queue: Promise<unknown> = Promise.resolve();

  constructor(path: string) {
    this.path = path;
  }

  async #read(): Promise<StoreFile> {
    let text: string;
    try {
      text = await readFile(this.path, "utf8");
    } catch (error) {
      if ((error as { code?: unknown }).code === "ENOENT") return { version: 1, keys: {} };
      throw error;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      throw new Error("the idempotency store file is not valid JSON");
    }
    if (!isStoreFile(parsed)) throw new Error("the idempotency store file has an unknown format");
    return parsed;
  }

  async get(scope: string, logicalId: string): Promise<string | undefined> {
    await this.#queue.catch(() => undefined);
    const data = await this.#read();
    const scoped = Object.hasOwn(data.keys, scope) ? data.keys[scope] : undefined;
    return scoped && Object.hasOwn(scoped, logicalId) ? scoped[logicalId] : undefined;
  }

  async reserve(scope: string, logicalId: string, candidate: string): Promise<string> {
    const file = resolve(this.path);
    const result = (fileQueues.get(file) ?? Promise.resolve()).catch(() => undefined).then(async () => {
      await mkdir(dirname(this.path), { recursive: true });
      const lease = `${this.path}.lock`;
      const token = await acquireLease(lease);
      try {
        const existing = await this.get(scope, logicalId);
        if (existing) return existing;
        await this.#write(scope, logicalId, candidate, async () => {
          if (!(await ownsLease(lease, token))) {
            throw new Error(`the idempotency store lease ${lease} was taken over by another writer; the key was not stored`);
          }
        });
        return candidate;
      } finally {
        // The outcome is decided; a failed release never fails the reservation.
        if (await ownsLease(lease, token)) await rm(lease, { force: true }).catch(() => undefined);
      }
    });
    fileQueues.set(file, result);
    // The last reservation of a file removes its queue entry when it settles.
    const forget = (): void => { if (fileQueues.get(file) === result) fileQueues.delete(file); };
    result.then(forget, forget);
    return result;
  }

  put(scope: string, logicalId: string, key: string): Promise<void> {
    return this.#write(scope, logicalId, key);
  }

  /** Stores one key; `fence`, when set, runs just before the rename that
   * commits the file, and its rejection abandons the write. */
  #write(scope: string, logicalId: string, key: string, fence?: () => Promise<void>): Promise<void> {
    const write = this.#queue
      .catch(() => undefined)
      .then(async () => {
        const data = await this.#read();
        const scoped: Record<string, string> = Object.hasOwn(data.keys, scope) ? { ...data.keys[scope] } : {};
        Object.defineProperty(scoped, logicalId, { value: key, enumerable: true, writable: true, configurable: true });
        Object.defineProperty(data.keys, scope, { value: scoped, enumerable: true, writable: true, configurable: true });
        const text = `${JSON.stringify({ version: 1, keys: sorted(data.keys) }, null, 2)}\n`;
        await mkdir(dirname(this.path), { recursive: true });
        const temporary = `${this.path}.${randomBytes(6).toString("hex")}.tmp`;
        try {
          const handle = await open(temporary, "wx", 0o600);
          try {
            await handle.writeFile(text);
            await handle.sync();
          } finally { await handle.close(); }
          if (fence) await fence();
          await rename(temporary, this.path);
        } catch (error) {
          await rm(temporary, { force: true });
          throw error;
        }
      });
    this.#queue = write;
    return write;
  }
}
