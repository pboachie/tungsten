// SPDX-License-Identifier: Apache-2.0
/**
 * `ClientCore`: the behaviour behind every generated SDK method
 * (planning/06 "Transport pipeline"):
 *
 * ```
 * call → preflight → auth → idempotency → send → classify → verify? → result
 *                                           ▲              │
 *                                           └── retries ◀──┘
 * ```
 *
 * Every public method resolves; none throws. Failures are diagnostic
 * envelopes (`{ok: false, error}`).
 */
import { type AuthPlan, isSafeMethod, resolveAuth, TokenError, type TokenSource } from "./auth.js";
import {
  type CallContext,
  changeOf,
  classifyError,
  decodeBody,
  isMutation,
  matchResponse,
  type OutcomeCheck,
  outcomeUnknown,
  requestIdOf,
  verifyCallable,
} from "./classify.js";
import { checkToken, CONFIRMATION_TTL_MS, issueToken } from "./confirm.js";
import { diagnostic, scrubDiagnostic, scrubText } from "./envelope.js";
import {
  containsPlaceholder,
  describePredicate,
  evaluateDry,
  evaluateExpr,
  evaluatePredicate,
  placeholdersIn,
  resolveRef,
  type Scope,
} from "./expr.js";
import { checkKeyFormat, hasReplayProtection, keyFormatDescription, keyHeader, MemoryIdempotencyStore } from "./idempotency.js";
import {
  bodyValue,
  encodeBody,
  HeaderBag,
  SerializationError,
  serializeCookieParam,
  serializeHeaderParam,
  serializePathParam,
  serializeQueryParam,
  validHeaderName,
  validHeaderValue,
} from "./serialize.js";
import { attempt, type AttemptOutcome, nextLink, parseRetryAfter } from "./transport.js";
import type {
  ApiDescriptor,
  CallOptions,
  ClientCoreApi,
  ClientCoreExtensions,
  ClientOptions,
  Diagnostic,
  HttpMethod,
  IdempotencyStore,
  MacroDescriptor,
  MacroStep,
  MacroStepPreview,
  Middleware,
  OperationDescriptor,
  Page,
  ParamDescriptor,
  Predicate,
  PreviewResult,
  RenderedRequest,
  RequestContext,
  ResponseMeta,
  Result,
  RetryOptions,
  SchemaLike,
  Verification,
} from "./types.js";
import {
  canonicalJson,
  describeError,
  envelopeValue,
  getPath,
  isRecord,
  looksSensitive,
  REDACTED,
  redactPaths,
  redactSensitiveKeys,
  sha256Hex,
  sleep,
  splitPath,
} from "./util.js";
import { RUNTIME_VERSION } from "./version.js";

type Failure = { ok: false; error: Diagnostic; partial?: unknown };
type Step<T> = { ok: true; value: T } | Failure;

const METHODS: ReadonlySet<string> = new Set(["GET", "PUT", "POST", "DELETE", "OPTIONS", "HEAD", "PATCH", "TRACE"]);
const SAFETIES: ReadonlySet<string> = new Set(["read_only", "mutating", "destructive", "irreversible"]);
const POLICIES: ReadonlySet<string> = new Set(["none", "auto", "caller_owned", "content_hash", "content_identity"]);
const RETRY_CATEGORIES: ReadonlySet<string> = new Set(["RATE_LIMITED", "UPSTREAM_UNAVAILABLE", "TRANSPORT_FAILED", "OUTCOME_UNKNOWN"]);
const DEFAULT_RETRIES: RetryOptions = { max: 3, baseMs: 200, maxMs: 5000, jitter: "full", honorRetryAfter: true };
const DEFAULT_TIMEOUT_MS = 30_000;
const DEFAULT_POLL_INTERVAL_MS = 1000;
const DEFAULT_VERIFY_BUDGET_MS = 30_000;
const DEFAULT_MACRO_BUDGET_MS = 30_000;
const MACRO_PAGE_LIMIT = 100;
/** Same-origin redirects followed for one read attempt. */
const MAX_REDIRECTS = 5;
const REDIRECT_STATUSES: ReadonlySet<number> = new Set([301, 302, 303, 307, 308]);

/** Why a request is being prepared. `macro_preview` renders a macro step
 * from a dry evaluation: argument values that are placeholders for earlier
 * results are not validated. */
type Purpose = "call" | "preview" | "server_preview" | "macro_preview";

/** Marks the call options of a macro step whose run was confirmed at the
 * macro level. Module-private, so callers cannot set it. */
const MACRO_CONFIRMED: unique symbol = Symbol("tungsten.macroConfirmed");
/** A macro step's hook that spends the macro's confirmation token; called
 * once a step's request passed pre-flight and is about to be sent, so a
 * step that fails pre-flight leaves the token unspent. */
const MACRO_CLAIM: unique symbol = Symbol("tungsten.macroClaim");
type StepOptions = CallOptions & { [MACRO_CONFIRMED]?: true; [MACRO_CLAIM]?: () => Failure | null };

const SAFETY_RANK: Readonly<Record<string, number>> = { read_only: 0, mutating: 1, destructive: 2, irreversible: 3 };

/** Stands in for a verification reference that did not resolve; equal to nothing. */
const UNRESOLVED: unique symbol = Symbol("tungsten.unresolved");

/** A request ready to send, with its redacted rendering. */
interface Prepared {
  op: OperationDescriptor;
  args: Record<string, unknown>;
  method: HttpMethod;
  url: string;
  displayUrl: string;
  headers: HeaderBag;
  body: BodyInit | null;
  /** The body middleware sees: `body`, or its redacted rendering when
   * redaction changed a JSON or form body. */
  observedBody: BodyInit | null;
  displayBody: unknown;
  key: string | null;
  keyHeader: string;
  /** Auth query parameters (API keys in the query), re-applied on a followed redirect. */
  authQuery: Array<{ name: string; value: string }>;
  /** Wire names of query parameters whose values are shown redacted. */
  hiddenQuery: string[];
  secrets: Set<string>;
}

function fail(error: Diagnostic): Failure {
  return { ok: false, error };
}

/** `value[key]` when it is a non-empty string, else the fallback; safe
 * on hostile objects (throwing getters, proxies). */
function safeName(value: unknown, key: string, fallback: string): string {
  try {
    const name = isRecord(value) ? value[key] : undefined;
    return typeof name === "string" && name !== "" ? name : fallback;
  } catch {
    return fallback;
  }
}

function safeId(op: unknown): string {
  return safeName(op, "id", "<unknown operation>");
}

/** A finite number in [min, max], or the fallback. */
function bounded(value: unknown, fallback: number, min = 0, max = Number.MAX_SAFE_INTEGER): number {
  return typeof value === "number" && Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : fallback;
}

function isSchema(value: unknown): value is SchemaLike {
  return isRecord(value) && typeof value.safeParse === "function";
}

/** Structural problems that would make the descriptor unusable, or null. */
function descriptorProblem(op: unknown): string | null {
  if (!isRecord(op)) return "the operation descriptor is not an object";
  if (typeof op.id !== "string" || op.id === "") return "`id` must be a non-empty string";
  if (typeof op.method !== "string" || !METHODS.has(op.method)) return "`method` must be an HTTP method";
  if (typeof op.path !== "string") return "`path` must be a string";
  if (!Array.isArray(op.params)) return "`params` must be an array";
  for (const p of op.params) {
    if (!isRecord(p) || typeof p.name !== "string" || typeof p.wire !== "string" || !["path", "query", "header", "cookie"].includes(p.in as string)) {
      return "every parameter needs string `name` and `wire` and a valid `in`";
    }
  }
  if (op.body !== null && op.body !== undefined) {
    const body = op.body;
    if (!isRecord(body) || !isRecord(body.shape)) return "`body.shape` must be an object";
    const shape = body.shape;
    if (shape.kind === "merged" ? !Array.isArray(shape.fields) : !(shape.kind === "arg" && typeof shape.arg === "string")) {
      return "`body.shape` must be {kind: merged, fields} or {kind: arg, arg}";
    }
  }
  if (!Array.isArray(op.responses)) return "`responses` must be an array";
  if (!Array.isArray(op.security)) return "`security` must be an array of arrays";
  if (op.rpc !== null && op.rpc !== undefined) {
    const rpc = op.rpc;
    if (!isRecord(rpc) || typeof rpc.field !== "string" || typeof rpc.paramsField !== "string") return "`rpc` needs `field` and `paramsField`";
  }
  if (!isRecord(op.status) || (op.status.kind !== "implemented" && op.status.kind !== "gated")) return "`status.kind` must be implemented or gated";
  const agent = op.agent;
  if (!isRecord(agent)) return "`agent` must be an object";
  if (typeof agent.safety !== "string" || !SAFETIES.has(agent.safety)) return "`agent.safety` must be a safety tier";
  if (!isRecord(agent.idempotency) || typeof agent.idempotency.policy !== "string" || !POLICIES.has(agent.idempotency.policy)) {
    return "`agent.idempotency.policy` must be an idempotency policy";
  }
  if (!isRecord(agent.preview)) return "`agent.preview` must be an object";
  return null;
}

/** Arg keys that are parameters supplied by the caller (not auth, key or origin). */
function argParams(op: OperationDescriptor): ParamDescriptor[] {
  return op.params.filter((p) => p.role !== "idempotency_key" && p.role !== "origin" && p.role !== "auth");
}

/** JSON path of an argument issue: `body.x` for body fields, else `args.x`. */
function argumentPath(op: OperationDescriptor, path: ReadonlyArray<string | number>): string {
  const format = (segments: ReadonlyArray<string | number>): string =>
    segments.map((s) => (typeof s === "number" || /^\d+$/.test(String(s)) ? `[${s}]` : `.${String(s)}`)).join("");
  const [head, ...rest] = path;
  const body = op.body;
  if (body && head !== undefined) {
    if (body.shape.kind === "merged" && body.shape.fields.includes(String(head))) return `body${format(path)}`;
    if (body.shape.kind === "arg" && body.shape.arg === String(head)) return `body${format(rest)}`;
  }
  return path.length === 0 ? "args" : `args${format(path)}`;
}

/** The operation's sensitive argument paths (`AgentMeta.sensitiveRequestFields`). */
function sensitiveRequestPaths(op: OperationDescriptor): string[] {
  const listed: unknown = isRecord(op.agent) ? op.agent.sensitiveRequestFields : undefined;
  return Array.isArray(listed) ? listed.filter((p): p is string => typeof p === "string" && p !== "") : [];
}

/** An args path without array indices, dotted (`users.0.pin` → `users.pin`). */
function dottedArgPath(path: ReadonlyArray<string | number>): string {
  return path.filter((s) => typeof s === "string" && !/^\d+$/.test(s)).join(".");
}

function sensitiveArg(op: OperationDescriptor, path: ReadonlyArray<string | number>): boolean {
  const head = path[0];
  if (op.params.some((p) => p.sensitive === true && p.name === head)) return true;
  const dotted = dottedArgPath(path);
  if (dotted !== "" && sensitiveRequestPaths(op).some((f) => dotted === f || dotted.startsWith(`${f}.`))) return true;
  return path.some((segment) => typeof segment === "string" && looksSensitive(segment));
}

/** `value` (the argument at `path`) with the sensitive fields below it redacted. */
function redactBelow(op: OperationDescriptor, path: ReadonlyArray<string | number>, value: unknown): unknown {
  const dotted = dottedArgPath(path);
  const prefix = dotted === "" ? "" : `${dotted}.`;
  const below = sensitiveRequestPaths(op)
    .filter((f) => f.startsWith(prefix))
    .map((f) => f.slice(prefix.length));
  return below.length > 0 ? redactPaths(value, below) : value;
}

/** Sensitive argument paths relative to the request body as sent (the
 * rpc envelope's params member for rpc operations); `""` is the whole body. */
function sensitiveBodyPaths(op: OperationDescriptor): string[] {
  const body = op.body;
  const paths = sensitiveRequestPaths(op);
  let relative: string[];
  if (body?.shape.kind === "merged") {
    const fields = new Set(body.shape.fields);
    relative = paths.filter((p) => fields.has(splitPath(p)[0] ?? ""));
  } else if (body?.shape.kind === "arg") {
    const arg = body.shape.arg;
    relative = paths.filter((p) => p === arg || p.startsWith(`${arg}.`)).map((p) => (p === arg ? "" : p.slice(arg.length + 1)));
  } else {
    relative = op.rpc ? paths : [];
  }
  return op.rpc ? relative.map((p) => (p === "" ? op.rpc!.paramsField : `${op.rpc!.paramsField}.${p}`)) : relative;
}

/** Every string or number found at a dotted path, through arrays. */
function valuesAt(value: unknown, segments: string[], out: string[], depth = 0): void {
  if (depth > 64) return;
  if (Array.isArray(value)) {
    for (const item of value) valuesAt(item, segments, out, depth + 1);
    return;
  }
  if (segments.length === 0) {
    if (typeof value === "string" || typeof value === "number") out.push(String(value));
    else if (isRecord(value)) for (const v of Object.values(value)) valuesAt(v, [], out, depth + 1);
    return;
  }
  if (!isRecord(value)) return;
  const [head, ...rest] = segments as [string, ...string[]];
  valuesAt(value[head], rest, out, depth + 1);
}

/** A copy of the args without `undefined` members at any depth: an
 * optional argument set to `undefined` (`{cursor: maybe}`) is absent, as
 * TypeScript callers without `exactOptionalPropertyTypes` expect. Only
 * plain objects and arrays are copied; dates, bytes and blobs are kept. */
function withoutUndefined(value: unknown, depth = 0): unknown {
  if (depth > 64) return value;
  if (Array.isArray(value)) return value.map((item) => withoutUndefined(item, depth + 1));
  if (!isRecord(value)) return value;
  const proto: unknown = Object.getPrototypeOf(value);
  if (proto !== Object.prototype && proto !== null) return value;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(value)) if (v !== undefined) out[k] = withoutUndefined(v, depth + 1);
  return out;
}

/** `args` plus each parameter's value under its wire name, so references
 * written against the API (agent.yml `{device_id}`, `$args.endpoint_id`)
 * find arguments whose generated name differs (`deviceId`). */
function withWireNames(op: OperationDescriptor, args: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = { ...args };
  for (const p of Array.isArray(op.params) ? op.params : []) {
    if (!isRecord(p) || typeof p.name !== "string" || typeof p.wire !== "string") continue;
    if (args[p.name] !== undefined && !Object.prototype.hasOwnProperty.call(out, p.wire)) out[p.wire] = args[p.name];
  }
  return out;
}

/** A path whose first segment is a parameter's wire name, rewritten to
 * the parameter's generated name. */
function argPath(op: OperationDescriptor, path: string[]): string[] {
  const [head, ...rest] = path;
  const param = op.params.find((p) => p.wire === head && p.name !== head);
  return param ? [param.name, ...rest] : path;
}

function interpolate(template: string, args: Record<string, unknown>, op: OperationDescriptor): string {
  return template.replace(/\{([^{}]+)\}/g, (_m, field: string) => {
    const path = argPath(op, field.trim().split("."));
    const value = getPath(args, path);
    if (value === undefined || value === null) return "<unset>";
    if (sensitiveArg(op, path)) return REDACTED;
    const shown = envelopeValue(value, false);
    return typeof shown === "string" ? shown : JSON.stringify(shown);
  });
}

/** What middleware may see of an encoded body: the body itself, unless
 * the preview rendering redacted something in it (sensitive arguments,
 * credential-like keys); then that rendering, serialized like the body. */
function observableBody(encoding: string, encoded: { body: BodyInit | null; display: unknown; raw: unknown }): BodyInit | null {
  if (encoded.body === null) return null;
  let changed: boolean;
  try {
    changed = canonicalJson(encoded.display) !== canonicalJson(encoded.raw);
  } catch {
    changed = true;
  }
  if (!changed) return encoded.body;
  if (encoding === "form" && isRecord(encoded.display)) {
    return new URLSearchParams(Object.entries(encoded.display).map(([k, v]) => [k, typeof v === "string" ? v : JSON.stringify(v)])).toString();
  }
  return typeof encoded.display === "string" ? encoded.display : JSON.stringify(encoded.display);
}

/** A preview's display URL with the placeholders of `args` shown as
 * written (`/v1/webhooks/<from step created: endpoint_id>/enable`) instead
 * of percent-encoded. Only renderings change; nothing with placeholders
 * is ever sent. */
function unescapePlaceholders(url: string, args: unknown): string {
  let out = url;
  for (const text of new Set(placeholdersIn(args))) {
    // Parameters are encoded with encodeURIComponent (serialize.ts).
    out = out.split(encodeURIComponent(text)).join(text);
  }
  return out;
}

/** ` with name "value", ...` for remediation text naming a call's
 * arguments; "" when there are none. */
function withArguments(args: unknown): string {
  if (!isRecord(args)) return "";
  const shown = Object.entries(args).map(([k, v]) => `${k} ${shortJson(v)}`);
  return shown.length > 0 ? ` with ${shown.join(", ")}` : "";
}

/** ` (name "value", ...)`: the scalar, non-sensitive arguments a call was
 * made with (at most four, by wire name), to recognize what it created. */
function sentArguments(op: OperationDescriptor, args: Record<string, unknown>): string {
  const shown: string[] = [];
  for (const [key, value] of Object.entries(args)) {
    if (shown.length === 4) break;
    if (!(typeof value === "string" || typeof value === "number" || typeof value === "boolean")) continue;
    if (sensitiveArg(op, [key])) continue;
    const wire = op.params.find((p) => p.name === key)?.wire ?? key;
    shown.push(`${wire} ${shortJson(value)}`);
  }
  return shown.length > 0 ? ` (${shown.join(", ")})` : "";
}

/** JSON for remediation text, cut to 80 characters. */
function shortJson(value: unknown): string {
  let text: string;
  try {
    text = JSON.stringify(value) ?? String(value);
  } catch {
    text = String(value);
  }
  return text.length > 80 ? `${text.slice(0, 77)}...` : text;
}

function trimTrailingSlash(base: string): string {
  return base.replace(/\/+$/, "");
}

/**
 * The runtime core used by generated clients. One instance per client:
 * it owns the idempotency store, the confirmation-token key, the OAuth2
 * token cache and the registry of operations resolvable by id.
 */
export class ClientCore implements ClientCoreApi, ClientCoreExtensions {
  readonly api: ApiDescriptor;
  readonly options: ClientOptions;
  readonly #registry = new Map<string, OperationDescriptor>();
  readonly #store: IdempotencyStore;
  readonly #confirmationKey: Uint8Array;
  readonly #tokenCache = new Map<string, { token: string; expiresAt: number }>();
  readonly #tokenInflight = new Map<string, Promise<string>>();
  /** Confirmation tokens already used to send, with the replay protection
   * of their first use, until they expire. */
  readonly #usedTokens = new Map<string, { bind: string | null; expiry: number }>();

  constructor(api: ApiDescriptor, options: ClientOptions = {}) {
    this.api = isRecord(api) ? api : ({} as ApiDescriptor);
    this.options = isRecord(options) ? options : {};
    let store: IdempotencyStore | undefined;
    let key: Uint8Array | undefined;
    try {
      const candidate = this.options.idempotencyStore;
      if (isRecord(candidate) && typeof candidate.get === "function" && typeof candidate.put === "function") store = candidate;
      const configured = this.options.confirmationKey;
      if (configured instanceof Uint8Array && configured.byteLength > 0) key = configured;
      if (Array.isArray(this.options.operations)) this.register(...this.options.operations);
    } catch {
      // Unreadable options: keep the defaults; calls report the problem.
    }
    this.#store = store ?? new MemoryIdempotencyStore();
    this.#confirmationKey = key ?? crypto.getRandomValues(new Uint8Array(32));
  }

  /** Make operations resolvable by id for verification hooks, endpoint
   * previews and macro steps. Invalid entries are ignored. */
  register(...ops: OperationDescriptor[]): void {
    for (const op of ops) {
      if (isRecord(op) && typeof op.id === "string" && op.id !== "" && !this.#registry.has(op.id)) this.#registry.set(op.id, op);
    }
  }

  /** A registered operation by id. */
  operation(id: string): OperationDescriptor | undefined {
    return this.#registry.get(id);
  }

  /** Validate, authenticate, apply idempotency and confirmation rules,
   * send with retries and classify the response. Never throws. */
  async call<T>(op: OperationDescriptor, args: Record<string, unknown>, opts?: CallOptions): Promise<Result<T>> {
    try {
      return await this.#call<T>(op, args, this.#callOptions(opts), null);
    } catch (error) {
      return fail(this.#internal(op, error, "building or sending the request"));
    }
  }

  /** Run the operation's preview mode: local rendering (no network), a
   * dry-run header, or a preview endpoint. Mutating operations get a
   * confirmation token bound to these exact arguments for five minutes. */
  async preview(op: OperationDescriptor, args: Record<string, unknown>, opts?: CallOptions): Promise<Result<PreviewResult>> {
    try {
      return await this.#preview(op, args, this.#callOptions(opts));
    } catch (error) {
      return fail(this.#internal(op, error, "rendering the preview"));
    }
  }

  /** Iterate pages (cursor, offset, page number or `Link` header); stops
   * after the last page or after yielding the first error. */
  async *pages<T>(op: OperationDescriptor, args: Record<string, unknown>, opts?: CallOptions): AsyncIterable<Result<Page<T>>> {
    try {
      yield* this.#pages<T>(op, args, this.#callOptions(opts));
    } catch (error) {
      yield fail(this.#internal(op, error, "paginating"));
    }
  }

  /** Call a read operation until `until` holds on its body or the budget
   * runs out (`timedOut: true` with the last result). */
  async poll<T>(
    op: OperationDescriptor,
    args: Record<string, unknown>,
    until: Predicate,
    intervalMs: number,
    budgetMs: number,
    opts?: CallOptions,
  ): Promise<Result<T> & { timedOut?: boolean }> {
    try {
      return await this.#poll<T>(op, args, until, intervalMs, budgetMs, this.#callOptions(opts));
    } catch (error) {
      return fail(this.#internal(op, error, "polling"));
    }
  }

  /** Run a compiled macro: steps in order, each step's success body bound
   * to its `as` name; returns the evaluated output, or the first failing
   * step's envelope (its remediation names the steps already completed). */
  async runMacro<T>(macro: MacroDescriptor, input: Record<string, unknown>, opts?: CallOptions): Promise<Result<T>> {
    try {
      return await this.#runMacro<T>(macro, input, this.#callOptions(opts));
    } catch (error) {
      return fail(this.#internal({ id: safeName(macro, "name", "<unknown macro>") }, error, "running the macro"));
    }
  }

  /** Preview a macro: no request is sent. Validates and renders the first
   * step, lists the effects of every step, and returns a confirmation token
   * bound to the macro and this exact input unless the macro is read-only. */
  async previewMacro(macro: MacroDescriptor, input: Record<string, unknown>, opts?: CallOptions): Promise<Result<PreviewResult>> {
    try {
      return await this.#previewMacro(macro, input, this.#callOptions(opts));
    } catch (error) {
      return fail(this.#internal({ id: safeName(macro, "name", "<unknown macro>") }, error, "previewing the macro"));
    }
  }

  // ------------------------------------------------------------ internals

  #callOptions(opts: unknown): CallOptions {
    return isRecord(opts) ? (opts as CallOptions) : {};
  }

  #internal(op: unknown, error: unknown, during: string): Diagnostic {
    return diagnostic(safeId(op), "UNEXPECTED_RESPONSE", {
      remediation: `The SDK failed while ${during} (${describeError(error)}). This is a bug in the generated SDK or the runtime; report it. If the call may have been sent, check its effect before repeating it.`,
      retryable: "never",
    });
  }

  #now(): number {
    const now = this.options.now;
    if (typeof now === "function") {
      const value = now();
      if (typeof value === "number" && Number.isFinite(value)) return value;
    }
    return Date.now();
  }

  #fetch(): typeof fetch {
    return typeof this.options.fetch === "function" ? this.options.fetch : globalThis.fetch.bind(globalThis);
  }

  /** The retry policy of one operation: the runtime defaults, then the
   * API's defaults for the operation's tier, then `ClientOptions.retries`;
   * each layer overrides the fields it sets. */
  #retryOptions(op: OperationDescriptor): RetryOptions {
    const tiers: Partial<NonNullable<ApiDescriptor["retries"]>> = isRecord(this.api.retries) ? this.api.retries : {};
    const tier: unknown = isMutation(op) ? tiers.mutating : tiers.readOnly;
    const layers: Array<Record<string, unknown>> = [isRecord(tier) ? tier : {}, isRecord(this.options.retries) ? this.options.retries : {}];
    const pick = <T>(key: keyof RetryOptions, valid: (v: unknown) => v is T, fallback: T): T => {
      let value = fallback;
      for (const layer of layers) {
        const candidate = layer[key];
        if (valid(candidate)) value = candidate;
      }
      return value;
    };
    const finite = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
    const jitter = (v: unknown): v is RetryOptions["jitter"] => v === "none" || v === "equal" || v === "full";
    const flag = (v: unknown): v is boolean => typeof v === "boolean";
    return {
      max: Math.floor(bounded(pick("max", finite, DEFAULT_RETRIES.max), DEFAULT_RETRIES.max, 0, 100)),
      baseMs: bounded(pick("baseMs", finite, DEFAULT_RETRIES.baseMs), DEFAULT_RETRIES.baseMs, 0),
      maxMs: bounded(pick("maxMs", finite, DEFAULT_RETRIES.maxMs), DEFAULT_RETRIES.maxMs, 0),
      jitter: pick("jitter", jitter, DEFAULT_RETRIES.jitter),
      honorRetryAfter: pick("honorRetryAfter", flag, DEFAULT_RETRIES.honorRetryAfter),
    };
  }

  #timeout(opts: CallOptions): number {
    return bounded(opts.timeoutMs, bounded(this.options.timeoutMs, DEFAULT_TIMEOUT_MS, 1), 1);
  }

  #emit(d: Diagnostic): void {
    try {
      this.options.onDiagnostic?.(d);
    } catch {
      // A failing observer never changes the call.
    }
  }

  #middleware(): Middleware[] {
    return Array.isArray(this.options.middleware) ? this.options.middleware.filter(isRecord) : [];
  }

  async #call<T>(op: OperationDescriptor, args: Record<string, unknown>, opts: CallOptions, urlOverride: string | null): Promise<Result<T>> {
    const prepared = await this.#prepare(op, args, opts, "call", urlOverride);
    if (!prepared.ok) return prepared;
    const result = await this.#send<T>(prepared.value, opts);
    if (result.ok && opts.verify === true && op.agent.verify) {
      return { ...result, verification: await this.#verify(op, prepared.value.args, result.value, opts) };
    }
    return result;
  }

  // ------------------------------------------------------------ preflight

  #validation(op: OperationDescriptor, path: ReadonlyArray<string | number>, value: unknown, expected: string, remediation: string): Failure {
    return fail(
      diagnostic(op.id, "VALIDATION_FAILED", {
        failed_parameter: argumentPath(op, path),
        received_value: envelopeValue(redactBelow(op, path, value), sensitiveArg(op, path)),
        expected,
        remediation,
      }),
    );
  }

  /** Pre-flight validation of the args. With `placeholders`, an issue at
   * (or below) a value that is a dry evaluation's placeholder is not a
   * failure: the value is only known once the earlier step has run. */
  #validateArgs(op: OperationDescriptor, args: Record<string, unknown>, placeholders: boolean): Failure | null {
    const params = argParams(op);
    for (const p of params) {
      if (p.required && (args[p.name] === undefined || args[p.name] === null)) {
        return this.#validation(
          op,
          [p.name],
          args[p.name],
          `a value for the required ${p.in} parameter ${p.wire}`,
          `Pass ${p.name}; ${op.id} cannot be called without it.`,
        );
      }
    }
    const body = op.body;
    if (body && body.required && body.shape.kind === "arg" && args[body.shape.arg] === undefined) {
      return this.#validation(op, [body.shape.arg], undefined, "a request body", `Pass the request body as ${body.shape.arg}.`);
    }
    const request = op.request;
    if (isSchema(request)) {
      const parsed = request.safeParse(args);
      if (!parsed.success) {
        const issues = isRecord(parsed.error) && Array.isArray(parsed.error.issues) ? parsed.error.issues : [];
        const pathOf = (issue: unknown): Array<string | number> =>
          isRecord(issue) && Array.isArray(issue.path) ? issue.path.filter((s): s is string | number => typeof s === "string" || typeof s === "number") : [];
        const pending = (path: Array<string | number>): boolean =>
          placeholders && path.some((_, i) => containsPlaceholder(getPath(args, path.slice(0, i + 1).map(String))));
        const issue = placeholders ? issues.find((candidate) => !pending(pathOf(candidate))) : issues[0];
        if (placeholders && issue === undefined) return null;
        const path = pathOf(issue);
        const message = isRecord(issue) && typeof issue.message === "string" ? issue.message : "a valid value";
        return this.#validation(op, path, getPath(args, path.map(String)), message, `Fix ${argumentPath(op, path)} (${message}) and call again.`);
      }
      return null;
    }
    // Without a schema, unknown keys are rejected so a misspelt argument is
    // never silently dropped (rpc methods without a body take free-form args).
    if (op.rpc && !body) return null;
    const allowed = new Set(params.map((p) => p.name));
    if (body?.shape.kind === "merged") for (const f of body.shape.fields) allowed.add(f);
    if (body?.shape.kind === "arg") allowed.add(body.shape.arg);
    for (const key of Object.keys(args)) {
      if (!allowed.has(key) && args[key] !== undefined) {
        const names = [...allowed].sort();
        const list = names.length > 0 ? names.slice(0, 20).join(", ") : "no arguments";
        return this.#validation(op, [key], args[key], `one of: ${list}`, `Remove ${key}; ${op.id} does not accept it.`);
      }
    }
    return null;
  }

  #checkKey(op: OperationDescriptor, opts: CallOptions, purpose: Purpose): Failure | null {
    const policy = op.agent.idempotency;
    const key = opts.idempotencyKey;
    const note = policy.note ? ` ${policy.note}` : "";
    if (key === undefined) {
      if (purpose === "call" && policy.policy === "caller_owned" && policy.persistRequired) {
        return fail(
          diagnostic(op.id, "VALIDATION_FAILED", {
            failed_parameter: "idempotencyKey",
            expected: keyFormatDescription(policy.format),
            remediation: `Generate a random ${/uuid/i.test(policy.format ?? "") ? "UUIDv4" : "unique key"}, persist it with the intent of this ${op.id} call, and pass it as idempotencyKey. Reuse the exact same key on every retry; a new key can apply the effect twice.${note}`,
          }),
        );
      }
      return null;
    }
    if (policy.policy === "none" && !op.params.some((p) => p.role === "idempotency_key")) return null;
    if (policy.policy === "content_identity" || policy.policy === "content_hash") return null;
    const expected = typeof key === "string" ? checkKeyFormat(key, policy.policy === "caller_owned" ? policy.format : null) : keyFormatDescription(policy.format);
    if (expected !== null) {
      return fail(
        diagnostic(op.id, "VALIDATION_FAILED", {
          failed_parameter: "idempotencyKey",
          received_value: envelopeValue(key, false),
          expected,
          remediation: `Pass idempotencyKey as ${expected}. Generate it once, persist it with the intent of this call, and reuse it on every retry.${note}`,
        }),
      );
    }
    return null;
  }

  async #checkConfirmation(op: OperationDescriptor, args: Record<string, unknown>, opts: CallOptions): Promise<Failure | null> {
    if ((opts as StepOptions)[MACRO_CONFIRMED] === true) return null;
    return this.#confirmed(op.id, op.id, op.agent.safety, args, opts.confirm, "preview(...)", opts.allowConfirmTrue !== false);
  }

  /** The confirmation rule of planning/04 for one tier: `destructive`
   * accepts `true` (unless `allowTrue` is false) or a token, `irreversible`
   * only a token issued for `subject` (an operation id, or `macro:<name>`)
   * and these exact args. */
  async #confirmed(id: string, subject: string, safety: string, args: unknown, confirm: unknown, previewCall: string, allowTrue: boolean): Promise<Failure | null> {
    if (safety !== "destructive" && safety !== "irreversible") return null;
    const how = `call ${previewCall} with the same arguments and pass its confirmation_token as confirm`;
    const required = (remediation: string): Failure =>
      fail(diagnostic(id, "CONFIRMATION_REQUIRED", { failed_parameter: "confirm", expected: "a confirmation_token from preview()", remediation }));
    if (confirm === true) {
      if (safety === "destructive" && allowTrue) return null;
      return required(`${id} is ${safety}, so confirm: true is not accepted: ${how}.`);
    }
    if (typeof confirm !== "string" || confirm === "") {
      return required(`${id} is ${safety}: ${how}${safety === "destructive" && allowTrue ? " (or pass confirm: true)" : ""}.`);
    }
    const check = await checkToken(this.#confirmationKey, confirm, subject, args, this.#now());
    if (check === "valid") return null;
    if (check === "expired") return required(`The confirmation token expired (tokens last ${CONFIRMATION_TTL_MS / 60000} minutes): ${how}.`);
    return required(`The confirmation token was not issued by this client for ${id} with exactly these arguments: ${how}.`);
  }

  /** Spend a valid confirmation token on the call about to be sent. A
   * token authorizes one intent: its first use binds it to that call's
   * replay protection (`bind`: the idempotency key sent, or the identity
   * of an identity body), and a later use is accepted only as a retry of
   * the same intent under the same protection. A call without protection
   * (no key) can use a token once. */
  #claimToken(id: string, token: string, bind: string | null, previewCall: string): Failure | null {
    const now = this.#now();
    for (const [used, entry] of this.#usedTokens) if (entry.expiry <= now) this.#usedTokens.delete(used);
    const previous = this.#usedTokens.get(token);
    if (previous) {
      if (previous.bind !== null && previous.bind === bind) return null;
      const why =
        previous.bind === null
          ? "This call has no idempotency key, so a repeat can apply the effect twice"
          : `It was used with ${bind === null ? "an idempotency key" : "another idempotency key"}; only a retry with that same key may reuse it`;
      return fail(
        diagnostic(id, "CONFIRMATION_REQUIRED", {
          failed_parameter: "confirm",
          expected: "a confirmation_token from preview()",
          remediation: `This confirmation token was already used for one ${id} call. ${why}. Check whether that call took effect; to send again, call ${previewCall} and confirm with its new token.`,
        }),
      );
    }
    const expiry = Number(/^tgc1\.(\d{1,16})\./.exec(token)?.[1] ?? now + CONFIRMATION_TTL_MS);
    this.#usedTokens.set(token, { bind, expiry });
    return null;
  }

  // ------------------------------------------------------------- building

  async #prepare(
    op: OperationDescriptor,
    rawArgs: Record<string, unknown>,
    opts: CallOptions,
    purpose: Purpose,
    urlOverride: string | null,
  ): Promise<Step<Prepared>> {
    const problem = descriptorProblem(op);
    if (problem) {
      return fail(
        diagnostic(safeId(op), "VALIDATION_FAILED", {
          failed_parameter: "operation",
          expected: "a valid operation descriptor",
          remediation: `The operation descriptor is invalid: ${problem}. Regenerate the SDK or fix the descriptor; nothing was sent.`,
        }),
      );
    }
    this.register(op);
    if (!isRecord(rawArgs)) {
      return fail(
        diagnostic(op.id, "VALIDATION_FAILED", {
          failed_parameter: "args",
          received_value: envelopeValue(rawArgs, false),
          expected: "an object of named arguments",
          remediation: `Pass the arguments of ${op.id} as one object, e.g. {${argParams(op)
            .slice(0, 3)
            .map((p) => `${p.name}: ...`)
            .join(", ")}}.`,
        }),
      );
    }
    try {
      canonicalJson(rawArgs);
    } catch {
      return this.#validation(op, [], "<cyclic value>", "a JSON-like value without cycles", "Pass arguments without circular references.");
    }
    const args = withoutUndefined(rawArgs) as Record<string, unknown>;
    const invalid = this.#validateArgs(op, args, purpose === "macro_preview") ?? this.#checkKey(op, opts, purpose);
    if (invalid) return invalid;
    if (purpose === "call") {
      const unconfirmed = await this.#checkConfirmation(op, args, opts);
      if (unconfirmed) return unconfirmed;
    }

    const method = op.method;
    const auth = await resolveAuth(this.api, op, isRecord(this.options.auth) ? this.options.auth : {}, method, this.#tokenSource);
    if (!auth.ok) return fail(diagnostic(op.id, "AUTH_FAILED", { remediation: auth.remediation }));
    const secrets = new Set(auth.secrets.filter((s) => s !== ""));
    for (const p of op.params) {
      const value = args[p.name];
      if (p.sensitive === true && (typeof value === "string" || typeof value === "number")) secrets.add(String(value));
    }
    for (const path of sensitiveRequestPaths(op)) {
      const found: string[] = [];
      valuesAt(args, splitPath(path), found);
      for (const value of found) secrets.add(value);
    }

    const url = this.#buildUrl(op, args, auth.plan, urlOverride);
    if (!url.ok) return url;

    const headers = new HeaderBag();
    this.#buildHeaders(op, args, opts, auth.plan, headers);

    let encoded;
    try {
      const value = isSafeMethod(method) && method !== "OPTIONS" ? undefined : bodyValue(op, args, new Set(argParams(op).map((p) => p.name)));
      const bodyPaths = sensitiveBodyPaths(op);
      encoded = await encodeBody(op.body ?? null, value, (display) =>
        redactSensitiveKeys(bodyPaths.includes("") ? REDACTED : redactPaths(display, bodyPaths.filter((p) => p !== ""))),
      );
    } catch (error) {
      if (error instanceof SerializationError) {
        return this.#validation(op, error.parameter === "body" && op.body?.shape.kind === "arg" ? [op.body.shape.arg] : [], error.value, error.expected, `Pass ${error.parameter} as ${error.expected}.`);
      }
      throw error;
    }
    if (encoded.contentType) headers.set("Content-Type", encoded.contentType);
    const observedBody = observableBody(op.body?.encoding ?? "json", encoded);

    const header = keyHeader(op);
    const key = await this.#idempotencyKey(op, args, opts, purpose, encoded.hashMaterial);
    if (!key.ok) return key;
    if (key.value !== null) {
      headers.set(header, key.value, true);
      secrets.add(key.value);
    } else if (purpose !== "call" && (op.agent.idempotency.policy === "auto" || op.agent.idempotency.policy === "caller_owned")) {
      headers.set(header, "<set at call time>");
    }
    for (const h of auth.plan.headers) headers.set(h.name, h.value, h.secret);
    if (purpose === "server_preview" && op.agent.preview.mode === "header") headers.set(op.agent.preview.header, op.agent.preview.value);

    const record = headers.toRecord(false);
    for (const [name, value] of Object.entries(record)) {
      if (!validHeaderName(name) || !validHeaderValue(value)) {
        const fromAuth = auth.plan.headers.some((h) => h.name.toLowerCase() === name.toLowerCase());
        if (fromAuth) {
          return fail(diagnostic(op.id, "AUTH_FAILED", { remediation: `The credential for header ${name} contains characters not allowed in an HTTP header; check ClientOptions.auth.` }));
        }
        const param = op.params.find((p) => p.wire.toLowerCase() === name.toLowerCase());
        return this.#validation(
          op,
          param ? [param.name] : [],
          headers.isSecret(name) ? REDACTED : value,
          "a header value of visible ASCII characters (no line breaks)",
          `Header ${name} cannot carry this value; remove line breaks and non-Latin-1 characters.`,
        );
      }
    }
    const claimMacro = (opts as StepOptions)[MACRO_CLAIM];
    if (purpose === "call" && typeof claimMacro === "function") {
      const spent = claimMacro();
      if (spent) return spent;
    } else if (purpose === "call" && typeof opts.confirm === "string" && (opts as StepOptions)[MACRO_CONFIRMED] !== true) {
      if (op.agent.safety === "destructive" || op.agent.safety === "irreversible") {
        const policy = op.agent.idempotency.policy;
        const bind = key.value ?? (policy === "content_identity" ? "content-identity" : null);
        const spent = this.#claimToken(op.id, opts.confirm, bind, "preview(...)");
        if (spent) return spent;
      }
    }
    return {
      ok: true,
      value: {
        op,
        args,
        method,
        url: url.value.url,
        displayUrl: url.value.display,
        headers,
        body: encoded.body,
        observedBody,
        displayBody: encoded.display,
        key: key.value,
        keyHeader: header,
        authQuery: auth.plan.query.map((q) => ({ name: q.name, value: q.value })),
        hiddenQuery: [...auth.plan.query.map((q) => q.name), ...op.params.filter((x) => x.in === "query" && x.sensitive === true).map((x) => x.wire)],
        secrets: new Set([...secrets, ...headers.secrets()]),
      },
    };
  }

  #buildUrl(op: OperationDescriptor, args: Record<string, unknown>, plan: AuthPlan, urlOverride: string | null): Step<{ url: string; display: string }> {
    const servers = Array.isArray(this.api.servers) ? this.api.servers.filter((s) => typeof s === "string") : [];
    const base = typeof this.options.baseUrl === "string" ? this.options.baseUrl : servers[0];
    if (base === undefined || base === "") {
      return fail(diagnostic(op.id, "TRANSPORT_FAILED", { retryable: "never", remediation: "No base URL is configured: set ClientOptions.baseUrl. Nothing was sent." }));
    }
    const enc = encodeURIComponent;
    const params = argParams(op);
    let url: string;
    let display: string;
    if (urlOverride !== null) {
      url = urlOverride;
      display = urlOverride;
    } else {
      let missing: ParamDescriptor | null = null;
      let unknownPlaceholder: string | null = null;
      const path = op.path.replace(/\{([^{}]+)\}/g, (match, wire: string) => {
        const p = params.find((x) => x.in === "path" && x.wire === wire) ?? params.find((x) => x.in === "path" && x.name === wire);
        if (!p) {
          unknownPlaceholder = wire;
          return match;
        }
        const value = args[p.name];
        if (value === undefined || value === null) {
          missing = p;
          return match;
        }
        return serializePathParam(p, value);
      });
      if (unknownPlaceholder !== null) {
        return fail(diagnostic(op.id, "VALIDATION_FAILED", { failed_parameter: "operation", expected: "a valid operation descriptor", remediation: `The path template has a placeholder {${String(unknownPlaceholder)}} with no path parameter; regenerate the SDK.` }));
      }
      if (missing !== null) {
        const p: ParamDescriptor = missing;
        return this.#validation(op, [p.name], undefined, `a value for path parameter ${p.wire}`, `Pass ${p.name}.`);
      }
      // A path segment that is empty or a dot segment (`.`, `..`, also
      // percent-encoded) would be removed or resolved by URL parsing, so
      // the request would reach another resource (DELETE /items/.. is
      // DELETE /): refuse it before anything is built or confirmed.
      const templateSegments = op.path.split("/");
      const builtSegments = path.split("/");
      if (templateSegments.length === builtSegments.length) {
        for (const [i, segment] of builtSegments.entries()) {
          const template = templateSegments[i] ?? "";
          if (!template.includes("{") || !(segment === "" || /^(?:\.|%2e){1,2}$/i.test(segment))) continue;
          const wire = /\{([^{}]+)\}/.exec(template)?.[1];
          const p = params.find((x) => x.in === "path" && (x.wire === wire || x.name === wire));
          return this.#validation(
            op,
            p ? [p.name] : [],
            p ? args[p.name] : segment,
            "a non-empty path segment other than . and ..",
            `Pass ${p ? p.name : "the path parameter"} as the identifier of one resource; "${segment}" would change the request path.`,
          );
        }
      }
      const parts: string[] = [];
      const shown: string[] = [];
      for (const p of params) {
        if (p.in !== "query") continue;
        const serialized = serializeQueryParam(p, args[p.name]);
        parts.push(...serialized);
        shown.push(...(p.sensitive ? serialized.map((s) => `${s.split("=")[0]}=${REDACTED}`) : serialized));
      }
      for (const q of plan.query) {
        parts.push(`${enc(q.name)}=${enc(q.value)}`);
        shown.push(`${enc(q.name)}=${REDACTED}`);
      }
      const root = trimTrailingSlash(base);
      const join = (list: string[]): string => (list.length > 0 ? `${path.includes("?") ? "&" : "?"}${list.join("&")}` : "");
      url = `${root}${path}${join(parts)}`;
      display = `${root}${path}${join(shown)}`;
      plan = { ...plan, query: [] };
    }
    if (urlOverride !== null && plan.query.length > 0) {
      const target = new URL(url);
      for (const q of plan.query) if (!target.searchParams.has(q.name)) target.searchParams.set(q.name, q.value);
      url = target.toString();
      const shownTarget = new URL(display);
      for (const q of plan.query) shownTarget.searchParams.set(q.name, REDACTED);
      display = shownTarget.toString();
    }
    try {
      const parsed = new URL(url);
      if (parsed.protocol !== "http:" && parsed.protocol !== "https:") throw new TypeError("unsupported protocol");
      if (parsed.username || parsed.password) throw new TypeError("credentials in the URL");
    } catch {
      return fail(
        diagnostic(op.id, "TRANSPORT_FAILED", {
          retryable: "never",
          remediation: "The base URL is not a valid absolute http(s) URL without credentials: fix ClientOptions.baseUrl. Nothing was sent.",
        }),
      );
    }
    return { ok: true, value: { url, display } };
  }

  #buildHeaders(
    op: OperationDescriptor,
    args: Record<string, unknown>,
    opts: CallOptions,
    plan: AuthPlan,
    headers: HeaderBag,
  ): void {
    const accept = [
      ...new Set(
        op.responses
          .filter((r) => isRecord(r) && r.kind === "success" && typeof r.mediaType === "string")
          .map((r) => r.mediaType as string),
      ),
    ];
    if (accept.length > 0) headers.set("Accept", accept.join(", "));
    const apiName = typeof this.api.name === "string" ? this.api.name : "api";
    const apiVersion = typeof this.api.version === "string" ? this.api.version : "0";
    headers.set("X-Tungsten-Runtime", `tungsten-ts/${RUNTIME_VERSION} ${apiName}-sdk/${apiVersion}`);
    headers.set("X-Tungsten-Operation", op.id);
    if ((globalThis as { document?: unknown }).document === undefined) {
      const tungsten = typeof this.api.tungstenVersion === "string" ? this.api.tungstenVersion : RUNTIME_VERSION;
      headers.set("User-Agent", `${apiName}-sdk/${apiVersion} tungsten/${tungsten} (typescript)`);
    }
    for (const extra of [this.options.headers, opts.headers]) {
      if (!isRecord(extra)) continue;
      for (const [name, value] of Object.entries(extra)) {
        if (typeof value === "string") headers.set(name, value, looksSensitive(name));
      }
    }
    const cookies: string[] = [];
    const existingCookie = headers.get("Cookie");
    if (existingCookie !== undefined) cookies.push(existingCookie);
    for (const p of argParams(op)) {
      const value = args[p.name];
      if (value === undefined || value === null) continue;
      if (p.in === "header") headers.set(p.wire, serializeHeaderParam(p, value), p.sensitive === true);
      else if (p.in === "cookie") cookies.push(serializeCookieParam(p, value));
    }
    for (const c of plan.cookies) cookies.push(`${c.name}=${c.value}`);
    // Cookies are session material: the whole header is always redacted.
    if (cookies.length > 0) headers.set("Cookie", cookies.join("; "), true);
  }

  async #idempotencyKey(
    op: OperationDescriptor,
    args: Record<string, unknown>,
    opts: CallOptions,
    purpose: Purpose,
    hashMaterial: string | Uint8Array | null,
  ): Promise<Step<string | null>> {
    const policy = op.agent.idempotency.policy;
    const supplied = typeof opts.idempotencyKey === "string" ? opts.idempotencyKey : null;
    switch (policy) {
      case "content_identity":
        return { ok: true, value: null };
      case "content_hash":
        return { ok: true, value: hashMaterial === null ? null : await sha256Hex(hashMaterial) };
      case "caller_owned":
        return { ok: true, value: supplied };
      case "none":
        return { ok: true, value: op.params.some((p) => p.role === "idempotency_key") ? supplied : null };
      case "auto": {
        if (supplied !== null) return { ok: true, value: supplied };
        if (purpose !== "call") return { ok: true, value: null };
        const logicalId = await sha256Hex(canonicalJson(args));
        try {
          const existing = await this.#store.get(op.id, logicalId);
          if (typeof existing === "string" && existing !== "") return { ok: true, value: existing };
          const key = crypto.randomUUID();
          await this.#store.put(op.id, logicalId, key);
          return { ok: true, value: key };
        } catch (error) {
          return fail(
            diagnostic(op.id, "TRANSPORT_FAILED", {
              retryable: "never",
              remediation: `The idempotency store failed (${describeError(error)}), so the call was not sent rather than risk a second key for the same intent. Fix or replace ClientOptions.idempotencyStore.`,
            }),
          );
        }
      }
    }
  }

  readonly #tokenSource: TokenSource = async (scheme, config) => {
    const cached = this.#tokenCache.get(scheme.name);
    if (cached && cached.expiresAt > Date.now() + 30_000) return cached.token;
    const inflight = this.#tokenInflight.get(scheme.name);
    if (inflight) return inflight;
    const request = this.#fetchToken(scheme.name, scheme.tokenUrl ?? "", scheme.scopes, config);
    this.#tokenInflight.set(scheme.name, request);
    try {
      return await request;
    } finally {
      this.#tokenInflight.delete(scheme.name);
    }
  };

  async #fetchToken(name: string, tokenUrl: string, scopes: unknown, config: Record<string, string>): Promise<string> {
    const form = new URLSearchParams({ grant_type: "client_credentials" });
    if (Array.isArray(scopes) && scopes.length > 0) form.set("scope", scopes.filter((s) => typeof s === "string").join(" "));
    const basic = btoa(`${encodeURIComponent(config.clientId ?? "")}:${encodeURIComponent(config.clientSecret ?? "")}`);
    const outcome = await attempt(this.#fetch(), {
      url: tokenUrl,
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded", Accept: "application/json", Authorization: `Basic ${basic}` },
      body: form.toString(),
      redirect: "manual",
      timeoutMs: bounded(this.options.timeoutMs, DEFAULT_TIMEOUT_MS, 1),
      signal: undefined,
    });
    if (outcome.kind !== "response" || outcome.body === null) {
      throw new TokenError(`The OAuth2 token endpoint for ${name} could not be reached; check network access and the token URL.`);
    }
    if (outcome.status < 200 || outcome.status > 299) {
      throw new TokenError(`The OAuth2 token endpoint for ${name} answered HTTP ${outcome.status}; check clientId, clientSecret and scopes.`);
    }
    const decoded = decodeBody(outcome.body, outcome.headers, "application/json");
    const token = getPath(decoded.value, "access_token");
    if (typeof token !== "string" || token === "") throw new TokenError(`The OAuth2 token endpoint for ${name} returned no access_token.`);
    const expiresIn = getPath(decoded.value, "expires_in");
    const lifetime = typeof expiresIn === "number" && Number.isFinite(expiresIn) ? expiresIn * 1000 : 3_600_000;
    this.#tokenCache.set(name, { token, expiresAt: Date.now() + lifetime });
    return token;
  }

  // -------------------------------------------------------------- sending

  async #send<T>(prepared: Prepared, opts: CallOptions): Promise<Result<T>> {
    const { op } = prepared;
    const retries = this.#retryOptions(op);
    const mutation = isMutation(op);
    const protectedReplay = hasReplayProtection(op, prepared.key);
    const check = mutation && !protectedReplay ? this.#outcomeCheck(op, prepared.args) : null;
    const timeoutMs = this.#timeout(opts);
    let attempts = 0;
    for (;;) {
      attempts += 1;
      const ctx: RequestContext = {
        operation: op,
        attempt: attempts,
        method: prepared.method,
        url: prepared.displayUrl,
        headers: prepared.headers.toRecord(true),
        body: prepared.observedBody,
      };
      const headers = await this.#beforeRequest(ctx, prepared);
      // Redirects are never followed by fetch: it would forward custom
      // credential headers (API keys, CSRF headers) to another origin. A
      // read follows same-origin redirects here; a mutation follows none.
      let outcome = await attempt(this.#fetch(), {
        url: prepared.url,
        method: prepared.method,
        headers,
        body: prepared.body,
        redirect: "manual",
        timeoutMs,
        signal: opts.signal,
      });
      let current = { url: prepared.url, method: prepared.method, body: prepared.body };
      for (let hops = 0; !mutation && hops < MAX_REDIRECTS && outcome.kind === "response" && REDIRECT_STATUSES.has(outcome.status); hops += 1) {
        const next = this.#redirectTarget(prepared, current.url, outcome.headers.location);
        if (next === null) break;
        const rewrite = outcome.status !== 307 && outcome.status !== 308 && current.method !== "GET" && current.method !== "HEAD";
        current = { url: next.url, method: rewrite ? "GET" : current.method, body: rewrite ? null : current.body };
        ctx.url = next.display;
        ctx.method = current.method;
        const hopHeaders = { ...headers };
        if (rewrite) for (const name of Object.keys(hopHeaders)) if (name.toLowerCase() === "content-type") delete hopHeaders[name];
        outcome = await attempt(this.#fetch(), {
          url: current.url,
          method: current.method,
          headers: hopHeaders,
          body: current.body,
          redirect: "manual",
          timeoutMs,
          signal: opts.signal,
        });
      }
      const callCtx: CallContext = { api: this.api, op, key: prepared.key, keyHeader: prepared.keyHeader, attempts, check };
      const result = await this.#classify<T>(callCtx, prepared, outcome, ctx, timeoutMs);
      if (result.ok) return result;
      const error = scrubDiagnostic(result.error, prepared.secrets);
      const done = (): Failure => (result.partial === undefined ? fail(error) : { ok: false, error, partial: result.partial });
      const cancelled = opts.signal?.aborted === true;
      if (cancelled || attempts > retries.max || !RETRY_CATEGORIES.has(error.category)) return done();
      if (error.retryable === "never" || error.retryable === "after_remediation") return done();
      if (mutation && !protectedReplay) return done();
      let delay: number;
      if (retries.honorRetryAfter && error.retry_after_ms !== null) {
        if (error.retry_after_ms > retries.maxMs) return done();
        delay = error.retry_after_ms;
      } else {
        const ceiling = Math.min(retries.maxMs, retries.baseMs * 2 ** (attempts - 1));
        const random = typeof this.options.random === "function" ? bounded(this.options.random(), 0.5, 0, 1) : Math.random();
        delay = retries.jitter === "none" ? ceiling : retries.jitter === "equal" ? ceiling / 2 + (random * ceiling) / 2 : random * ceiling;
      }
      for (const m of this.#middleware()) {
        try {
          await m.onRetry?.(ctx, error);
        } catch {
          // Observers never change the call.
        }
      }
      if (!(await sleep(delay, opts.signal))) return done();
    }
  }

  /** How to find out whether `op` (a mutation without replay protection)
   * took effect after its answer was lost: its verification hook when it
   * can be called without the lost response, else a registered read of the
   * same resource. Null when neither exists. */
  #outcomeCheck(op: OperationDescriptor, args: Record<string, unknown>): OutcomeCheck | null {
    const sent = sentArguments(op, args);
    const hook = op.agent.verify;
    if (hook && typeof hook.operation === "string" && verifyCallable(hook)) {
      const scope = { args: withWireNames(op, args) };
      const call = `${hook.operation}${withArguments(evaluateExpr(hook.args ?? {}, scope))}`;
      // `expect` is what a successful call leaves behind; references to the
      // call's arguments are known, references to its response are not.
      const lost: string[] = [];
      const resolve = (node: unknown, depth = 0): unknown => {
        if (depth > 32) return null;
        if (typeof node === "string" && node.startsWith("$response")) {
          lost.push(node.slice("$response".length).replace(/^\./, "") || "body");
          return `<${node.slice(1)}>`;
        }
        if (typeof node === "string" && node.startsWith("$")) return resolveRef(node, scope) ?? `<${node.slice(1)}>`;
        if (Array.isArray(node)) return node.map((item) => resolve(item, depth + 1));
        if (isRecord(node)) return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, resolve(v, depth + 1)]));
        return node;
      };
      const expect = isRecord(hook.expect) ? (resolve(hook.expect) as Record<string, unknown>) : {};
      const fields = Object.keys(expect);
      let shows: string;
      if (fields.length === 0) shows = `whether it shows ${changeOf(op)}`;
      else if (lost.length > 0) {
        shows = `whether ${fields.join(" and ")} holds what ${op.id} creates, matching the arguments you sent${sent} (its ${[...new Set(lost)].join(", ")} was in the lost response)`;
      } else shows = `whether ${describePredicate(expect)}`;
      return { call, shows };
    }
    const read = this.#resourceRead(op, args);
    return read ? { call: `${read.id}${withArguments(read.args)}`, shows: `whether it shows ${changeOf(op)}` } : null;
  }

  /** A registered read of the resource `op` changes: a `GET` whose path is
   * the longest prefix of `op`'s path (`/v1/items/{id}/cancel` →
   * `/v1/items/{id}`, then `/v1/items`), never the API root, whose required
   * parameters are all path parameters `op` was called with. Candidates on
   * one path are taken in id order. rpc methods have no resource paths. */
  #resourceRead(op: OperationDescriptor, args: Record<string, unknown>): { id: string; args: Record<string, unknown> } | null {
    if (op.rpc) return null;
    const known = new Map<string, unknown>();
    for (const p of op.params) if (p.in === "path" && args[p.name] !== undefined && args[p.name] !== null) known.set(p.wire, args[p.name]);
    const segments = op.path.split("/");
    const root = (s: string): boolean => s === "" || s === "api" || /^v\d+(?:\.\d+)*$/i.test(s);
    const reads = [...this.#registry.values()]
      .filter((r) => r !== op && r.method === "GET" && r.agent?.safety === "read_only" && !r.rpc && Array.isArray(r.params))
      .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    for (let end = segments.length; end > 0; end -= 1) {
      const prefix = segments.slice(0, end);
      if (prefix.every(root)) break;
      const path = prefix.join("/");
      for (const read of reads) {
        if (read.path !== path) continue;
        const required = argParams(read).filter((p) => p.required);
        if (!required.every((p) => p.in === "path" && known.has(p.wire))) continue;
        const readArgs: Record<string, unknown> = {};
        for (const p of argParams(read)) if (p.in === "path" && known.has(p.wire)) readArgs[p.wire] = known.get(p.wire);
        return { id: read.id, args: readArgs };
      }
    }
    return null;
  }

  /** The URL a read's redirect points to, when it stays on the request's
   * origin (scheme, host and port), with the auth query re-applied; null
   * for any other origin, which is reported instead of followed. */
  #redirectTarget(prepared: Prepared, from: string, location: string | undefined): { url: string; display: string } | null {
    if (typeof location !== "string" || location === "") return null;
    let target: URL;
    let origin: string;
    try {
      origin = new URL(from).origin;
      target = new URL(location, from);
    } catch {
      return null;
    }
    if (target.origin !== origin || target.username !== "" || target.password !== "") return null;
    for (const q of prepared.authQuery) if (!target.searchParams.has(q.name)) target.searchParams.set(q.name, q.value);
    const shown = new URL(target.toString());
    for (const name of prepared.hiddenQuery) if (shown.searchParams.has(name)) shown.searchParams.set(name, REDACTED);
    return { url: target.toString(), display: shown.toString() };
  }

  /** Run `onRequest` with redacted headers; headers a middleware adds or
   * changes (other than redacted ones) are applied to the real request. */
  async #beforeRequest(ctx: RequestContext, prepared: Prepared): Promise<Record<string, string>> {
    const real = prepared.headers.toRecord(false);
    const shown = { ...ctx.headers };
    for (const m of this.#middleware()) {
      try {
        await m.onRequest?.(ctx);
      } catch {
        // Observers never change the call.
      }
    }
    if (!isRecord(ctx.headers)) return real;
    const lower = new Map(Object.keys(real).map((k) => [k.toLowerCase(), k]));
    for (const [name, value] of Object.entries(ctx.headers)) {
      if (typeof value !== "string" || value === REDACTED || shown[name] === value) continue;
      if (!validHeaderName(name) || !validHeaderValue(value)) continue;
      const existing = lower.get(name.toLowerCase());
      if (existing !== undefined && prepared.headers.isSecret(existing)) continue;
      if (existing !== undefined) delete real[existing];
      real[name] = value;
    }
    return real;
  }

  async #afterResponse(ctx: RequestContext, prepared: Prepared, response: Response, status: number, body: Uint8Array | null, headers: Record<string, string>): Promise<void> {
    const middleware = this.#middleware().filter((m) => typeof m.onResponse === "function");
    if (middleware.length === 0) return;
    let copy: Response;
    try {
      let payload: BodyInit | null = null;
      if (body !== null && body.byteLength > 0 && ![101, 204, 205, 304].includes(status)) {
        const decoded = decodeBody(body, headers, null);
        const fields = Array.isArray(prepared.op.agent.sensitiveResponseFields) ? prepared.op.agent.sensitiveResponseFields : [];
        if (decoded.json) payload = scrubText(JSON.stringify(redactPaths(decoded.value, fields)), prepared.secrets);
        else if (typeof decoded.value === "string") payload = scrubText(decoded.value, prepared.secrets);
        else payload = body.slice() as Uint8Array<ArrayBuffer>;
      }
      copy = new Response(payload, { status: status >= 200 && status <= 599 ? status : 200, statusText: response.statusText, headers });
    } catch {
      return;
    }
    for (const m of middleware) {
      try {
        await m.onResponse?.(ctx, copy.clone());
      } catch {
        // Observers never change the call.
      }
    }
  }

  async #classify<T>(callCtx: CallContext, prepared: Prepared, outcome: AttemptOutcome, ctx: RequestContext, timeoutMs: number): Promise<Result<T>> {
    const { op } = prepared;
    const mutation = isMutation(op);
    const attempts = callCtx.attempts;
    switch (outcome.kind) {
      case "not_sent":
        return fail(
          diagnostic(op.id, "TRANSPORT_FAILED", {
            remediation: `The request could not be delivered (${outcome.detail}), so the server did not receive it. Check baseUrl and network access, then call again.`,
            trace: { attempts },
          }),
        );
      case "lost":
        return fail(
          mutation
            ? outcomeUnknown(callCtx, `The connection failed before a response arrived (${outcome.detail}).`)
            : diagnostic(op.id, "TRANSPORT_FAILED", {
                remediation: `The connection failed before a response arrived (${outcome.detail}). This read has no side effects; call again.`,
                trace: { attempts },
              }),
        );
      case "timeout":
        return fail(
          mutation
            ? outcomeUnknown(callCtx, `No response arrived within ${timeoutMs} ms.`)
            : diagnostic(op.id, "UPSTREAM_UNAVAILABLE", {
                remediation: `No response arrived within ${timeoutMs} ms. This read has no side effects; call again later or with a larger timeoutMs.`,
                trace: { attempts },
              }),
        );
      case "aborted":
        return fail(
          mutation && outcome.sent
            ? outcomeUnknown(callCtx, "The caller's AbortSignal cancelled the call after it was sent.")
            : diagnostic(op.id, "TRANSPORT_FAILED", {
                retryable: "never",
                remediation: `The caller's AbortSignal cancelled the call${outcome.sent ? "" : " before it was sent"}.`,
                trace: { attempts },
              }),
        );
      case "response":
        break;
    }
    const { status, headers } = outcome;
    await this.#afterResponse(ctx, prepared, outcome.response, status, outcome.body, headers);
    const requestId = requestIdOf(headers);
    const retryAfter = parseRetryAfter(headers["retry-after"], this.#now());
    const declared = matchResponse(op.responses, status);
    const success = (status >= 200 && status <= 299) || (declared?.kind === "success" && status >= 100 && status <= 399);
    const hint = op.agent.verify ? `Call ${op.agent.verify.operation} to read the current state.` : null;

    if (success && outcome.body === null) {
      if (!mutation) {
        return fail(
          diagnostic(op.id, outcome.bodyFailure === "aborted" ? "TRANSPORT_FAILED" : "UPSTREAM_UNAVAILABLE", {
            http_status: status,
            request_id: requestId,
            retryable: outcome.bodyFailure === "aborted" ? "never" : "after_delay",
            remediation: `The response body was cut off (${outcome.bodyFailure ?? "broken"}). This read has no side effects; call again.`,
            trace: { attempts },
          }),
        );
      }
      return fail(
        diagnostic(op.id, "UNEXPECTED_RESPONSE", {
          http_status: status,
          request_id: requestId,
          retryable: "never",
          remediation: `The server accepted ${op.id} (HTTP ${status}) but the response body was cut off (${outcome.bodyFailure ?? "broken"}). The call took effect; do not repeat it.`,
          next_action: hint,
          trace: { attempts },
        }),
      );
    }
    const bytes = outcome.body ?? new Uint8Array(0);
    if (!success) {
      const isRedirect = status === 0 || (status >= 300 && status <= 399) || outcome.response.type === "opaqueredirect";
      if (isRedirect) {
        const to = headers.location ? ` to ${headers.location.slice(0, 200)}` : "";
        return fail(
          diagnostic(op.id, "UNEXPECTED_RESPONSE", {
            http_status: status || null,
            request_id: requestId,
            remediation: mutation
              ? `The server answered with a redirect${to}. Redirects are never followed on mutations; check baseUrl (scheme, host, trailing slash). Whether the call took effect is unknown only if the server applied it before redirecting.`
              : `The server answered with a redirect${to} that leaves the API's origin (or too many redirects). It is not followed, so credentials never leave the API's host; check baseUrl.`,
            next_action: mutation ? hint : null,
            trace: { attempts },
          }),
        );
      }
      const decoded = decodeBody(bytes, headers, declared?.mediaType ?? null);
      return fail(classifyError(callCtx, status, headers, decoded, retryAfter));
    }

    const decoded = decodeBody(bytes, headers, declared?.mediaType ?? null);
    const mode = this.options.validateResponses === "off" || this.options.validateResponses === "strict" ? this.options.validateResponses : "warn";
    const sensitive = Array.isArray(op.agent.sensitiveResponseFields) ? op.agent.sensitiveResponseFields : [];
    const afterEffect = mutation ? " The call took effect; do not repeat it." : "";
    let problem: Diagnostic | null = null;
    if (decoded.invalidJson) {
      problem = diagnostic(op.id, "UNEXPECTED_RESPONSE", {
        http_status: status,
        request_id: requestId,
        failed_parameter: "response",
        expected: "a JSON body",
        remediation: `The success response announced JSON but did not parse.${afterEffect}`,
        trace: { attempts },
      });
    } else if (mode !== "off" && isSchema(op.response) && !decoded.empty) {
      let parsed: ReturnType<SchemaLike["safeParse"]>;
      try {
        parsed = op.response.safeParse(decoded.value);
      } catch (error) {
        parsed = { success: false, error: { issues: [{ path: [], message: `the response schema failed (${describeError(error)})` }] } };
      }
      if (!parsed.success) {
        const issues = isRecord(parsed.error) && Array.isArray(parsed.error.issues) ? parsed.error.issues : [];
        const issue = issues[0];
        const path = issue && Array.isArray(issue.path) ? issue.path.map(String) : [];
        const message = issue && typeof issue.message === "string" ? issue.message : "a valid response";
        const pathText = path.map((s) => (/^\d+$/.test(s) ? `[${s}]` : `.${s}`)).join("");
        const kept = mutation && mode === "strict" ? " The decoded body is in the result's partial; store any value shown only once from it before anything else." : "";
        problem = diagnostic(op.id, "UNEXPECTED_RESPONSE", {
          http_status: status,
          request_id: requestId,
          failed_parameter: `response${pathText}`,
          received_value: envelopeValue(getPath(redactPaths(decoded.value, sensitive), path), path.some(looksSensitive)),
          expected: message,
          remediation: `The success response does not match the API description at response${pathText} (${message}).${afterEffect}${kept}`,
          trace: { attempts },
        });
      }
    }
    if (problem) {
      const scrubbed = scrubDiagnostic(problem, prepared.secrets);
      // The effect of a mutation happened: its body is never dropped, since
      // it can hold the only copy of a value (a one-time signing secret).
      if (mode === "strict") return mutation && !decoded.invalidJson ? { ok: false, error: scrubbed, partial: decoded.value } : fail(scrubbed);
      this.#emit(scrubbed);
    }
    const meta: ResponseMeta = { status, headers, requestId, attempts };
    return { ok: true, value: decoded.value as T, meta };
  }

  // -------------------------------------------------------------- preview

  async #preview(op: OperationDescriptor, args: Record<string, unknown>, opts: CallOptions): Promise<Result<PreviewResult>> {
    const prepared = await this.#prepare(op, args, opts, "preview", null);
    if (!prepared.ok) return prepared;
    const p = prepared.value;
    const effects: string[] = [];
    const confirmation = op.agent.confirmation;
    if (confirmation && typeof confirmation.message === "string" && confirmation.message !== "") effects.push(interpolate(confirmation.message, args, op));
    if (typeof op.agent.remediationNote === "string" && op.agent.remediationNote !== "") effects.push(op.agent.remediationNote);
    if (confirmation && Array.isArray(confirmation.summaryFields) && confirmation.summaryFields.length > 0) {
      const fields = confirmation.summaryFields.filter((f) => typeof f === "string").map((f) => `${f}={${f}}`);
      effects.push(interpolate(`Arguments: ${fields.join(", ")}.`, args, op));
    }
    if (typeof op.summary === "string" && op.summary !== "") effects.push(op.summary);
    const readOnly = op.agent.safety === "read_only";
    const token = readOnly ? null : await issueToken(this.#confirmationKey, op.id, args, this.#now());
    const request: RenderedRequest = { method: p.method, url: p.displayUrl, headers: p.headers.toRecord(true), body: p.displayBody };
    const result: PreviewResult = {
      operation: op.id,
      safety: op.agent.safety,
      request,
      effects,
      confirmation_token: token,
      expires_in_ms: token === null ? null : CONFIRMATION_TTL_MS,
    };
    const meta: ResponseMeta = { status: 0, headers: {}, requestId: null, attempts: 0 };
    const mode = op.agent.preview;
    if (mode.mode === "header") {
      const dry = await this.#prepare(op, args, opts, "server_preview", null);
      if (!dry.ok) return dry;
      const answer = await this.#send<unknown>(dry.value, opts);
      if (!answer.ok) return answer;
      return { ok: true, value: { ...result, server_preview: redactPaths(answer.value, op.agent.sensitiveResponseFields ?? []) }, meta: answer.meta };
    }
    if (mode.mode === "endpoint") {
      const target = this.#registry.get(mode.operation);
      if (!target) {
        return fail(
          diagnostic(op.id, "VALIDATION_FAILED", {
            failed_parameter: "operation",
            expected: `the preview operation ${mode.operation} registered with the client`,
            remediation: `Register ${mode.operation} (ClientCore.register or ClientOptions.operations) so preview() can call it; nothing was sent.`,
          }),
        );
      }
      const { confirm: _confirm, verify: _verify, ...rest } = opts;
      const answer = await this.#call<unknown>(target, args, rest, null);
      if (!answer.ok) return answer;
      return { ok: true, value: { ...result, server_preview: answer.value }, meta: answer.meta };
    }
    return { ok: true, value: result, meta };
  }

  // ----------------------------------------------------- pages and polling

  #argName(op: OperationDescriptor, key: string): string {
    const params = Array.isArray(op.params) ? op.params : [];
    return (params.find((p) => p.name === key) ?? params.find((p) => p.wire === key))?.name ?? key;
  }

  async *#pages<T>(op: OperationDescriptor, args: Record<string, unknown>, opts: CallOptions): AsyncGenerator<Result<Page<T>>> {
    const pagination = isRecord(op) && isRecord(op.pagination) ? op.pagination : null;
    let current: Record<string, unknown> = isRecord(args) ? { ...args } : args;
    let override: string | null = null;
    let previousCursor: unknown = undefined;
    for (;;) {
      const result = await this.#call<unknown>(op, current, opts, override);
      if (!result.ok) {
        yield result;
        return;
      }
      const body = result.value;
      const itemsField = pagination ? pagination.itemsField : "";
      const found = itemsField ? getPath(body, itemsField) : body;
      const items = (Array.isArray(found) ? found : []) as T[];
      let next: unknown = null;
      if (pagination) {
        switch (pagination.style) {
          case "cursor": {
            const cursor = getPath(body, pagination.responseField);
            next = cursor === undefined || cursor === null || cursor === "" || canonicalJson(cursor) === canonicalJson(previousCursor) ? null : cursor;
            if (next !== null) {
              previousCursor = next;
              current = { ...current, [this.#argName(op, pagination.requestParam)]: next };
            }
            break;
          }
          case "offset": {
            const offsetKey = this.#argName(op, pagination.offsetParam);
            const limit = current[this.#argName(op, pagination.limitParam)];
            const offset = bounded(current[offsetKey], 0, 0);
            const short = typeof limit === "number" && items.length < limit;
            next = items.length === 0 || short ? null : offset + items.length;
            if (next !== null) current = { ...current, [offsetKey]: next };
            break;
          }
          case "page": {
            const pageKey = this.#argName(op, pagination.pageParam);
            const size = current[this.#argName(op, pagination.sizeParam)];
            const page = bounded(current[pageKey], 1, 0);
            const short = typeof size === "number" && items.length < size;
            next = items.length === 0 || short ? null : page + 1;
            if (next !== null) current = { ...current, [pageKey]: next };
            break;
          }
          case "link_header": {
            const link = nextLink(result.meta.headers.link);
            if (link !== null) {
              const base = override ?? (typeof this.options.baseUrl === "string" ? this.options.baseUrl : (this.api.servers?.[0] ?? ""));
              let resolved: URL;
              try {
                resolved = new URL(link, base);
              } catch {
                resolved = new URL("invalid:");
              }
              const origin = (() => {
                try {
                  return new URL(base).origin;
                } catch {
                  return null;
                }
              })();
              if (resolved.origin !== origin || origin === null) {
                yield { ok: true, value: { items, body, next: null }, meta: result.meta };
                yield fail(
                  diagnostic(op.id, "UNEXPECTED_RESPONSE", {
                    http_status: result.meta.status,
                    request_id: result.meta.requestId,
                    remediation: "The next-page Link header points to another origin; it is not followed so credentials never leave the API's host. Stop paginating here.",
                    trace: { attempts: result.meta.attempts },
                  }),
                );
                return;
              }
              next = resolved.toString();
              override = next as string;
            }
            break;
          }
        }
      }
      yield { ok: true, value: { items, body, next }, meta: result.meta };
      if (next === null) return;
    }
  }

  async #poll<T>(
    op: OperationDescriptor,
    args: Record<string, unknown>,
    until: Predicate,
    intervalMs: number,
    budgetMs: number,
    opts: CallOptions,
  ): Promise<Result<T> & { timedOut?: boolean }> {
    const interval = bounded(intervalMs, DEFAULT_POLL_INTERVAL_MS, 0);
    const budget = bounded(budgetMs, 0, 0);
    const started = Date.now();
    for (;;) {
      const result = await this.#call<T>(op, args, { ...opts, verify: false }, null);
      if (!result.ok) return result;
      if (evaluatePredicate(until, result.value)) return { ...result, timedOut: false };
      if (Date.now() - started + interval > budget) return { ...result, timedOut: true };
      if (!(await sleep(interval, opts.signal))) return { ...result, timedOut: true };
    }
  }

  async #verify(op: OperationDescriptor, args: Record<string, unknown>, value: unknown, opts: CallOptions): Promise<Verification> {
    const hook = op.agent.verify;
    const unchecked: Verification = { checked: false, passed: false, observed: null };
    if (!hook || typeof hook.operation !== "string") return unchecked;
    const target = this.#registry.get(hook.operation);
    if (!target) return unchecked;
    // The hook is written against the API: argument keys and `$args`
    // references use wire names, and predicates may reference the call.
    const scope = { response: value, args: withWireNames(op, args) };
    const evaluated = evaluateExpr(hook.args ?? {}, scope);
    if (!isRecord(evaluated)) return unchecked;
    const mapped: Record<string, unknown> = {};
    for (const [key, arg] of Object.entries(evaluated)) mapped[this.#argName(target, key)] = arg;
    // A reference that does not resolve never matches (it is not dropped,
    // which would make the predicate hold vacuously).
    const refs = (node: unknown, depth = 0): unknown => {
      if (depth > 64) return UNRESOLVED;
      if (typeof node === "string" && node.startsWith("$")) return resolveRef(node, scope) ?? UNRESOLVED;
      if (Array.isArray(node)) return node.map((item) => refs(item, depth + 1));
      if (isRecord(node)) return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, refs(v, depth + 1)]));
      return node;
    };
    const resolve = (predicate: unknown): Record<string, unknown> | null =>
      isRecord(predicate) && Object.keys(predicate).length > 0 ? (refs(predicate) as Record<string, unknown>) : null;
    const terminal = resolve(hook.terminal);
    const expect = resolve(hook.expect) ?? {};
    const interval = bounded(hook.pollIntervalMs, DEFAULT_POLL_INTERVAL_MS, 0);
    const budget = bounded(hook.pollBudgetMs, terminal ? DEFAULT_VERIFY_BUDGET_MS : 0, 0);
    const { confirm: _confirm, idempotencyKey: _key, ...rest } = opts;
    const result = await this.#poll<unknown>(target, mapped, terminal ?? expect, interval, budget, { ...rest, verify: false });
    if (!result.ok) return { ...unchecked, error: result.error };
    const observed = redactPaths(result.value, Array.isArray(target.agent?.sensitiveResponseFields) ? target.agent.sensitiveResponseFields : []);
    const verification: Verification = { checked: true, passed: evaluatePredicate(expect, result.value), observed };
    if (budget > 0) verification.timedOut = result.timedOut === true;
    return verification;
  }

  // --------------------------------------------------------------- macros

  /** The macro's input with the `add` defaults applied, or a failure. */
  #macroInput(macro: MacroDescriptor, input: unknown, name: string): Step<Record<string, unknown>> {
    if (!isRecord(input)) {
      return fail(diagnostic(name, "VALIDATION_FAILED", { failed_parameter: "input", expected: "an object", remediation: `Pass the input of ${name} as one object.` }));
    }
    const effective: Record<string, unknown> = { ...input };
    const add = isRecord(macro.input) && isRecord(macro.input.add) ? macro.input.add : {};
    for (const [key, schema] of Object.entries(add)) {
      if (effective[key] === undefined && isRecord(schema) && schema.default !== undefined) effective[key] = schema.default;
    }
    return { ok: true, value: effective };
  }

  /** The macro's steps resolved against the registry, and its effective
   * tier: the strictest of the declared one and every step's. */
  #macroPlan(macro: MacroDescriptor, name: string): Step<{ steps: Array<{ step: MacroStep; op: OperationDescriptor }>; safety: string }> {
    const invalid = (remediation: string): Failure =>
      fail(diagnostic(name, "VALIDATION_FAILED", { failed_parameter: "macro", expected: "a valid macro descriptor", remediation }));
    if (!isRecord(macro) || !Array.isArray(macro.steps)) return invalid("The macro descriptor has no steps; regenerate the SDK.");
    const steps: Array<{ step: MacroStep; op: OperationDescriptor }> = [];
    let safety = typeof macro.safety === "string" && SAFETIES.has(macro.safety) ? macro.safety : "irreversible";
    for (const [index, step] of (macro.steps.filter(isRecord) as MacroStep[]).entries()) {
      const op = typeof step.operation === "string" ? this.#registry.get(step.operation) : undefined;
      if (!op) return invalid(`Step ${index + 1} of ${name} names ${String(step.operation)}, which is not registered with the client.`);
      const tier = op.agent?.safety;
      if (typeof tier === "string" && (SAFETY_RANK[tier] ?? 3) > (SAFETY_RANK[safety] ?? 3)) safety = tier;
      steps.push({ step, op });
    }
    return { ok: true, value: { steps, safety } };
  }

  /** Step arguments evaluated against `scope` (a dry evaluation when
   * `pending` names results not produced yet); for the step whose args the
   * input extends, the fields the macro adds are removed. */
  #stepArgs(macro: MacroDescriptor, step: MacroStep, op: OperationDescriptor, scope: Scope, pending?: ReadonlySet<string>): unknown {
    let args = pending ? evaluateDry(step.args ?? {}, scope, pending) : evaluateExpr(step.args ?? {}, scope);
    if (args === undefined || args === null) args = {};
    const extendsOp = isRecord(macro.input) && typeof macro.input.extends === "string" ? macro.input.extends : null;
    const added = isRecord(macro.input) && isRecord(macro.input.add) ? Object.keys(macro.input.add) : [];
    if (isRecord(args) && op.id === extendsOp && added.length > 0) {
      const trimmed = { ...args };
      for (const key of added) delete trimmed[key];
      args = trimmed;
    }
    return args;
  }

  async #previewMacro(macro: MacroDescriptor, input: Record<string, unknown>, opts: CallOptions): Promise<Result<PreviewResult>> {
    const name = safeName(macro, "name", "<unknown macro>");
    const plan = this.#macroPlan(macro, name);
    if (!plan.ok) return plan;
    const effective = this.#macroInput(macro, input, name);
    if (!effective.ok) return effective;
    const { steps, safety } = plan.value;
    if (steps.length === 0) {
      return fail(diagnostic(name, "VALIDATION_FAILED", { failed_parameter: "macro", expected: "a valid macro descriptor", remediation: `${name} has no steps; regenerate the SDK.` }));
    }
    const { confirm: _confirm, verify: _verify, ...rest } = opts;
    // A dry evaluation: the input is known, earlier steps' results are
    // placeholders (`<from step NAME: path>`).
    const scope: Record<string, unknown> = { input: effective.value };
    const pending = new Set<string>();
    const previews: MacroStepPreview[] = [];
    const effects: string[] = [];
    if (typeof macro.summary === "string" && macro.summary !== "") effects.push(macro.summary);
    let keyUsed = false;
    for (const [index, { step, op }] of steps.entries()) {
      const kind = step.kind === "poll" || step.kind === "paginate" ? step.kind : "call";
      const where = `step ${index + 1} of ${name}: ${op.id}`;
      const args = this.#stepArgs(macro, step, op, scope, pending);
      const stepOpts: CallOptions = { ...rest, verify: false };
      const usesKey = op.agent?.idempotency?.policy === "caller_owned" || op.agent?.idempotency?.policy === "auto";
      if (!usesKey || keyUsed) delete stepOpts.idempotencyKey;
      else if (opts.idempotencyKey !== undefined) keyUsed = true;
      let request: RenderedRequest | null = null;
      if (isRecord(args)) {
        const encoding = op.body?.encoding;
        // Bytes and multipart bodies cannot be encoded from a value not known yet.
        const unrenderable = (encoding === "bytes" || encoding === "multipart") && containsPlaceholder(args);
        if (!unrenderable) {
          const prepared = await this.#prepare(op, args, stepOpts, "macro_preview", null);
          if (!prepared.ok) return fail({ ...prepared.error, operation: name, remediation: `${prepared.error.remediation} (${where})` });
          const p = prepared.value;
          request = { method: p.method, url: unescapePlaceholders(p.displayUrl, args), headers: p.headers.toRecord(true), body: p.displayBody };
        }
      } else if (!containsPlaceholder(args)) {
        return fail(diagnostic(name, "VALIDATION_FAILED", { failed_parameter: "input", expected: "an object", remediation: `Step ${index + 1} of ${name} does not evaluate to an argument object.` }));
      }
      const own: string[] = [];
      const confirmation = op.agent.confirmation;
      if (confirmation && typeof confirmation.message === "string" && confirmation.message !== "") {
        own.push(interpolate(confirmation.message, isRecord(args) ? args : {}, op));
      }
      if (kind === "poll") {
        const until = describePredicate(step.until);
        const budget = bounded(evaluateExpr(step.budget_ms, scope), DEFAULT_MACRO_BUDGET_MS, 0);
        const interval = bounded(step.interval_ms, DEFAULT_POLL_INTERVAL_MS, 0);
        own.push(`Repeats ${op.id} every ${interval} ms${until ? ` until ${until}` : ""}, for at most ${budget} ms.`);
      } else if (kind === "paginate") {
        own.push(`Reads up to ${Math.floor(bounded(step.max_pages, MACRO_PAGE_LIMIT, 1, 10_000))} pages of ${op.id}.`);
      }
      if (typeof op.agent.remediationNote === "string" && op.agent.remediationNote !== "") own.push(op.agent.remediationNote);
      const as = typeof step.as === "string" && step.as !== "" ? step.as : null;
      previews.push({ step: index + 1, kind, operation: op.id, as, safety: op.agent.safety, request, effects: own });
      effects.push(`Step ${index + 1}: ${kind} ${op.id} (${op.agent.safety}).`, ...own);
      if (as !== null) pending.add(as);
    }
    if (macro.shownOnce === true) effects.push("The result contains values the API shows only once; store them immediately.");
    // Step 1 has no earlier results, so it is always rendered.
    const first = previews[0]?.request;
    if (!first) return fail(diagnostic(name, "VALIDATION_FAILED", { failed_parameter: "input", expected: "an object", remediation: `Step 1 of ${name} cannot be rendered.` }));
    const token = safety === "read_only" ? null : await issueToken(this.#confirmationKey, `macro:${name}`, effective.value, this.#now());
    const value: PreviewResult = {
      operation: name,
      safety: safety as PreviewResult["safety"],
      request: first,
      effects,
      confirmation_token: token,
      expires_in_ms: token === null ? null : CONFIRMATION_TTL_MS,
      steps: previews,
    };
    return { ok: true, value, meta: { status: 0, headers: {}, requestId: null, attempts: 0 } };
  }

  /** Whether running this step again (by rerunning its macro with the
   * same input and options) answers the first result instead of applying
   * the effect twice: an identity or content-hash body, a caller's key, or
   * an automatic key (the store maps identical arguments to one key). */
  #rerunProtected(op: OperationDescriptor, opts: CallOptions): boolean {
    const policy = op.agent?.idempotency?.policy;
    if (policy === "content_identity" || policy === "content_hash" || policy === "auto") return true;
    return typeof opts.idempotencyKey === "string" && (policy === "caller_owned" || op.params.some((p) => p.role === "idempotency_key"));
  }

  /** The replay protection a macro's confirmation token is bound to: the
   * run's idempotency key when a step sends it and every mutating step is
   * protected against a rerun; otherwise none, so the token is used once
   * (a key no step sends protects nothing). */
  #macroBind(ops: OperationDescriptor[], opts: CallOptions): string | null {
    if (typeof opts.idempotencyKey !== "string") return null;
    let sent = false;
    for (const op of ops) {
      const usesKey = op.agent?.idempotency?.policy === "caller_owned" || op.agent?.idempotency?.policy === "auto";
      const stepOpts: CallOptions = { ...opts };
      if (!usesKey || sent) delete stepOpts.idempotencyKey;
      else sent = true;
      if (isMutation(op) && !this.#rerunProtected(op, stepOpts)) return null;
    }
    return sent ? opts.idempotencyKey : null;
  }

  async #runMacro<T>(macro: MacroDescriptor, input: Record<string, unknown>, opts: CallOptions): Promise<Result<T>> {
    const name = safeName(macro, "name", "<unknown macro>");
    const plan = this.#macroPlan(macro, name);
    if (!plan.ok) return plan;
    const effective = this.#macroInput(macro, input, name);
    if (!effective.ok) return effective;
    const { steps, safety } = plan.value;
    const unconfirmed = await this.#confirmed(name, `macro:${name}`, safety, effective.value, opts.confirm, `the macro's preview(...)`, opts.allowConfirmTrue !== false);
    if (unconfirmed) return unconfirmed;
    // The token is spent by the first step that sends (as an operation's
    // token is): a step failing pre-flight, before anything was sent,
    // leaves it valid for the corrected run.
    const token = typeof opts.confirm === "string" ? opts.confirm : null;
    const bind = this.#macroBind(
      steps.map(({ op }) => op),
      opts,
    );
    let claimed = false;
    const claim = (): Failure | null => {
      if (claimed || token === null) return null;
      claimed = true;
      return this.#claimToken(name, token, bind, "the macro's preview(...)");
    };
    const scope: Record<string, unknown> = { input: effective.value };
    const completed: string[] = [];
    /** Results of completed steps by `as` name: returned as `partial` when a later step fails. */
    const produced: Record<string, unknown> = {};
    /** Completed mutating steps that a rerun of the macro would apply again. */
    const unprotected: string[] = [];
    let keyUsed = false;
    let meta: ResponseMeta = { status: 0, headers: {}, requestId: null, attempts: 0 };
    for (const [index, { step, op }] of steps.entries()) {
      const args = this.#stepArgs(macro, step, op, scope as Scope);
      if (!isRecord(args)) {
        return fail(
          diagnostic(name, "VALIDATION_FAILED", {
            failed_parameter: "input",
            expected: "an object",
            remediation: `Step ${index + 1} of ${name} does not evaluate to an argument object.`,
          }),
        );
      }
      const usesKey = op.agent?.idempotency?.policy === "caller_owned" || op.agent?.idempotency?.policy === "auto";
      const { confirm: _confirm, ...rest } = opts;
      const stepOpts: StepOptions = { ...rest, verify: false, [MACRO_CONFIRMED]: true, [MACRO_CLAIM]: claim };
      if (!usesKey || keyUsed) delete stepOpts.idempotencyKey;
      else if (opts.idempotencyKey !== undefined) keyUsed = true;
      let value: unknown;
      let failure: Diagnostic | null = null;
      let failedPartial: unknown;
      if (step.kind === "poll") {
        const budget = evaluateExpr(step.budget_ms, scope as Scope);
        const result = await this.#poll<unknown>(
          op,
          args,
          isRecord(step.until) ? step.until : {},
          bounded(step.interval_ms, DEFAULT_POLL_INTERVAL_MS, 0),
          bounded(budget, DEFAULT_MACRO_BUDGET_MS, 0),
          stepOpts,
        );
        if (result.ok) {
          value = result.timedOut === true ? null : result.value;
          meta = result.meta;
        } else failure = result.error;
      } else if (step.kind === "paginate") {
        const items: unknown[] = [];
        const limit = Math.floor(bounded(step.max_pages, MACRO_PAGE_LIMIT, 1, 10_000));
        let pages = 0;
        for await (const page of this.#pages<unknown>(op, args, stepOpts)) {
          if (!page.ok) {
            failure = page.error;
            break;
          }
          items.push(...page.value.items);
          meta = page.meta;
          pages += 1;
          if (pages >= limit) break;
        }
        value = items;
      } else {
        const result = await this.#call<unknown>(op, args, stepOpts, null);
        if (result.ok) {
          value = result.value;
          meta = result.meta;
        } else {
          failure = result.error;
          failedPartial = result.partial;
        }
      }
      if (failure) {
        if (failedPartial !== undefined && typeof step.as === "string" && step.as !== "") produced[step.as] = failedPartial;
        const done = completed.length > 0 ? `completed before it: ${completed.join(", ")}` : "no step completed before it";
        let remediation = `${failure.remediation} Macro ${name} stopped at step ${index + 1} of ${steps.length} (${op.id}); ${done}.`;
        let retryable = failure.retryable;
        let nextAction = failure.next_action;
        const kept = Object.keys(produced);
        if (kept.length > 0) {
          remediation += ` The result's partial holds the completed steps' results (${kept.join(", ")})${macro.shownOnce === true ? ", including values the API shows only once: store them now" : ""}.`;
        }
        if (unprotected.length > 0) {
          // Rerunning the macro would apply these steps again (a second
          // endpoint, a lost one-time secret): finish from here instead.
          remediation += ` Do not run ${name} again: ${unprotected.join(", ")} already took effect and has no replay protection.`;
          if (retryable === "after_delay" || retryable === "same_key_only") retryable = "after_remediation";
          const finish = `Finish ${name} without rerunning it: call ${op.id} yourself with the values from the result's partial.`;
          nextAction = nextAction === null ? finish : `${nextAction} ${finish}`;
        }
        // The envelope names what was called (the macro); the remediation
        // names the step that failed.
        const error: Diagnostic = { ...failure, operation: name, remediation, retryable, next_action: nextAction };
        return kept.length > 0 ? { ok: false, error, partial: { ...produced } } : fail(error);
      }
      completed.push(op.id);
      if (typeof step.as === "string" && step.as !== "") {
        scope[step.as] = value;
        produced[step.as] = value;
      }
      if (isMutation(op) && !this.#rerunProtected(op, stepOpts)) unprotected.push(op.id);
    }
    return { ok: true, value: evaluateExpr(macro.output, scope as Scope) as T, meta };
  }
}
