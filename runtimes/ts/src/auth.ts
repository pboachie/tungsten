// SPDX-License-Identifier: Apache-2.0
/**
 * Auth resolution (planning/06 "Auth profiles"): an operation's security is
 * an OR of AND-sets of scheme names; the first alternative that
 * `ClientOptions.auth` fully satisfies is applied. A composite profile
 * satisfies the scheme names in its `satisfies` list when every part it
 * needs for this request is configured.
 */
import type { ApiDescriptor, AuthConfig, AuthSchemeDescriptor, HttpMethod, OperationDescriptor } from "./types.js";
import { bytesToBase64, isRecord, utf8 } from "./util.js";

type Composite = Extract<AuthSchemeDescriptor, { kind: "composite" }>;
type OAuth2 = Extract<AuthSchemeDescriptor, { kind: "oauth2" }>;

/** Credentials to apply to one request. */
export interface AuthPlan {
  headers: Array<{ name: string; value: string; secret: boolean }>;
  cookies: Array<{ name: string; value: string }>;
  query: Array<{ name: string; value: string }>;
}

export type AuthResolution = { ok: true; plan: AuthPlan; secrets: string[] } | { ok: false; remediation: string };

/** Fetches (and caches) OAuth2 client-credentials tokens. */
export type TokenSource = (scheme: OAuth2, config: Record<string, string>) => Promise<string>;

/** Thrown by a token source; the message is shown in the AUTH_FAILED envelope. */
export class TokenError extends Error {}

const SAFE_METHODS: ReadonlySet<HttpMethod> = new Set<HttpMethod>(["GET", "HEAD", "OPTIONS", "TRACE"]);

export function isSafeMethod(method: HttpMethod): boolean {
  return SAFE_METHODS.has(method);
}

interface Satisfier {
  apply: (plan: AuthPlan, secrets: string[], tokens: TokenSource) => Promise<string | null>;
  key: string;
}

type Attempt = { ok: true; satisfier: Satisfier } | { ok: false; missing: string[] };

function compositeConfigKey(part: Composite["parts"][number]): string {
  switch (part.kind) {
    case "cookie":
      return part.name;
    case "bearer":
      return "bearer";
    case "header":
      return part.equalsCookie ?? part.fromConfig ?? part.name;
  }
}

function checkPrefix(token: string, prefix: string | null, what: string): string | null {
  return prefix && !token.startsWith(prefix) ? `The ${what} credential must start with "${prefix}"; check that the right kind of token is configured.` : null;
}

function tryComposite(c: Composite, config: unknown, method: HttpMethod): Attempt {
  if (!isRecord(config)) return { ok: false, missing: [`auth.${c.name} (an object for the composite profile ${c.name})`] };
  const values = config as Record<string, unknown>;
  const missing: string[] = [];
  const mutation = !isSafeMethod(method);
  for (const part of c.parts) {
    if (part.kind === "header" && part.mutationOnly && !mutation) continue;
    const key = compositeConfigKey(part);
    const value = values[key];
    if (typeof value !== "string" || value === "") {
      const role =
        part.kind === "cookie"
          ? `cookie ${part.name}`
          : part.kind === "bearer"
            ? "bearer token"
            : `header ${part.name}${part.equalsCookie ? ` (equal to cookie ${part.equalsCookie})` : ""}`;
      missing.push(`auth.${c.name}[${JSON.stringify(key)}] (${role} of the composite profile ${c.name})`);
    }
  }
  if (missing.length > 0) return { ok: false, missing };
  return {
    ok: true,
    satisfier: {
      key: `composite:${c.name}`,
      apply: async (plan, secrets) => {
        for (const part of c.parts) {
          if (part.kind === "header" && part.mutationOnly && !mutation) continue;
          const value = values[compositeConfigKey(part)] as string;
          if (part.kind === "cookie") {
            plan.cookies.push({ name: part.name, value });
            secrets.push(value);
          } else if (part.kind === "bearer") {
            const problem = checkPrefix(value, part.prefix, `${c.name} bearer`);
            if (problem) return problem;
            plan.headers.push({ name: "Authorization", value: `Bearer ${value}`, secret: true });
            secrets.push(value);
          } else {
            const secret = part.fromConfig === null;
            plan.headers.push({ name: part.name, value, secret });
            if (secret) secrets.push(value);
          }
        }
        return null;
      },
    },
  };
}

function tryDirect(scheme: Exclude<AuthSchemeDescriptor, Composite>, config: unknown): Attempt {
  const name = scheme.name;
  if (scheme.kind === "oauth2" && isRecord(config)) {
    const cfg = config as Record<string, unknown>;
    if (typeof cfg.clientId === "string" && typeof cfg.clientSecret === "string" && scheme.tokenUrl) {
      const strings = { clientId: cfg.clientId, clientSecret: cfg.clientSecret };
      return {
        ok: true,
        satisfier: {
          key: `scheme:${name}`,
          apply: async (plan, secrets, tokens) => {
            secrets.push(strings.clientSecret);
            let token: string;
            try {
              token = await tokens(scheme, strings);
            } catch (error) {
              return error instanceof TokenError ? error.message : `Could not obtain an OAuth2 token for ${name}.`;
            }
            secrets.push(token);
            plan.headers.push({ name: "Authorization", value: `Bearer ${token}`, secret: true });
            return null;
          },
        },
      };
    }
    return { ok: false, missing: [`auth.${name} (an access token, or {clientId, clientSecret} for the token URL)`] };
  }
  if (typeof config !== "string" || config === "") {
    const what =
      scheme.kind === "http_basic"
        ? `"user:password" for HTTP Basic scheme ${name}`
        : scheme.kind === "api_key"
          ? `API key for scheme ${name} (${scheme.in} ${scheme.wire})`
          : `bearer token for scheme ${name}`;
    return { ok: false, missing: [`auth.${name} (${what})`] };
  }
  const secret = config;
  return {
    ok: true,
    satisfier: {
      key: `scheme:${name}`,
      apply: async (plan, secrets) => {
        secrets.push(secret);
        switch (scheme.kind) {
          case "api_key":
            if (scheme.in === "header") plan.headers.push({ name: scheme.wire, value: secret, secret: true });
            else if (scheme.in === "query") plan.query.push({ name: scheme.wire, value: secret });
            else plan.cookies.push({ name: scheme.wire, value: secret });
            return null;
          case "http_bearer":
          case "oauth2": {
            const problem = scheme.kind === "http_bearer" ? checkPrefix(secret, scheme.prefix, name) : null;
            if (problem) return problem;
            plan.headers.push({ name: "Authorization", value: `Bearer ${secret}`, secret: true });
            return null;
          }
          case "http_basic": {
            const encoded = bytesToBase64(utf8(secret));
            secrets.push(encoded);
            plan.headers.push({ name: "Authorization", value: `Basic ${encoded}`, secret: true });
            return null;
          }
        }
      },
    },
  };
}

function trySchemeName(api: ApiDescriptor, auth: AuthConfig, name: string, method: HttpMethod): Attempt {
  const schemes = Array.isArray(api.auth) ? api.auth.filter(isRecord) as AuthSchemeDescriptor[] : [];
  const missing: string[] = [];
  for (const scheme of schemes) {
    if (scheme.kind === "composite" && Array.isArray(scheme.satisfies) && scheme.satisfies.includes(name) && Array.isArray(scheme.parts)) {
      if (auth[scheme.name] === undefined) {
        missing.push(`auth.${scheme.name} (composite profile ${scheme.name})`);
        continue;
      }
      const attempt = tryComposite(scheme, auth[scheme.name], method);
      if (attempt.ok) return attempt;
      missing.push(...attempt.missing);
    }
  }
  const direct = schemes.find((s): s is Exclude<AuthSchemeDescriptor, Composite> => s.name === name && s.kind !== "composite");
  if (direct) {
    const attempt = tryDirect(direct, auth[name]);
    if (attempt.ok) return attempt;
    // When a composite profile covers the name, its missing parts explain
    // the gap better than the raw spec scheme.
    if (missing.length === 0) missing.push(...attempt.missing);
  } else if (missing.length === 0) {
    missing.push(`a scheme named ${name} (the API descriptor does not define it)`);
  }
  return { ok: false, missing };
}

/** Whether `auth` has an entry for the scheme `name` or a composite profile satisfying it. */
function configured(api: ApiDescriptor, auth: AuthConfig, name: string): boolean {
  if (auth[name] !== undefined) return true;
  const schemes = Array.isArray(api.auth) ? api.auth.filter(isRecord) : [];
  return schemes.some(
    (s) => s.kind === "composite" && Array.isArray(s.satisfies) && s.satisfies.includes(name) && typeof s.name === "string" && auth[s.name] !== undefined,
  );
}

/** Resolve the credentials for `op`. Never throws; a token source failure
 * becomes an `ok: false` resolution. */
export async function resolveAuth(
  api: ApiDescriptor,
  op: OperationDescriptor,
  auth: AuthConfig,
  method: HttpMethod,
  tokens: TokenSource,
): Promise<AuthResolution> {
  const security = Array.isArray(op.security) ? op.security.filter(Array.isArray) : [];
  if (security.length === 0) return { ok: true, plan: { headers: [], cookies: [], query: [] }, secrets: [] };
  let best: { missing: string[]; touched: boolean } | null = null;
  // An empty alternative (anonymous access) is the fallback, never preferred
  // over configured credentials.
  const ordered = [...security.filter((alt) => alt.length > 0), ...security.filter((alt) => alt.length === 0)];
  for (const alternative of ordered) {
    const names = alternative.filter((n): n is string => typeof n === "string");
    const satisfiers = new Map<string, Satisfier>();
    const missing: string[] = [];
    for (const name of names) {
      const attempt = trySchemeName(api, auth, name, method);
      if (attempt.ok) satisfiers.set(attempt.satisfier.key, attempt.satisfier);
      else missing.push(...attempt.missing);
    }
    if (missing.length === 0) {
      const plan: AuthPlan = { headers: [], cookies: [], query: [] };
      const secrets: string[] = [];
      for (const satisfier of satisfiers.values()) {
        const problem = await satisfier.apply(plan, secrets, tokens);
        if (problem) return { ok: false, remediation: `${problem} Operation ${op.id} was not sent.` };
      }
      return { ok: true, plan, secrets };
    }
    // Report the alternative the caller started to configure, then the one
    // with the fewest missing pieces.
    const unique = [...new Set(missing)];
    const touched = names.some((name) => configured(api, auth, name));
    if (best === null || (touched && !best.touched) || (touched === best.touched && unique.length < best.missing.length)) {
      best = { missing: unique, touched };
    }
  }
  const alternatives = security.map((alt) => alt.join(" AND ") || "no credentials").join(" OR ");
  const missingText = best ? best.missing.join("; ") : "credentials";
  return {
    ok: false,
    remediation: `Operation ${op.id} needs ${alternatives}. Missing: ${missingText}. Configure it in ClientOptions.auth and call again; the request was not sent.`,
  };
}
