// SPDX-License-Identifier: Apache-2.0
/// <reference types="node" />
/**
 * `@elysra/tungsten-runtime/node`: adapters that need Node APIs (also available in
 * Bun and Deno's Node compatibility). The core entry point never imports
 * this module.
 */
import { randomBytes } from "node:crypto";
import { mkdir, open, readFile, rename, rm, rmdir, stat } from "node:fs/promises";
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

/** How long `reserve` waits for another writer's lease before refusing. */
const LEASE_WAIT_MS = 2000;
/** A lease older than this is a crashed writer's and is broken. */
const LEASE_STALE_MS = 30_000;

const pause = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Takes the cross-process lease `lease` (a directory: mkdir is atomic). A
 * busy lease is retried with backoff for up to {@link LEASE_WAIT_MS}; one
 * whose mtime is older than {@link LEASE_STALE_MS} is left by a writer that
 * died and is removed. A lease still busy after the wait refuses the call.
 */
async function acquireLease(lease: string): Promise<void> {
  const deadline = Date.now() + LEASE_WAIT_MS;
  for (let delay = 5; ; ) {
    try {
      await mkdir(lease);
      return;
    } catch (error) {
      if ((error as { code?: unknown }).code !== "EEXIST") throw error;
    }
    // Retry at once when the lease is gone or a stale one was removed.
    let retryNow: boolean;
    try {
      const held = Date.now() - (await stat(lease)).mtimeMs;
      retryNow = held > LEASE_STALE_MS && (await rmdir(lease).then(() => true, () => false));
    } catch (error) {
      retryNow = (error as { code?: unknown }).code === "ENOENT";
    }
    const left = deadline - Date.now();
    if (left <= 0) throw new Error(`the idempotency store is locked by another writer (${lease}); try again`);
    if (!retryNow) {
      await pause(Math.min(delay, left));
      delay = Math.min(delay * 2, 100);
    }
  }
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
 * cross-process lease `<path>.lock`: it waits up to 2 s for another
 * process's lease and breaks one older than 30 s (its writer died).
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
      await acquireLease(lease);
      try {
        const existing = await this.get(scope, logicalId);
        if (existing) return existing;
        await this.put(scope, logicalId, candidate);
        return candidate;
      } finally {
        // The outcome is decided; a lease left behind turns stale and is broken.
        await rmdir(lease).catch(() => undefined);
      }
    });
    fileQueues.set(file, result);
    // The last reservation of a file removes its queue entry when it settles.
    const forget = (): void => { if (fileQueues.get(file) === result) fileQueues.delete(file); };
    result.then(forget, forget);
    return result;
  }

  put(scope: string, logicalId: string, key: string): Promise<void> {
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
