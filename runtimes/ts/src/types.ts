// SPDX-License-Identifier: Apache-2.0
/**
 * Descriptor, option and result contracts shared by generated TypeScript
 * SDKs and the runtime.
 *
 * Generated descriptors supply API metadata; {@link ClientCoreApi} performs
 * validation, authentication, transport and safety checks. Applications
 * usually configure {@link ClientOptions}, pass {@link CallOptions} per
 * invocation, and inspect {@link Result} rather than parse exception text.
 *
 * These types are shared by the runtime and TypeScript emitter. Keep changes
 * additive (new optional fields or union members), and update both sides
 * together.
 */
import type { TokenStore } from "./oauth.js";
import type { EventStream } from "./streams.js";
import type { RequestScheduler, SchedulerOptions } from "./scheduler.js";

// ---------------------------------------------------------------- errors

/**
 * Effect tier used to select retry and confirmation rules. It describes
 * the operation policy; it is not a substitute for server authorization.
 */
export type Safety = "read_only" | "mutating" | "destructive" | "irreversible";

/**
 * Guidance for a subsequent attempt. `same_key_only` requires retaining
 * replay protection; `after_remediation` requires action before retrying.
 */
export type Retryable = "never" | "after_delay" | "same_key_only" | "after_remediation";

/** Closed set of error categories shared by every tungsten runtime. */
export type Category =
  | "VALIDATION_FAILED"
  | "MALFORMED_REQUEST"
  | "REQUEST_TOO_LARGE"
  | "AUTH_FAILED"
  | "NOT_FOUND"
  | "CONFLICT"
  | "PRECONDITION_FAILED"
  | "RATE_LIMITED"
  | "UPSTREAM_UNAVAILABLE"
  | "OUTCOME_UNKNOWN"
  | "TRANSPORT_FAILED"
  | "CONFIRMATION_REQUIRED"
  | "GATE_DISABLED"
  | "UNEXPECTED_RESPONSE";

/**
 * Structured failure information returned by runtime calls. Snake-case
 * field names form the shared diagnostic wire contract. Prefer category,
 * retryable and remediation over parsing human-readable messages.
 */
export interface Diagnostic {
  /**
   * Discriminator for this diagnostic envelope.
   */
  status: "error";
  /**
   * Machine-readable failure category.
   */
  category: Category;
  /** Operation id, e.g. `public.replayWebhookDelivery`. */
  operation: string;
  /**
   * HTTP status when a response was received, otherwise null.
   */
  http_status: number | null;
  /** The API's error code from the response body, when present. */
  code: string | null;
  /** JSON path of the offending argument (`body.recipient_e164`). */
  failed_parameter: string | null;
  /** Redacted when the parameter is sensitive; strings cut to 200 chars. */
  received_value: unknown;
  /**
   * Expected argument shape or condition, when available.
   */
  expected: string | null;
  /**
   * Suggested next action for a caller or operator.
   */
  remediation: string;
  /**
   * Retry guidance after applying the operation's replay policy.
   */
  retryable: Retryable;
  /**
   * Suggested delay in milliseconds, when known.
   */
  retry_after_ms: number | null;
  /** What to do next, e.g. the verification operation to call. */
  next_action: string | null;
  /**
   * Request identifier extracted from response headers, when available.
   */
  request_id: string | null;
  /**
   * Attempt count associated with this failure.
   */
  trace: { attempts: number };
}

/**
 * HTTP response information for the successful attempt, including the
 * number of attempts used to obtain it.
 */
export interface ResponseMeta {
  /**
   * HTTP status of the successful response.
   */
  status: number;
  /**
   * Response headers exposed by the transport.
   */
  headers: Record<string, string>;
  /**
   * Request identifier, or null when no recognized header was present.
   */
  requestId: string | null;
  /**
   * Number of request attempts, including the successful one.
   */
  attempts: number;
}

/** The meta of a stream item: the answer of the connection the event
 * arrived on (the last one, after a reconnect) and how often the stream
 * reconnected up to this event. */
export interface StreamMeta extends ResponseMeta {
  reconnects: number;
}

/**
 * Outcome of a post-call read check. Inspect checked and error before
 * interpreting passed; timedOut reports an exhausted polling budget.
 */
export interface Verification {
  /**
   * Whether a verification observation was available for evaluation.
   */
  checked: boolean;
  /** True when `expect` held; false when it did not within the budget. */
  passed: boolean;
  /** The verification operation's last result body. */
  observed: unknown;
  /** True when polling for `terminal` ran out of budget. */
  timedOut?: boolean;
  /** Why the check could not run (verification call failed); `checked` is false. */
  error?: Diagnostic;
}

/** The outcome of a call. A success response without a body (204, or any
 * success status the spec declares without content) has `value: undefined`;
 * generated SDKs type such operations as `void` (or `T | undefined` when the
 * operation can also answer with a body). */
export type Result<T> =
  | { ok: true; value: T; meta: ResponseMeta; verification?: Verification }
  | {
      ok: false;
      error: Diagnostic;
      /** What the failed call or macro already produced, when its effect
       * happened: the success body of a mutation whose response failed
       * strict validation, or a macro's completed step results by their
       * `as` names. It can hold values the API shows only once (a signing
       * secret), so store them before acting on the error; they are never
       * repeated in the envelope. */
      partial?: unknown;
    };

// ----------------------------------------------------------- descriptors

/**
 * HTTP request methods accepted by an operation descriptor.
 */
export type HttpMethod = "GET" | "PUT" | "POST" | "DELETE" | "OPTIONS" | "HEAD" | "PATCH" | "TRACE";

/**
 * Wire location of an OpenAPI parameter.
 */
export type ParamLocation = "path" | "query" | "header" | "cookie";

/**
 * Parameter serialization style used with ParamDescriptor.explode.
 */
export type ParamStyle = "simple" | "form" | "label" | "matrix" | "space_delimited" | "pipe_delimited" | "deep_object";

/**
 * Source of a parameter value: ordinary args, replay key, authentication,
 * origin profile or a fixed descriptor value.
 */
export type ParamRole = "plain" | "idempotency_key" | "dry_run" | "origin" | "auth" | "constant";

/**
 * Wire mapping and serialization rules for one operation parameter.
 */
export interface ParamDescriptor {
  /** Key in the generated method's args object. Roles `idempotency_key`,
   * `origin`, `auth` and `constant` are not args: they come from
   * `CallOptions.idempotencyKey` and the auth profile (`ClientOptions.auth`),
   * and their `name` is informational. */
  name: string;
  /** Name on the wire. */
  wire: string;
  in: ParamLocation;
  required: boolean;
  style: ParamStyle;
  explode: boolean;
  role: ParamRole;
  /** Header value of a `constant` parameter: the runtime sends it on every
   * call (`ClientOptions.headers` and `CallOptions.headers` replace it). */
  constant?: string;
  sensitive?: boolean;
}

/**
 * Encoding selected by a request body descriptor.
 */
export type BodyEncoding = "json" | "form" | "multipart" | "bytes" | "text";

/**
 * How an operation takes its body from the generated argument object
 * and encodes it on the wire.
 */
export interface BodyDescriptor {
  mediaType: string;
  encoding: BodyEncoding;
  required: boolean;
  /** `merged`: the body is a JSON object whose fields are keys of the args
   * object, listed in `fields` (wire names; TypeScript models keep wire
   * names). `arg`: the whole body is `args[arg]` (bytes, text, unions,
   * arrays, or field names that collide with a parameter). */
  shape: { kind: "merged"; fields: string[] } | { kind: "arg"; arg: string };
}

/**
 * An exact HTTP status, a status class, or the fallback `default` match.
 */
export type StatusMatch = number | "1XX" | "2XX" | "3XX" | "4XX" | "5XX" | "default";

/**
 * A declared response alternative used to classify a status and media type.
 */
export interface ResponseDescriptor {
  status: StatusMatch;
  kind: "success" | "error" | "ambiguous";
  /** First media type, or null for a bare status. */
  mediaType: string | null;
}

/**
 * Request parameters and response fields used to traverse pages. The
 * client supports cursor, offset, page-number and Link-header traversal.
 */
export type PaginationDescriptor =
  | {
      style: "cursor";
      requestParam: string;
      /** Response field holding the next cursor; empty when only `cursorItemField` applies. */
      responseField: string;
      itemsField: string;
      pageSizeParam: string | null;
      /** Response boolean (`has_more`): iteration stops as soon as it is `false`. */
      hasMoreField?: string;
      /** Without a cursor in the response, the next cursor is this field of the last item. */
      cursorItemField?: string;
    }
  | { style: "offset"; offsetParam: string; limitParam: string; itemsField: string }
  | { style: "page"; pageParam: string; sizeParam: string; itemsField: string }
  | { style: "link_header"; itemsField: string };

/**
 * Replay-protection policy. `auto` uses a supplied key first; otherwise,
 * only an actual call reserves a stored key. `caller_owned` uses a supplied
 * key. `content_hash` hashes available body material and yields no key when
 * that material is absent. `content_identity` relies on request identity
 * without a key header.
 */
export type IdempotencyKind = "none" | "auto" | "caller_owned" | "content_hash" | "content_identity";

/**
 * Optional diagnostic overrides for an API error code. Omitted fields
 * retain the runtime or API defaults.
 */
export interface RemediationEntry {
  category?: Category;
  text?: string;
  retryable?: Retryable;
  next_action?: string;
}

/**
 * Read operation, argument mappings and predicates for checking a
 * successful call's effect. Polling uses a separate interval and budget.
 */
export interface VerifyDescriptor {
  /** Operation id of the read operation to call. */
  operation: string;
  /** Argument mapping, keyed by the read operation's parameter wire names
   * (or generated names); string values starting with `$response.` or
   * `$args.` are resolved against the call, `$args` by generated or wire
   * name. */
  args: Record<string, unknown>;
  /** Field path → predicate (`{in: [...]}`, `{equals: x}`, `{contains: {...}}`).
   * `$response.` and `$args.` references in `expect` and `terminal` are
   * resolved against the call; an unresolved one never matches. */
  expect: Record<string, unknown>;
  terminal: Record<string, unknown>;
  pollIntervalMs: number | null;
  pollBudgetMs: number | null;
}

/**
 * Generated safety, replay, preview, verification and redaction policy
 * for one operation. These policies do not grant API permissions.
 */
export interface AgentMeta {
  importPolicy?: { inferredSafety: Safety; reviewed: boolean; source: string; allowedOrigins: string[] };
  safety: Safety;
  idempotency: { policy: IdempotencyKind; header: string | null; format: string | null; persistRequired: boolean; note: string | null };
  preview:
    | { mode: "local" }
    | { mode: "header"; header: string; value: string }
    | { mode: "endpoint"; operation: string }
    | { mode: "none" };
  confirmation: { summaryFields: string[]; message: string | null } | null;
  verify: VerifyDescriptor | null;
  /** Operation-specific remediation by API error code. */
  remediation: Record<string, RemediationEntry>;
  remediationNote: string | null;
  sensitiveResponseFields: string[];
  /** Arguments marked sensitive (`x-agent-sensitive` on request body
   * fields), as dotted paths into the args object (`pin`, `body.pin`,
   * `profile.recovery_phrase`; array items are not indexed). Redacted in
   * `received_value`, previews, middleware and diagnostics. */
  sensitiveRequestFields?: string[];
  shownOnce: boolean;
}

/** Structural subset of a Zod schema (`safeParse`), so the runtime does not
 * depend on a Zod version. */
export interface SchemaLike<T = unknown> {
  safeParse(value: unknown):
    | { success: true; data: T; notes?: PatternNote[] }
    | { success: false; error: { issues: Array<{ path: Array<string | number>; message: string; code?: string }> }; notes?: PatternNote[] };
}

/** A schema `pattern` the validator could not check, so it accepted the value:
 * where, which pattern and why (never the value). A schema returns these in
 * `notes`; the client reports each once per call and path as a
 * `PATTERN_UNCHECKED` diagnostic through `onDiagnostic`. */
export interface PatternNote {
  path: Array<string | number>;
  pattern: string;
  reason: string;
}

/**
 * One generated operation's HTTP shape, schemas and runtime policy.
 * Register related descriptors with the core when resolving operation IDs.
 */
export interface OperationDescriptor {
  /** IR operation id (`public.replayWebhookDelivery`). */
  id: string;
  method: HttpMethod;
  /** Path template as in the spec (`/v1/webhooks/{endpoint_id}/rotate`);
   * placeholders use wire names. */
  path: string;
  params: ParamDescriptor[];
  body: BodyDescriptor | null;
  responses: ResponseDescriptor[];
  /** OR of AND-sets of auth scheme names. Empty: no auth. */
  security: string[][];
  pagination: PaginationDescriptor | null;
  /** For rpc-unflattened operations: the envelope field and value to send,
   * and the field the args are placed under. `constants` are further
   * envelope members sent verbatim (JSON-RPC `jsonrpc: "2.0"`). */
  rpc: { field: string; value: string; paramsField: string; constants?: Record<string, unknown> } | null;
  /** Field path of the API error code in error bodies (`code`, `error.code`). */
  errorCodeField: string | null;
  status: { kind: "implemented" } | { kind: "gated"; envVar: string; disabledStatus: number };
  agent: AgentMeta;
  /** Validates the args object before any network call. */
  request?: SchemaLike;
  /** Validates the success body (per `ClientOptions.validateResponses`).
   *
   * Untagged unions (IR `UnionStrategy::Untagged`, TG0301) are emitted as
   * `z.union([...])` with the variants in IR order, and Zod tries them in
   * that order: the body is accepted when any variant accepts it, and the
   * first variant that does decides which refinements applied. The
   * result's `value` is always the decoded wire body itself, never the
   * schema's output, so an earlier variant that overlaps a later one (or
   * strips unknown members) cannot drop or rewrite members of the body. */
  response?: SchemaLike;
  /** One-line summary (compacted description); the last line of preview effects. */
  summary?: string | null;
  /** The operation's event stream (`ClientCore.stream`); absent when it has none. */
  stream?: StreamDescriptor;
}

/** The server-sent events of an operation (IR `StreamSpec`). */
export interface StreamDescriptor {
  /** Validates the decoded `data` of every event (per
   * `ClientOptions.validateResponses`); absent: events are any JSON value. */
  event?: SchemaLike;
  /** A `data` value that ends the stream without being an event (`[DONE]`). */
  done?: string;
  /** Wire name of the boolean request body field that selects the stream;
   * `ClientCore.stream` sets it to `true`. */
  flag?: string;
}

/** The `authorizationCode` flow of an OAuth2 scheme (present only when the
 * spec declares one). `refreshUrl` falls back to `tokenUrl`. */
export interface AuthorizationCodeDescriptor {
  authorizationUrl: string;
  tokenUrl: string;
  refreshUrl: string | null;
  scopes: string[];
}

/**
 * Authentication scheme or composite profile declared by the API.
 * Credential values are supplied separately through AuthConfig.
 */
export type AuthSchemeDescriptor =
  | { kind: "api_key"; name: string; in: "header" | "query" | "cookie"; wire: string }
  /** `prefix`: the token must start with it (tungsten.yml
   * `auth_profiles.<name>.bearer.prefix`, e.g. `ztw_`); checked before any
   * request is sent, and a mismatch is `AUTH_FAILED` with zero network calls. */
  | { kind: "http_bearer"; name: string; prefix: string | null }
  | { kind: "http_basic"; name: string }
  | { kind: "oauth2"; name: string; tokenUrl: string | null; scopes: string[]; authorizationCode?: AuthorizationCodeDescriptor }
  | {
      kind: "composite";
      name: string;
      /** Spec scheme names this profile satisfies together. */
      satisfies: string[];
      parts: Array<
        | { kind: "cookie"; name: string }
        | { kind: "header"; name: string; equalsCookie: string | null; fromConfig: string | null; mutationOnly: boolean }
        | { kind: "bearer"; prefix: string | null }
      >;
    };

/** `none` wins over both manifest and client defaults. */
export type RedirectPolicy = "same_origin_read" | "none";

/**
 * API-wide servers, authentication, error mapping and retry defaults
 * shared by the generated operations.
 */
export interface ApiDescriptor {
  redirects?: RedirectPolicy;
  /** Machine name (`zrotext`). */
  name: string;
  /** SDK package version. */
  version: string;
  tungstenVersion: string;
  servers: string[];
  auth: AuthSchemeDescriptor[];
  /** Global remediation by API error code (agent.yml `errors.codes`). */
  errorCodes: Record<string, RemediationEntry>;
  /** Statuses that make a mutation's outcome unknown. */
  ambiguousStatuses: number[];
  /** Non-JSON error mapping (agent.yml `errors.non_json`); `media` is a
   * media type or `"none"` for a bare status. */
  nonJson: Array<{ status: number; media: string; category: Category; retryable: Retryable; text: string | null }>;
  /** Runtime gate env var → explanation. */
  gates: Record<string, string>;
  /** Retry defaults by safety tier (agent.yml `defaults.retries`):
   * `readOnly` for read-only operations, `mutating` for every other tier
   * (a mutation is still retried only with an idempotency key or an
   * identity body). `ClientOptions.retries` overrides them field by field.
   * Absent: the runtime defaults apply to every tier. */
  retries?: { readOnly: Partial<RetryOptions>; mutating: Partial<RetryOptions> };
}

// --------------------------------------------------------------- options

/** Credentials per auth scheme name. A string is the secret of an
 * `api_key`, `http_bearer` or `http_basic` (`user:password`) scheme. A
 * composite profile takes an object keyed by cookie name or config key
 * (for ZROtext's owner session: the two cookie values and `origin`). */
export type AuthConfig = Record<string, string | Record<string, string>>;

/**
 * Replay-key storage indexed by operation scope and logical argument
 * identity. Implement reserve for atomic arbitration across independent
 * writers; the get/put fallback only serializes callers in this runtime module.
 * Store failures prevent automatic-key dispatch rather than issue a second key.
 */
export interface IdempotencyStore {
  /** Atomically return the existing key or persist candidate. */
  reserve?(scope: string, logicalId: string, candidate: string): Promise<string>;
  /**
   * Read a slot without creating a key; undefined means no stored key.
   */
  get(scope: string, logicalId: string): Promise<string | undefined>;
  /**
   * Persist a slot. Reject if storage fails; cross-writer arbitration
   * belongs to reserve rather than an isolated get/put pair.
   */
  put(scope: string, logicalId: string, key: string): Promise<void>;
}

/** What middleware sees of one attempt. Secrets are redacted: secret
 * headers, auth query values, and in a JSON or form body the arguments
 * marked sensitive (or named like credentials) as `<redacted>`. */
export interface RequestContext {
  operation: OperationDescriptor;
  attempt: number;
  method: HttpMethod;
  url: string;
  headers: Record<string, string>;
  body: BodyInit | null;
}

/**
 * Optional asynchronous hooks around each request attempt and retry.
 * Request contexts are redacted views; hooks are not transport adapters.
 */
export interface Middleware {
  /**
   * Called with a redacted request view before dispatch.
   */
  onRequest?(ctx: RequestContext): void | Promise<void>;
  /**
   * Called for the received response with its redacted request context.
   */
  onResponse?(ctx: RequestContext, response: Response): void | Promise<void>;
  /**
   * Called with the classified reason before a permitted retry.
   */
  onRetry?(ctx: RequestContext, reason: Diagnostic): void | Promise<void>;
}

/**
 * Backoff configuration for outcomes the operation is permitted to retry.
 * The maximum counts retries after the initial attempt, not total requests.
 */
export interface RetryOptions {
  /** Attempts after the first, for retryable outcomes. */
  max: number;
  /**
   * Initial exponential-backoff delay in milliseconds.
   */
  baseMs: number;
  /**
   * Upper bound for the computed delay in milliseconds.
   */
  maxMs: number;
  /**
   * Randomization applied to the backoff delay.
   */
  jitter: "none" | "full" | "equal";
  /**
   * Whether recognized Retry-After response headers influence the delay.
   */
  honorRetryAfter: boolean;
}

/**
 * Client-wide transport, credential, storage and response policies.
 * Per-call options override the settings that explicitly support overrides.
 */
export interface ClientOptions {
  /** Allows credentials over non-loopback HTTP, including OAuth token requests.
   * Default false: use HTTPS. localhost, 127/8 and ::1 remain usable locally. */
  allowInsecureHttp?: boolean;
  /** May tighten the API default; cannot re-enable an API configured with none. */
  redirects?: RedirectPolicy;
  /** Defaults to the first server. */
  baseUrl?: string;
  /**
   * Credentials keyed by descriptor authentication scheme name.
   */
  auth?: AuthConfig;
  /** Holds the tokens of `authorizationCode` schemes (configure such a
   * scheme as `{flow: "authorizationCode", clientId, clientSecret?,
   * redirectUri?}`). Default: in memory, per client. */
  tokenStore?: TokenStore;
  /** Defaults to `globalThis.fetch`. */
  fetch?: typeof fetch;
  /** Per-attempt timeout. Default 30000. */
  timeoutMs?: number;
  /** Total deadline of one logical call in milliseconds, see
   * `CallOptions.deadlineMs`. Default: none (only `timeoutMs` per attempt). */
  deadlineMs?: number;
  /** Ordinary-response byte cap; default 4 MiB. Null/omitted inherits; a finite call override wins. */
  maxResponseBytes?: number | null;
  /** OAuth code/refresh responses only: 65,536 bytes by default. A positive
   * safe integer overrides this owner limit; ordinary call limits do not. */
  maxOAuthTokenResponseBytes?: number;
  /** Overrides the API's per-tier retry defaults (`ApiDescriptor.retries`)
   * field by field, for every tier. */
  retries?: Partial<RetryOptions>;
  /**
   * Store for auto-generated replay keys; defaults to a new in-memory store.
   */
  idempotencyStore?: IdempotencyStore;
  /**
   * Hooks invoked for request attempts and retries.
   */
  middleware?: Middleware[];
  /** Default `warn`: report through `onDiagnostic`, never fail the call. */
  validateResponses?: "off" | "warn" | "strict";
  /** Receives warnings (response validation, deprecations). */
  onDiagnostic?: (d: Diagnostic) => void;
  /** Extra headers on every request. */
  headers?: Record<string, string>;
  /** Largest server-sent event `stream()` accepts, in UTF-8 bytes (the field
   * lines of one event plus the line being read). A larger one ends the
   * stream with an `UNEXPECTED_RESPONSE` failure. Default 1048576 (1 MiB). */
  maxEventBytes?: number;
  /** How often `stream()` reconnects a dropped stream (a lost connection, or
   * a clean end before the `done` event the operation declares) with
   * `Last-Event-ID` set to the last event id it delivered; events whose id
   * was delivered are never delivered twice. Only for operations that are
   * safe to repeat (read-only, or with replay protection) and only once
   * an event id is known (or before the first event). 0 disables it.
   * Default 3. */
  maxReconnects?: number;
  /** Longest wait before a reconnect, in milliseconds: the server's `retry`
   * value (250 ms when it sent none) is capped at this and then reduced by
   * up to 25 % at random. Default 30000. */
  reconnectMaxMs?: number;
  /** A stream that delivers no bytes (comments count) for this many
   * milliseconds is idle: it is reconnected like a dropped one, else ends
   * with a failure whose `code` is `STREAM_IDLE`. Default: the per-attempt
   * `timeoutMs`, which ends the stream without a reconnect. */
  idleTimeoutMs?: number;
  /** Overrides the key used to sign confirmation tokens. Supply at least 32
   * cryptographically random bytes; shorter keys fail construction. The key
   * is copied. Default: random per client instance. */
  confirmationKey?: Uint8Array;
  /** Operations the core can resolve by id: verification hooks, endpoint
   * previews and macro steps. Operations passed to `call` are added too. */
  operations?: OperationDescriptor[];
  /** Clock in epoch milliseconds (token expiry, Retry-After dates). Default `Date.now`. */
  now?: () => number;
  /** Random number in [0, 1) for retry jitter. Default `Math.random`. */
  random?: () => number;
  /** Bounds the requests this client has in flight and waiting, and shares
   * rate-limit pauses between them. Pass `SchedulerOptions` for a scheduler
   * of this client alone, or one `RequestScheduler` to several clients that
   * share a quota. Default: none, every call is sent at once. */
  scheduler?: RequestScheduler | SchedulerOptions;
  /** The tenant or principal this client calls for. Clients sharing a
   * scheduler keep separate capacity, queues and pauses per tenant. */
  tenant?: string;
}

/**
 * Options for one call, preview, traversal or macro. Keep the same
 * idempotency key when retrying an intent that may already have taken effect.
 */
export interface CallOptions {
  /** Maximum successful pages per traversal; omitted or zero means unbounded. */
  maxPages?: number;
  /** Required for `caller_owned` operations with `persistRequired`. */
  idempotencyKey?: string;
  /** Token from `preview()`; required for `irreversible`, and for
   * `destructive` unless `true` is given. A token authorizes one intent:
   * the first call that sends spends it, and it is accepted again only for
   * a retry with the same idempotency key (or the identical body of a
   * `content_identity` operation). */
  confirm?: string | true;
  /** `false`: `confirm: true` does not confirm a destructive operation
   * either, and remediation never offers it (callers that only forward
   * preview tokens, such as an MCP server). Default `true`. */
  allowConfirmTrue?: boolean;
  /**
   * Caller cancellation signal forwarded to owned request work. Custom
   * transports still need to honor cancellation.
   */
  signal?: AbortSignal;
  /**
   * Per-attempt timeout in milliseconds, overriding the client default.
   */
  timeoutMs?: number;
  /**
   * Total deadline in milliseconds for this call, overriding the client
   * default; 0 or omitted means none. Measured on a monotonic clock from
   * the call, it bounds authentication, idempotency-key reservation, every
   * attempt (each one's `timeoutMs` is cut to the time left), retry waits,
   * response bodies and verification; for `pages`, `poll` and `runMacro` it
   * bounds the whole traversal, poll or macro. No attempt starts after it
   * expires (`code` `DEADLINE_EXCEEDED`). For `stream()` it bounds
   * connecting and each reconnect, not an open stream (see
   * `idleTimeoutMs`). A mutation already sent when it expires reports
   * `OUTCOME_UNKNOWN`, never a blind replay. Shared token refreshes and
   * custom `fetch` or store work are not cancelled, only no longer waited for.
   */
  deadlineMs?: number;
  /** Override the client response-body cap. Null/omitted inherits. Positive safe integer; checked
   * against actual bytes while reading, including non-SSE responses. */
  maxResponseBytes?: number | null;
  /** Run the operation's verification hook after success. */
  verify?: boolean;
  /**
   * Extra headers for this invocation, overriding client header values.
   */
  headers?: Record<string, string>;
}

// --------------------------------------------------------------- results

/**
 * A request description for previews. Secret headers and sensitive
 * body values are redacted; this is not a transport-ready request.
 */
export interface RenderedRequest {
  method: HttpMethod;
  url: string;
  /** Secrets redacted as `<redacted>`. */
  headers: Record<string, string>;
  body: unknown;
}

/** One step of a macro preview (`ClientCore.previewMacro`). */
export interface MacroStepPreview {
  /** 1-based position in the macro. */
  step: number;
  kind: MacroStep["kind"];
  /** The step's operation id. */
  operation: string;
  /** The name the step's result is bound to, or null. */
  as: string | null;
  safety: Safety;
  /** The request the step sends (a `poll` or `paginate` step's first
   * request), rendered without sending anything. Arguments taken from an
   * earlier step's result show as `<from step NAME: path>` (NAME is that
   * step's `as` name; `<from step NAME>` for the whole result), shown as
   * written in the URL, headers and body, and exempt from pre-flight
   * validation. Null when the step's whole argument object comes from an
   * earlier result, or a value from an earlier result would have to be
   * encoded before it is known (a `bytes` or `multipart` body). */
  request: RenderedRequest | null;
  /** The step's own effects: its confirmation message (with the same
   * placeholders), what a `poll` or `paginate` step repeats
   * (`Repeats <op> every N ms until <predicate>, for at most M ms.`,
   * `Reads up to N pages of <op>.`) and its remediation note. */
  effects: string[];
}

/**
 * Rendered request and declared effects, with an optional confirmation
 * token. Local previews do not send; header or endpoint previews may contact
 * the server. A preview does not establish that the eventual effect occurred.
 */
export interface PreviewResult {
  /**
   * Operation ID, or macro name for a macro preview.
   */
  operation: string;
  /**
   * Effective safety tier used for confirmation.
   */
  safety: Safety;
  /** For a macro: the first step's request (also `steps[0].request`). */
  request: RenderedRequest;
  /** Side-effect summary from the manifest's confirmation message, the
   * remediation note and the operation's summary. */
  effects: string[];
  /** Present for mutating operations; bound to the exact arguments. */
  confirmation_token: string | null;
  /**
   * Confirmation-token lifetime in milliseconds, or null without a token.
   */
  expires_in_ms: number | null;
  /** For `header`/`endpoint` previews: the server's answer. */
  server_preview?: unknown;
  /** Macro previews only: every step's rendered request and effects, in
   * order. `effects` above lists them all, each step's under a
   * `Step N: <kind> <operation> (<safety>).` line. */
  steps?: MacroStepPreview[];
}

/**
 * One successful pagination step: extracted items, original response body
 * and a next-page marker. A null next marker ends traversal.
 */
export interface Page<T> {
  items: T[];
  /** The full page body. */
  body: unknown;
  /** Cursor, offset, page number or URL for the next page; null at the end. */
  next: unknown;
}

/** Until-predicate for `ClientCore.poll`, same shape as `VerifyDescriptor.expect`. */
export type Predicate = Record<string, unknown>;

// ---------------------------------------------------------------- macros

/** One macro step in the canonical form (`tungsten_ir::Macro.steps`).
 * `args`, `budget_ms` and the macro output are expressions: strings
 * starting with `$` reference `$input` or an earlier step's `as` name,
 * `{expr: "<ref> in [..]" | "<ref> == x" | "<ref> != x"}` is a boolean,
 * anything else a literal. */
export interface MacroStep {
  kind: "call" | "poll" | "paginate";
  /** Operation id, resolved among the operations registered with the core. */
  operation: string;
  args: unknown;
  as: string | null;
  /** `poll` only: predicate on the step's success body. When the budget
   * runs out before it holds, the step's `as` name is bound to `null`, so
   * an output like `{expr: "$status == null"}` reports the timeout. */
  until: Predicate | null;
  interval_ms: number | null;
  budget_ms: unknown;
  /** `paginate` only: page limit. */
  max_pages: number | null;
}

/** A compiled macro (`tungsten_ir::Macro`).
 *
 * Confirmation is macro-level: when the macro's tier (the strictest of its
 * declared `safety` and its steps' tiers) is `destructive` or
 * `irreversible`, `runMacro` requires `confirm` like an operation of that
 * tier, with a token from `previewMacro` bound to the macro name and its
 * exact input. That one confirmation authorizes every step of the run;
 * steps never ask for their own tokens, since their arguments may depend on
 * earlier steps' results. The token is spent (bound to the run's
 * idempotency key) by the first step that passes pre-flight and sends, so
 * a run refused before sending anything can be corrected and repeated with
 * the same token. */
export interface MacroDescriptor {
  name: string;
  summary: string;
  safety: Safety;
  steps: MacroStep[];
  output: unknown;
  /** The macro input: an operation's args (`extends`) plus added fields.
   * An added field's JSON Schema `default` is applied when the input
   * omits it. */
  input: { extends: string | null; add: Record<string, unknown> };
  /** Output fields carrying secrets (dotted paths); redacted in traces. */
  sensitiveResponseFields?: string[];
  /** The output holds values the API returns only once. */
  shownOnce?: boolean;
  /** Disclosure cluster (agent.yml `disclosure.clusters`). */
  cluster?: string | null;
}

/** Further `ClientCore` methods beyond {@link ClientCoreApi}. */
export interface ClientCoreExtensions {
  /** Make operations resolvable by id (verification, endpoint previews, macros). */
  register(...ops: OperationDescriptor[]): void;
  /** A registered operation by id. */
  operation(id: string): OperationDescriptor | undefined;
  /** Run a compiled macro; returns its output or the first failing step's
   * envelope, with the completed steps' results as `partial` (a rerun is
   * never offered when a completed mutating step has no replay
   * protection). A `destructive` or `irreversible` macro needs
   * `opts.confirm` (see {@link MacroDescriptor}). */
  runMacro<T>(macro: MacroDescriptor, input: Record<string, unknown>, opts?: CallOptions): Promise<Result<T>>;
  /** Preview a macro without sending anything: validates and renders
   * every step's request (arguments from earlier results as
   * `<from step NAME: path>` placeholders, from a dry evaluation), lists
   * every step's effects and, unless the macro is read-only, returns a
   * confirmation token bound to the macro and this exact input. A step
   * whose arguments fail pre-flight for reasons other than a placeholder
   * fails the preview, naming the step. */
  previewMacro(macro: MacroDescriptor, input: Record<string, unknown>, opts?: CallOptions): Promise<Result<PreviewResult>>;
}

/** One item of a stream: an event, or the failure that ends the stream
 * (an envelope; a failure before the first event is the only item). */
export type StreamItem<T> =
  | {
      ok: true;
      /** The event's `data`, decoded from JSON (and validated). */
      value: T;
      /** The `event` field of the server-sent event, `message` when it has none. */
      event: string;
      /** The last event id the stream has set so far, or null. */
      id: string | null;
      /** The last `retry` value (milliseconds) the stream has set so far, or null. */
      retry: number | null;
      meta: StreamMeta;
    }
  | { ok: false; error: Diagnostic };

/** What `ClientCore` (implemented in `client.ts`) offers generated code. */
export interface ClientCoreApi {
  readonly api: ApiDescriptor;
  /** Validate, authenticate, apply idempotency and confirmation rules, send
   * with retries, classify the response. Never throws for API or transport
   * errors; returns `{ok: false, error}`. */
  call<T>(op: OperationDescriptor, args: Record<string, unknown>, opts?: CallOptions): Promise<Result<T>>;
  /** Run the operation's preview mode. */
  preview(op: OperationDescriptor, args: Record<string, unknown>, opts?: CallOptions): Promise<Result<PreviewResult>>;
  /** Iterate the events of an operation's event stream (`text/event-stream`).
   * Errors before the stream starts (validation, auth, an error status) are
   * the only item, as for `call`, and are retried by the same rules. A
   * started stream that drops is reconnected with `Last-Event-ID`
   * (`ClientOptions.maxReconnects`) when the operation is safe to repeat;
   * otherwise, or when the reconnects are used up, a connection lost or an
   * idle timeout ends it with a final failure item (`TRANSPORT_FAILED`,
   * `UPSTREAM_UNAVAILABLE` for a read; `OUTCOME_UNKNOWN` for a mutation;
   * `code` `STREAM_IDLE` for `ClientOptions.idleTimeoutMs`), as does an event
   * that is not JSON or (with `validateResponses: "strict"`) does not match
   * the event type. The result is an async iterable with helpers
   * (`on`, `collect`, `reduce`, `first`, `cancel`). */
  stream<T>(op: OperationDescriptor, args: Record<string, unknown>, opts?: CallOptions): EventStream<T>;
  /** Iterate pages; stops after the last page or the first error result. */
  pages<T>(op: OperationDescriptor, args: Record<string, unknown>, opts?: CallOptions): AsyncIterable<Result<Page<T>>>;
  /** Call a read operation until `until` holds or the budget runs out. */
  poll<T>(
    op: OperationDescriptor,
    args: Record<string, unknown>,
    until: Predicate,
    intervalMs: number,
    budgetMs: number,
    opts?: CallOptions,
  ): Promise<Result<T> & { timedOut?: boolean }>;
}
