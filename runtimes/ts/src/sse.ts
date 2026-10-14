// SPDX-License-Identifier: Apache-2.0
/**
 * A server-sent events parser (WHATWG HTML 9.2 "Server-sent events"):
 * lines end with CRLF, LF or CR; a line is a comment (`:`), a field
 * (`event`, `data`, `id`, `retry`; the value loses one leading space) or
 * the blank line that dispatches the event. Several `data` lines are
 * joined with LF; an event without `data` lines is not dispatched; an
 * event still open when the stream ends is discarded.
 *
 * An event may hold at most `maxEventBytes` (default {@link DEFAULT_MAX_EVENT_BYTES},
 * 1 MiB): the UTF-8 bytes of its field lines (each counted with a one-byte
 * terminator; a finished comment line is not kept and does not count) plus
 * the line being read, whatever it turns out to be, so a comment longer than
 * the limit with no line end also stops the parser. A parser that sees more stops: {@link SseParser.exceeded}
 * becomes true, the events completed before the oversize one are still
 * returned, and later input is ignored, so nothing is buffered without bound.
 */

/** The default size limit of one event, in UTF-8 bytes. */
export const DEFAULT_MAX_EVENT_BYTES = 1024 * 1024;

/** The number of bytes of `text` in UTF-8. */
export function utf8Length(text: string): number {
  let bytes = 0;
  for (let i = 0; i < text.length; i += 1) {
    const c = text.charCodeAt(i);
    if (c < 0x80) bytes += 1;
    else if (c < 0x800) bytes += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < text.length && text.charCodeAt(i + 1) >= 0xdc00 && text.charCodeAt(i + 1) <= 0xdfff) {
      bytes += 4;
      i += 1;
    } else bytes += 3;
  }
  return bytes;
}

export interface SseEvent {
  /** The `event` field, `message` when the event has none. */
  event: string;
  data: string;
  /** The last event id seen so far (it survives across events), or null. */
  id: string | null;
  /** The last valid `retry` value seen so far, in milliseconds, or null. */
  retry: number | null;
}

export class SseParser {
  #buffer: string[] = [];
  #started = false;
  #pendingCR = false;
  #event = "";
  #data: string[] = [];
  #hasData = false;
  #id: string | null = null;
  #retry: number | null = null;
  #eventBytes = 0;
  #bufferBytes = 0;
  #exceeded = false;
  readonly #max: number;

  constructor(maxEventBytes: number = DEFAULT_MAX_EVENT_BYTES) {
    this.#max = maxEventBytes;
  }

  /** The event being read is larger than the limit: the parser has stopped. */
  get exceeded(): boolean {
    return this.#exceeded;
  }

  /** Feed decoded text; returns the events it completes. A CR at the end
   * of the text is held back until the next text shows whether an LF
   * follows. */
  push(chunk: string): SseEvent[] {
    if (this.#exceeded) return [];
    if (!this.#started && chunk.length > 0) {
      this.#started = true;
      if (chunk.charCodeAt(0) === 0xfeff) chunk = chunk.slice(1);
    }
    const out: SseEvent[] = [];
    if (this.#pendingCR) {
      if (chunk.length === 0) return out;
      this.#pendingCR = false;
      this.#complete(out);
      if (chunk.startsWith("\n")) chunk = chunk.slice(1);
    }
    let start = 0;
    for (let i = 0; i < chunk.length && !this.#exceeded; i += 1) {
      const c = chunk.charCodeAt(i);
      if (c !== 10 && c !== 13) continue;
      this.#append(chunk.slice(start, i));
      if (c === 13 && i + 1 === chunk.length) {
        this.#bufferBytes += 1;
        this.#pendingCR = true;
        this.#checkLimit();
        return out;
      }
      this.#complete(out);
      if (c === 13 && chunk.charCodeAt(i + 1) === 10) i += 1;
      start = i + 1;
    }
    if (!this.#exceeded) {
      this.#append(chunk.slice(start));
      this.#checkLimit();
    }
    return out;
  }

  // Fragments are scanned and counted once, then joined only at a line end.
  #append(text: string): void {
    if (!text) return;
    const previous = this.#buffer.at(-1);
    const last = previous?.charCodeAt(previous.length - 1) ?? 0;
    const first = text.charCodeAt(0);
    this.#bufferBytes += utf8Length(text);
    if (last >= 0xd800 && last <= 0xdbff && first >= 0xdc00 && first <= 0xdfff) this.#bufferBytes -= 2;
    this.#buffer.push(text);
  }

  #checkLimit(): void {
    if (this.#eventBytes + this.#bufferBytes > this.#max) this.#exceeded = true;
    if (this.#exceeded) {
      this.#buffer = [];
      this.#bufferBytes = 0;
      this.#data = [];
      this.#event = "";
      this.#id = null;
      this.#hasData = false;
    }
  }

  #complete(out: SseEvent[]): void {
    const line = this.#buffer.join("");
    this.#buffer = [];
    this.#bufferBytes = 0;
    this.#line(line, out);
    this.#checkLimit();
  }

  /** A held CR ends its line; any open event is discarded. */
  end(): SseEvent[] {
    const out: SseEvent[] = [];
    if (!this.#exceeded && this.#pendingCR) this.#complete(out);
    this.#pendingCR = false;
    this.#buffer = [];
    this.#bufferBytes = 0;
    this.#event = "";
    this.#data = [];
    this.#hasData = false;
    this.#eventBytes = 0;
    return out;
  }

  #line(line: string, out: SseEvent[]): void {
    if (line === "") {
      if (this.#hasData) {
        out.push({ event: this.#event === "" ? "message" : this.#event, data: this.#data.join("\n"), id: this.#id, retry: this.#retry });
      }
      this.#event = "";
      this.#data = [];
      this.#hasData = false;
      this.#eventBytes = 0;
      return;
    }
    if (line.startsWith(":")) return;
    this.#eventBytes += utf8Length(line) + 1;
    if (this.#eventBytes > this.#max) {
      this.#exceeded = true;
      return;
    }
    const colon = line.indexOf(":");
    const field = colon < 0 ? line : line.slice(0, colon);
    let value = colon < 0 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    switch (field) {
      case "event":
        this.#event = value;
        break;
      case "data":
        this.#data.push(value);
        this.#hasData = true;
        break;
      case "id":
        if (!value.includes("\0")) this.#id = value;
        break;
      case "retry":
        if (/^[0-9]+$/.test(value)) {
          const ms = Number(value);
          if (Number.isSafeInteger(ms)) this.#retry = ms;
        }
        break;
      default:
        break;
    }
  }
}
