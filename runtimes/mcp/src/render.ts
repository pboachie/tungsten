// SPDX-License-Identifier: Apache-2.0
/**
 * Result rendering for MCP tool calls (planning/05 "MCP server",
 * planning/07 "MCP tool surface"): diagnostic envelopes for problems found
 * by the server itself, canonical JSON, redaction of sensitive response
 * fields and truncation of large results.
 */
import type { Category, Diagnostic, Retryable } from "@tungsten/runtime";

/** Replacement for a sensitive response field on a repeated identical call. */
export const REDACTED_REPEAT = "<redacted after first read>";

/** The `shown_once` line of the text rendering. */
export const SHOWN_ONCE_LINE = "Store this secret now; it will not be shown again.";

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Strings are cut to 200 characters in `received_value` (planning/06). */
function receivedValue(value: unknown): unknown {
  if (typeof value === "string") return value.length > 200 ? `${value.slice(0, 200)}…` : value;
  if (value === undefined) return null;
  try {
    const json = JSON.stringify(value);
    if (json === undefined) return null;
    return json.length > 200 ? `${json.slice(0, 200)}…` : value;
  } catch {
    return null;
  }
}

export interface EnvelopeFields {
  failed_parameter?: string | null;
  received_value?: unknown;
  expected?: string | null;
  remediation: string;
  retryable?: Retryable;
  next_action?: string | null;
}

/** A caller-supplied name cut for envelopes and messages. */
export function clip(name: string): string {
  return name.length > 100 ? `${name.slice(0, 100)}…` : name;
}

/** A diagnostic envelope with the documented field order. */
export function envelope(operation: string, category: Category, fields: EnvelopeFields): Diagnostic {
  return {
    status: "error",
    category,
    operation: clip(operation),
    http_status: null,
    code: null,
    failed_parameter: fields.failed_parameter ?? null,
    received_value: receivedValue(fields.received_value),
    expected: fields.expected ?? null,
    remediation: fields.remediation,
    retryable: fields.retryable ?? "never",
    retry_after_ms: null,
    next_action: fields.next_action ?? null,
    request_id: null,
    trace: { attempts: 0 },
  };
}

/** JSON with object keys sorted at every depth; `undefined` members dropped. */
export function canonicalJson(value: unknown): string {
  const seen = new Set<object>();
  const walk = (v: unknown): unknown => {
    if (Array.isArray(v)) {
      if (seen.has(v)) throw new TypeError("cyclic value");
      seen.add(v);
      const out = v.map((x) => (x === undefined ? null : walk(x)));
      seen.delete(v);
      return out;
    }
    if (isRecord(v)) {
      if (seen.has(v)) throw new TypeError("cyclic value");
      seen.add(v);
      const out: Record<string, unknown> = {};
      for (const key of Object.keys(v).sort()) if (v[key] !== undefined) out[key] = walk(v[key]);
      seen.delete(v);
      return out;
    }
    return v;
  };
  return JSON.stringify(walk(value)) ?? "null";
}

/** Compact JSON for text renderings; never throws. */
export function compactJson(value: unknown): string {
  try {
    return JSON.stringify(value) ?? "null";
  } catch {
    return '"<unserializable value>"';
  }
}

/**
 * A copy of `value` with every field at the dotted `paths` replaced by
 * `replacement` (arrays on the way are traversed element by element), and
 * the paths that held a value.
 */
export function redactPaths(value: unknown, paths: readonly string[], replacement: unknown): { value: unknown; found: string[] } {
  const found: string[] = [];
  let out = value;
  for (const path of paths) {
    const segments = path.split(".").filter((s) => s !== "");
    if (segments.length === 0) continue;
    let hit = false;
    const replace = (v: unknown, i: number): unknown => {
      if (Array.isArray(v)) return v.map((x) => replace(x, i));
      if (!isRecord(v)) return v;
      const key = segments[i] as string;
      if (!Object.prototype.hasOwnProperty.call(v, key) || v[key] === undefined || v[key] === null) return v;
      hit = true;
      return { ...v, [key]: i === segments.length - 1 ? replacement : replace(v[key], i + 1) };
    };
    out = replace(out, 0);
    if (hit) found.push(path);
  }
  return { value: out, found };
}

/** Whether any of the dotted `paths` holds a value in `value`. */
export function holdsAny(value: unknown, paths: readonly string[]): boolean {
  return redactPaths(value, paths, null).found.length > 0;
}

function maxArrayLength(v: unknown): number {
  if (Array.isArray(v)) return v.reduce<number>((m, x) => Math.max(m, maxArrayLength(x)), v.length);
  if (isRecord(v)) return Object.values(v).reduce<number>((m, x) => Math.max(m, maxArrayLength(x)), 0);
  return 0;
}

function maxStringLength(v: unknown): number {
  if (typeof v === "string") return v.length;
  if (Array.isArray(v)) return v.reduce<number>((m, x) => Math.max(m, maxStringLength(x)), 0);
  if (isRecord(v)) return Object.values(v).reduce<number>((m, x) => Math.max(m, maxStringLength(x)), 0);
  return 0;
}

function limitArrays(v: unknown, items: number): unknown {
  if (Array.isArray(v)) return v.slice(0, items).map((x) => limitArrays(x, items));
  if (isRecord(v)) return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, limitArrays(x, items)]));
  return v;
}

function limitStrings(v: unknown, chars: number): unknown {
  if (typeof v === "string") return v.length > chars ? `${v.slice(0, chars)}…` : v;
  if (Array.isArray(v)) return v.map((x) => limitStrings(x, chars));
  if (isRecord(v)) return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, limitStrings(x, chars)]));
  return v;
}

/** Largest n in [lo, hi] with fits(n), or null; fits is monotone. */
function largest(lo: number, hi: number, fits: (n: number) => boolean): number | null {
  if (hi < lo || !fits(lo)) return null;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (fits(mid)) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

/**
 * `value` cut to fit `cap` characters of JSON: arrays (at every depth) to
 * their first items, then strings to a common length. Returns the value and
 * a note for the text rendering, or `note: null` when nothing was cut.
 */
export function truncate(value: unknown, cap: number): { value: unknown; note: string | null } {
  const size = (v: unknown): number => compactJson(v).length;
  const full = size(value);
  if (full <= cap) return { value, note: null };
  const cuts: string[] = [];
  let current = value;
  const longest = maxArrayLength(current);
  if (longest > 1) {
    const items = largest(1, longest - 1, (n) => size(limitArrays(value, n)) <= cap) ?? 1;
    current = limitArrays(value, items);
    cuts.push(`arrays are cut to their first ${items} item${items === 1 ? "" : "s"}`);
  }
  if (size(current) > cap) {
    const base = current;
    const chars = largest(0, Math.max(0, maxStringLength(base) - 1), (n) => size(limitStrings(base, n)) <= cap) ?? 0;
    current = limitStrings(base, chars);
    cuts.push(`strings to ${chars} characters`);
  }
  const over = size(current) > cap ? " (still over the cap: the result has too many fields)" : "";
  return {
    value: current,
    note: `[Truncated to fit ${cap} characters: the full result has ${full}; ${cuts.join(" and ")}${over}. Narrow the request (filters, a smaller page size) to see the rest.]`,
  };
}

/** Placeholder value for a JSON Schema in an example call skeleton. */
export function skeleton(schema: unknown, depth = 0): unknown {
  if (!isRecord(schema) || depth > 6) return "<value>";
  if ("const" in schema) return schema.const;
  if (Array.isArray(schema.enum) && schema.enum.length > 0) return schema.enum[0];
  if ("default" in schema) return schema.default;
  for (const key of ["anyOf", "oneOf"]) {
    const options = schema[key];
    if (Array.isArray(options)) {
      const first = options.find((o) => !(isRecord(o) && o.type === "null"));
      if (first !== undefined) return skeleton(first, depth + 1);
    }
  }
  const types = Array.isArray(schema.type) ? schema.type.filter((t) => t !== "null") : [schema.type];
  const type = types[0];
  switch (type) {
    case "string":
      return typeof schema.format === "string" ? `<${schema.format}>` : "<string>";
    case "integer":
    case "number":
      return typeof schema.minimum === "number" ? schema.minimum : 0;
    case "boolean":
      return false;
    case "array":
      return [];
    case "object": {
      const out: Record<string, unknown> = {};
      const properties = isRecord(schema.properties) ? schema.properties : {};
      const required = Array.isArray(schema.required) ? schema.required.filter((r): r is string => typeof r === "string") : [];
      for (const name of required) out[name] = skeleton(properties[name], depth + 1);
      return out;
    }
    default:
      return "<value>";
  }
}
