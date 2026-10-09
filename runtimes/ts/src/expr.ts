// SPDX-License-Identifier: Apache-2.0
/**
 * Predicates (verify `expect`/`terminal`, poll `until`) and the reference
 * expressions used by verification argument mappings and macros.
 *
 * Predicate: field path → `{in: [...]}` | `{equals: x}` | `{contains: {...}}`;
 * any other value is compared for equality. Every entry must hold.
 *
 * Expression: JSON in which a string starting with `$` is a reference
 * resolved against a scope (`$input.a.b`, `$<name>.a`, `$response.x`,
 * `$args.y`), an object `{expr: "<ref> in [..]" | "<ref> == x" | "<ref> != x"}`
 * evaluates to a boolean, and anything else is a literal.
 *
 * A dry evaluation (macro previews) knows the input but not the results of
 * the steps before: a reference to such a result evaluates to a placeholder
 * string, `<from step NAME: path>` (`<from step NAME>` for the whole
 * result), and so does an `expr` over one.
 */
import { containsSubset, deepEqual, getPath, hasOwn, isRecord, splitPath } from "./util.js";

/** True when every entry of `predicate` holds on `value`. Malformed
 * predicates never hold. */
export function evaluatePredicate(predicate: unknown, value: unknown): boolean {
  if (predicate === null || predicate === undefined) return true;
  if (!isRecord(predicate)) return false;
  return Object.keys(predicate).every((path) => holds(predicate[path], getPath(value, path)));
}

function holds(test: unknown, actual: unknown): boolean {
  if (isRecord(test)) {
    const keys = Object.keys(test);
    if (keys.length === 1 && keys[0] === "in") {
      return Array.isArray(test.in) && test.in.some((candidate) => deepEqual(candidate, actual));
    }
    if (keys.length === 1 && keys[0] === "equals") return deepEqual(test.equals, actual);
    if (keys.length === 1 && keys[0] === "contains") {
      const pattern = test.contains;
      if (typeof pattern === "string") {
        return (typeof actual === "string" && actual.includes(pattern)) || (Array.isArray(actual) && actual.includes(pattern));
      }
      if (Array.isArray(actual) && !Array.isArray(pattern)) return actual.some((item) => containsSubset(item, pattern));
      return containsSubset(actual, pattern);
    }
  }
  return deepEqual(test, actual);
}

/** Bindings an expression can reference, by name without `$`. */
export type Scope = Readonly<Record<string, unknown>>;

/** Resolve one `$name.path` reference; `undefined` when the name is unbound. */
export function resolveRef(ref: string, scope: Scope): unknown {
  const segments = splitPath(ref.slice(1));
  const [name, ...rest] = segments;
  if (name === undefined || !hasOwn(scope, name)) return undefined;
  return getPath(scope[name], rest);
}

/** Evaluate an expression against `scope`. Unknown references are
 * `undefined` (dropped from objects); malformed `expr` strings are `null`. */
export function evaluateExpr(expr: unknown, scope: Scope, depth = 0): unknown {
  if (depth > 64) return null;
  if (typeof expr === "string") return expr.startsWith("$") ? resolveRef(expr, scope) : expr;
  if (Array.isArray(expr)) return expr.map((item) => evaluateExpr(item, scope, depth + 1));
  if (isRecord(expr)) {
    const keys = Object.keys(expr);
    if (keys.length === 1 && keys[0] === "expr" && typeof expr.expr === "string") return evaluateBoolean(expr.expr, scope);
    const out: Record<string, unknown> = {};
    for (const key of keys) {
      const value = evaluateExpr(expr[key], scope, depth + 1);
      if (value !== undefined) out[key] = value;
    }
    return out;
  }
  return expr;
}

/** A JSON literal, also accepting single-quoted strings (`'a'`). */
/** The placeholder a dry evaluation shows for a result not produced yet. */
export function placeholder(name: string, path: string): string {
  return path === "" ? `<from step ${name}>` : `<from step ${name}: ${path}>`;
}

const PLACEHOLDER = /^<from step [^<>]+>$/;

/** The placeholders in `value`, at any depth, in order of appearance. */
export function placeholdersIn(value: unknown, out: string[] = [], depth = 0): string[] {
  if (depth > 64) return out;
  if (typeof value === "string" && PLACEHOLDER.test(value)) out.push(value);
  else if (Array.isArray(value)) for (const item of value) placeholdersIn(item, out, depth + 1);
  else if (isRecord(value)) for (const item of Object.values(value)) placeholdersIn(item, out, depth + 1);
  return out;
}

/** Whether `value` is a placeholder, or holds one at any depth. */
export function containsPlaceholder(value: unknown): boolean {
  return placeholdersIn(value).length > 0;
}

/** The pending binding a `$name.path` reference names, with the path as
 * written after the name; null when the name is not pending. */
function pendingRef(ref: string, pending: ReadonlySet<string>): { name: string; path: string } | null {
  const name = splitPath(ref.slice(1))[0];
  if (name === undefined || !pending.has(name) || !ref.startsWith(`$${name}`)) return null;
  return { name, path: ref.slice(name.length + 1).replace(/^\./, "") };
}

/** {@link evaluateExpr} for a dry run: references to the `pending` names
 * (results of steps that have not run) become placeholders. */
export function evaluateDry(expr: unknown, scope: Scope, pending: ReadonlySet<string>, depth = 0): unknown {
  if (depth > 64) return null;
  if (typeof expr === "string") {
    if (!expr.startsWith("$")) return expr;
    const ref = pendingRef(expr, pending);
    return ref ? placeholder(ref.name, ref.path) : resolveRef(expr, scope);
  }
  if (Array.isArray(expr)) return expr.map((item) => evaluateDry(item, scope, pending, depth + 1));
  if (isRecord(expr)) {
    const keys = Object.keys(expr);
    if (keys.length === 1 && keys[0] === "expr" && typeof expr.expr === "string") {
      const source = expr.expr.trim();
      const ref = pendingRef(/^\$[^\s=!]+/.exec(source)?.[0] ?? "", pending);
      return ref ? placeholder(ref.name, source.slice(ref.name.length + 1).replace(/^\./, "")) : evaluateBoolean(source, scope);
    }
    const out: Record<string, unknown> = {};
    for (const key of keys) {
      const value = evaluateDry(expr[key], scope, pending, depth + 1);
      if (value !== undefined) out[key] = value;
    }
    return out;
  }
  return expr;
}

/** A predicate in words, for remediation and preview text:
 * `state in ["delivered", "failed"]`, `endpoints contains {...}`, `a = 1`,
 * joined with "and"; "" for an empty predicate. */
export function describePredicate(predicate: unknown): string {
  if (!isRecord(predicate)) return "";
  const json = (value: unknown): string => {
    try {
      return JSON.stringify(value) ?? String(value);
    } catch {
      return String(value);
    }
  };
  const parts = Object.keys(predicate).map((path) => {
    const test = predicate[path];
    if (isRecord(test) && Object.keys(test).length === 1) {
      if (Array.isArray(test.in)) return `${path} in [${test.in.map(json).join(", ")}]`;
      if ("equals" in test) return `${path} = ${json(test.equals)}`;
      if ("contains" in test) return `${path} contains ${json(test.contains)}`;
    }
    return `${path} = ${json(test)}`;
  });
  return parts.join(" and ");
}

function parseLiteral(text: string): { ok: true; value: unknown } | { ok: false } {
  const trimmed = text.trim();
  try {
    return { ok: true, value: JSON.parse(trimmed) as unknown };
  } catch {
    const normalized = trimmed.replace(/'((?:[^'\\]|\\.)*)'/g, (_m, inner: string) => JSON.stringify(inner.replace(/\\'/g, "'")));
    try {
      return { ok: true, value: JSON.parse(normalized) as unknown };
    } catch {
      return { ok: false };
    }
  }
}

function evaluateBoolean(source: string, scope: Scope): boolean | null {
  const match = /^\s*(\$[^\s=!]+)\s+(in|==|!=)\s+([\s\S]+)$/.exec(source);
  if (!match) return null;
  const [, ref, op, literalText] = match as unknown as [string, string, string, string];
  const literal = parseLiteral(literalText);
  if (!literal.ok) return null;
  const actual = resolveRef(ref, scope);
  if (op === "in") return Array.isArray(literal.value) ? literal.value.some((v) => deepEqual(v, actual)) : null;
  const equal = deepEqual(actual === undefined ? null : actual, literal.value);
  return op === "==" ? equal : !equal;
}
