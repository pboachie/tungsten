// SPDX-License-Identifier: Apache-2.0
/**
 * Streamed upload sources: a request body read chunk by chunk from the
 * caller instead of held in memory, with an explicit replay rule. A one-shot
 * source is sent at most once: once it was read, the request is never
 * repeated (no retry, no redirect). A recreatable source is opened again for
 * every attempt. A declared length or SHA-256 is checked against the bytes
 * read; the last chunk is held back until both match, so a source that
 * differs never completes its request.
 */

type Chunks = ReadableStream<Uint8Array> | AsyncIterable<Uint8Array>;

/** How an {@link UploadSource} may be sent again. */
export type UploadReplay = "one_shot" | "recreatable";

/** What a source declares about its bytes. */
export interface UploadDeclaration {
  /** The exact number of bytes; sent as `Content-Length`. */
  length?: number;
  /** The SHA-256 of the bytes as 64 hex digits. It is the `content_hash`
   * idempotency key, binds confirmation tokens to the bytes, and is
   * required for operations whose key or confirmation depends on the body. */
  sha256?: string;
}

/** Why a source ended an attempt. */
export type UploadFailure = { kind: "length"; read: number } | { kind: "sha256" } | { kind: "source"; detail: string };

/** The state of one attempt's body. */
export interface UploadAttempt {
  stream: ReadableStream<Uint8Array>;
  /** Bytes handed to the transport so far. */
  sent: () => number;
  /** Why the source ended the attempt, or null. */
  failure: () => UploadFailure | null;
}

/** A request body streamed from the caller. Pass it where the operation
 * takes binary data for its whole body (not inside a multipart field). */
export class UploadSource {
  /** One-shot or recreatable. */
  readonly replay: UploadReplay;
  /** The declared length, or null. */
  readonly length: number | null;
  /** The declared SHA-256 (lower-case hex), or null. */
  readonly sha256: string | null;
  readonly #open: () => Chunks | Promise<Chunks>;
  #opened = false;

  private constructor(replay: UploadReplay, open: () => Chunks | Promise<Chunks>, declared: UploadDeclaration | undefined) {
    const length = declared?.length;
    if (length !== undefined && !(Number.isSafeInteger(length) && length >= 0)) {
      throw new RangeError("UploadSource length must be a non-negative safe integer");
    }
    const sha256 = declared?.sha256;
    if (sha256 !== undefined && !/^[0-9a-fA-F]{64}$/.test(sha256)) {
      throw new RangeError("UploadSource sha256 must be 64 hexadecimal digits");
    }
    this.replay = replay;
    this.#open = open;
    this.length = length ?? null;
    this.sha256 = sha256?.toLowerCase() ?? null;
  }

  /** A source that can be read once: a request whose body was read is never
   * sent again. */
  static oneShot(chunks: Chunks, declared?: UploadDeclaration): UploadSource {
    return new UploadSource("one_shot", () => chunks, declared);
  }

  /** A source that `open` recreates from the start for every attempt (a
   * file opened again, an object re-fetched); every attempt must produce the
   * same bytes. */
  static recreatable(open: () => Chunks | Promise<Chunks>, declared?: UploadDeclaration): UploadSource {
    return new UploadSource("recreatable", open, declared);
  }

  /** Whether an attempt has started reading the source. */
  get opened(): boolean {
    return this.#opened;
  }

  /** Whether another attempt may send this source. */
  get sendable(): boolean {
    return this.replay === "recreatable" || !this.#opened;
  }

  /** @internal The body of one attempt. */
  attempt(): UploadAttempt {
    let iterator: AsyncIterator<Uint8Array> | null = null;
    let sent = 0;
    let read = 0;
    let failure: UploadFailure | null = null;
    let pending: Uint8Array | null = null;
    const hash = this.sha256 === null ? null : new Sha256();
    const verify = this.length !== null || this.sha256 !== null;
    const open = async (): Promise<AsyncIterator<Uint8Array>> => {
      this.#opened = true;
      const chunks = await this.#open();
      if (typeof (chunks as ReadableStream<Uint8Array>).getReader === "function") {
        const reader = (chunks as ReadableStream<Uint8Array>).getReader();
        return {
          next: async () => {
            const r = await reader.read();
            return r.done ? { done: true, value: undefined } : { done: false, value: r.value };
          },
          return: async () => {
            await reader.cancel().catch(() => undefined);
            return { done: true, value: undefined };
          },
        };
      }
      return (chunks as AsyncIterable<Uint8Array>)[Symbol.asyncIterator]();
    };
    const stop = (controller: ReadableStreamDefaultController<Uint8Array>, why: UploadFailure): void => {
      failure = why;
      void iterator?.return?.().catch(() => undefined);
      controller.error(new Error("upload source rejected"));
    };
    const emit = (controller: ReadableStreamDefaultController<Uint8Array>, chunk: Uint8Array): void => {
      sent += chunk.byteLength;
      controller.enqueue(chunk);
    };
    const stream = new ReadableStream<Uint8Array>({
      pull: async (controller) => {
        try {
          iterator ??= await open();
          for (;;) {
            const next = await iterator.next();
            if (next.done === true) {
              if (this.length !== null && read !== this.length) return stop(controller, { kind: "length", read });
              if (hash !== null && hash.hex() !== this.sha256) return stop(controller, { kind: "sha256" });
              if (pending !== null) emit(controller, pending);
              pending = null;
              controller.close();
              return;
            }
            const chunk = next.value;
            if (!(chunk instanceof Uint8Array)) return stop(controller, { kind: "source", detail: "a chunk that is not a Uint8Array" });
            if (chunk.byteLength === 0) continue;
            read += chunk.byteLength;
            hash?.update(chunk);
            if (this.length !== null && read > this.length) return stop(controller, { kind: "length", read });
            if (!verify) return emit(controller, chunk);
            // The last chunk waits until the declared length and digest match.
            const ready = pending;
            pending = chunk;
            if (ready !== null) return emit(controller, ready);
          }
        } catch (error) {
          stop(controller, { kind: "source", detail: error instanceof Error ? error.name : "error" });
        }
      },
      cancel: async () => {
        await iterator?.return?.().catch(() => undefined);
      },
    });
    return { stream, sent: () => sent, failure: () => failure };
  }
}

/** An incremental SHA-256 (FIPS 180-4), for sources too large to hold. */
class Sha256 {
  static readonly #K = new Uint32Array([
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
  ]);
  readonly #h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
  readonly #block = new Uint8Array(64);
  readonly #w = new Uint32Array(64);
  #filled = 0;
  #total = 0;

  update(data: Uint8Array): void {
    this.#total += data.byteLength;
    let i = 0;
    while (i < data.byteLength) {
      const take = Math.min(64 - this.#filled, data.byteLength - i);
      this.#block.set(data.subarray(i, i + take), this.#filled);
      this.#filled += take;
      i += take;
      if (this.#filled === 64) {
        this.#compress();
        this.#filled = 0;
      }
    }
  }

  hex(): string {
    const bits = this.#total * 8;
    const tail = new Uint8Array(((this.#filled < 56 ? 56 : 120) - this.#filled) + 8);
    tail[0] = 0x80;
    const view = new DataView(tail.buffer);
    view.setUint32(tail.length - 8, Math.floor(bits / 0x100000000));
    view.setUint32(tail.length - 4, bits >>> 0);
    const total = this.#total;
    this.update(tail);
    this.#total = total;
    return [...this.#h].map((word) => word.toString(16).padStart(8, "0")).join("");
  }

  #compress(): void {
    const w = this.#w;
    const b = this.#block;
    for (let t = 0; t < 16; t += 1) w[t] = ((b[t * 4]! << 24) | (b[t * 4 + 1]! << 16) | (b[t * 4 + 2]! << 8) | b[t * 4 + 3]!) >>> 0;
    for (let t = 16; t < 64; t += 1) {
      const x = w[t - 15]!;
      const y = w[t - 2]!;
      const s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
      const s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
      w[t] = (w[t - 16]! + s0 + w[t - 7]! + s1) >>> 0;
    }
    const h = this.#h;
    let [a, bb, c, d, e, f, g, hh] = [h[0]!, h[1]!, h[2]!, h[3]!, h[4]!, h[5]!, h[6]!, h[7]!];
    for (let t = 0; t < 64; t += 1) {
      const S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const ch = (e & f) ^ (~e & g);
      const t1 = (hh + S1 + ch + Sha256.#K[t]! + w[t]!) >>> 0;
      const S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const maj = (a & bb) ^ (a & c) ^ (bb & c);
      const t2 = (S0 + maj) >>> 0;
      hh = g;
      g = f;
      f = e;
      e = (d + t1) >>> 0;
      d = c;
      c = bb;
      bb = a;
      a = (t1 + t2) >>> 0;
    }
    h[0] = (h[0]! + a) >>> 0;
    h[1] = (h[1]! + bb) >>> 0;
    h[2] = (h[2]! + c) >>> 0;
    h[3] = (h[3]! + d) >>> 0;
    h[4] = (h[4]! + e) >>> 0;
    h[5] = (h[5]! + f) >>> 0;
    h[6] = (h[6]! + g) >>> 0;
    h[7] = (h[7]! + hh) >>> 0;
  }
}
