// SPDX-License-Identifier: Apache-2.0
/**
 * Durable checkpoints for resumable read-only page exports.
 *
 * A checkpoint is a caller-owned token,
 * `tgp1.<base64url(canonical JSON payload)>.<base64url(HMAC-SHA-256(key, …))>`,
 * recording how far a traversal got after a page the application committed.
 * The payload binds the traversal to the API, operation, contract shape,
 * base URL, tenant, caller scope and every argument but the position, as an
 * HMAC under the same key, so neither arguments nor credentials are stored.
 * The next-page value (a provider cursor or URL) is stored as is: treat the
 * token as sensitive and keep it out of logs.
 */

import { base64url, canonicalJson, hmacSha256, isRecord, timingSafeEqual, utf8 } from "./util.js";

/** Version prefix of checkpoint tokens. */
export const CHECKPOINT_PREFIX = "tgp1.";
/** Longest accepted checkpoint token, in characters. */
export const MAX_CHECKPOINT_CHARS = 16384;

/** Where a checkpointed traversal stands: more pages to read, finished, or
 * stopped by a server fault (a repeated cursor or a cross-origin Link) that
 * resuming would not fix. */
export type CheckpointState = "partial" | "complete" | "stopped";

/** The secret-free part of a checkpoint, from `ClientCore.describeCheckpoint`. */
export interface CheckpointStatus {
  version: 1;
  operation: string;
  style: string;
  state: CheckpointState;
  /** Pages acknowledged across every run of this export. */
  pages: number;
  /** Epoch milliseconds the token was issued. */
  issuedAt: number;
}

export interface CheckpointPayload extends Omit<CheckpointStatus, "issuedAt" | "version"> {
  v: 1;
  binding: string;
  next: unknown;
  issued: number;
}

const decoder = new TextDecoder("utf-8", { fatal: true });

function fromBase64url(text: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
  try {
    const binary = atob(text.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (text.length % 4)) % 4));
    return Uint8Array.from(binary, (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

/** The binding of a traversal's identity under `key`. */
export async function checkpointBinding(key: Uint8Array, identity: Record<string, unknown>): Promise<string> {
  return base64url(await hmacSha256(key, `tungsten-page-binding\n${canonicalJson(identity)}`));
}

/** Seal `payload` into a token. */
export async function sealCheckpoint(key: Uint8Array, payload: CheckpointPayload): Promise<string> {
  const body = base64url(utf8(canonicalJson(payload)));
  return `${CHECKPOINT_PREFIX}${body}.${base64url(await hmacSha256(key, `tungsten-page-checkpoint\n${body}`))}`;
}

/** The payload of a well-formed token, without checking its seal. */
export function readCheckpoint(token: unknown): CheckpointPayload | null {
  if (typeof token !== "string" || token.length > MAX_CHECKPOINT_CHARS || !token.startsWith(CHECKPOINT_PREFIX)) return null;
  const parts = token.slice(CHECKPOINT_PREFIX.length).split(".");
  if (parts.length !== 2) return null;
  const bytes = fromBase64url(parts[0]!);
  if (bytes === null) return null;
  let payload: unknown;
  try {
    payload = JSON.parse(decoder.decode(bytes));
  } catch {
    return null;
  }
  if (!isRecord(payload)) return null;
  const { v, operation, style, binding, state, pages, issued } = payload;
  if (v !== 1 || typeof operation !== "string" || typeof style !== "string" || typeof binding !== "string") return null;
  if (state !== "partial" && state !== "complete" && state !== "stopped") return null;
  if (!Number.isSafeInteger(pages) || (pages as number) < 0 || !Number.isSafeInteger(issued) || (issued as number) < 0) return null;
  if (!("next" in payload)) return null;
  return payload as unknown as CheckpointPayload;
}

/** The payload of `token` when its seal verifies under `key`. */
export async function openCheckpoint(key: Uint8Array, token: string): Promise<CheckpointPayload | null> {
  const payload = readCheckpoint(token);
  if (payload === null) return null;
  const [body, seal] = token.slice(CHECKPOINT_PREFIX.length).split(".") as [string, string];
  const expected = base64url(await hmacSha256(key, `tungsten-page-checkpoint\n${body}`));
  return timingSafeEqual(expected, seal) ? payload : null;
}

/** The secret-free status of a well-formed token (its seal unchecked). */
export function checkpointStatus(token: unknown): CheckpointStatus | null {
  const payload = readCheckpoint(token);
  if (payload === null) return null;
  return { version: 1, operation: payload.operation, style: payload.style, state: payload.state, pages: payload.pages, issuedAt: payload.issued };
}
