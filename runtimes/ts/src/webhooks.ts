// SPDX-License-Identifier: Apache-2.0
/**
 * Incoming webhook verification with no framework dependency.
 *
 * The first adapter is the Stripe-compatible signature scheme: a header such
 * as `Stripe-Signature: t=1700000000,v1=<hex>` where `v1` is the lowercase
 * hex HMAC-SHA256 of `"<t>.<raw body>"` under the endpoint secret. Pass the
 * raw request bytes exactly as received, before any JSON parsing.
 *
 * Checks run in a fixed order, identical in every tungsten runtime, and the
 * first failure is reported: body size, secret presence, header presence,
 * header syntax, signature, timestamp tolerance, JSON payload, typed decoding
 * (`parse`), replay claim.
 * Failures carry a stable `reason` and a message that never contains the
 * secret, the signature or the payload.
 *
 * @module
 */

/** Default largest accepted body: 1 MiB. */
export const DEFAULT_WEBHOOK_MAX_BODY_BYTES = 1_048_576;
/** Default allowed distance between the signed timestamp and now: 5 minutes. */
export const DEFAULT_WEBHOOK_TOLERANCE_SECONDS = 300;
/** Longest accepted signature header, in UTF-16 code units (ASCII bytes). */
export const MAX_WEBHOOK_SIGNATURE_HEADER = 8192;

/** Why a delivery was rejected. Stable across runtimes. */
export type WebhookRejection =
  | "body_too_large"
  | "secret_missing"
  | "signature_missing"
  | "signature_malformed"
  | "signature_mismatch"
  | "timestamp_out_of_tolerance"
  | "duplicate"
  | "payload_invalid";

export interface VerifyWebhookOptions<T = unknown> {
  /** The raw request body, exactly as received. Strings are encoded as UTF-8. */
  body: Uint8Array | string;
  /** The signature header value (`Stripe-Signature`), or null when absent. */
  signatureHeader: string | null | undefined;
  /** Endpoint secret, or several during rotation; any one may match. */
  secrets: string | readonly string[];
  /** Allowed |now - t| in seconds; default 300. Must be a positive integer. */
  toleranceSeconds?: number;
  /** Largest accepted body in bytes; default 1 MiB. */
  maxBodyBytes?: number;
  /** Current Unix time in seconds; defaults to the system clock. */
  now?: number;
  /**
   * Application-owned replay tracking, called only after the signature and
   * timestamp are verified. Return false when the key was already accepted;
   * the delivery is then rejected as `duplicate`. The key is the event's
   * string `id` when present, otherwise `"<t>.<matched v1 signature>"`.
   * The store owns bounds and expiry (keep keys at least for the tolerance).
   */
  claim?: (key: string) => boolean | Promise<boolean>;
  /**
   * Typed decoding of the parsed JSON payload, run before `claim`: return
   * the typed event, or throw to reject the delivery as `payload_invalid`
   * (a Zod schema's `parse`, for example).
   */
  parse?: (event: unknown) => T;
}

/** A verified, parsed delivery. */
export interface VerifiedWebhook<T = unknown> {
  ok: true;
  /** The parsed JSON payload. */
  event: T;
  /** The signed timestamp (`t`), Unix seconds. */
  timestamp: number;
  /** The payload's string `type`, or null. */
  type: string | null;
  /** The payload's string `id`, or null. */
  id: string | null;
}

/** A rejected delivery; safe to log. */
export interface RejectedWebhook {
  ok: false;
  reason: WebhookRejection;
  message: string;
}

export type WebhookOutcome<T = unknown> = VerifiedWebhook<T> | RejectedWebhook;

const encoder = new TextEncoder();

function reject(reason: WebhookRejection, message: string): RejectedWebhook {
  return { ok: false, reason, message };
}

function positiveInteger(value: number | undefined, fallback: number, name: string): number {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0) throw new RangeError(`${name} must be a positive integer`);
  return value;
}

interface ParsedHeader {
  timestamp: string;
  signatures: Uint8Array[];
}

function fromHex(text: string): Uint8Array | null {
  if (!/^[0-9a-fA-F]{64}$/.test(text)) return null;
  const out = new Uint8Array(32);
  for (let i = 0; i < 32; i++) out[i] = Number.parseInt(text.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Parse `t=<digits>,v1=<hex>[,v1=...]`; other schemes (`v0`) are ignored. */
function parseHeader(header: string): ParsedHeader | string {
  if (header.length > MAX_WEBHOOK_SIGNATURE_HEADER) return "the signature header is too long";
  let timestamp: string | null = null;
  const signatures: Uint8Array[] = [];
  for (const raw of header.split(",")) {
    const item = raw.trim();
    const eq = item.indexOf("=");
    if (eq <= 0) return "the signature header has an item without a key";
    const key = item.slice(0, eq);
    const value = item.slice(eq + 1);
    if (key === "t") {
      if (timestamp !== null) return "the signature header has more than one timestamp";
      if (!/^[0-9]{1,15}$/.test(value)) return "the signature header timestamp is not a decimal integer";
      timestamp = value;
    } else if (key === "v1") {
      const bytes = fromHex(value);
      if (bytes !== null) signatures.push(bytes);
    }
  }
  if (timestamp === null) return "the signature header has no timestamp";
  if (signatures.length === 0) return "the signature header has no v1 signature";
  return { timestamp, signatures };
}

function concat(prefix: string, body: Uint8Array): Uint8Array<ArrayBuffer> {
  const head = encoder.encode(prefix);
  const out = new Uint8Array(new ArrayBuffer(head.byteLength + body.byteLength));
  out.set(head);
  out.set(body, head.byteLength);
  return out;
}

async function hmacKey(secret: string): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", encoder.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, [
    "sign",
    "verify",
  ]);
}

function bodyBytes(body: Uint8Array | string): Uint8Array {
  return typeof body === "string" ? encoder.encode(body) : body;
}

/**
 * Verify a Stripe-compatible signed delivery and parse its JSON payload.
 * Never throws for a bad delivery; throws `RangeError` for invalid options.
 */
export async function verifyWebhook<T = unknown>(options: VerifyWebhookOptions<T>): Promise<WebhookOutcome<T>> {
  const tolerance = positiveInteger(options.toleranceSeconds, DEFAULT_WEBHOOK_TOLERANCE_SECONDS, "toleranceSeconds");
  const maxBody = positiveInteger(options.maxBodyBytes, DEFAULT_WEBHOOK_MAX_BODY_BYTES, "maxBodyBytes");
  const body = bodyBytes(options.body);
  if (body.byteLength > maxBody) return reject("body_too_large", `the body exceeds ${maxBody} bytes`);
  const secrets = (typeof options.secrets === "string" ? [options.secrets] : [...options.secrets]).filter(
    (s) => s.length > 0,
  );
  if (secrets.length === 0) return reject("secret_missing", "no webhook secret is configured");
  const header = options.signatureHeader;
  if (header === null || header === undefined || header.trim() === "") {
    return reject("signature_missing", "the request has no signature header");
  }
  const parsed = parseHeader(header);
  if (typeof parsed === "string") return reject("signature_malformed", parsed);
  const signed = concat(`${parsed.timestamp}.`, body);
  let matched: Uint8Array | null = null;
  for (const secret of secrets) {
    const key = await hmacKey(secret);
    for (const signature of parsed.signatures) {
      // WebCrypto's verify compares in constant time.
      if (matched === null && (await crypto.subtle.verify("HMAC", key, signature as Uint8Array<ArrayBuffer>, signed))) {
        matched = signature;
      }
    }
  }
  if (matched === null) return reject("signature_mismatch", "no signature matches a configured secret");
  const timestamp = Number(parsed.timestamp);
  const now = options.now ?? Math.floor(Date.now() / 1000);
  if (Math.abs(now - timestamp) > tolerance) {
    return reject("timestamp_out_of_tolerance", `the signed timestamp is more than ${tolerance} seconds from now`);
  }
  let event: unknown;
  try {
    event = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(body));
  } catch {
    return reject("payload_invalid", "the body is not UTF-8 JSON");
  }
  const record = typeof event === "object" && event !== null && !Array.isArray(event) ? (event as Record<string, unknown>) : null;
  const id = record !== null && typeof record.id === "string" ? record.id : null;
  const type = record !== null && typeof record.type === "string" ? record.type : null;
  if (options.parse !== undefined) {
    try {
      event = options.parse(event);
    } catch {
      return reject("payload_invalid", "the payload does not match the event type");
    }
  }
  if (options.claim !== undefined) {
    const claimed = await options.claim(id ?? `${parsed.timestamp}.${toHex(matched)}`);
    if (!claimed) return reject("duplicate", "the delivery was already accepted");
  }
  return { ok: true, event: event as T, timestamp, type, id };
}

/**
 * Build a Stripe-compatible signature header for `body` at `timestamp`
 * (Unix seconds). Intended for local examples and outbound test deliveries.
 */
export async function signWebhook(body: Uint8Array | string, secret: string, timestamp: number): Promise<string> {
  if (!Number.isSafeInteger(timestamp) || timestamp < 0) throw new RangeError("timestamp must be a non-negative integer");
  const t = String(timestamp);
  const mac = await crypto.subtle.sign("HMAC", await hmacKey(secret), concat(`${t}.`, bodyBytes(body)));
  return `t=${t},v1=${toHex(new Uint8Array(mac))}`;
}
