// SPDX-License-Identifier: Apache-2.0
/**
 * Small pure helpers shared by the runtime modules: value inspection,
 * field paths, canonical JSON, hashing, encodings and redaction. Only
 * platform APIs available in every supported runtime are used (Web Crypto,
 * TextEncoder, btoa).
 */

/** Placeholder written wherever a secret or sensitive value would appear. */
export const REDACTED = "<redacted>";

/** Longest string kept in `Diagnostic.received_value`. */
export const MAX_RECEIVED_CHARS = 200;

const encoder = new TextEncoder();

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isBlob(value: unknown): value is Blob {
  return typeof Blob !== "undefined" && value instanceof Blob;
}

export function isBinary(value: unknown): value is Uint8Array | ArrayBuffer | Blob {
  return value instanceof Uint8Array || value instanceof ArrayBuffer || isBlob(value);
}

/** Binary data with the file name and media type of its multipart part. A
 * `File` carries its own name and type; use this for bytes or a plain
 * `Blob`. Without a file name a part is named after its field. */
export interface NamedBinary {
  data: Uint8Array | ArrayBuffer | Blob;
  filename?: string;
  contentType?: string;
}

/** What a binary request field takes: bytes, a `Blob` or `File`, or
 * {@link NamedBinary}. */
export type BinaryInput = Uint8Array | ArrayBuffer | Blob | NamedBinary;

export function isNamedBinary(value: unknown): value is NamedBinary {
  if (!isRecord(value) || isBinary(value) || value instanceof Date) return false;
  const keys = Object.keys(value);
  return (
    keys.includes("data") &&
    keys.every((k) => k === "data" || k === "filename" || k === "contentType") &&
    isBinary(value.data) &&
    (value.filename === undefined || typeof value.filename === "string") &&
    (value.contentType === undefined || typeof value.contentType === "string")
  );
}

/** Whether `value` is a `BinaryInput`: bytes, a Blob or File, or `{ data, filename?, contentType? }`. */
export function isBinaryInput(value: unknown): value is BinaryInput {
  return isBinary(value) || isNamedBinary(value);
}

export function binarySize(value: Uint8Array | ArrayBuffer | Blob): number {
  return isBlob(value) ? value.size : value.byteLength;
}

export function hasOwn(value: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(value, key);
}

/** Split a field path (`a.b`, `items[0].id`, `items.0.id`) into segments. */
export function splitPath(path: string): string[] {
  if (path === "" || path === ".") return [];
  return path
    .replace(/\[(\d+)\]/g, ".$1")
    .split(".")
    .filter((s) => s !== "");
}

/** Read a field path from a JSON-like value; `undefined` when absent. */
export function getPath(value: unknown, path: string | string[]): unknown {
  const segments = typeof path === "string" ? splitPath(path) : path;
  let current: unknown = value;
  for (const segment of segments) {
    if (Array.isArray(current)) {
      if (!/^\d+$/.test(segment)) return undefined;
      current = current[Number(segment)];
    } else if (isRecord(current)) {
      if (!hasOwn(current, segment)) return undefined;
      current = current[segment];
    } else {
      return undefined;
    }
  }
  return current;
}

/** Structural equality over JSON-like values. */
export function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a === "number" && typeof b === "number") return a === b;
  if (Array.isArray(a)) {
    return Array.isArray(b) && a.length === b.length && a.every((x, i) => deepEqual(x, b[i]));
  }
  if (isRecord(a) && isRecord(b) && !Array.isArray(b)) {
    const ka = Object.keys(a).filter((k) => a[k] !== undefined);
    const kb = Object.keys(b).filter((k) => b[k] !== undefined);
    return ka.length === kb.length && ka.every((k) => hasOwn(b, k) && deepEqual(a[k], b[k]));
  }
  return false;
}

/** Every field of `pattern` appears in `value` with an equal (recursively
 * contained) value. Arrays in `pattern` match when each element is
 * contained by some element of the corresponding array. */
export function containsSubset(value: unknown, pattern: unknown): boolean {
  if (isRecord(pattern)) {
    if (!isRecord(value)) return false;
    return Object.keys(pattern).every((k) => hasOwn(value, k) && containsSubset(value[k], pattern[k]));
  }
  if (Array.isArray(pattern)) {
    if (!Array.isArray(value)) return false;
    return pattern.every((p) => value.some((v) => containsSubset(v, p)));
  }
  return deepEqual(value, pattern);
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

export function base64url(bytes: Uint8Array): string {
  return bytesToBase64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function utf8(text: string): Uint8Array {
  return encoder.encode(text);
}

function toHex(buffer: ArrayBuffer): string {
  return Array.from(new Uint8Array(buffer), (b) => b.toString(16).padStart(2, "0")).join("");
}

function bufferSource(data: string | Uint8Array): Uint8Array<ArrayBuffer> {
  const bytes = typeof data === "string" ? utf8(data) : data;
  const copy = new Uint8Array(new ArrayBuffer(bytes.byteLength));
  copy.set(bytes);
  return copy;
}

/** Lowercase hex SHA-256 of UTF-8 text or bytes. */
export async function sha256Hex(data: string | Uint8Array): Promise<string> {
  return toHex(await crypto.subtle.digest("SHA-256", bufferSource(data)));
}

/** HMAC-SHA-256 of `message` under `key`. */
export async function hmacSha256(key: Uint8Array, message: string): Promise<Uint8Array> {
  const cryptoKey = await crypto.subtle.importKey("raw", bufferSource(key), { name: "HMAC", hash: "SHA-256" }, false, [
    "sign",
  ]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", cryptoKey, bufferSource(message)));
}

/** Constant-time comparison of two strings of known public length. */
export function timingSafeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

/**
 * Canonical JSON: object keys sorted by UTF-16 code units, `undefined` and
 * functions dropped from objects (`null` in arrays), non-finite numbers as
 * `null`, dates as ISO strings, bytes as `{"$bytes": base64}`, blobs as
 * `{"$blob": {size, type}}`. Throws `TypeError` on cycles.
 */
export function canonicalJson(value: unknown): string {
  const stack = new Set<object>();
  const encode = (v: unknown): string | undefined => {
    switch (typeof v) {
      case "undefined":
      case "function":
      case "symbol":
        return undefined;
      case "string":
        return JSON.stringify(v);
      case "number":
        return Number.isFinite(v) ? JSON.stringify(v) : "null";
      case "boolean":
        return v ? "true" : "false";
      case "bigint":
        return JSON.stringify(v.toString());
    }
    if (v === null) return "null";
    if (v instanceof Date) return Number.isNaN(v.getTime()) ? "null" : JSON.stringify(v.toISOString());
    if (v instanceof Uint8Array) return `{"$bytes":${JSON.stringify(bytesToBase64(v))}}`;
    if (v instanceof ArrayBuffer) return `{"$bytes":${JSON.stringify(bytesToBase64(new Uint8Array(v)))}}`;
    if (isBlob(v)) return `{"$blob":{"size":${v.size},"type":${JSON.stringify(v.type)}}}`;
    const obj = v as object;
    if (stack.has(obj)) throw new TypeError("the value contains a cycle");
    stack.add(obj);
    let out: string;
    if (Array.isArray(obj)) {
      out = `[${obj.map((item) => encode(item) ?? "null").join(",")}]`;
    } else {
      const record = obj as Record<string, unknown>;
      const parts: string[] = [];
      for (const key of Object.keys(record).sort()) {
        const encoded = encode(record[key]);
        if (encoded !== undefined) parts.push(`${JSON.stringify(key)}:${encoded}`);
      }
      out = `{${parts.join(",")}}`;
    }
    stack.delete(obj);
    return out;
  };
  return encode(value) ?? "null";
}

const SENSITIVE_NAME =
  /(secret|password|passwd|passphrase|token|api[-_]?key|private[-_]?key|credential|authorization|cookie|session)/i;

/** Names that very likely hold a secret, redacted even without metadata. */
export function looksSensitive(name: string): boolean {
  return SENSITIVE_NAME.test(name);
}

/** Deep copy of a JSON-like value with sensitive-looking keys redacted. */
export function redactSensitiveKeys(value: unknown, depth = 0): unknown {
  if (depth > 32) return null;
  if (Array.isArray(value)) return value.map((v) => redactSensitiveKeys(v, depth + 1));
  if (isRecord(value) && !isBinary(value) && !(value instanceof Date)) {
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(value)) {
      out[key] = looksSensitive(key) ? REDACTED : redactSensitiveKeys(value[key], depth + 1);
    }
    return out;
  }
  return value;
}

/** Deep copy with every listed field path redacted; arrays on the way fan
 * out, so `items.secret` redacts the field in each item. */
export function redactPaths(value: unknown, paths: readonly string[]): unknown {
  if (paths.length === 0) return value;
  const apply = (v: unknown, segments: string[]): unknown => {
    if (segments.length === 0) return REDACTED;
    if (Array.isArray(v)) return v.map((item) => apply(item, segments));
    if (!isRecord(v)) return v;
    const [head, ...rest] = segments as [string, ...string[]];
    if (!hasOwn(v, head)) return v;
    return { ...v, [head]: apply(v[head], rest) };
  };
  let out = value;
  for (const path of paths) {
    const segments = splitPath(path);
    if (segments.length > 0) out = apply(out, segments);
  }
  return out;
}

function cut(text: string): string {
  return text.length > MAX_RECEIVED_CHARS ? `${text.slice(0, MAX_RECEIVED_CHARS - 1)}…` : text;
}

/** A value made safe for `Diagnostic.received_value`: redacted when
 * sensitive, binary summarised, strings and large values cut to 200
 * characters, sensitive-looking nested keys redacted. */
export function envelopeValue(value: unknown, sensitive: boolean): unknown {
  if (sensitive) return REDACTED;
  if (value === undefined) return null;
  if (typeof value === "string") return cut(value);
  if (typeof value === "number") return Number.isFinite(value) ? value : String(value);
  if (typeof value === "boolean" || value === null) return value;
  if (typeof value === "bigint") return value.toString();
  if (isBinary(value)) return `<${binarySize(value)} bytes>`;
  if (typeof value !== "object") return `<${typeof value}>`;
  let json: string;
  try {
    json = JSON.stringify(redactSensitiveKeys(value)) ?? "null";
  } catch {
    return "<unserializable value>";
  }
  return json.length > MAX_RECEIVED_CHARS ? cut(json) : (JSON.parse(json) as unknown);
}

/** Sleep for `ms`, resolving early (to `false`) when `signal` aborts. */
export function sleep(ms: number, signal?: AbortSignal): Promise<boolean> {
  return new Promise((resolve) => {
    if (signal?.aborted) {
      resolve(false);
      return;
    }
    const onAbort = (): void => {
      clearTimeout(timer);
      resolve(false);
    };
    const timer = setTimeout(
      () => {
        signal?.removeEventListener("abort", onAbort);
        resolve(true);
      },
      Math.max(0, ms),
    );
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

/** A short, secret-free description of an unknown thrown value. */
export function describeError(error: unknown): string {
  if (error instanceof Error) {
    const name = error.name && error.name !== "Error" ? `${error.name}: ` : "";
    return cut(`${name}${error.message}`);
  }
  return cut(typeof error === "string" ? error : "unknown error");
}
