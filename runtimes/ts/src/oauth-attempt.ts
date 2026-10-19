// SPDX-License-Identifier: Apache-2.0
/** In-memory, one-shot browser authorization transactions. No persistent adapter is implied. */
import { base64url, utf8 } from "./util.js";

export const AUTHORIZATION_ATTEMPT_TTL_MS = 600_000;
export const MAX_AUTHORIZATION_ATTEMPTS = 32;
export const MAX_AUTHORIZATION_ATTEMPT_BYTES = 32_768;
export const MAX_OAUTH_CALLBACK_BYTES = 16_384;
export const MAX_OAUTH_CALLBACK_PARAMETERS = 64;
export const MAX_OAUTH_TOKEN_RESPONSE_BYTES = 65_536;
export const MAX_AUTHORIZATION_INPUT_ITEMS = 64;

/** Pin one mix-up defense before opening the browser. No fallback is allowed. */
export type IssuerDefense =
  | { kind: "issuerResponse"; issuer: string }
  | { kind: "distinctRedirect"; qualification: string };

export interface AuthorizationAttempt {
  attemptHandle: string;
  url: string;
  expiresInMs: number;
}

export interface AuthorizationUrlParams {
  /** Authenticated browser session or owning native listener/invocation identity. */
  sessionBinding: string;
  issuerDefense: IssuerDefense;
  /** Explicit provider qualification: S256 is supported; never downgrade. */
  s256Supported: true;
  redirectUri?: string;
  scopes?: string[];
  resource?: string;
  audience?: string;
  accountContext?: string;
  extra?: Record<string, string>;
  signal?: AbortSignal;
}

export interface ExchangeCodeParams {
  attemptHandle: string;
  callbackUrl: string;
  sessionBinding: string;
  signal?: AbortSignal;
}

const RESERVED = new Set([
  "response_type", "client_id", "client_secret", "redirect_uri", "scope", "resource", "audience",
  "state", "code_challenge", "code_challenge_method", "code_verifier", "grant_type", "code",
  "iss", "error", "error_description", "error_uri", "request", "request_uri", "response_mode",
]);

/** Strict application/x-www-form-urlencoded decoding, with limits before allocation. */
export function callbackFields(query: string): Map<string, string> {
  if (query.length > MAX_OAUTH_CALLBACK_BYTES || utf8(query).byteLength > MAX_OAUTH_CALLBACK_BYTES) throw new Error();
  let count = query === "" ? 0 : 1;
  for (let i = 0; i < query.length; i++) if (query[i] === "&" && ++count > MAX_OAUTH_CALLBACK_PARAMETERS) throw new Error();
  const fields = new Map<string, string>();
  let bytes = 0;
  for (const pair of query === "" ? [] : query.split("&")) {
    const equals = pair.indexOf("=");
    if (equals < 1) throw new Error();
    const key = decodeURIComponent(pair.slice(0, equals).replace(/\+/g, " "));
    const value = decodeURIComponent(pair.slice(equals + 1).replace(/\+/g, " "));
    bytes += utf8(key).byteLength + utf8(value).byteLength;
    if (bytes > MAX_OAUTH_CALLBACK_BYTES || fields.has(key)) throw new Error();
    fields.set(key, value);
  }
  return fields;
}

export function qualifiedEndpoint(url: string, redirect = false): void {
  const parsed = new URL(url);
  if (parsed.username || parsed.password || parsed.hash || url.includes("#")) throw new Error();
  if (redirect) {
    if (url.includes("?")) throw new Error();
  } else {
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") throw new Error();
    for (const key of callbackFields(parsed.search.slice(1)).keys()) if (RESERVED.has(key)) throw new Error();
  }
}

export function attemptInput(params: AuthorizationUrlParams, strings: Iterable<string>): AuthorizationUrlParams {
  if (params.s256Supported !== true || typeof params.sessionBinding !== "string" || !params.sessionBinding) throw new Error();
  const defense = params.issuerDefense;
  let size = 0;
  const charge = (value: string): void => {
    if (typeof value !== "string" || value.length > MAX_AUTHORIZATION_ATTEMPT_BYTES) throw new Error();
    size += utf8(value).byteLength;
    if (size > MAX_AUTHORIZATION_ATTEMPT_BYTES) throw new Error();
  };
  for (const value of strings) charge(value);
  if (defense?.kind === "issuerResponse") {
    if (typeof defense.issuer !== "string" || !defense.issuer) throw new Error();
    charge(defense.issuer);
  } else if (defense?.kind === "distinctRedirect") {
    if (typeof defense.qualification !== "string" || !defense.qualification) throw new Error();
    charge(defense.qualification);
  } else throw new Error();
  const extra: Record<string, string> = {};
  for (const value of [params.sessionBinding, params.accountContext ?? "", params.resource ?? "", params.audience ?? ""]) charge(value);
  const scopes: string[] | undefined = params.scopes === undefined ? undefined : [];
  if (params.scopes !== undefined) {
    if (!Array.isArray(params.scopes) || params.scopes.length > MAX_AUTHORIZATION_INPUT_ITEMS) throw new Error();
    for (const scope of params.scopes) { charge(scope); scopes!.push(scope); }
  }
  let count = 0;
  for (const key in params.extra ?? {}) {
    if (!Object.hasOwn(params.extra!, key)) continue;
    if (++count > MAX_AUTHORIZATION_INPUT_ITEMS) throw new Error();
    const value = params.extra![key];
    if (RESERVED.has(key) || typeof value !== "string") throw new Error();
    charge(key); charge(value);
    extra[key] = value;
  }
  // All mutable caller collections are owned snapshots before asynchronous work.
  return {
    sessionBinding: params.sessionBinding, issuerDefense: { ...defense }, s256Supported: true,
    ...(scopes === undefined ? {} : { scopes }), extra,
    ...(params.resource === undefined ? {} : { resource: params.resource }),
    ...(params.audience === undefined ? {} : { audience: params.audience }),
    ...(params.accountContext === undefined ? {} : { accountContext: params.accountContext }),
    ...(params.signal === undefined ? {} : { signal: params.signal }),
  };
}

export function randomAttemptSecret(): string {
  return base64url(crypto.getRandomValues(new Uint8Array(32)));
}

export interface PendingAuthorization {
  scheme: string;
  generation: number;
  started: number;
  binding: string;
  redirectUri: string;
  clientId: string;
  clientSecret: string | undefined;
  configuredRedirect: string | undefined;
  authorizationUrl: string;
  tokenUrl: string;
  refreshUrl: string | null;
  configuredScopes: string;
  requestedScopes: readonly string[];
  resource: string | undefined;
  audience: string | undefined;
  accountContext: string | undefined;
  defense: IssuerDefense;
  state: string;
  verifier: string | undefined;
  claimed: boolean;
  canceled: boolean;
  controller: AbortController;
  dispose: () => void;
}
