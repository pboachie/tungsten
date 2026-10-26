// SPDX-License-Identifier: Apache-2.0
/// <reference types="node" />
/**
 * Node filesystem adapters for the Tungsten runtime.
 *
 * {@link FileIdempotencyStore} persists replay keys to a caller-owned file.
 * Pass it as the core client's idempotencyStore option. Import this entry
 * point separately from `@elysra/tungsten-runtime`; it imports Node APIs and
 * should stay out of browser bundles. Applications own the file location,
 * access controls and removal of abandoned leases.
 *
 * @module
 */
import { randomBytes } from "node:crypto";
import { lstat, mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { hostname } from "node:os";
import { basename, dirname, join, resolve } from "node:path";

import type { Download, DownloadResult } from "./download.js";

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
 * LEASE_WAIT_MS, then `reserve` refuses, naming the owner. Only its owner
 * removes a lease: no writer reclaims another's, because a reclaimer cannot
 * remove a lease and check whose it was in one step (a delayed reclaimer
 * could remove a newer live lease). A lease left by a process that died is
 * removed by hand. The holder re-checks its token just before it commits the
 * store file, so a writer whose lease was removed never overwrites a key.
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

/** The owner of `lease`; undefined when it cannot be read or parsed (still
 * being written, or not a lease). The host may be empty: it is only reported. */
async function readOwner(lease: string): Promise<LeaseOwner | undefined> {
  let value: unknown;
  try {
    value = JSON.parse(await readFile(lease, "utf8"));
  } catch {
    return undefined;
  }
  if (typeof value !== "object" || value === null) return undefined;
  const { version, host, pid, token } = value as Record<string, unknown>;
  if (version !== 1 || typeof host !== "string" || !Number.isSafeInteger(pid) || (pid as number) <= 0 || typeof token !== "string" || !token) return undefined;
  return { host, pid: pid as number, token };
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
    const left = deadline - Date.now();
    if (left <= 0) {
      const owner = await readOwner(lease);
      throw new Error(
        owner
          ? `the idempotency store is locked by another writer (${lease}, process ${owner.pid} on ${JSON.stringify(owner.host)}); try again, or remove the lease once that process no longer runs`
          : `the idempotency store is locked by another writer (${lease}); try again`,
      );
    }
    await pause(Math.min(delay, left));
    delay = Math.min(delay * 2, 100);
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
 * cross-process lease `<path>.lock`: it waits up to 2 s for another writer,
 * then refuses; only its owner removes a lease.
 */
export class FileIdempotencyStore implements IdempotencyStore {
  /**
   * Caller-supplied file path. Relative paths use the process working directory.
   */
  readonly path: string;
  #queue: Promise<unknown> = Promise.resolve();

  /**
   * Choose a JSON store file; construction performs no filesystem I/O.
   * Directories and files are created when writing.
   * @param path Application-owned storage location.
   */
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

  /**
   * Read a stored key, or undefined for a missing slot or file.
   * Malformed store contents and other filesystem failures reject.
   */
  async get(scope: string, logicalId: string): Promise<string | undefined> {
    await this.#queue.catch(() => undefined);
    const data = await this.#read();
    const scoped = Object.hasOwn(data.keys, scope) ? data.keys[scope] : undefined;
    return scoped && Object.hasOwn(scoped, logicalId) ? scoped[logicalId] : undefined;
  }

  /**
   * Return an existing key or persist candidate while holding the shared
   * file lease. Competing reservations are serialized; a busy lease is
   * waited for up to two seconds and is never reclaimed automatically.
   * Lease or storage failures reject before a new key is returned.
   */
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
            throw new Error(`the idempotency store lease ${lease} is no longer this writer's; the key was not stored`);
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

  /**
   * Write or replace a slot through this instance's write queue.
   * Use reserve for competing writers: put does not acquire its cross-process lease.
   */
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

/** Options of {@link saveDownload}. */
export interface SaveOptions {
  /** Replace an existing file at the destination. Default false: an
   * existing file (or link) fails the save before any byte is read. */
  overwrite?: boolean;
}

/**
 * Write a download to the file at `path`, chosen by the caller (never by
 * the server: `Download.filename` is only a suggestion). The bytes go to a
 * new temporary file next to it (`<name>.<random>.part`, created
 * exclusively), which replaces `path` only once the whole body arrived; on
 * any failure the temporary file is removed and `path` is left as it was.
 * Without `overwrite`, an existing destination fails with `code`
 * `DESTINATION_EXISTS` before any byte is read, and is checked again
 * before the rename (a file created in between is still kept, except on
 * platforms whose rename replaces it: check before downloading when that
 * matters). The parent directory must exist. The download is closed when
 * this returns. Never throws.
 */
export async function saveDownload(download: Download, path: string, options?: SaveOptions): Promise<DownloadResult> {
  const target = resolve(path);
  const overwrite = options?.overwrite === true;
  const exists = async (): Promise<boolean> => {
    try {
      await lstat(target);
      return true;
    } catch (error) {
      return codeOf(error) !== "ENOENT";
    }
  };
  if (!overwrite && (await exists())) return download.abandon("exists");
  const part = join(dirname(target), `${basename(target)}.${randomBytes(4).toString("hex")}.part`);
  let handle;
  try {
    handle = await open(part, "wx", 0o600);
  } catch (error) {
    return download.abandon("sink", errorName(error));
  }
  let result: DownloadResult;
  try {
    result = await download.writeTo(async (chunk) => {
      await handle.write(chunk);
    });
    if (result.ok) await handle.sync();
  } catch (error) {
    result = download.abandon("sink", errorName(error));
  } finally {
    await handle.close().catch(() => undefined);
  }
  try {
    if (result.ok && !overwrite && (await exists())) result = download.abandon("exists");
    if (result.ok) await rename(part, target);
  } catch (error) {
    result = download.abandon("sink", errorName(error));
  }
  if (!result.ok) await rm(part, { force: true }).catch(() => undefined);
  return result;
}

function errorName(error: unknown): string {
  const code = codeOf(error);
  return typeof code === "string" ? code : error instanceof Error ? error.name : "error";
}
