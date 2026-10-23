// SPDX-License-Identifier: Apache-2.0
/**
 * Confirmation tokens:
 * `tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, op id, args digest, expiry))>`.
 * A token is valid only for the operation and the exact arguments it was
 * issued for, until its expiry.
 */
import { base64url, canonicalJson, hmacSha256, sha256Hex, timingSafeEqual } from "./util.js";

/** Lifetime of a confirmation token. */
export const CONFIRMATION_TTL_MS = 5 * 60 * 1000;

const PREFIX = "tgc1";

/** Canonical argument digest, with a separate content manifest for Blobs/Files. */
export async function argsDigest(args: unknown): Promise<string> {
  const binaries: unknown[] = [];
  const value = await digestValue(argumentSnapshot(args), [], binaries);
  // JSON-like arguments cannot impersonate a Blob marker: the manifest is
  // outside canonical JSON and binds every binary to its structural path.
  return sha256Hex(canonicalJson(value) + (binaries.length ? `\nblob-content-v1\n${canonicalJson(binaries)}` : ""));
}

async function signature(key: Uint8Array, operation: string, digest: string, expiry: number): Promise<string> {
  return base64url(await hmacSha256(key, `${operation}\n${digest}\n${expiry}`));
}

export async function issueToken(key: Uint8Array, operation: string, args: unknown, now: number): Promise<string> {
  const expiry = Math.floor(now) + CONFIRMATION_TTL_MS;
  return `${PREFIX}.${expiry}.${await signature(key, operation, await argsDigest(args), expiry)}`;
}

export type TokenCheck = "valid" | "expired" | "mismatch" | "malformed";

export async function checkToken(key: Uint8Array, token: string, operation: string, args: unknown, now: number): Promise<TokenCheck> {
  const match = /^tgc1\.(\d{1,16})\.([A-Za-z0-9_-]{43})$/.exec(token);
  if (!match || match[0] !== token) return "malformed";
  const expiry = Number(match[1]);
  if (!Number.isSafeInteger(expiry) || String(expiry) !== match[1]) return "malformed";
  const expected = await signature(key, operation, await argsDigest(args), expiry);
  if (!timingSafeEqual(expected, match[2] as string)) return "mismatch";
  return expiry > now ? "valid" : "expired";
}

/** Capture own data before any asynchronous auth/digest/transport work. */
export function argumentSnapshot(value: unknown, stack = new Set<object>()): unknown {
  if (typeof value === "function" || typeof value === "symbol") return undefined;
  if (value === null || typeof value !== "object") return value;
  if (value instanceof Uint8Array) return new Uint8Array(value);
  if (value instanceof ArrayBuffer) return value.slice(0);
  if (value instanceof Date) return new Date(value.getTime());
  if (typeof Blob !== "undefined" && value instanceof Blob) {
    const blob = Blob.prototype.slice.call(value, 0, value.size, value.type) as Blob;
    return typeof File !== "undefined" && value instanceof File
      ? new File([blob], value.name, { type: value.type, lastModified: value.lastModified }) : blob;
  }
  if (stack.has(value)) throw new TypeError("the value contains a cycle");
  stack.add(value);
  let out: unknown;
  if (Array.isArray(value)) out = Object.freeze(value.map(v => argumentSnapshot(v, stack)));
  else {
    const record: Record<string, unknown> = Object.create(null);
    for (const key of Object.keys(value)) {
      const v: unknown = (value as Record<string, unknown>)[key];
      const captured = argumentSnapshot(v, stack);
      if (captured !== undefined) record[key] = captured;
    }
    out = Object.freeze(record);
  }
  stack.delete(value);
  return out;
}

/** Content and multipart filename/type participate; random boundaries do not. */
async function digestValue(value: unknown, path: string[], binaries: unknown[]): Promise<unknown> {
  if (typeof Blob !== "undefined" && value instanceof Blob) {
    const content = await sha256Hex(new Uint8Array(await value.arrayBuffer()));
    binaries.push({ path, content, type: value.type,
      ...(typeof File !== "undefined" && value instanceof File ? { name: value.name } : {}) });
    return null;
  }
  if (Array.isArray(value)) {
    const out: unknown[] = [];
    for (const [index, item] of value.entries()) out.push(await digestValue(item, [...path, String(index)], binaries));
    return out;
  }
  if (value && typeof value === "object" && !(value instanceof Date) && !(value instanceof Uint8Array) && !(value instanceof ArrayBuffer)) {
    const out: Record<string, unknown> = Object.create(null);
    for (const key of Object.keys(value).sort()) out[key] = await digestValue((value as Record<string, unknown>)[key], [...path, key], binaries);
    return out;
  }
  return value;
}
