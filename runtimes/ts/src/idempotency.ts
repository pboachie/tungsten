// SPDX-License-Identifier: Apache-2.0
/**
 * Replay-key storage and policy helpers. The default
 * {@link MemoryIdempotencyStore} stores keys in one instance; use a persistent
 * adapter when an automatically reserved key must survive its lifetime.
 */
import type { IdempotencyStore, OperationDescriptor } from "./types.js";

/**
 * The default {@link IdempotencyStore}: keys belong to this store instance
 * and are not persisted. Share an instance to share its reservations. Use
 * FileIdempotencyStore from `@elysra/tungsten-runtime/node`, or another
 * persistent adapter, when a key must survive a restart.
 */
export class MemoryIdempotencyStore implements IdempotencyStore {
  readonly #keys = new Map<string, string>();

  /**
   * Return the existing nonempty key, otherwise store and return candidate
   * without an intervening await. This is atomic within this store instance.
   */
  async reserve(scope: string, logicalId: string, candidate: string): Promise<string> {
    const slot = `${scope}\u0000${logicalId}`;
    const existing = this.#keys.get(slot);
    if (existing) return existing;
    this.#keys.set(slot, candidate);
    return candidate;
  }

  /**
   * Read this instance's slot; undefined means it has not been stored.
   */
  async get(scope: string, logicalId: string): Promise<string | undefined> {
    return this.#keys.get(`${scope}\u0000${logicalId}`);
  }

  /**
   * Replace this instance's slot. No key is persisted outside memory.
   */
  async put(scope: string, logicalId: string, key: string): Promise<void> {
    this.#keys.set(`${scope}\u0000${logicalId}`, key);
  }
}

const UUID_ANY = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function formatKind(format: string | null): "uuid_v4" | "uuid" | "ascii_token_128" | "token" {
  const f = (format ?? "").toLowerCase().replace(/[^a-z0-9]/g, "");
  if (f === "uuidv4" || f === "uuid4") return "uuid_v4";
  if (f === "asciitoken128") return "ascii_token_128";
  return f === "uuid" ? "uuid" : "token";
}

/** Human description of the key format a policy expects. */
export function keyFormatDescription(format: string | null): string {
  switch (formatKind(format)) {
    case "uuid_v4":
      return "UUIDv4 string";
    case "uuid":
      return "UUID string";
    case "ascii_token_128":
      return "1-128 ASCII letters, digits, dots, underscores or hyphens";
    case "token":
      return "a non-empty printable ASCII string of at most 255 characters";
  }
}

/** Checks a caller-owned key against the policy's `format`. Returns the
 * description of the expected format when the key does not match. */
export function checkKeyFormat(key: string, format: string | null): string | null {
  const kind = formatKind(format);
  let valid: boolean;
  switch (kind) {
    case "uuid_v4":
      valid = UUID_V4.test(key);
      break;
    case "uuid":
      valid = UUID_ANY.test(key);
      break;
    case "ascii_token_128":
      valid = key.length >= 1 && key.length <= 128 && !/[^A-Za-z0-9._-]/.test(key);
      break;
    case "token":
      valid = key.length > 0 && key.length <= 255 && /^[\x21-\x7e]+$/.test(key);
      break;
  }
  return valid ? null : keyFormatDescription(format);
}

/** The wire header of the operation's key: the policy's header, else the
 * parameter with role `idempotency_key`, else `Idempotency-Key`. */
export function keyHeader(op: OperationDescriptor): string {
  const fromPolicy = op.agent.idempotency.header;
  if (fromPolicy) return fromPolicy;
  const param = op.params.find((p) => p.role === "idempotency_key");
  return param?.wire ?? "Idempotency-Key";
}

/** Whether the operation sends an idempotency key or an identity body, so
 * resending it cannot apply its effect twice. */
export function hasReplayProtection(op: OperationDescriptor, key: string | null): boolean {
  if (op.agent.idempotency.policy === "content_identity") return true;
  return key !== null;
}

// Legacy adapters serialize all get/put transactions in this module. Stores
// shared with another process must implement reserve themselves.
let legacyQueue: Promise<unknown> = Promise.resolve();
export function reserveKey(store: IdempotencyStore, scope: string, id: string, candidate: string): Promise<string> {
  if (store.reserve) return store.reserve(scope, id, candidate);
  const result = legacyQueue.catch(() => undefined).then(async () => {
    const old = await store.get(scope, id);
    if (old) return old;
    await store.put(scope, id, candidate);
    if (await store.get(scope, id) !== candidate) throw new Error("the key could not be stored");
    return candidate;
  });
  legacyQueue = result;
  return result;
}
