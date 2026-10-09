// SPDX-License-Identifier: Apache-2.0
/**
 * A server-sent events parser (WHATWG HTML 9.2 "Server-sent events"):
 * lines end with CRLF, LF or CR; a line is a comment (`:`), a field
 * (`event`, `data`, `id`, `retry`; the value loses one leading space) or
 * the blank line that dispatches the event. Several `data` lines are
 * joined with LF; an event without `data` lines is not dispatched; an
 * event still open when the stream ends is discarded.
 */

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

  /** Feed decoded text; returns the events it completes. A CR at the end
   * of the text is held back until the next text shows whether an LF
   * follows. */
  push(chunk: string): SseEvent[] {
    let text = this.#buffer + chunk;
    this.#buffer = "";
    if (!this.#started && text.length > 0) {
      this.#started = true;
      if (text.charCodeAt(0) === 0xfeff) text = text.slice(1);
    }
    const out: SseEvent[] = [];
    let start = 0;
    for (let i = 0; i < text.length; i += 1) {
      const c = text.charCodeAt(i);
      if (c !== 10 && c !== 13) continue;
      if (c === 13 && i + 1 === text.length) {
        this.#buffer = text.slice(start);
        return out;
      }
      const line = text.slice(start, i);
      if (c === 13 && text.charCodeAt(i + 1) === 10) i += 1;
      start = i + 1;
      this.#line(line, out);
    }
    this.#buffer = text.slice(start);
    return out;
  }

  /** The stream ended: a CR held back ends its line. Whatever is still
   * open is discarded. */
  end(): SseEvent[] {
    const out: SseEvent[] = [];
    if (this.#buffer.endsWith("\r")) this.#line(this.#buffer.slice(0, -1), out);
    this.#buffer = "";
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
      return;
    }
    if (line.startsWith(":")) return;
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
