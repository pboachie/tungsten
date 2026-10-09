// SPDX-License-Identifier: Apache-2.0
/**
 * Parameter serialization (OpenAPI 3 `style`/`explode` for path, query,
 * header and cookie parameters) and request body encoding.
 */
import type { BodyDescriptor, OperationDescriptor, ParamDescriptor } from "./types.js";
import { binarySize, canonicalJson, isBinary, isBlob, isNamedBinary, isRecord, REDACTED, utf8 } from "./util.js";

/** Problem found while serializing an argument; becomes VALIDATION_FAILED. */
export class SerializationError extends Error {
  constructor(
    readonly parameter: string,
    readonly expected: string,
    readonly value: unknown,
  ) {
    super(`${parameter}: expected ${expected}`);
  }
}

function scalar(value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean" || typeof value === "bigint") return String(value);
  if (value instanceof Date) return value.toISOString();
  return JSON.stringify(value) ?? "";
}

function enc(text: string): string {
  return encodeURIComponent(text);
}

function entries(value: Record<string, unknown>): Array<[string, unknown]> {
  return Object.entries(value).filter(([, v]) => v !== undefined && v !== null);
}

function present(value: unknown): boolean {
  return value !== undefined && value !== null;
}

/** One path parameter value, already percent-encoded (`simple`, `label`, `matrix`). */
export function serializePathParam(p: ParamDescriptor, value: unknown): string {
  const name = enc(p.wire);
  const style = p.style === "label" || p.style === "matrix" ? p.style : "simple";
  if (Array.isArray(value)) {
    const items = value.filter(present).map((v) => enc(scalar(v)));
    if (style === "label") return `.${items.join(p.explode ? "." : ",")}`;
    if (style === "matrix") return p.explode ? items.map((i) => `;${name}=${i}`).join("") : `;${name}=${items.join(",")}`;
    return items.join(",");
  }
  if (isRecord(value) && !(value instanceof Date)) {
    const pairs = entries(value).map(([k, v]) => [enc(k), enc(scalar(v))] as const);
    const exploded = pairs.map(([k, v]) => `${k}=${v}`);
    const flat = pairs.flat();
    if (style === "label") return p.explode ? `.${exploded.join(".")}` : `.${flat.join(",")}`;
    if (style === "matrix") return p.explode ? exploded.map((e) => `;${e}`).join("") : `;${name}=${flat.join(",")}`;
    return p.explode ? exploded.join(",") : flat.join(",");
  }
  const v = enc(scalar(value));
  if (style === "label") return `.${v}`;
  if (style === "matrix") return `;${name}=${v}`;
  return v;
}

function deepObjectPairs(prefix: string, value: unknown, out: string[], depth: number): void {
  if (depth > 16 || !present(value)) return;
  if (Array.isArray(value)) {
    for (const item of value) deepObjectPairs(prefix, item, out, depth + 1);
  } else if (isRecord(value) && !(value instanceof Date)) {
    for (const [k, v] of entries(value)) deepObjectPairs(`${prefix}[${enc(k)}]`, v, out, depth + 1);
  } else {
    out.push(`${prefix}=${enc(scalar(value))}`);
  }
}

/** Query string parts (`name=value`, encoded) for one query parameter. */
export function serializeQueryParam(p: ParamDescriptor, value: unknown): string[] {
  if (!present(value)) return [];
  const name = enc(p.wire);
  const delimiter = p.style === "space_delimited" ? "%20" : p.style === "pipe_delimited" ? "|" : ",";
  if (p.style === "deep_object" && isRecord(value) && !(value instanceof Date)) {
    const out: string[] = [];
    deepObjectPairs(name, value, out, 0);
    return out;
  }
  if (Array.isArray(value)) {
    const items = value.filter(present).map((v) => enc(scalar(v)));
    if (items.length === 0) return [];
    if (p.explode || p.style === "deep_object") return items.map((i) => `${name}=${i}`);
    return [`${name}=${items.join(delimiter)}`];
  }
  if (isRecord(value) && !(value instanceof Date)) {
    const pairs = entries(value).map(([k, v]) => [enc(k), enc(scalar(v))] as const);
    if (p.explode) return pairs.map(([k, v]) => `${k}=${v}`);
    return [`${name}=${pairs.flat().join(delimiter)}`];
  }
  return [`${name}=${enc(scalar(value))}`];
}

/** Header value for one header parameter (`simple` style, not encoded). */
export function serializeHeaderParam(p: ParamDescriptor, value: unknown): string {
  if (Array.isArray(value)) return value.filter(present).map(scalar).join(",");
  if (isRecord(value) && !(value instanceof Date)) {
    const pairs = entries(value).map(([k, v]) => [k, scalar(v)] as const);
    return p.explode ? pairs.map(([k, v]) => `${k}=${v}`).join(",") : pairs.flat().join(",");
  }
  return scalar(value);
}

/** `name=value` for one cookie parameter (`form` style, values encoded). */
export function serializeCookieParam(p: ParamDescriptor, value: unknown): string {
  if (Array.isArray(value)) return `${p.wire}=${value.filter(present).map((v) => enc(scalar(v))).join(",")}`;
  if (isRecord(value) && !(value instanceof Date)) {
    return `${p.wire}=${entries(value)
      .flatMap(([k, v]) => [enc(k), enc(scalar(v))])
      .join(",")}`;
  }
  return `${p.wire}=${enc(scalar(value))}`;
}

/** An encoded request body. */
export interface EncodedBody {
  /** What fetch sends; null for no body. */
  body: BodyInit | null;
  /** Content-Type to set, or null (multipart: fetch sets the boundary). */
  contentType: string | null;
  /** JSON-friendly rendering for previews (binary summarised), redacted. */
  display: unknown;
  /** The same rendering before redaction (never shown). */
  raw: unknown;
  /** Bytes or text whose SHA-256 is the `content_hash` key. */
  hashMaterial: string | Uint8Array | null;
}

const NO_BODY: EncodedBody = { body: null, contentType: null, display: null, raw: null, hashMaterial: null };

/** The body value before encoding: merged fields picked from the args, or
 * the whole `args[arg]`; wrapped in the rpc envelope when the operation
 * has one. `undefined` means no body. */
export function bodyValue(op: OperationDescriptor, args: Record<string, unknown>, paramNames: ReadonlySet<string>): unknown {
  let value: unknown;
  const body = op.body;
  if (body) {
    if (body.shape.kind === "merged") {
      const picked: Record<string, unknown> = {};
      for (const field of body.shape.fields) {
        if (args[field] !== undefined) picked[field] = args[field];
      }
      value = Object.keys(picked).length > 0 || body.required || op.rpc ? picked : undefined;
    } else {
      value = args[body.shape.arg];
    }
  }
  if (op.rpc) {
    let params = value;
    if (!body) {
      const rest: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(args)) {
        if (!paramNames.has(k) && v !== undefined) rest[k] = v;
      }
      params = rest;
    }
    const envelope: Record<string, unknown> = { ...(op.rpc.constants ?? {}) };
    envelope[op.rpc.field] = op.rpc.value;
    envelope[op.rpc.paramsField] = params ?? {};
    return envelope;
  }
  return value;
}

function formPairs(value: unknown, parameter: string): Array<[string, string]> {
  if (!isRecord(value)) throw new SerializationError(parameter, "an object of form fields", value);
  const out: Array<[string, string]> = [];
  for (const [k, v] of entries(value)) {
    if (Array.isArray(v)) {
      for (const item of v) if (present(item)) out.push([k, scalar(item)]);
    } else {
      out.push([k, scalar(v)]);
    }
  }
  return out;
}

function toBlob(value: Uint8Array | ArrayBuffer | Blob, type: string): Blob {
  if (isBlob(value)) return type !== "" && value.type !== type ? value.slice(0, value.size, type) : value;
  const bytes = value instanceof ArrayBuffer ? new Uint8Array(value) : value;
  const copy = new Uint8Array(new ArrayBuffer(bytes.byteLength));
  copy.set(bytes);
  return new Blob([copy], type ? { type } : {});
}

function binaryDisplay(value: Uint8Array | ArrayBuffer | Blob, mediaType: string): string {
  return `<${binarySize(value)} bytes${mediaType ? ` of ${mediaType}` : ""}>`;
}

/** Encode `value` per the body descriptor. Throws `SerializationError` for
 * values the encoding cannot carry. */
export async function encodeBody(
  descriptor: BodyDescriptor | null,
  value: unknown,
  redactDisplay: (display: unknown) => unknown,
): Promise<EncodedBody> {
  if (value === undefined) return NO_BODY;
  const encoding = descriptor?.encoding ?? "json";
  const mediaType = descriptor?.mediaType ?? "application/json";
  const parameter = "body";
  switch (encoding) {
    case "json": {
      let text: string | undefined;
      try {
        text = JSON.stringify(value);
      } catch {
        throw new SerializationError(parameter, "a JSON-serializable value", value);
      }
      if (text === undefined) throw new SerializationError(parameter, "a JSON-serializable value", value);
      const raw = JSON.parse(text) as unknown;
      return {
        body: text,
        contentType: mediaType,
        display: redactDisplay(raw),
        raw,
        hashMaterial: canonicalJson(JSON.parse(text) as unknown),
      };
    }
    case "form": {
      const params = new URLSearchParams(formPairs(value, parameter));
      const text = params.toString();
      const raw = Object.fromEntries(params.entries());
      return {
        body: text,
        contentType: mediaType,
        display: redactDisplay(raw),
        raw,
        hashMaterial: text,
      };
    }
    case "multipart": {
      if (!isRecord(value)) throw new SerializationError(parameter, "an object of multipart fields", value);
      const form = new FormData();
      const display: Record<string, unknown> = {};
      for (const [k, v] of entries(value)) {
        const items = Array.isArray(v) ? v.filter(present) : [v];
        const shown: unknown[] = [];
        for (const item of items) {
          if (isBinary(item) || isNamedBinary(item)) {
            const { data, filename, contentType } = isNamedBinary(item) ? item : { data: item, filename: undefined, contentType: undefined };
            const blob = toBlob(data, contentType ?? "");
            const name = filename ?? (typeof File !== "undefined" && data instanceof File ? data.name : k);
            form.append(k, blob, name);
            shown.push(binaryDisplay(data, blob.type));
          } else if (isRecord(item) && !(item instanceof Date)) {
            form.append(k, new Blob([JSON.stringify(item)], { type: "application/json" }));
            shown.push(item);
          } else {
            form.append(k, scalar(item));
            shown.push(scalar(item));
          }
        }
        display[k] = Array.isArray(v) ? shown : shown[0];
      }
      return { body: form, contentType: null, display: redactDisplay(display), raw: display, hashMaterial: canonicalJson(value) };
    }
    case "bytes": {
      let bytes: Uint8Array | Blob;
      if (value instanceof Uint8Array) bytes = value;
      else if (value instanceof ArrayBuffer) bytes = new Uint8Array(value);
      else if (isBlob(value)) bytes = value;
      else if (typeof value === "string") bytes = utf8(value);
      else throw new SerializationError(parameter, "binary data (Uint8Array, ArrayBuffer or Blob)", value);
      const material = isBlob(bytes) ? new Uint8Array(await bytes.arrayBuffer()) : bytes;
      return {
        body: toBlob(material, mediaType),
        contentType: mediaType,
        display: binaryDisplay(material, mediaType),
        raw: binaryDisplay(material, mediaType),
        hashMaterial: material,
      };
    }
    case "text": {
      if (typeof value !== "string" && typeof value !== "number" && typeof value !== "boolean") {
        throw new SerializationError(parameter, "a string", value);
      }
      const text = String(value);
      return { body: text, contentType: mediaType, display: text, raw: text, hashMaterial: text };
    }
  }
}

/** Header names and values that `fetch` accepts, checked before sending. */
const TOKEN = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
const HEADER_VALUE = /^[\t\x20-\x7e\x80-\xff]*$/;

export function validHeaderName(name: string): boolean {
  return TOKEN.test(name);
}

export function validHeaderValue(value: string): boolean {
  return HEADER_VALUE.test(value);
}

/** Case-insensitive header collection that remembers which values are
 * secrets, so previews and middleware see them redacted. */
export class HeaderBag {
  readonly #entries = new Map<string, { name: string; value: string; secret: boolean }>();

  set(name: string, value: string, secret = false): void {
    this.#entries.set(name.toLowerCase(), { name, value, secret });
  }

  get(name: string): string | undefined {
    return this.#entries.get(name.toLowerCase())?.value;
  }

  has(name: string): boolean {
    return this.#entries.has(name.toLowerCase());
  }

  isSecret(name: string): boolean {
    return this.#entries.get(name.toLowerCase())?.secret ?? false;
  }

  /** Plain record; secret values replaced by `<redacted>` when `redact`. */
  toRecord(redact: boolean): Record<string, string> {
    const out: Record<string, string> = {};
    for (const { name, value, secret } of this.#entries.values()) out[name] = redact && secret ? REDACTED : value;
    return out;
  }

  /** Secret values, for scrubbing diagnostics. */
  secrets(): string[] {
    return [...this.#entries.values()].filter((e) => e.secret).map((e) => e.value);
  }
}
