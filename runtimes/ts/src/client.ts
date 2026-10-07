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
  classifyError,
  decodeBody,
  isMutation,
  matchResponse,
  outcomeUnknown,
  requestIdOf,
} from "./classify.js";
import { checkToken, CONFIRMATION_TTL_MS, issueToken } from "./confirm.js";
import { diagnostic, scrubDiagnostic, scrubText } from "./envelope.js";
import { evaluateExpr, evaluatePredicate, type Scope } from "./expr.js";
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
} from "./util.js";
import { RUNTIME_VERSION } from "./version.js";

type Failure = { ok: false; error: Diagnostic };
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

/** Why a request is being prepared. */
type Purpose = "call" | "preview" | "server_preview";

/** Marks the call options of a macro step whose run was confirmed at the
 * macro level. Module-private, so callers cannot set it. */
const MACRO_CONFIRMED: unique symbol = Symbol("tungsten.macroConfirmed");
type StepOptions = CallOptions & { [MACRO_CONFIRMED]?: true };

const SAFETY_RANK: Readonly<Record<string, number>> = { read_only: 0, mutating: 1, destructive: 2, irreversible: 3 };

/** Placeholder for macro step arguments only known after earlier steps. */
const LATER = "<from an earlier step>";

/** A request ready to send, with its redacted rendering. */
interface Prepared {
  op: OperationDescriptor;
  args: Record<string, unknown>;
  method: HttpMethod;
  url: string;
  displayUrl: string;
  headers: HeaderBag;
  body: BodyInit | null;
  displayBody: unknown;
  key: string | null;
  keyHeader: string;
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

function sensitiveArg(op: OperationDescriptor, path: ReadonlyArray<string | number>): boolean {
  const head = path[0];
  if (op.params.some((p) => p.sensitive === true && p.name === head)) return true;
  return path.some((segment) => typeof segment === "string" && looksSensitive(segment));
}

function interpolate(template: string, args: Record<string, unknown>, op: OperationDescriptor): string {
  return template.replace(/\{([^{}]+)\}/g, (_m, field: string) => {
    const value = getPath(args, field.trim());
    if (value === undefined || value === null) return "<unset>";
    if (sensitiveArg(op, field.trim().split("."))) return REDACTED;
    const shown = envelopeValue(value, false);
    return typeof shown === "string" ? shown : JSON.stringify(shown);
  });
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
        received_value: envelopeValue(value, sensitiveArg(op, path)),
        expected,
        remediation,
      }),
    );
  }

  #validateArgs(op: OperationDescriptor, args: Record<string, unknown>): Failure | null {
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
        const issue = issues[0];
        const path = issue && Array.isArray(issue.path) ? issue.path.filter((s) => typeof s === "string" || typeof s === "number") : [];
        const message = issue && typeof issue.message === "string" ? issue.message : "a valid value";
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
    return this.#confirmed(op.id, op.id, op.agent.safety, args, opts.confirm, "preview(...)");
  }

  /** The confirmation rule of planning/04 for one tier: `destructive`
   * accepts `true` or a token, `irreversible` only a token issued for
   * `subject` (an operation id, or `macro:<name>`) and these exact args. */
  async #confirmed(id: string, subject: string, safety: string, args: unknown, confirm: unknown, previewCall: string): Promise<Failure | null> {
    if (safety !== "destructive" && safety !== "irreversible") return null;
    const how = `call ${previewCall} with the same arguments and pass its confirmation_token as confirm`;
    const required = (remediation: string): Failure =>
      fail(diagnostic(id, "CONFIRMATION_REQUIRED", { failed_parameter: "confirm", expected: "a confirmation_token from preview()", remediation }));
    if (confirm === true) {
      if (safety === "destructive") return null;
      return required(`${id} is irreversible, so confirm: true is not accepted: ${how}.`);
    }
    if (typeof confirm !== "string" || confirm === "") {
      return required(`${id} is ${safety}: ${how}${safety === "destructive" ? " (or pass confirm: true)" : ""}.`);
    }
    const check = await checkToken(this.#confirmationKey, confirm, subject, args, this.#now());
    if (check === "valid") return null;
    if (check === "expired") return required(`The confirmation token expired (tokens last ${CONFIRMATION_TTL_MS / 60000} minutes): ${how}.`);
    return required(`The confirmation token was not issued by this client for ${id} with exactly these arguments: ${how}.`);
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
    const args = rawArgs;
    try {
      canonicalJson(args);
    } catch {
      return this.#validation(op, [], "<cyclic value>", "a JSON-like value without cycles", "Pass arguments without circular references.");
    }
    const invalid = this.#validateArgs(op, args) ?? this.#checkKey(op, opts, purpose);
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

    const url = this.#buildUrl(op, args, auth.plan, urlOverride);
    if (!url.ok) return url;

    const headers = new HeaderBag();
    this.#buildHeaders(op, args, opts, auth.plan, headers);

    let encoded;
    try {
      const value = isSafeMethod(method) && method !== "OPTIONS" ? undefined : bodyValue(op, args, new Set(argParams(op).map((p) => p.name)));
      encoded = await encodeBody(op.body ?? null, value, (display) => redactSensitiveKeys(display));
    } catch (error) {
      if (error instanceof SerializationError) {
        return this.#validation(op, error.parameter === "body" && op.body?.shape.kind === "arg" ? [op.body.shape.arg] : [], error.value, error.expected, `Pass ${error.parameter} as ${error.expected}.`);
      }
      throw error;
    }
    if (encoded.contentType) headers.set("Content-Type", encoded.contentType);

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
        displayBody: encoded.display,
        key: key.value,
        keyHeader: header,
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
        body: prepared.body,
      };
      const headers = await this.#beforeRequest(ctx, prepared);
      const outcome = await attempt(this.#fetch(), {
        url: prepared.url,
        method: prepared.method,
        headers,
        body: prepared.body,
        redirect: mutation ? "manual" : "follow",
        timeoutMs,
        signal: opts.signal,
      });
      const callCtx: CallContext = { api: this.api, op, key: prepared.key, keyHeader: prepared.keyHeader, attempts };
      const result = await this.#classify<T>(callCtx, prepared, outcome, ctx, timeoutMs);
      if (result.ok) return result;
      const error = scrubDiagnostic(result.error, prepared.secrets);
      const cancelled = opts.signal?.aborted === true;
      if (cancelled || attempts > retries.max || !RETRY_CATEGORIES.has(error.category)) return fail(error);
      if (error.retryable === "never" || error.retryable === "after_remediation") return fail(error);
      if (mutation && !protectedReplay) return fail(error);
      let delay: number;
      if (retries.honorRetryAfter && error.retry_after_ms !== null) {
        if (error.retry_after_ms > retries.maxMs) return fail(error);
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
      if (!(await sleep(delay, opts.signal))) return fail(error);
    }
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
        return fail(
          diagnostic(op.id, "UNEXPECTED_RESPONSE", {
            http_status: status || null,
            request_id: requestId,
            remediation: `The server answered with a redirect${headers.location ? ` to ${headers.location.slice(0, 200)}` : ""}. Redirects are never followed on mutations; check baseUrl (scheme, host, trailing slash). Whether the call took effect is unknown only if the server applied it before redirecting.`,
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
        problem = diagnostic(op.id, "UNEXPECTED_RESPONSE", {
          http_status: status,
          request_id: requestId,
          failed_parameter: `response${pathText}`,
          received_value: envelopeValue(getPath(redactPaths(decoded.value, sensitive), path), path.some(looksSensitive)),
          expected: message,
          remediation: `The success response does not match the API description at response${pathText} (${message}).${afterEffect}`,
          trace: { attempts },
        });
      }
    }
    if (problem) {
      const scrubbed = scrubDiagnostic(problem, prepared.secrets);
      if (mode === "strict") return fail(scrubbed);
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
    const mapped = evaluateExpr(hook.args ?? {}, { response: value, args });
    if (!isRecord(mapped)) return unchecked;
    const terminal = isRecord(hook.terminal) && Object.keys(hook.terminal).length > 0 ? hook.terminal : null;
    const expect = isRecord(hook.expect) ? hook.expect : {};
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

  /** Step arguments evaluated against `scope`; for the step whose args the
   * input extends, the fields the macro adds are removed. */
  #stepArgs(macro: MacroDescriptor, step: MacroStep, op: OperationDescriptor, scope: Scope): unknown {
    let args = evaluateExpr(step.args ?? {}, scope);
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
    const first = steps[0];
    if (!first) return fail(diagnostic(name, "VALIDATION_FAILED", { failed_parameter: "macro", expected: "a valid macro descriptor", remediation: `${name} has no steps; regenerate the SDK.` }));
    const firstArgs = this.#stepArgs(macro, first.step, first.op, { input: effective.value });
    if (!isRecord(firstArgs)) {
      return fail(diagnostic(name, "VALIDATION_FAILED", { failed_parameter: "input", expected: "an object", remediation: `Step 1 of ${name} does not evaluate to an argument object.` }));
    }
    const { confirm: _confirm, verify: _verify, ...rest } = opts;
    const prepared = await this.#prepare(first.op, firstArgs, rest, "preview", null);
    if (!prepared.ok) return prepared;
    const p = prepared.value;
    const effects: string[] = [];
    if (typeof macro.summary === "string" && macro.summary !== "") effects.push(macro.summary);
    for (const [index, { step, op }] of steps.entries()) {
      const kind = typeof step.kind === "string" ? step.kind : "call";
      effects.push(`Step ${index + 1}: ${kind} ${op.id} (${op.agent.safety}).`);
      const confirmation = op.agent.confirmation;
      if (confirmation && typeof confirmation.message === "string" && confirmation.message !== "") {
        // Later steps' arguments that come from earlier results are not known yet.
        const evaluated = index === 0 ? firstArgs : this.#stepArgs(macro, step, op, { input: effective.value });
        const args: Record<string, unknown> = isRecord(evaluated) ? { ...evaluated } : {};
        if (index > 0 && isRecord(step.args)) for (const key of Object.keys(step.args)) if (args[key] === undefined) args[key] = LATER;
        effects.push(interpolate(confirmation.message, args, op));
      }
      if (typeof op.agent.remediationNote === "string" && op.agent.remediationNote !== "") effects.push(op.agent.remediationNote);
    }
    if (macro.shownOnce === true) effects.push("The result contains values the API shows only once; store them immediately.");
    const token = safety === "read_only" ? null : await issueToken(this.#confirmationKey, `macro:${name}`, effective.value, this.#now());
    const request: RenderedRequest = { method: p.method, url: p.displayUrl, headers: p.headers.toRecord(true), body: p.displayBody };
    const value: PreviewResult = {
      operation: name,
      safety: safety as PreviewResult["safety"],
      request,
      effects,
      confirmation_token: token,
      expires_in_ms: token === null ? null : CONFIRMATION_TTL_MS,
    };
    return { ok: true, value, meta: { status: 0, headers: {}, requestId: null, attempts: 0 } };
  }

  async #runMacro<T>(macro: MacroDescriptor, input: Record<string, unknown>, opts: CallOptions): Promise<Result<T>> {
    const name = safeName(macro, "name", "<unknown macro>");
    const plan = this.#macroPlan(macro, name);
    if (!plan.ok) return plan;
    const effective = this.#macroInput(macro, input, name);
    if (!effective.ok) return effective;
    const { steps, safety } = plan.value;
    const unconfirmed = await this.#confirmed(name, `macro:${name}`, safety, effective.value, opts.confirm, `the macro's preview(...)`);
    if (unconfirmed) return unconfirmed;
    const scope: Record<string, unknown> = { input: effective.value };
    const completed: string[] = [];
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
      const stepOpts: StepOptions = { ...rest, verify: false, [MACRO_CONFIRMED]: true };
      if (!usesKey || keyUsed) delete stepOpts.idempotencyKey;
      else if (opts.idempotencyKey !== undefined) keyUsed = true;
      let value: unknown;
      let failure: Diagnostic | null = null;
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
        } else failure = result.error;
      }
      if (failure) {
        const done = completed.length > 0 ? `completed before it: ${completed.join(", ")}` : "no step completed before it";
        return fail({ ...failure, remediation: `${failure.remediation} Macro ${name} stopped at step ${index + 1} of ${steps.length} (${op.id}); ${done}.` });
      }
      completed.push(op.id);
      if (typeof step.as === "string" && step.as !== "") scope[step.as] = value;
    }
    return { ok: true, value: evaluateExpr(macro.output, scope as Scope) as T, meta };
  }
}
