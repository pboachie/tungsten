// SPDX-License-Identifier: Apache-2.0
/**
 * Classification of HTTP responses and lost requests into results and
 * diagnostic envelopes (planning/06 "Diagnostic error envelope",
 * "Unknown outcome", planning/04 "Remediation table resolution").
 */
import { categoryForStatus, diagnostic, GENERIC_REMEDIATION, isCategory, isRetryable } from "./envelope.js";
import type {
  ApiDescriptor,
  Category,
  Diagnostic,
  OperationDescriptor,
  RemediationEntry,
  ResponseDescriptor,
  Retryable,
} from "./types.js";
import { getPath, isRecord } from "./util.js";

/** Headers that carry a request id, in lookup order. */
const REQUEST_ID_HEADERS = ["x-request-id", "request-id", "x-correlation-id", "x-amzn-requestid", "cf-ray"];

export function requestIdOf(headers: Record<string, string>): string | null {
  for (const name of REQUEST_ID_HEADERS) {
    const value = headers[name];
    if (value) return value.slice(0, 200);
  }
  return null;
}

/** The media type without parameters, lowercased; "" when absent. */
export function mediaTypeOf(headers: Record<string, string>): string {
  return (headers["content-type"] ?? "").split(";")[0]?.trim().toLowerCase() ?? "";
}

export function isJsonMedia(media: string): boolean {
  return media === "application/json" || media.endsWith("+json") || media === "text/json";
}

/** The response descriptor matching a status: exact, then `NXX`, then `default`. */
export function matchResponse(responses: readonly ResponseDescriptor[], status: number): ResponseDescriptor | undefined {
  const list = Array.isArray(responses) ? (responses.filter(isRecord) as unknown as ResponseDescriptor[]) : [];
  return (
    list.find((r) => r.status === status) ??
    list.find((r) => typeof r.status === "string" && r.status === `${Math.floor(status / 100)}XX`) ??
    list.find((r) => r.status === "default")
  );
}

/** A decoded response body. */
export interface DecodedBody {
  /** Parsed JSON, text, bytes, or undefined for an empty body. */
  value: unknown;
  json: boolean;
  /** JSON was announced but did not parse. */
  invalidJson: boolean;
  empty: boolean;
}

export function decodeBody(bytes: Uint8Array, headers: Record<string, string>, declared: string | null): DecodedBody {
  if (bytes.byteLength === 0) return { value: undefined, json: false, invalidJson: false, empty: true };
  const media = mediaTypeOf(headers) || (declared ?? "").toLowerCase();
  const text = (): string => new TextDecoder().decode(bytes);
  if (isJsonMedia(media)) {
    const raw = text();
    try {
      return { value: JSON.parse(raw) as unknown, json: true, invalidJson: false, empty: false };
    } catch {
      return { value: raw, json: false, invalidJson: true, empty: false };
    }
  }
  if (media === "" || media.startsWith("text/") || media === "application/problem+xml" || media.endsWith("+xml") || media === "application/xml") {
    const raw = text();
    if (media === "" && /^\s*[[{]/.test(raw)) {
      try {
        return { value: JSON.parse(raw) as unknown, json: true, invalidJson: false, empty: false };
      } catch {
        // not JSON after all; keep the text
      }
    }
    return { value: raw, json: false, invalidJson: false, empty: false };
  }
  return { value: bytes, json: false, invalidJson: false, empty: false };
}

const MESSAGE_FIELDS = ["message", "error.message", "detail", "error_description", "title", "error"];

function serverMessage(body: unknown): string | null {
  for (const field of MESSAGE_FIELDS) {
    const value = getPath(body, field);
    if (typeof value === "string" && value.trim() !== "") return value.trim().slice(0, 200);
  }
  return null;
}

function errorCode(op: OperationDescriptor, body: unknown): string | null {
  const field = op.errorCodeField;
  if (!field) return null;
  const value = getPath(body, field);
  if (typeof value === "string" && value !== "") return value.slice(0, 200);
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  return null;
}

function remediationEntry(table: unknown, code: string | null): RemediationEntry | undefined {
  if (code === null || !isRecord(table)) return undefined;
  const entry = Object.prototype.hasOwnProperty.call(table, code) ? table[code] : undefined;
  return isRecord(entry) ? (entry as RemediationEntry) : undefined;
}

/** Context shared by every classification of one call. */
export interface CallContext {
  api: ApiDescriptor;
  op: OperationDescriptor;
  /** Key sent with the request, if any (never shown). */
  key: string | null;
  /** The key's header name, for remediation text. */
  keyHeader: string;
  attempts: number;
}

function verifyHint(ctx: CallContext): string | null {
  const verify = ctx.op.agent.verify;
  if (!verify || typeof verify.operation !== "string") return null;
  const keys = isRecord(verify.args) ? Object.keys(verify.args) : [];
  const withArgs = keys.length > 0 ? ` with ${keys.join(", ")}` : "";
  return `Call ${verify.operation}${withArgs} to check whether ${ctx.op.id} took effect before doing anything else.`;
}

/** OUTCOME_UNKNOWN for a mutation whose effect cannot be known. */
export function outcomeUnknown(ctx: CallContext, cause: string, fields: Partial<Diagnostic> = {}): Diagnostic {
  const op = ctx.op;
  let rule: string;
  if (op.agent.idempotency.policy === "content_identity") {
    rule = "If you retry, resend the identical bytes only; the body is its own identity.";
  } else if (ctx.key !== null) {
    rule = `If you retry, reuse the SAME ${ctx.keyHeader} value; a new key can apply the effect twice.`;
  } else {
    rule = "This operation has no idempotency key, so repeating it can apply the effect twice; do not repeat it before checking.";
  }
  return diagnostic(op.id, "OUTCOME_UNKNOWN", {
    remediation: `${cause} The server may or may not have applied ${op.id}. ${rule}`,
    retryable: "same_key_only",
    next_action: fields.next_action ?? verifyHint(ctx),
    http_status: fields.http_status ?? null,
    code: fields.code ?? null,
    request_id: fields.request_id ?? null,
    retry_after_ms: fields.retry_after_ms ?? null,
    trace: { attempts: ctx.attempts },
  });
}

export function isMutation(op: OperationDescriptor): boolean {
  return op.agent.safety !== "read_only";
}

/** The classification of an HTTP error status. */
export function classifyError(
  ctx: CallContext,
  status: number,
  headers: Record<string, string>,
  decoded: DecodedBody,
  retryAfterMs: number | null,
): Diagnostic {
  const { api, op } = ctx;
  const requestId = requestIdOf(headers);
  const base = { http_status: status, request_id: requestId, retry_after_ms: retryAfterMs, trace: { attempts: ctx.attempts } };
  const code = decoded.json ? errorCode(op, decoded.value) : null;

  if (op.status.kind === "gated" && status === op.status.disabledStatus) {
    const gates = isRecord(api.gates) ? api.gates : {};
    const text = typeof gates[op.status.envVar] === "string" ? (gates[op.status.envVar] as string) : null;
    return diagnostic(op.id, "GATE_DISABLED", {
      ...base,
      code,
      remediation:
        text ??
        `This deployment disables ${op.id} because ${op.status.envVar} is off (HTTP ${status}). It is a deployment setting, not a missing resource; do not retry.`,
      retryable: "never",
    });
  }

  const declared = matchResponse(op.responses, status);
  const ambiguous = (Array.isArray(api.ambiguousStatuses) && api.ambiguousStatuses.includes(status)) || declared?.kind === "ambiguous";
  if (ambiguous && isMutation(op)) {
    const entry = remediationEntry(op.agent.remediation, code) ?? remediationEntry(api.errorCodes, code);
    return outcomeUnknown(ctx, `HTTP ${status} leaves the outcome of this call unknown.`, {
      ...base,
      code,
      next_action: entry?.next_action ?? null,
    });
  }

  const entry = remediationEntry(op.agent.remediation, code) ?? remediationEntry(api.errorCodes, code);
  let category: Category;
  let retryable: Retryable | undefined;
  let text: string | null = null;
  let nextAction: string | null = null;
  if (entry) {
    category = isCategory(entry.category) ? entry.category : categoryForStatus(status, decoded.json);
    retryable = isRetryable(entry.retryable) ? entry.retryable : undefined;
    text = typeof entry.text === "string" ? entry.text : null;
    nextAction = typeof entry.next_action === "string" ? entry.next_action : null;
  } else {
    const media = decoded.empty ? "none" : mediaTypeOf(headers) || "none";
    const nonJson = decoded.json
      ? undefined
      : (Array.isArray(api.nonJson) ? api.nonJson : []).find(
          (n) => isRecord(n) && n.status === status && (n.media === media || (n.media === "none" && decoded.empty)),
        );
    if (nonJson && isCategory(nonJson.category)) {
      category = nonJson.category;
      retryable = isRetryable(nonJson.retryable) ? nonJson.retryable : undefined;
      text = typeof nonJson.text === "string" ? nonJson.text : null;
    } else {
      category = categoryForStatus(status, decoded.json);
    }
  }
  if (text === null) {
    const said = decoded.json ? serverMessage(decoded.value) : null;
    text = `${GENERIC_REMEDIATION[category]}${said ? ` Server message: ${JSON.stringify(said)}` : ""}`;
  }
  return diagnostic(op.id, category, {
    ...base,
    code,
    remediation: text,
    ...(retryable ? { retryable } : {}),
    next_action: nextAction,
  });
}
