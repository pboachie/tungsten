// SPDX-License-Identifier: Apache-2.0
/**
 * OAuth2 authorization-code helpers (planning/06 "Auth profiles"): PKCE
 * (RFC 7636, S256), the authorization URL, the code exchange, refresh with
 * a single flight per scheme, and the token store behind an
 * `authorizationCode` scheme.
 *
 * Credentials: `ClientOptions.auth[scheme] = { flow: "authorizationCode",
 * clientId, clientSecret?, redirectUri? }`. Tokens live in
 * `ClientOptions.tokenStore` (memory by default), never in the options,
 * envelopes or logs.
 */
import { TokenError } from "./auth.js";
import { attempt, type AttemptOutcome } from "./transport.js";
import type { ApiDescriptor, AuthorizationCodeDescriptor, ClientOptions, Diagnostic } from "./types.js";
import { base64url, bytesToBase64, getPath, isRecord, utf8 } from "./util.js";
import { decodeBody } from "./classify.js";
import { diagnostic } from "./envelope.js";

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

export type OAuthResult<T> = { ok: true; value: T } | { ok: false; error: Diagnostic };

export interface AuthorizationUrlParams {
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

export interface ExchangeCodeParams {
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
export function buildAuthorizationUrl(flow: AuthorizationCodeDescriptor, clientId: string, p: AuthorizationUrlParams & { redirectUri: string }): string {
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

  /** The URL to send the user to. */
  authorizationUrl(scheme: string, params: AuthorizationUrlParams = {}): OAuthResult<string> {
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
  async exchangeCode(scheme: string, params: ExchangeCodeParams): Promise<OAuthResult<TokenInfo>> {
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
    try {
      await this.token(scheme, true);
      const stored = await this.store.get(scheme);
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
    const stored = await this.#read(scheme);
    if (!stored) {
      throw new TokenError(
        `No OAuth2 token is stored for ${scheme}; send the user to authorizationUrl(), then call exchangeCode() with the code, or put a token in ClientOptions.tokenStore.`,
      );
    }
    const fresh = stored.expiresAt === undefined || stored.expiresAt > this.#deps.now() + EXPIRY_SKEW_MS;
    if (fresh && !force) return stored.accessToken;
    const running = this.#inflight.get(scheme);
    if (running) return running;
    const flight = this.#refreshStored(scheme, stored, force);
    this.#inflight.set(scheme, flight);
    try {
      return await flight;
    } finally {
      this.#inflight.delete(scheme);
    }
  }

  /** Force a refresh after a 401, when the stored token has a refresh
   * token. Returns the new access token, or null when nothing changed. */
  async refreshAfterRejection(scheme: string, rejected: string): Promise<string | null> {
    const stored = await this.#read(scheme);
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
    try {
      const stored = await this.store.get(scheme);
      return isRecord(stored) && typeof stored.accessToken === "string" && stored.accessToken !== "" ? stored : undefined;
    } catch {
      throw new TokenError(`The token store failed while reading the token for ${scheme}.`);
    }
  }

  async #refreshStored(scheme: string, stored: StoredToken, force: boolean): Promise<string> {
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
    const fields: Array<[string, string]> = [
      ["grant_type", "refresh_token"],
      ["refresh_token", stored.refreshToken],
    ];
    let next: StoredToken;
    try {
      next = await this.#tokenRequest(scheme, flow.refreshUrl ?? flow.tokenUrl, client, fields, stored.refreshToken);
    } catch (error) {
      if (error instanceof TokenError && error.rejected) {
        // The server no longer accepts the refresh token: forget the tokens.
        try {
          await this.store.delete(scheme);
        } catch {
          // The failure below is what the caller needs.
        }
      }
      throw error;
    }
    try {
      await this.store.set(scheme, next);
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
  ): Promise<StoredToken> {
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
      signal: undefined,
    });
    if (outcome.kind !== "response" || outcome.body === null) {
      throw new TokenError(`The OAuth2 token endpoint for ${scheme} could not be reached; check network access and the token URL.`);
    }
    const decoded = decodeBody(outcome.body, outcome.headers, "application/json");
    if (outcome.status < 200 || outcome.status > 299) {
      const code = getPath(decoded.value, "error");
      const named = typeof code === "string" && /^[a-z_]{1,40}$/.test(code) ? ` (${code})` : "";
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

  /** A fresh PKCE pair; pass `challenge` to {@link authorizationUrl} and keep `verifier` for {@link exchangeCode}. */
  pkce(): Promise<Pkce> {
    return generatePkce();
  }

  /** The URL to send the user to. */
  authorizationUrl(params: AuthorizationUrlParams = {}): OAuthResult<string> {
    return this.#session.authorizationUrl(this.scheme, params);
  }

  /** Trade the code from the redirect for tokens, stored for later calls. */
  exchangeCode(params: ExchangeCodeParams): Promise<OAuthResult<TokenInfo>> {
    return this.#session.exchangeCode(this.scheme, params);
  }

  /** Refresh the stored token now. Calls refresh on their own when it expires. */
  refresh(): Promise<OAuthResult<TokenInfo>> {
    return this.#session.refresh(this.scheme);
  }
}
