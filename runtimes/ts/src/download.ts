// SPDX-License-Identifier: Apache-2.0
/**
 * Streamed downloads: the body of a 2xx answer handed to the caller chunk by
 * chunk instead of being held in memory, with a total-byte limit, progress
 * reports, the idle timeout and the call's deadline, and one release of the
 * connection (and its scheduler capacity) however the download ends.
 */
import type { StreamBody } from "./transport.js";
import type { Diagnostic, ResponseMeta } from "./types.js";

/** How far a download has come. */
export interface TransferProgress {
  /** Body bytes received so far. */
  bytes: number;
  /** The declared `Content-Length`, or null when the server sent none. */
  total: number | null;
}

/** Why a download ended without its whole body. */
export type DownloadFailure = "idle" | "deadline" | "aborted" | "lost" | "truncated" | "oversized" | "sink";

/** One read of a download: a chunk, `null` at the end of the body, or the
 * failure that ended it. */
export type DownloadRead = { ok: true; value: Uint8Array | null } | { ok: false; error: Diagnostic };

/** The answer of {@link Download.writeTo}: the bytes delivered, or the
 * failure that ended the download. */
export type DownloadResult = { ok: true; value: { bytes: number }; meta: ResponseMeta } | { ok: false; error: Diagnostic };

/** A streamed response body. Read it with `read()`, iterate it, or hand it
 * to a sink with `writeTo()`; `close()` abandons it. The connection is
 * released once the body ended, failed or was closed. After the first
 * failure every read returns that failure. */
export class Download {
  /** The answer's status, headers, request id and attempts. */
  readonly meta: ResponseMeta;
  /** The declared `Content-Length`, or null. */
  readonly contentLength: number | null;
  /** The `Content-Type`, or null. */
  readonly contentType: string | null;
  /** The file name of `Content-Disposition`, reduced to a plain base name
   * (no directory part, no control characters), or null. It names no
   * destination: callers choose where the bytes go. */
  readonly filename: string | null;
  /** Null for an answer without a body (204, 205, 304, `HEAD`). */
  readonly #body: StreamBody | null;
  readonly #maxBytes: number | undefined;
  readonly #onProgress: ((progress: TransferProgress) => void) | undefined;
  readonly #failure: (why: DownloadFailure, bytes: number, detail?: string) => Diagnostic;
  #bytes = 0;
  #final: DownloadRead | null = null;

  /** @internal Built by `ClientCore.download`. */
  constructor(
    body: StreamBody | null,
    meta: ResponseMeta,
    maxBytes: number | undefined,
    onProgress: ((progress: TransferProgress) => void) | undefined,
    failure: (why: DownloadFailure, bytes: number, detail?: string) => Diagnostic,
  ) {
    this.#body = body;
    this.meta = meta;
    this.contentLength = declaredLength(meta.headers["content-length"]);
    this.contentType = meta.headers["content-type"] ?? null;
    this.filename = dispositionFilename(meta.headers["content-disposition"]);
    this.#maxBytes = maxBytes;
    this.#onProgress = onProgress;
    this.#failure = failure;
  }

  /** Body bytes received so far. */
  get bytes(): number {
    return this.#bytes;
  }

  /** The next chunk, `null` at the end, or the failure that ended the
   * download. Never throws. */
  async read(): Promise<DownloadRead> {
    if (this.#final !== null) return this.#final;
    const read = this.#body === null ? { kind: "end" as const } : await this.#body.read();
    switch (read.kind) {
      case "chunk": {
        this.#bytes += read.bytes.byteLength;
        if (this.#maxBytes !== undefined && this.#bytes > this.#maxBytes) return this.#end("oversized");
        this.#progress();
        return { ok: true, value: read.bytes };
      }
      case "end":
        if (this.contentLength !== null && this.#bytes < this.contentLength) return this.#end("truncated");
        this.#final = { ok: true, value: null };
        this.#body?.close();
        return this.#final;
      case "timeout":
        return this.#end("idle");
      case "deadline":
        return this.#end("deadline");
      case "aborted":
        return this.#end("aborted");
      case "lost":
        return this.#end("lost");
    }
  }

  /** Every chunk in order; a failure is the last item. Leaving the loop
   * early closes the download. */
  async *[Symbol.asyncIterator](): AsyncGenerator<DownloadRead> {
    try {
      for (;;) {
        const item = await this.read();
        if (item.ok && item.value === null) return;
        yield item;
        if (!item.ok) return;
      }
    } finally {
      this.close();
    }
  }

  /** Hand every chunk to `sink`, waiting for it before the next read (a
   * slow sink slows the transfer instead of growing a buffer). A sink that
   * throws or rejects ends the download (`code` `SINK_FAILED`). The
   * download is closed when this returns. Never throws. */
  async writeTo(sink: (chunk: Uint8Array) => unknown): Promise<DownloadResult> {
    try {
      for (;;) {
        const item = await this.read();
        if (!item.ok) return item;
        if (item.value === null) return { ok: true, value: { bytes: this.#bytes }, meta: this.meta };
        try {
          await sink(item.value);
        } catch (error) {
          return this.#end("sink", error instanceof Error ? error.name : "error") as { ok: false; error: Diagnostic };
        }
      }
    } finally {
      this.close();
    }
  }

  /** Abandon the download and release the connection. Safe to call more
   * than once. */
  close(): void {
    this.#final ??= this.#failed("aborted");
    this.#body?.close();
  }

  #progress(): void {
    try {
      this.#onProgress?.({ bytes: this.#bytes, total: this.contentLength });
    } catch {
      // Observers never change the transfer.
    }
  }

  #failed(why: DownloadFailure, detail?: string): DownloadRead {
    return { ok: false, error: this.#failure(why, this.#bytes, detail) };
  }

  #end(why: DownloadFailure, detail?: string): DownloadRead {
    this.#final = this.#failed(why, detail);
    this.#body?.close();
    return this.#final;
  }
}

/** A `Content-Length` of digits only, else null. */
export function declaredLength(value: string | undefined): number | null {
  if (value === undefined || !/^\d+$/.test(value.trim())) return null;
  const length = Number(value.trim());
  return Number.isSafeInteger(length) ? length : null;
}

/** The parameters of a header value such as `Content-Disposition`, after
 * its first `;`; names lower-cased, quoted strings unescaped. */
function headerParams(value: string): Map<string, string> {
  const out = new Map<string, string>();
  let i = value.indexOf(";");
  if (i < 0) return out;
  i += 1;
  while (i < value.length) {
    const eq = value.indexOf("=", i);
    const semi = value.indexOf(";", i);
    if (eq < 0 || (semi >= 0 && semi < eq)) {
      if (semi < 0) break;
      i = semi + 1;
      continue;
    }
    const name = value.slice(i, eq).trim().toLowerCase();
    let j = eq + 1;
    while (j < value.length && (value[j] === " " || value[j] === "\t")) j += 1;
    let param = "";
    if (value[j] === '"') {
      j += 1;
      while (j < value.length && value[j] !== '"') {
        if (value[j] === "\\" && j + 1 < value.length) j += 1;
        param += value[j];
        j += 1;
      }
      const next = value.indexOf(";", j);
      i = next < 0 ? value.length : next + 1;
    } else {
      const next = value.indexOf(";", j);
      param = value.slice(j, next < 0 ? value.length : next).trim();
      i = next < 0 ? value.length : next + 1;
    }
    if (name !== "" && !out.has(name)) out.set(name, param);
  }
  return out;
}

/** An RFC 8187 `UTF-8''percent-encoded` value, decoded; null otherwise. */
function extendedValue(value: string): string | null {
  const quote = value.indexOf("'");
  const second = quote < 0 ? -1 : value.indexOf("'", quote + 1);
  if (second < 0 || value.slice(0, quote).toLowerCase() !== "utf-8") return null;
  const encoded = value.slice(second + 1);
  const bytes: number[] = [];
  for (let i = 0; i < encoded.length; i += 1) {
    const c = encoded.charCodeAt(i);
    if (encoded[i] === "%") {
      const hex = encoded.slice(i + 1, i + 3);
      if (!/^[0-9A-Fa-f]{2}$/.test(hex)) return null;
      bytes.push(parseInt(hex, 16));
      i += 2;
    } else if (c > 0x7e || c < 0x21) {
      return null;
    } else {
      bytes.push(c);
    }
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes));
  } catch {
    return null;
  }
}

/** The file name a `Content-Disposition` value suggests (`filename*`
 * before `filename`), reduced to a base name that cannot leave a
 * directory: the part after the last `/` or `\`, control characters
 * removed, spaces trimmed, at most 255 characters; null when nothing usable is
 * left (empty, `.` or `..`). */
export function dispositionFilename(value: string | undefined): string | null {
  if (value === undefined) return null;
  const params = headerParams(value);
  const extended = params.get("filename*");
  const raw = (extended === undefined ? null : extendedValue(extended)) ?? params.get("filename");
  if (raw === undefined || raw === null) return null;
  const base = raw.split(/[/\\]/).pop() ?? "";
  // eslint-disable-next-line no-control-regex
  const spaces = /^ +| +$/g;
  const cleaned = [...base.replace(/[\u0000-\u001f\u007f]/g, "").replace(spaces, "")].slice(0, 255).join("").replace(spaces, "");
  return cleaned === "" || cleaned === "." || cleaned === ".." ? null : cleaned;
}
