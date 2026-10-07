// SPDX-License-Identifier: Apache-2.0
/**
 * Confirmation tokens (planning/06 "Preview and confirmation"):
 * `tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, op id, args digest, expiry))>`.
 * A token is valid only for the operation and the exact arguments it was
 * issued for, until its expiry.
 */
import { base64url, canonicalJson, hmacSha256, sha256Hex, timingSafeEqual } from "./util.js";

/** Lifetime of a confirmation token. */
export const CONFIRMATION_TTL_MS = 5 * 60 * 1000;

const PREFIX = "tgc1";

/** SHA-256 of the canonical JSON of the arguments. */
export async function argsDigest(args: unknown): Promise<string> {
  return sha256Hex(canonicalJson(args));
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
  if (!match) return "malformed";
  const expiry = Number(match[1]);
  const expected = await signature(key, operation, await argsDigest(args), expiry);
  if (!timingSafeEqual(expected, match[2] as string)) return "mismatch";
  return expiry > now ? "valid" : "expired";
}
