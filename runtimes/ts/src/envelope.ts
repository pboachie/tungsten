// SPDX-License-Identifier: Apache-2.0
/**
 * The diagnostic error envelope (planning/06): construction with stable
 * field order, default `retryable` per category, status → category
 * mapping and the generic remediation texts used when neither the
 * operation nor the API has a specific entry.
 */
import type { Category, Diagnostic, Retryable } from "./types.js";
import { REDACTED } from "./util.js";

const CATEGORIES: readonly Category[] = [
  "VALIDATION_FAILED",
  "MALFORMED_REQUEST",
  "REQUEST_TOO_LARGE",
  "AUTH_FAILED",
  "NOT_FOUND",
  "CONFLICT",
  "PRECONDITION_FAILED",
  "RATE_LIMITED",
  "UPSTREAM_UNAVAILABLE",
  "OUTCOME_UNKNOWN",
  "TRANSPORT_FAILED",
  "CONFIRMATION_REQUIRED",
  "GATE_DISABLED",
  "UNEXPECTED_RESPONSE",
];

const RETRYABLE_VALUES: readonly Retryable[] = ["never", "after_delay", "same_key_only", "after_remediation"];

/** Default `retryable` per category (planning/06 table). */
export const DEFAULT_RETRYABLE: Readonly<Record<Category, Retryable>> = {
  VALIDATION_FAILED: "never",
  MALFORMED_REQUEST: "never",
  REQUEST_TOO_LARGE: "never",
  AUTH_FAILED: "never",
  NOT_FOUND: "never",
  CONFLICT: "never",
  PRECONDITION_FAILED: "never",
  RATE_LIMITED: "after_delay",
  UPSTREAM_UNAVAILABLE: "after_delay",
  OUTCOME_UNKNOWN: "same_key_only",
  TRANSPORT_FAILED: "after_delay",
  CONFIRMATION_REQUIRED: "never",
  GATE_DISABLED: "never",
  UNEXPECTED_RESPONSE: "never",
};

/** Remediation used when no manifest entry applies. */
export const GENERIC_REMEDIATION: Readonly<Record<Category, string>> = {
  VALIDATION_FAILED:
    "The server rejected the arguments. Fix the parameter the API error names and call again; the same request fails the same way.",
  MALFORMED_REQUEST:
    "The server could not parse the request. Check the format of path and query parameters (for example canonical UUIDs); do not retry unchanged.",
  REQUEST_TOO_LARGE: "The request body is larger than the server accepts. Send a smaller body; do not retry unchanged.",
  AUTH_FAILED:
    "The credential is missing, expired, revoked or lacks permission. Do not retry; fix the credential configured in ClientOptions.auth.",
  NOT_FOUND:
    "The resource does not exist or is not visible to this credential. Check the identifiers; do not retry unchanged.",
  CONFLICT:
    "The request conflicts with the resource's current state. Read the current state before deciding what to do; do not retry unchanged.",
  PRECONDITION_FAILED:
    "A precondition of this operation is not met (account, billing or resource state). Resolve it first; do not retry unchanged.",
  RATE_LIMITED: "Rate limited. Wait retry_after_ms (or a few seconds when it is null) before calling again.",
  UPSTREAM_UNAVAILABLE: "The service is temporarily unavailable. Wait, then call again.",
  OUTCOME_UNKNOWN:
    "The server may or may not have applied this call. Check whether it took effect before doing anything else.",
  TRANSPORT_FAILED:
    "The request could not be delivered (DNS, TLS or connection failure), so the server did not receive it. Check baseUrl and network access, then call again.",
  CONFIRMATION_REQUIRED: "This operation needs confirmation: call preview(...) and pass its confirmation_token.",
  GATE_DISABLED: "This operation is disabled on this deployment. It is a deployment setting; do not retry.",
  UNEXPECTED_RESPONSE: "The server's response did not match the API description. Do not retry blindly; report it.",
};

export function isCategory(value: unknown): value is Category {
  return typeof value === "string" && (CATEGORIES as readonly string[]).includes(value);
}

export function isRetryable(value: unknown): value is Retryable {
  return typeof value === "string" && (RETRYABLE_VALUES as readonly string[]).includes(value);
}

export type DiagnosticFields = Partial<Omit<Diagnostic, "status" | "category" | "operation">>;

/** Build an envelope with every field present, in the documented order. */
export function diagnostic(operation: string, category: Category, fields: DiagnosticFields = {}): Diagnostic {
  return {
    status: "error",
    category,
    operation,
    http_status: fields.http_status ?? null,
    code: fields.code ?? null,
    failed_parameter: fields.failed_parameter ?? null,
    received_value: fields.received_value === undefined ? null : fields.received_value,
    expected: fields.expected ?? null,
    remediation: fields.remediation ?? GENERIC_REMEDIATION[category],
    retryable: fields.retryable ?? DEFAULT_RETRYABLE[category],
    retry_after_ms: fields.retry_after_ms ?? null,
    next_action: fields.next_action ?? null,
    request_id: fields.request_id ?? null,
    trace: { attempts: fields.trace?.attempts ?? 0 },
  };
}

/** Category of an HTTP error status when no manifest entry says otherwise. */
export function categoryForStatus(status: number, jsonBody: boolean): Category {
  switch (status) {
    case 400:
      return jsonBody ? "VALIDATION_FAILED" : "MALFORMED_REQUEST";
    case 401:
    case 403:
    case 407:
      return "AUTH_FAILED";
    case 404:
    case 410:
      return "NOT_FOUND";
    case 409:
      return "CONFLICT";
    case 402:
    case 412:
    case 428:
      return "PRECONDITION_FAILED";
    case 413:
      return "REQUEST_TOO_LARGE";
    case 422:
      return "VALIDATION_FAILED";
    case 429:
      return "RATE_LIMITED";
    case 408:
    case 502:
    case 503:
    case 504:
      return "UPSTREAM_UNAVAILABLE";
  }
  if (status >= 500 && status <= 599) return "UPSTREAM_UNAVAILABLE";
  if (status >= 400 && status <= 499) return jsonBody ? "VALIDATION_FAILED" : "MALFORMED_REQUEST";
  return "UNEXPECTED_RESPONSE";
}

/** Replace every occurrence of the given secrets (four characters or
 * more) in a text. */
export function scrubText(text: string, secrets: ReadonlySet<string>): string {
  let out = text;
  for (const secret of secrets) {
    if (secret.length >= 4 && out.includes(secret)) out = out.split(secret).join(REDACTED);
  }
  return out;
}

/** Replace every occurrence of the given secrets in the envelope's string
 * fields. A last line of defence: no field is built from a secret. */
export function scrubDiagnostic(d: Diagnostic, secrets: ReadonlySet<string>): Diagnostic {
  if (secrets.size === 0) return d;
  const scrub = (text: string): string => scrubText(text, secrets);
  const scrubValue = (value: unknown): unknown => {
    if (typeof value === "string") return scrub(value);
    if (value === null || typeof value !== "object") return value;
    try {
      return JSON.parse(scrub(JSON.stringify(value))) as unknown;
    } catch {
      return REDACTED;
    }
  };
  return {
    ...d,
    code: d.code === null ? null : scrub(d.code),
    failed_parameter: d.failed_parameter === null ? null : scrub(d.failed_parameter),
    received_value: scrubValue(d.received_value),
    expected: d.expected === null ? null : scrub(d.expected),
    remediation: scrub(d.remediation),
    next_action: d.next_action === null ? null : scrub(d.next_action),
    request_id: d.request_id === null ? null : scrub(d.request_id),
  };
}
