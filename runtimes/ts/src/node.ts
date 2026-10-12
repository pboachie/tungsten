// SPDX-License-Identifier: Apache-2.0
/// <reference types="node" />
/**
 * `@elysra/tungsten-runtime/node`: adapters that need Node APIs (also available in
 * Bun and Deno's Node compatibility). The core entry point never imports
 * this module.
 */
import { randomBytes } from "node:crypto";
import { mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { dirname } from "node:path";

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

/**
 * An {@link IdempotencyStore} persisted to one JSON file, so `auto` keys
 * survive a restart: a call retried after a crash reuses its key. Writes
 * are serialized within the process and atomic (temporary file and
 * rename); the file is created with mode 0600 because keys are never
 * meant to be shown. A file that exists but cannot be parsed makes `get`
 * and `put` reject, and the runtime then refuses the call rather than
 * issue a second key for the same intent.
 */
let fileQueue: Promise<unknown> = Promise.resolve();

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
    const result = fileQueue.catch(() => undefined).then(async () => {
      await mkdir(dirname(this.path), { recursive: true });
      const lease = `${this.path}.lock`;
      // A busy/stale lease refuses the call. Never steal a crashed writer's lease.
      await mkdir(lease);
      try {
        const existing = await this.get(scope, logicalId);
        if (existing) return existing;
        await this.put(scope, logicalId, candidate);
        return candidate;
      } finally { await rm(lease, { recursive: true }); }
    });
    fileQueue = result;
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
