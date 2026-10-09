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
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < text.length) {
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
  #buffer = "";
  #started = false;
  #event = "";
  #data = "";
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
    let text = this.#buffer + chunk;
    this.#buffer = "";
    let chunkBytes = utf8Length(chunk);
    if (!this.#started && text.length > 0) {
      this.#started = true;
      if (text.charCodeAt(0) === 0xfeff) {
        text = text.slice(1);
        chunkBytes -= 3;
      }
    }
    const out: SseEvent[] = [];
    let start = 0;
    let completed = false;
    for (let i = 0; i < text.length; i += 1) {
      const c = text.charCodeAt(i);
      if (c !== 10 && c !== 13) continue;
      if (c === 13 && i + 1 === text.length) {
        this.#hold(text, start, completed, chunkBytes);
        return out;
      }
      const line = text.slice(start, i);
      if (c === 13 && text.charCodeAt(i + 1) === 10) i += 1;
      start = i + 1;
      completed = true;
      this.#line(line, out);
      if (this.#exceeded) return out;
    }
    this.#hold(text, start, completed, chunkBytes);
    return out;
  }

  /** Keep the unfinished line `text[start..]`; its bytes are counted from
   * the chunk alone unless a line ended in this push. Too much ends the parser. */
  #hold(text: string, start: number, completed: boolean, chunkBytes: number): void {
    this.#buffer = text.slice(start);
    this.#bufferBytes = completed ? utf8Length(this.#buffer) : this.#bufferBytes + chunkBytes;
    if (this.#eventBytes + this.#bufferBytes > this.#max) {
      this.#exceeded = true;
      this.#buffer = "";
      this.#bufferBytes = 0;
    }
  }

  /** The stream ended: a CR held back ends its line. Whatever is still
   * open is discarded. */
  end(): SseEvent[] {
    const out: SseEvent[] = [];
    if (!this.#exceeded && this.#buffer.endsWith("\r")) this.#line(this.#buffer.slice(0, -1), out);
    this.#buffer = "";
    this.#bufferBytes = 0;
    this.#event = "";
    this.#data = "";
    this.#hasData = false;
    return out;
  }

  #line(line: string, out: SseEvent[]): void {
    if (line === "") {
      if (this.#hasData) {
        out.push({ event: this.#event === "" ? "message" : this.#event, data: this.#data.slice(0, -1), id: this.#id, retry: this.#retry });
      }
      this.#event = "";
      this.#data = "";
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
        this.#data += `${value}\n`;
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
