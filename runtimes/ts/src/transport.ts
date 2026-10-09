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
  /** Hand a `text/event-stream` answer with a 2xx status back unread, as a
   * {@link StreamBody}; any other answer is read as usual. */
  stream?: boolean;
}

/** What one read of an event stream gives. */
export type StreamRead =
  | { kind: "chunk"; bytes: Uint8Array }
  | { kind: "end" }
  /** No bytes arrived within the attempt timeout. */
  | { kind: "timeout" }
  /** The caller's signal aborted. */
  | { kind: "aborted" }
  /** The connection failed or was closed before the stream ended. */
  | { kind: "lost" };

/** The unread body of an event stream: chunks of bytes with an idle
 * timeout (the attempt timeout, restarted by every read) and the caller's
 * signal. */
export class StreamBody {
  readonly #reader: ReadableStreamDefaultReader<Uint8Array>;
  readonly #controller: AbortController;
  readonly #signal: AbortSignal | undefined;
  readonly #onAbort: () => void;
  readonly #idleMs: number;
  #timedOut = false;
  #closed = false;

  constructor(reader: ReadableStreamDefaultReader<Uint8Array>, controller: AbortController, signal: AbortSignal | undefined, onAbort: () => void, idleMs: number) {
    this.#reader = reader;
    this.#controller = controller;
    this.#signal = signal;
    this.#onAbort = onAbort;
    this.#idleMs = Math.max(1, idleMs);
  }

  async read(): Promise<StreamRead> {
    if (this.#closed) return { kind: "end" };
    const timer = setTimeout(() => {
      this.#timedOut = true;
      this.#controller.abort();
    }, this.#idleMs);
    try {
      const chunk = await this.#reader.read();
      if (chunk.done) return { kind: "end" };
      return { kind: "chunk", bytes: chunk.value };
    } catch {
      if (this.#timedOut) return { kind: "timeout" };
      return this.#signal?.aborted === true ? { kind: "aborted" } : { kind: "lost" };
    } finally {
      clearTimeout(timer);
    }
  }

  /** Release the connection. Safe to call more than once. */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#signal?.removeEventListener("abort", this.#onAbort);
    this.#reader.cancel().catch(() => undefined);
    this.#controller.abort();
  }
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
      /** The unread body of an event stream (`AttemptRequest.stream`); `body` is then null. */
      stream?: StreamBody;
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
  let handedOver = false;
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
    if (req.stream === true && response.status >= 200 && response.status <= 299 && response.body !== null && isEventStream(headers["content-type"])) {
      handedOver = true;
      clearTimeout(timer);
      const stream = new StreamBody(response.body.getReader(), controller, req.signal, onAbort, req.timeoutMs);
      return { kind: "response", response, status: response.status, headers, body: null, bodyFailure: null, stream };
    }
    let body: Uint8Array | null = null;
    let bodyFailure: "timeout" | "aborted" | "broken" | null = null;
    try {
      body = new Uint8Array(await response.arrayBuffer());
    } catch {
      bodyFailure = timedOut ? "timeout" : req.signal?.aborted ? "aborted" : "broken";
    }
    return { kind: "response", response, status: response.status, headers, body, bodyFailure };
  } finally {
    if (!handedOver) {
      clearTimeout(timer);
      req.signal?.removeEventListener("abort", onAbort);
    }
  }
}

/** Whether a `Content-Type` value is `text/event-stream` (parameters and case ignored). */
export function isEventStream(contentType: string | undefined): boolean {
  return (contentType ?? "").split(";")[0]?.trim().toLowerCase() === "text/event-stream";
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
