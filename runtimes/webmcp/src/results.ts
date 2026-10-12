// SPDX-License-Identifier: Apache-2.0
import type { Diagnostic, EventStream, Safety } from "@elysra/tungsten-runtime";
import type { WebMcpBinaryResultOptions, WebMcpStreamCollection } from "./types.js";

type Normalize = (value: unknown, maxBytes: number) => unknown;
export function resultFailure(operation: string, code: string, remediation: string, category: Diagnostic["category"] = "UNEXPECTED_RESPONSE"): { ok: false; error: Diagnostic } {
  return { ok: false, error: { status: "error", category, operation, code, http_status: null, failed_parameter: null, received_value: null, expected: null, remediation, retryable: "never", retry_after_ms: null, next_action: null, request_id: null, trace: { attempts: 0 } } };
}

/** Consume the SDK's stream, retaining event metadata and every failure envelope. */
export async function collectStream(stream: EventStream<unknown>, limits: WebMcpStreamCollection, operation: string, safety: Safety, signal: AbortSignal, normalize: Normalize, maxResultBytes: number): Promise<unknown> {
  const events: unknown[] = [];
  let reconnects = 0;
  let bytes = 2;
  const encoder = new TextEncoder();
  let stopCode: string | undefined;
  let release!: () => void;
  const interrupted = new Promise<{ stopped: true }>((resolve) => { release = () => resolve({ stopped: true }); });
  const stop = (code: string): void => { if (stopCode) return; stopCode = code; stream.cancel(); release(); };
  const abort = (): void => stop("CANCELLED");
  const deadline = Date.now() + limits.maxDurationMs;
  const timer = setTimeout(() => stop("STREAM_DURATION_EXCEEDED"), limits.maxDurationMs);
  signal.addEventListener("abort", abort, { once: true });
  if (signal.aborted) abort();
  const fail = (code: string): unknown => ({ ...resultFailure(operation, code, `Stream collection stopped after ${events.length} events. ${safety === "read_only" ? "The read is incomplete." : "The mutation may have taken effect; inspect API state before retrying."}`, safety === "read_only" ? "UNEXPECTED_RESPONSE" : "OUTCOME_UNKNOWN"), partial: events, reconnects });
  const iterator = stream[Symbol.asyncIterator]();
  try {
    for (;;) {
      const next = await Promise.race([iterator.next(), interrupted]);
      if (Date.now() >= deadline) stop("STREAM_DURATION_EXCEEDED");
      if ("stopped" in next || stopCode) return fail(stopCode ?? "CANCELLED");
      if (next.done) return { ok: true, value: events, reconnects };
      const item = next.value;
      if (!item.ok) return { ...item, partial: events, reconnects };
      if (events.length >= limits.maxEvents) { stop("STREAM_EVENT_LIMIT_EXCEEDED"); return fail(stopCode!); }
      let event: unknown;
      try { event = normalize(item, Math.min(limits.maxBytes, maxResultBytes)); } catch (error) { stop(error instanceof Error && error.message === "RESULT_TOO_LARGE" ? "STREAM_BYTE_LIMIT_EXCEEDED" : error instanceof Error ? error.message : "NON_JSON_RESULT"); return fail(stopCode!); }
      const size = encoder.encode(JSON.stringify(event)).byteLength + (events.length ? 1 : 0);
      // Reserve room for the success/failure envelope, never silently truncate.
      if (bytes + size > Math.min(limits.maxBytes, maxResultBytes - 1024)) { stop("STREAM_BYTE_LIMIT_EXCEEDED"); return fail(stopCode!); }
      if (Date.now() >= deadline) { stop("STREAM_DURATION_EXCEEDED"); return fail(stopCode!); }
      events.push(event); bytes += size; reconnects = item.meta.reconnects;
    }
  } finally {
    clearTimeout(timer); signal.removeEventListener("abort", abort); stream.cancel();
    // Cancellation aborts pending transport reads. Do not wait forever on a
    // custom transport which ignores AbortSignal.
    void iterator.return?.(undefined).catch(() => undefined);
  }
}

export async function renderBinary(bytes: Uint8Array, mediaType: string, operation: string, options: WebMcpBinaryResultOptions, signal: AbortSignal, maxResultBytes: number): Promise<unknown> {
  if (bytes.byteLength > options.maxBytes) throw new Error("BINARY_TOO_LARGE");
  if (signal.aborted) throw new Error("CANCELLED");
  if (options.mode === "base64") {
    if (4 * Math.ceil(bytes.byteLength / 3) + 512 > maxResultBytes) throw new Error("RESULT_TOO_LARGE");
    let raw = "";
    for (let i = 0; i < bytes.byteLength; i += 8192) raw += String.fromCharCode(...bytes.subarray(i, i + 8192));
    return { kind: "binary", encoding: "base64", mediaType, byteLength: bytes.byteLength, data: btoa(raw) };
  }
  let abort!: () => void;
  const interrupted = new Promise<never>((_resolve, reject) => { abort = () => reject(new Error("CANCELLED")); });
  signal.addEventListener("abort", abort, { once: true });
  let handle;
  try {
    if (signal.aborted) abort();
    handle = await Promise.race([options.render({ bytes: bytes.slice(), mediaType, operation, signal }), interrupted]);
  } finally { signal.removeEventListener("abort", abort); }
  if (signal.aborted) throw new Error("CANCELLED");
  if (!handle || typeof handle.url !== "string" || !Number.isSafeInteger(handle.expiresAt) || handle.expiresAt <= Date.now()) throw new Error("INVALID_DOWNLOAD_HANDLE");
  const url = new URL(handle.url);
  if (url.protocol !== "https:" || url.username || url.password) throw new Error("INVALID_DOWNLOAD_HANDLE");
  return { kind: "download", mediaType, byteLength: bytes.byteLength, url: url.href, expiresAt: handle.expiresAt };
}

export function binaryMediaType(media: string | null): boolean {
  if (!media) return false;
  const value = media.toLowerCase().split(";")[0]!.trim();
  return !value.startsWith("text/") && !value.endsWith("json") && !value.endsWith("+json") && !value.endsWith("xml") && !value.endsWith("+xml") && value !== "application/x-ndjson" && value !== "application/jsonl" && value !== "application/jsonlines";
}
