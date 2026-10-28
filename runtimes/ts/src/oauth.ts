// SPDX-License-Identifier: Apache-2.0
/**
 * Authorization-code and token-storage helpers.
 *
 * Use {@link OAuthFlow.authorizationUrl} and {@link OAuthFlow.exchangeCode}
 * for managed attempts. Low-level helpers leave PKCE, callback/session and
 * issuer validation to the application. Returned authorization URLs are
 * data: these helpers do not launch a browser or shell.
 */
import { insecureHttp, INSECURE_HTTP_AUTH, TokenError } from "./auth.js";
import { attempt, type AttemptOutcome } from "./transport.js";
import type { ApiDescriptor, AuthorizationCodeDescriptor, ClientOptions, Diagnostic } from "./types.js";
import { base64url, bytesToBase64, getPath, isRecord, utf8 } from "./util.js";
import { decodeBody } from "./classify.js";
import { diagnostic } from "./envelope.js";
import {
  AUTHORIZATION_ATTEMPT_TTL_MS, MAX_AUTHORIZATION_ATTEMPTS, MAX_OAUTH_CALLBACK_BYTES,
  MAX_OAUTH_TOKEN_RESPONSE_BYTES, attemptInput, callbackFields, qualifiedEndpoint, randomAttemptSecret,
  MAX_AUTHORIZATION_INPUT_ITEMS,
  type AuthorizationAttempt, type AuthorizationUrlParams, type ExchangeCodeParams, type PendingAuthorization,
} from "./oauth-attempt.js";
export type { AuthorizationAttempt, AuthorizationUrlParams, ExchangeCodeParams, IssuerDefense } from "./oauth-attempt.js";

// Provider-supplied labels can contain secrets even when they look like codes.
const TOKEN_ERROR_CODES = new Set([
  "invalid_request", "invalid_client", "invalid_grant", "unauthorized_client",
  "unsupported_grant_type", "invalid_scope",
]);

/** What the store keeps for one scheme. Secrets: never log it. */
export interface StoredToken {
  accessToken: string;
  refreshToken?: string;
  /** Epoch milliseconds; absent when the server gave no lifetime. */
  expiresAt?: number;
  tokenType?: string;
  scope?: string;
}

/** Holds the tokens of the authorization-code schemes, by scheme name.
 * Implement it to persist tokens (keychain, database). */
export interface TokenStore {
  get(scheme: string): Promise<StoredToken | undefined>;
  set(scheme: string, token: StoredToken): Promise<void>;
  delete(scheme: string): Promise<void>;
  /** Managed capability: invoke the predicate exactly once at the final serialized
   * mutation, against the current token. It advances private session epochs: no
   * speculative checks/retries or await gap before the write are permitted. Return
   * true only after commit. On failure preserve the old token and return false/throw;
   * the already advanced epoch may conservatively invalidate older refresh work.
   * Direct/low-level store writes are outside this managed-session guarantee. */
  setIfCurrent?(scheme: string, token: StoredToken, isCurrent: (stored: StoredToken | undefined) => boolean): Promise<boolean>;
  /** Same exactly-once final-mutation callback contract as setIfCurrent. */
  deleteIfCurrent?(scheme: string, isCurrent: (stored: StoredToken | undefined) => boolean): Promise<boolean>;
}

/** The default store: in memory, per client. */
export class MemoryTokenStore implements TokenStore {
  readonly #tokens = new Map<string, StoredToken>();
  async get(scheme: string): Promise<StoredToken | undefined> {
    const found = this.#tokens.get(scheme);
    return found ? { ...found } : undefined;
  }
  async set(scheme: string, token: StoredToken): Promise<void> {
    this.#tokens.set(scheme, { ...token });
  }
  async delete(scheme: string): Promise<void> {
    this.#tokens.delete(scheme);
  }
  async setIfCurrent(scheme: string, token: StoredToken, isCurrent: (stored: StoredToken | undefined) => boolean): Promise<boolean> {
    if (!isCurrent(this.#tokens.get(scheme))) return false;
    this.#tokens.set(scheme, { ...token });
    return true;
  }
  async deleteIfCurrent(scheme: string, isCurrent: (stored: StoredToken | undefined) => boolean): Promise<boolean> {
    if (!isCurrent(this.#tokens.get(scheme))) return false;
    this.#tokens.delete(scheme);
    return true;
  }
}

/** A PKCE pair (RFC 7636). The verifier is a secret until the exchange. */
export interface Pkce {
  verifier: string;
  challenge: string;
  method: "S256";
}

/** What `exchangeCode` and `refresh` report: no secrets. */
export interface TokenInfo {
  scheme: string;
  /** Epoch milliseconds, or null when the server gave no lifetime. */
  expiresAt: number | null;
  scope: string | null;
  /** Whether a refresh token is stored. */
  refreshable: boolean;
}

/**
 * OAuth helper outcome. Failures carry a diagnostic rather than provider
 * response secrets; successful token exchanges return metadata only.
 */
export type OAuthResult<T> = { ok: true; value: T } | { ok: false; error: Diagnostic };

/**
 * Inputs for the explicit low-level URL builder. The application owns
 * state, PKCE, callback and issuer validation; prefer managed attempts.
 */
export interface LowLevelAuthorizationUrlParams {
  /** Defaults to the `redirectUri` of the scheme's auth config. */
  redirectUri?: string;
  /** Defaults to the scopes of the flow. */
  scopes?: string[];
  state?: string;
  /** The `challenge` of a {@link Pkce}. */
  codeChallenge?: string;
  /** Further query parameters (`prompt`, `login_hint`, `audience`), added in key order. */
  extra?: Record<string, string>;
}

/**
 * Code exchange inputs for the explicit compatibility path, outside the
 * managed attempt lifecycle. Never pass a managed attempt through this path.
 */
export interface LowLevelExchangeCodeParams {
  code: string;
  /** Defaults to the `redirectUri` of the scheme's auth config. */
  redirectUri?: string;
  /** The `verifier` of the {@link Pkce} whose challenge started the flow. */
  codeVerifier?: string;
}

/** Tokens expiring within this many milliseconds count as expired. */
export const EXPIRY_SKEW_MS = 30_000;

const VERIFIER_BYTES = 32;

/** The S256 code challenge of `verifier`: base64url(SHA-256(ASCII(verifier))). */
export async function pkceChallenge(verifier: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", utf8(verifier) as Uint8Array<ArrayBuffer>);
  return base64url(new Uint8Array(digest));
}

/** A fresh verifier (43 URL-safe characters from 32 random bytes) and its S256 challenge. */
export async function generatePkce(): Promise<Pkce> {
  const verifier = base64url(crypto.getRandomValues(new Uint8Array(VERIFIER_BYTES)));
  return { verifier, challenge: await pkceChallenge(verifier), method: "S256" };
}

/** RFC 3986 percent-encoding: everything but `A-Za-z0-9-._~`. */
export function percentEncode(text: string): string {
  let out = "";
  for (const byte of utf8(text)) {
    const c = String.fromCharCode(byte);
    out += /[A-Za-z0-9\-._~]/.test(c) ? c : `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}

export function formBody(fields: Array<[string, string]>): string {
  return fields.map(([k, v]) => `${percentEncode(k)}=${percentEncode(v)}`).join("&");
}

/** The URL that sends the user to the authorization server. Query order:
 * response_type, client_id, redirect_uri, scope, state, code_challenge,
 * code_challenge_method, then `extra` in key order. */
export function buildAuthorizationUrl(flow: AuthorizationCodeDescriptor, clientId: string, p: LowLevelAuthorizationUrlParams & { redirectUri: string }): string {
  const fields: Array<[string, string]> = [
    ["response_type", "code"],
    ["client_id", clientId],
    ["redirect_uri", p.redirectUri],
  ];
  const scopes = p.scopes ?? flow.scopes;
  if (scopes.length > 0) fields.push(["scope", scopes.join(" ")]);
  if (p.state !== undefined && p.state !== "") fields.push(["state", p.state]);
  if (p.codeChallenge !== undefined && p.codeChallenge !== "") {
    fields.push(["code_challenge", p.codeChallenge], ["code_challenge_method", "S256"]);
  }
  const reserved = new Set(fields.map(([k]) => k));
  for (const key of Object.keys(p.extra ?? {}).sort()) {
    const value = p.extra?.[key];
    if (typeof value === "string" && !reserved.has(key)) fields.push([key, value]);
  }
  const separator = flow.authorizationUrl.includes("?") ? (/[?&]$/.test(flow.authorizationUrl) ? "" : "&") : "?";
  return `${flow.authorizationUrl}${separator}${formBody(fields)}`;
}

/** The auth config of an authorization-code scheme. */
interface CodeClient {
  clientId: string;
  clientSecret: string | undefined;
  redirectUri: string | undefined;
}

export interface OAuthDeps {
  api: ApiDescriptor;
  options: ClientOptions;
  fetch: () => typeof fetch;
  now: () => number;
  timeoutMs: () => number;
  /** Monotonic milliseconds; independent of token epoch expiry. */
  monotonicNow?: () => number;
}

function str(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

/** Token handling of one client: the store, the code flow's requests and
 * the single flight per scheme. */
export class OAuthSession {
  readonly store: TokenStore;
  readonly #deps: OAuthDeps;
  readonly #inflight = new Map<string, Promise<string>>();
  readonly #attempts = new Map<string, PendingAuthorization>();
  readonly #generations = new Map<string, number>();
  readonly #tokenEpochs = new Map<string, number>();
  readonly #loggingOut = new Map<string, number>();

  constructor(deps: OAuthDeps) {
    this.#deps = deps;
    let store: TokenStore | undefined;
    try {
      const candidate = deps.options.tokenStore;
      if (isRecord(candidate) && typeof candidate.get === "function" && typeof candidate.set === "function" && typeof candidate.delete === "function") {
        store = candidate as unknown as TokenStore;
      }
    } catch {
      // Unreadable options: keep the default; calls report the problem.
    }
    this.store = store ?? new MemoryTokenStore();
  }

  /** The authorization-code flow of `scheme`, or the reason there is none. */
  flowOf(scheme: string): AuthorizationCodeDescriptor | string {
    const schemes = Array.isArray(this.#deps.api.auth) ? this.#deps.api.auth : [];
    const found = schemes.find((s) => isRecord(s) && s.name === scheme);
    if (!found) return `the API descriptor defines no scheme named ${scheme}`;
    if (found.kind !== "oauth2" || !found.authorizationCode) return `the scheme ${scheme} has no authorizationCode flow`;
    return found.authorizationCode;
  }

  #client(scheme: string): CodeClient | string {
    try {
      const config = isRecord(this.#deps.options.auth) ? this.#deps.options.auth[scheme] : undefined;
      if (!isRecord(config) || config.flow !== "authorizationCode") {
        return `auth.${scheme} must be {flow: "authorizationCode", clientId, clientSecret?, redirectUri?}`;
      }
      const clientId = str(config.clientId);
      if (clientId === undefined) return `auth.${scheme} needs a clientId`;
      return { clientId, clientSecret: str(config.clientSecret), redirectUri: str(config.redirectUri) };
    } catch {
      return `auth.${scheme} could not be read`;
    }
  }

  #failure(operation: string, remediation: string): { ok: false; error: Diagnostic } {
    return { ok: false, error: diagnostic(operation, "AUTH_FAILED", { remediation }) };
  }

  #generation(scheme: string): number { return this.#generations.get(scheme) ?? 0; }
  #tokenEpoch(scheme: string): number { return this.#tokenEpochs.get(scheme) ?? 0; }
  #mutatedToken(scheme: string): void { this.#tokenEpochs.set(scheme, this.#tokenEpoch(scheme) + 1); }
  #monotonic(): number { return this.#deps.monotonicNow?.() ?? performance.now(); }
  #fencedStore(): boolean { return typeof this.store.setIfCurrent === "function" && typeof this.store.deleteIfCurrent === "function"; }

  #tokenResponseLimit(): number {
    const limit = this.#deps.options.maxOAuthTokenResponseBytes ?? MAX_OAUTH_TOKEN_RESPONSE_BYTES;
    if (!Number.isSafeInteger(limit) || limit <= 0) throw new TokenError("maxOAuthTokenResponseBytes must be a positive safe integer.");
    return limit;
  }

  #remove(handle: string, pending: PendingAuthorization): void {
    pending.verifier = undefined;
    pending.dispose();
    if (this.#attempts.get(handle) === pending) this.#attempts.delete(handle);
  }

  #cancel(handle: string, pending: PendingAuthorization): void {
    pending.canceled = true;
    pending.verifier = undefined;
    pending.controller.abort();
    // Claimed work owns admission until its HTTP/store work actually settles.
    if (!pending.claimed) this.#remove(handle, pending);
  }

  #sweep(): void {
    for (const [handle, pending] of this.#attempts) {
      if (this.#monotonic() - pending.started >= AUTHORIZATION_ATTEMPT_TTL_MS) this.#cancel(handle, pending);
    }
  }

  /** Begin a managed, session-bound S256 authorization attempt. */
  async authorizationUrl(scheme: string, params: AuthorizationUrlParams): Promise<OAuthResult<AuthorizationAttempt>> {
    const operation = `oauth.${scheme}.authorizationUrl`;
    const refuse = () => this.#failure(operation, "Cannot begin authorization: qualify S256 and issuer defense, supply a session binding, valid bounded endpoints and a fenced token store; protected query fields are reserved.");
    this.#sweep();
    if (this.#attempts.size >= MAX_AUTHORIZATION_ATTEMPTS) return this.#failure(operation, "Authorization attempt capacity is exhausted; finish or cancel an existing attempt.");
    let handle: string | undefined;
    let pending: PendingAuthorization | undefined;
    try {
      const flow = this.flowOf(scheme);
      const client = this.#client(scheme);
      if (typeof flow === "string" || typeof client === "string" || !this.#fencedStore() || this.#loggingOut.has(scheme)) return refuse();
      this.#tokenResponseLimit();
      const redirectUri = params.redirectUri ?? client.redirectUri;
      if (!redirectUri || params.signal?.aborted) return refuse();
      if (!Array.isArray(flow.scopes) || flow.scopes.length > MAX_AUTHORIZATION_INPUT_ITEMS) return refuse();
      const ownedFlow = { authorizationUrl: flow.authorizationUrl, tokenUrl: flow.tokenUrl, refreshUrl: flow.refreshUrl, scopes: flow.scopes.slice() };
      function* captured(capturedClient: CodeClient, capturedFlow: AuthorizationCodeDescriptor): Iterable<string> {
        yield scheme; yield capturedClient.clientId; yield capturedClient.clientSecret ?? ""; yield capturedClient.redirectUri ?? "";
        yield redirectUri!; yield capturedFlow.authorizationUrl; yield capturedFlow.tokenUrl; yield capturedFlow.refreshUrl ?? "";
        yield* capturedFlow.scopes;
      }
      const owned = attemptInput(params, captured(client, ownedFlow));
      const scopes = owned.scopes ?? ownedFlow.scopes;
      qualifiedEndpoint(ownedFlow.authorizationUrl);
      qualifiedEndpoint(ownedFlow.tokenUrl);
      if (ownedFlow.refreshUrl !== null && ownedFlow.refreshUrl !== undefined) qualifiedEndpoint(ownedFlow.refreshUrl);
      qualifiedEndpoint(redirectUri, true);
      if (insecureHttp(ownedFlow.authorizationUrl, this.#deps.options.allowInsecureHttp) || insecureHttp(ownedFlow.tokenUrl, this.#deps.options.allowInsecureHttp)) return refuse();
      handle = randomAttemptSecret();
      pending = {
        scheme, generation: this.#generation(scheme), started: this.#monotonic(), binding: owned.sessionBinding,
        redirectUri, clientId: client.clientId, clientSecret: client.clientSecret, configuredRedirect: client.redirectUri,
        authorizationUrl: ownedFlow.authorizationUrl, tokenUrl: ownedFlow.tokenUrl, refreshUrl: ownedFlow.refreshUrl, configuredScopes: JSON.stringify(ownedFlow.scopes),
        requestedScopes: Object.freeze(scopes.slice()), resource: owned.resource, audience: owned.audience, accountContext: owned.accountContext,
        defense: owned.issuerDefense, state: randomAttemptSecret(), verifier: undefined,
        claimed: false, canceled: false, controller: new AbortController(), dispose: () => {},
      };
      this.#attempts.set(handle, pending);
      const attemptHandle = handle;
      const record = pending;
      const onAbort = () => this.#cancel(attemptHandle, record);
      owned.signal?.addEventListener("abort", onAbort, { once: true });
      const timer = setTimeout(onAbort, AUTHORIZATION_ATTEMPT_TTL_MS);
      // Node's timer must not keep a process alive; browsers return a number.
      (timer as unknown as { unref?: () => void }).unref?.();
      pending.dispose = () => { clearTimeout(timer); owned.signal?.removeEventListener("abort", onAbort); };
      const pkce = await generatePkce();
      if (pending.canceled || this.#generation(scheme) !== pending.generation || this.#monotonic() - pending.started >= AUTHORIZATION_ATTEMPT_TTL_MS) {
        this.#remove(handle, pending);
        return refuse();
      }
      pending.verifier = pkce.verifier;
      const extra = { ...owned.extra, ...(owned.resource === undefined ? {} : { resource: owned.resource }), ...(owned.audience === undefined ? {} : { audience: owned.audience }) };
      const url = buildAuthorizationUrl(ownedFlow, client.clientId, { redirectUri, scopes, state: pending.state, codeChallenge: pkce.challenge, extra });
      return { ok: true, value: Object.freeze({ attemptHandle: handle, url, expiresInMs: Math.max(0, AUTHORIZATION_ATTEMPT_TTL_MS - (this.#monotonic() - pending.started)) }) };
    } catch {
      if (handle !== undefined && pending !== undefined) this.#remove(handle, pending);
      return refuse();
    }
  }

  #current(pending: PendingAuthorization): boolean {
    if (pending.canceled || this.#loggingOut.has(pending.scheme) || this.#generation(pending.scheme) !== pending.generation || this.#monotonic() - pending.started >= AUTHORIZATION_ATTEMPT_TTL_MS) return false;
    const client = this.#client(pending.scheme);
    const flow = this.flowOf(pending.scheme);
    return typeof client !== "string" && typeof flow !== "string"
      && client.clientId === pending.clientId && client.clientSecret === pending.clientSecret && client.redirectUri === pending.configuredRedirect
      && flow.authorizationUrl === pending.authorizationUrl && flow.tokenUrl === pending.tokenUrl && flow.refreshUrl === pending.refreshUrl
      && flow.scopes.length <= MAX_AUTHORIZATION_INPUT_ITEMS && JSON.stringify(flow.scopes) === pending.configuredScopes;
  }

  /** Validate and atomically claim a callback. A claimed handle is never reopened. */
  async exchangeCode(scheme: string, params: ExchangeCodeParams): Promise<OAuthResult<TokenInfo>> {
    const operation = `oauth.${scheme}.exchangeCode`;
    const refuse = () => this.#failure(operation, "Authorization callback refused: invalid, expired, consumed or mismatched attempt. Begin a new authorization attempt if needed.");
    this.#sweep();
    let pending: PendingAuthorization | undefined;
    let code: string | undefined;
    let attemptHandle: string;
    let signal: AbortSignal | undefined;
    try {
      // Caller-owned parameter objects may change while token/store work awaits.
      attemptHandle = params.attemptHandle;
      signal = params.signal;
      pending = this.#attempts.get(attemptHandle);
      if (!pending || pending.claimed || pending.scheme !== scheme || params.sessionBinding !== pending.binding || signal?.aborted || !this.#current(pending)) return refuse();
      const callback = params.callbackUrl;
      if (typeof callback !== "string" || callback.length > MAX_OAUTH_CALLBACK_BYTES || utf8(callback).byteLength > MAX_OAUTH_CALLBACK_BYTES || callback.includes("#")) return refuse();
      const question = callback.indexOf("?");
      if (question < 0 || callback.slice(0, question) !== pending.redirectUri) return refuse();
      const fields = callbackFields(callback.slice(question + 1));
      if (fields.get("state") !== pending.state) return refuse();
      if (pending.defense.kind === "issuerResponse" && fields.get("iss") !== pending.defense.issuer) return refuse();
      const denied = fields.get("error");
      code = fields.get("code");
      if ((code === undefined) === (denied === undefined) || code === "" || denied === "" || (code !== undefined && (fields.has("error_description") || fields.has("error_uri")))) return refuse();
      // No await separates validation from this single-owner transition.
      pending.claimed = true;
      if (denied !== undefined) {
        this.#remove(attemptHandle, pending);
        return this.#failure(operation, "Authorization was denied. Begin a new attempt to authorize again.");
      }
    } catch { return refuse(); }
    const record = pending;
    const verifier = record.verifier;
    record.verifier = undefined;
    if (code === undefined || verifier === undefined) { this.#remove(attemptHandle, record); return refuse(); }
    const onAbort = () => this.#cancel(attemptHandle, record);
    try {
      signal?.addEventListener("abort", onAbort, { once: true });
      if (!this.#current(record)) return refuse();
      const fields: Array<[string, string]> = [["grant_type", "authorization_code"], ["code", code], ["redirect_uri", record.redirectUri], ["code_verifier", verifier]];
      const stored = await this.#tokenRequest(scheme, record.tokenUrl, { clientId: record.clientId, clientSecret: record.clientSecret, redirectUri: record.redirectUri }, fields, undefined, record.controller.signal);
      let committedEpoch: number | undefined;
      if (!this.#current(record) || !await this.store.setIfCurrent!(scheme, stored, () => {
        if (!this.#current(record)) return false;
        this.#mutatedToken(scheme);
        committedEpoch = this.#tokenEpoch(scheme);
        return true;
      })) return refuse();
      if (!this.#current(record) || this.#tokenEpoch(scheme) !== committedEpoch) return refuse();
      return { ok: true, value: info(scheme, stored) };
    } catch { return this.#failure(operation, "Authorization exchange did not complete. The code may have been consumed; begin a new authorization attempt."); }
    finally {
      try { signal?.removeEventListener("abort", onAbort); }
      finally { this.#remove(attemptHandle, record); }
    }
  }

  cancelAuthorization(scheme: string, attemptHandle: string, sessionBinding: string): OAuthResult<void> {
    const pending = this.#attempts.get(attemptHandle);
    if (!pending || pending.scheme !== scheme || pending.binding !== sessionBinding) return this.#failure(`oauth.${scheme}.cancelAuthorization`, "Authorization attempt is unavailable or belongs to a different session.");
    this.#cancel(attemptHandle, pending);
    return { ok: true, value: undefined };
  }

  /** Local logout: invalidate attempts and fence late exchange/refresh writes. */
  async logout(scheme: string): Promise<OAuthResult<void>> {
    this.#generations.set(scheme, this.#generation(scheme) + 1);
    this.#loggingOut.set(scheme, (this.#loggingOut.get(scheme) ?? 0) + 1);
    for (const [handle, pending] of this.#attempts) if (pending.scheme === scheme) this.#cancel(handle, pending);
    try { await this.store.delete(scheme); return { ok: true, value: undefined }; }
    catch { return this.#failure(`oauth.${scheme}.logout`, "The token store failed while removing local credentials."); }
    finally {
      const remaining = this.#loggingOut.get(scheme)! - 1;
      if (remaining === 0) this.#loggingOut.delete(scheme); else this.#loggingOut.set(scheme, remaining);
    }
  }

  /** The URL to send the user to. */
  lowLevelAuthorizationUrl(scheme: string, params: LowLevelAuthorizationUrlParams = {}): OAuthResult<string> {
    const operation = `oauth.${scheme}.authorizationUrl`;
    const flow = this.flowOf(scheme);
    if (typeof flow === "string") return this.#failure(operation, `Cannot build an authorization URL: ${flow}.`);
    const client = this.#client(scheme);
    if (typeof client === "string") return this.#failure(operation, `Cannot build an authorization URL: ${client}.`);
    const redirectUri = str(params.redirectUri) ?? client.redirectUri;
    if (redirectUri === undefined) {
      return this.#failure(operation, `Pass redirectUri, or set redirectUri in auth.${scheme}; the authorization URL needs one.`);
    }
    return { ok: true, value: buildAuthorizationUrl(flow, client.clientId, { ...params, redirectUri }) };
  }

  /** Trade an authorization code for tokens and store them. */
  async lowLevelExchangeCode(scheme: string, params: LowLevelExchangeCodeParams): Promise<OAuthResult<TokenInfo>> {
    const operation = `oauth.${scheme}.exchangeCode`;
    const flow = this.flowOf(scheme);
    if (typeof flow === "string") return this.#failure(operation, `Cannot exchange the code: ${flow}.`);
    const client = this.#client(scheme);
    if (typeof client === "string") return this.#failure(operation, `Cannot exchange the code: ${client}.`);
    const redirectUri = str(isRecord(params) ? params.redirectUri : undefined) ?? client.redirectUri;
    const code = isRecord(params) ? str(params.code) : undefined;
    if (code === undefined) return this.#failure(operation, "Pass the authorization code the server redirected back with as code.");
    if (redirectUri === undefined) {
      return this.#failure(operation, `Pass redirectUri, or set redirectUri in auth.${scheme}; it must equal the one used for the authorization URL.`);
    }
    const fields: Array<[string, string]> = [
      ["grant_type", "authorization_code"],
      ["code", code],
      ["redirect_uri", redirectUri],
    ];
    const verifier = str(params.codeVerifier);
    if (verifier !== undefined) fields.push(["code_verifier", verifier]);
    try {
      const stored = await this.#tokenRequest(scheme, flow.tokenUrl, client, fields, undefined);
      await this.store.set(scheme, stored);
      return { ok: true, value: info(scheme, stored) };
    } catch (error) {
      return this.#failure(operation, error instanceof TokenError ? error.message : `The token store failed while saving the tokens for ${scheme}.`);
    }
  }

  /** Refresh the stored token of `scheme` now (single flight). */
  async refresh(scheme: string): Promise<OAuthResult<TokenInfo>> {
    const operation = `oauth.${scheme}.refresh`;
    const generation = this.#generation(scheme);
    try {
      await this.token(scheme, true);
      const stored = await this.#read(scheme);
      if (this.#generation(scheme) !== generation) throw new TokenError("OAuth session changed; authorize again.");
      if (!stored) return this.#failure(operation, `No token is stored for ${scheme}.`);
      return { ok: true, value: info(scheme, stored) };
    } catch (error) {
      return this.#failure(operation, error instanceof TokenError ? error.message : `The token store failed while refreshing ${scheme}.`);
    }
  }

  /** The access token to send: the stored one, refreshed first when it
   * expires within {@link EXPIRY_SKEW_MS} (or `force`). Throws a
   * {@link TokenError} whose text is shown in the AUTH_FAILED envelope. */
  async token(scheme: string, force = false): Promise<string> {
    const generation = this.#generation(scheme);
    const tokenEpoch = this.#tokenEpoch(scheme);
    const stored = await this.#read(scheme);
    if (generation !== this.#generation(scheme) || tokenEpoch !== this.#tokenEpoch(scheme)) throw new TokenError("OAuth session changed; authorize again.");
    if (!stored) {
      throw new TokenError(
        `No OAuth2 token is stored for ${scheme}; begin with authorizationUrl(), then call exchangeCode() with its attempt handle, full callback URL and session binding, or put a token in ClientOptions.tokenStore.`,
      );
    }
    const fresh = stored.expiresAt === undefined || stored.expiresAt > this.#deps.now() + EXPIRY_SKEW_MS;
    if (fresh && !force) return stored.accessToken;
    const flightKey = `${generation}:${tokenEpoch}:${scheme}`;
    const running = this.#inflight.get(flightKey);
    if (running) return running;
    if (this.#inflight.size >= MAX_AUTHORIZATION_ATTEMPTS) throw new TokenError("OAuth refresh capacity is exhausted; wait for an owned refresh to settle.");
    const flight = this.#refreshStored(scheme, stored, force, generation, tokenEpoch);
    this.#inflight.set(flightKey, flight);
    try {
      return await flight;
    } finally {
      this.#inflight.delete(flightKey);
    }
  }

  /** Force a refresh after a 401, when the stored token has a refresh
   * token. Returns the new access token, or null when nothing changed. */
  async refreshAfterRejection(scheme: string, rejected: string): Promise<string | null> {
    let stored: StoredToken | undefined;
    const generation = this.#generation(scheme);
    const tokenEpoch = this.#tokenEpoch(scheme);
    try { stored = await this.#read(scheme); } catch { return null; }
    if (generation !== this.#generation(scheme) || tokenEpoch !== this.#tokenEpoch(scheme)) return null;
    if (!stored) return null;
    // Another call already replaced the rejected token.
    if (stored.accessToken !== rejected) return stored.accessToken;
    if (stored.refreshToken === undefined) return null;
    try {
      return await this.token(scheme, true);
    } catch {
      return null;
    }
  }

  async #read(scheme: string): Promise<StoredToken | undefined> {
    const generation = this.#generation(scheme);
    const tokenEpoch = this.#tokenEpoch(scheme);
    try {
      if (this.#loggingOut.has(scheme)) throw new Error();
      const stored = await this.store.get(scheme);
      if (this.#loggingOut.has(scheme) || generation !== this.#generation(scheme) || tokenEpoch !== this.#tokenEpoch(scheme)) throw new Error();
      return isRecord(stored) && typeof stored.accessToken === "string" && stored.accessToken !== "" ? stored : undefined;
    } catch {
      throw new TokenError(`The token store failed while reading the token for ${scheme}.`);
    }
  }

  async #refreshStored(scheme: string, stored: StoredToken, force: boolean, generation: number, tokenEpoch: number): Promise<string> {
    if (!this.#fencedStore()) throw new TokenError("OAuth refresh requires an atomic fenced token store.");
    const flow = this.flowOf(scheme);
    const client = this.#client(scheme);
    if (typeof flow === "string" || typeof client === "string") {
      throw new TokenError(`Cannot refresh the OAuth2 token for ${scheme}: ${typeof flow === "string" ? flow : client}.`);
    }
    if (stored.refreshToken === undefined) {
      throw new TokenError(
        force
          ? `The OAuth2 token for ${scheme} has no refresh token; authorize again with authorizationUrl() and exchangeCode().`
          : `The OAuth2 token for ${scheme} expired and has no refresh token; authorize again with authorizationUrl() and exchangeCode().`,
      );
    }
    const url = flow.refreshUrl ?? flow.tokenUrl;
    try { qualifiedEndpoint(url); }
    catch { throw new TokenError("Cannot refresh OAuth2 tokens: provide a valid endpoint without protected query fields."); }
    const fields: Array<[string, string]> = [
      ["grant_type", "refresh_token"],
      ["refresh_token", stored.refreshToken],
    ];
    let next: StoredToken;
    try {
      next = await this.#tokenRequest(scheme, url, client, fields, stored.refreshToken);
    } catch (error) {
      if (error instanceof TokenError && error.rejected && this.#generation(scheme) === generation) {
        // The server no longer accepts the refresh token: forget the tokens.
        try {
          await this.store.deleteIfCurrent!(scheme, current => {
            if (this.#generation(scheme) !== generation || this.#tokenEpoch(scheme) !== tokenEpoch || current?.accessToken !== stored.accessToken || current?.refreshToken !== stored.refreshToken) return false;
            this.#mutatedToken(scheme);
            return true;
          });
        } catch {
          // The failure below is what the caller needs.
        }
      }
      throw error;
    }
    try {
      if (this.#generation(scheme) !== generation || this.#tokenEpoch(scheme) !== tokenEpoch) throw new TokenError("OAuth session changed; authorize again.");
      if (!await this.store.setIfCurrent!(scheme, next, current => {
        if (this.#generation(scheme) !== generation || this.#loggingOut.has(scheme) || this.#tokenEpoch(scheme) !== tokenEpoch || current?.accessToken !== stored.accessToken || current?.refreshToken !== stored.refreshToken) return false;
        this.#mutatedToken(scheme);
        return true;
      })) throw new TokenError("OAuth session changed; authorize again.");
      if (this.#generation(scheme) !== generation || this.#tokenEpoch(scheme) !== tokenEpoch + 1) throw new TokenError("OAuth session changed; authorize again.");
    } catch {
      throw new TokenError(`The token store failed while saving the refreshed token for ${scheme}.`);
    }
    return next.accessToken;
  }

  /** One request to the token endpoint. `previous` is the refresh token a
   * response without one keeps. */
  async #tokenRequest(
    scheme: string,
    url: string,
    client: CodeClient,
    fields: Array<[string, string]>,
    previous: string | undefined,
    signal?: AbortSignal,
  ): Promise<StoredToken> {
    const maxResponseBytes = this.#tokenResponseLimit();
    if (insecureHttp(url, this.#deps.options.allowInsecureHttp)) throw new TokenError(INSECURE_HTTP_AUTH);
    const headers: Record<string, string> = { "Content-Type": "application/x-www-form-urlencoded", Accept: "application/json" };
    if (client.clientSecret !== undefined) {
      headers.Authorization = `Basic ${bytesToBase64(utf8(`${percentEncode(client.clientId)}:${percentEncode(client.clientSecret)}`))}`;
    } else {
      fields.push(["client_id", client.clientId]);
    }
    const outcome: AttemptOutcome = await attempt(this.#deps.fetch(), {
      url,
      method: "POST",
      headers,
      body: formBody(fields),
      redirect: "manual",
      timeoutMs: this.#deps.timeoutMs(),
      signal,
      maxResponseBytes,
    });
    if (outcome.kind !== "response" || outcome.body === null) {
      throw new TokenError(`The OAuth2 token endpoint for ${scheme} could not be reached; check network access and the token URL.`);
    }
    const decoded = decodeBody(outcome.body, outcome.headers, "application/json");
    if (outcome.status < 200 || outcome.status > 299) {
      const code = getPath(decoded.value, "error");
      const named = typeof code === "string" && TOKEN_ERROR_CODES.has(code) ? ` (${code})` : "";
      const error = new TokenError(
        `The OAuth2 token endpoint for ${scheme} answered HTTP ${outcome.status}${named}; check the code or refresh token, client credentials and redirect URI.`,
      );
      error.rejected = outcome.status >= 400 && outcome.status < 500;
      throw error;
    }
    const access = getPath(decoded.value, "access_token");
    if (typeof access !== "string" || access === "") throw new TokenError(`The OAuth2 token endpoint for ${scheme} returned no access_token.`);
    const token: StoredToken = { accessToken: access };
    const refresh = getPath(decoded.value, "refresh_token");
    if (typeof refresh === "string" && refresh !== "") token.refreshToken = refresh;
    else if (previous !== undefined) token.refreshToken = previous;
    const lifetime = getPath(decoded.value, "expires_in");
    if (typeof lifetime === "number" && Number.isFinite(lifetime)) token.expiresAt = this.#deps.now() + Math.max(0, lifetime) * 1000;
    const type = getPath(decoded.value, "token_type");
    if (typeof type === "string" && type !== "") token.tokenType = type;
    const scope = getPath(decoded.value, "scope");
    if (typeof scope === "string" && scope !== "") token.scope = scope;
    return token;
  }
}

function info(scheme: string, token: StoredToken): TokenInfo {
  return { scheme, expiresAt: token.expiresAt ?? null, scope: token.scope ?? null, refreshable: token.refreshToken !== undefined };
}

/** The helpers of one authorization-code scheme, as generated clients expose them. */
export class OAuthFlow {
  readonly scheme: string;
  readonly #session: OAuthSession;

  constructor(session: OAuthSession, scheme: string) {
    this.#session = session;
    this.scheme = scheme;
  }

  /** The tokens' store. */
  get tokenStore(): TokenStore {
    return this.#session.store;
  }

  /** Low-level PKCE primitive. Managed authorization generates and owns its own pair. */
  pkce(): Promise<Pkce> {
    return generatePkce();
  }

  /** The URL to send the user to. */
  authorizationUrl(params: AuthorizationUrlParams): Promise<OAuthResult<AuthorizationAttempt>> {
    return this.#session.authorizationUrl(this.scheme, params);
  }

  /** Trade the code from the redirect for tokens, stored for later calls. */
  exchangeCode(params: ExchangeCodeParams): Promise<OAuthResult<TokenInfo>> {
    return this.#session.exchangeCode(this.scheme, params);
  }

  /** Explicit compatibility primitive; the caller owns PKCE, CSRF and issuer validation. */
  lowLevelAuthorizationUrl(params: LowLevelAuthorizationUrlParams = {}): OAuthResult<string> {
    return this.#session.lowLevelAuthorizationUrl(this.scheme, params);
  }

  /** Explicit compatibility primitive; never use for a managed attempt. */
  lowLevelExchangeCode(params: LowLevelExchangeCodeParams): Promise<OAuthResult<TokenInfo>> {
    return this.#session.lowLevelExchangeCode(this.scheme, params);
  }

  /**
   * Cancel an owned managed attempt using the same session binding.
   * In-flight work may settle later but cannot commit that canceled attempt.
   */
  cancelAuthorization(attemptHandle: string, sessionBinding: string): OAuthResult<void> {
    return this.#session.cancelAuthorization(this.scheme, attemptHandle, sessionBinding);
  }

  /**
   * Invalidate pending attempts and remove this scheme's local tokens.
   * This does not revoke credentials at the provider. Store failures are diagnostics.
   */
  logout(): Promise<OAuthResult<void>> { return this.#session.logout(this.scheme); }

  /** Refresh the stored token now. Calls refresh on their own when it expires. */
  refresh(): Promise<OAuthResult<TokenInfo>> {
    return this.#session.refresh(this.scheme);
  }
}
