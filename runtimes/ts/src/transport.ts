// SPDX-License-Identifier: Apache-2.0
/**
 * One HTTP attempt: per-attempt timeout combined with the caller's signal,
 * the body read inside the same deadline, and network failures classified
 * by whether the request can have reached the server.
 */
import type { HttpMethod } from "./types.js";
import { isRecord } from "./util.js";

export interface AttemptRequest {
  url: string;
  method: HttpMethod;
  headers: Record<string, string>;
  body: BodyInit | null;
  redirect: RequestRedirect;
  timeoutMs: number;
  signal: AbortSignal | undefined;
}

export type AttemptOutcome =
  | {
      kind: "response";
      response: Response;
      status: number;
      headers: Record<string, string>;
      /** The body, or null when reading it failed. */
      body: Uint8Array | null;
      /** Why the body could not be read: deadline, caller abort or a broken connection. */
      bodyFailure: "timeout" | "aborted" | "broken" | null;
    }
  /** The request cannot have reached the server (DNS, refused, TLS). */
  | { kind: "not_sent"; detail: string }
  /** Sent or possibly sent, then the connection failed without a response. */
  | { kind: "lost"; detail: string }
  | { kind: "timeout" }
  /** The caller's signal aborted; `sent` is false only when it was already aborted. */
  | { kind: "aborted"; sent: boolean };

/** Error codes (Node, undici, Bun, Deno) that mean nothing was sent. */
const NOT_SENT_CODES = new Set([
  "ECONNREFUSED",
  "ENOTFOUND",
  "EAI_AGAIN",
  "EAI_FAIL",
  "EAI_NONAME",
  "EHOSTUNREACH",
  "ENETUNREACH",
  "ENETDOWN",
  "EHOSTDOWN",
  "EADDRNOTAVAIL",
  "UND_ERR_CONNECT_TIMEOUT",
  "UND_ERR_INVALID_ARG",
  "ERR_INVALID_URL",
  "ConnectionRefused",
  "FailedToOpenSocket",
  "DEPTH_ZERO_SELF_SIGNED_CERT",
  "SELF_SIGNED_CERT_IN_CHAIN",
  "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
  "UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
  "CERT_HAS_EXPIRED",
  "ERR_TLS_CERT_ALTNAME_INVALID",
]);

function errorCodes(error: unknown): string[] {
  const codes: string[] = [];
  const seen = new Set<unknown>();
  const visit = (e: unknown, depth: number): void => {
    if (depth > 8 || !isRecord(e) || seen.has(e)) return;
    seen.add(e);
    const record = e as Record<string, unknown>;
    if (typeof record.code === "string") codes.push(record.code);
    if (typeof record.name === "string" && record.name === "NotFound") codes.push("ENOTFOUND");
    visit(record.cause, depth + 1);
    if (Array.isArray(record.errors)) for (const inner of record.errors) visit(inner, depth + 1);
  };
  visit(error, 0);
  return codes;
}

function detail(error: unknown): string {
  const codes = errorCodes(error);
  if (codes.length > 0) return codes.join(", ");
  return error instanceof Error ? error.name : "network error";
}

function isNotSent(error: unknown): boolean {
  return errorCodes(error).some((code) => NOT_SENT_CODES.has(code) || code.startsWith("ERR_TLS_") || code.startsWith("CERT_"));
}

/** Send once. Never throws. */
export async function attempt(fetchImpl: typeof fetch, req: AttemptRequest): Promise<AttemptOutcome> {
  if (req.signal?.aborted) return { kind: "aborted", sent: false };
  const controller = new AbortController();
  let timedOut = false;
  const timer = setTimeout(
    () => {
      timedOut = true;
      controller.abort();
    },
    Math.max(1, req.timeoutMs),
  );
  const onAbort = (): void => controller.abort();
  req.signal?.addEventListener("abort", onAbort, { once: true });
  try {
    let response: Response;
    try {
      response = await fetchImpl(req.url, {
        method: req.method,
        headers: req.headers,
        body: req.body,
        redirect: req.redirect,
        signal: controller.signal,
      });
    } catch (error) {
      if (timedOut) return { kind: "timeout" };
      if (req.signal?.aborted) return { kind: "aborted", sent: true };
      return isNotSent(error) ? { kind: "not_sent", detail: detail(error) } : { kind: "lost", detail: detail(error) };
    }
    const headers: Record<string, string> = {};
    response.headers.forEach((value, name) => {
      headers[name.toLowerCase()] = value;
    });
    let body: Uint8Array | null = null;
    let bodyFailure: "timeout" | "aborted" | "broken" | null = null;
    try {
      body = new Uint8Array(await response.arrayBuffer());
    } catch {
      bodyFailure = timedOut ? "timeout" : req.signal?.aborted ? "aborted" : "broken";
    }
    return { kind: "response", response, status: response.status, headers, body, bodyFailure };
  } finally {
    clearTimeout(timer);
    req.signal?.removeEventListener("abort", onAbort);
  }
}

/** Retry-After in milliseconds (delta seconds or HTTP date), or null. */
export function parseRetryAfter(value: string | undefined, now: number): number | null {
  if (value === undefined) return null;
  const trimmed = value.trim();
  if (/^\d+$/.test(trimmed)) return Number(trimmed) * 1000;
  const date = Date.parse(trimmed);
  if (Number.isNaN(date)) return null;
  return Math.max(0, Math.round(date - now));
}

/** Next page URL from a `Link` header (`rel="next"`), or null. */
export function nextLink(header: string | undefined): string | null {
  if (!header) return null;
  for (const part of header.split(/,(?=\s*<)/)) {
    const match = /^\s*<([^>]*)>(.*)$/.exec(part);
    if (!match) continue;
    const params = match[2] ?? "";
    if (/;\s*rel\s*=\s*"?([^";]*\s)?next(\s[^";]*)?"?\s*(;|$)/i.test(params)) return match[1] ?? null;
  }
  return null;
}
