// SPDX-License-Identifier: Apache-2.0
/**
 * Idempotency (planning/04 "Idempotency policies", planning/06
 * "Idempotency store"): the default in-memory store, key formats and the
 * header an operation's key travels in.
 */
import type { IdempotencyStore, OperationDescriptor } from "./types.js";

/**
 * The default {@link IdempotencyStore}: keys live as long as the process.
 * Use a persistent store (`FileIdempotencyStore` from
 * `@tungsten/runtime/node`, or your own) when a key must survive a crash.
 */
export class MemoryIdempotencyStore implements IdempotencyStore {
  readonly #keys = new Map<string, string>();

  async get(scope: string, logicalId: string): Promise<string | undefined> {
    return this.#keys.get(`${scope}\u0000${logicalId}`);
  }

  async put(scope: string, logicalId: string, key: string): Promise<void> {
    this.#keys.set(`${scope}\u0000${logicalId}`, key);
  }
}

const UUID_ANY = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function formatKind(format: string | null): "uuid_v4" | "uuid" | "token" {
  const f = (format ?? "").toLowerCase().replace(/[^a-z0-9]/g, "");
  if (f === "uuidv4" || f === "uuid4") return "uuid_v4";
  return f === "uuid" ? "uuid" : "token";
}

/** Human description of the key format a policy expects. */
export function keyFormatDescription(format: string | null): string {
  switch (formatKind(format)) {
    case "uuid_v4":
      return "UUIDv4 string";
    case "uuid":
      return "UUID string";
    case "token":
      return "a non-empty printable ASCII string of at most 255 characters";
  }
}

/** Checks a caller-owned key against the policy's `format`. Returns the
 * description of the expected format when the key does not match. */
export function checkKeyFormat(key: string, format: string | null): string | null {
  const kind = formatKind(format);
  const valid =
    kind === "uuid_v4" ? UUID_V4.test(key) : kind === "uuid" ? UUID_ANY.test(key) : key.length > 0 && key.length <= 255 && /^[\x21-\x7e]+$/.test(key);
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
