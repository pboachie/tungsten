// SPDX-License-Identifier: Apache-2.0
//! Incoming webhook verification with no framework dependency.
//!
//! The first adapter is the Stripe-compatible signature scheme: a header such
//! as `Stripe-Signature: t=1700000000,v1=<hex>` where `v1` is the lowercase
//! hex HMAC-SHA256 of `"<t>.<raw body>"` under the endpoint secret. Pass the
//! raw request bytes exactly as received, before any JSON parsing.
//!
//! Checks run in a fixed order, identical in every tungsten runtime, and the
//! first failure is reported: body size, secret presence, header presence,
//! header syntax, signature, timestamp tolerance, JSON payload, typed
//! decoding, replay claim.
//! Failures carry a stable [`WebhookRejection`] and a message that never
//! contains the secret, the signature or the payload.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::Sha256;

/// Default largest accepted body: 1 MiB.
pub const DEFAULT_WEBHOOK_MAX_BODY_BYTES: usize = 1_048_576;
/// Default allowed distance between the signed timestamp and now: 5 minutes.
pub const DEFAULT_WEBHOOK_TOLERANCE_SECONDS: u64 = 300;
/// Longest accepted signature header, in UTF-16 code units (ASCII bytes).
pub const MAX_WEBHOOK_SIGNATURE_HEADER: usize = 8192;
/// Deepest accepted nesting of arrays and objects in the payload (`[]` is 1).
pub const MAX_WEBHOOK_JSON_DEPTH: usize = 100;

/// Why a delivery was rejected. Stable across runtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookRejection {
    BodyTooLarge,
    SecretMissing,
    SignatureMissing,
    SignatureMalformed,
    SignatureMismatch,
    TimestampOutOfTolerance,
    Duplicate,
    PayloadInvalid,
}

impl WebhookRejection {
    /// The reason code shared with the TypeScript and Python runtimes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BodyTooLarge => "body_too_large",
            Self::SecretMissing => "secret_missing",
            Self::SignatureMissing => "signature_missing",
            Self::SignatureMalformed => "signature_malformed",
            Self::SignatureMismatch => "signature_mismatch",
            Self::TimestampOutOfTolerance => "timestamp_out_of_tolerance",
            Self::Duplicate => "duplicate",
            Self::PayloadInvalid => "payload_invalid",
        }
    }
}

/// A rejected delivery; safe to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedWebhook {
    pub reason: WebhookRejection,
    pub message: String,
}

impl fmt::Display for RejectedWebhook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason.as_str(), self.message)
    }
}

impl std::error::Error for RejectedWebhook {}

/// A verified, parsed delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedWebhook<T = Value> {
    /// The parsed JSON payload.
    pub event: T,
    /// The signed timestamp (`t`), Unix seconds.
    pub timestamp: u64,
    /// The payload's string `type`, if any.
    pub r#type: Option<String>,
    /// The payload's string `id`, if any.
    pub id: Option<String>,
}

/// Invalid options; a programming error rather than a bad delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookOptionsError(pub &'static str);

impl fmt::Display for WebhookOptionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for WebhookOptionsError {}

/// Replay tracking owned by the application: return `false` when `key` was
/// already accepted. Called only after the signature and timestamp verify.
pub type WebhookClaim<'a> = &'a dyn Fn(&str) -> bool;

/// Options of [`verify_webhook`]. `Default` gives no secrets.
#[derive(Clone, Copy, Default)]
pub struct VerifyWebhookOptions<'a> {
    /// Endpoint secrets; several during rotation, any one may match.
    pub secrets: &'a [&'a str],
    /// Allowed |now - t| in seconds; default 300. Must be positive.
    pub tolerance_seconds: Option<u64>,
    /// Largest accepted body in bytes; default 1 MiB. Must be a positive
    /// safe integer (at most 2^53 - 1), as in the other runtimes.
    pub max_body_bytes: Option<usize>,
    /// Current Unix time in seconds; defaults to the system clock.
    pub now: Option<u64>,
    /// Replay tracking. The key is the event's string `id` when present,
    /// otherwise `"<t>.<matched v1 signature>"`. The store owns bounds and
    /// expiry (keep keys at least for the tolerance).
    pub claim: Option<WebhookClaim<'a>>,
}

impl fmt::Debug for VerifyWebhookOptions<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifyWebhookOptions")
            .field(
                "secrets",
                &format_args!("[{} redacted]", self.secrets.len()),
            )
            .field("tolerance_seconds", &self.tolerance_seconds)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("now", &self.now)
            .field("claim", &self.claim.is_some())
            .finish()
    }
}

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

fn reject<T>(reason: WebhookRejection, message: impl Into<String>) -> Outcome<T> {
    Ok(Err(RejectedWebhook {
        reason,
        message: message.into(),
    }))
}

type Outcome<T> = Result<Result<VerifiedWebhook<T>, RejectedWebhook>, WebhookOptionsError>;

// The whitespace JavaScript's `String.prototype.trim` removes.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| {
        matches!(
            c,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    })
}

fn from_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_header(header: &str) -> Result<(&str, Vec<[u8; 32]>), &'static str> {
    if header.encode_utf16().count() > MAX_WEBHOOK_SIGNATURE_HEADER {
        return Err("the signature header is too long");
    }
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for raw in header.split(',') {
        let item = js_trim(raw);
        let Some(eq) = item.find('=').filter(|eq| *eq > 0) else {
            return Err("the signature header has an item without a key");
        };
        let (key, value) = (&item[..eq], &item[eq + 1..]);
        if key == "t" {
            if timestamp.is_some() {
                return Err("the signature header has more than one timestamp");
            }
            if value.is_empty() || value.len() > 15 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("the signature header timestamp is not a decimal integer");
            }
            timestamp = Some(value);
        } else if key == "v1"
            && let Some(bytes) = from_hex(value)
        {
            signatures.push(bytes);
        }
    }
    let Some(timestamp) = timestamp else {
        return Err("the signature header has no timestamp");
    };
    if signatures.is_empty() {
        return Err("the signature header has no v1 signature");
    }
    Ok((timestamp, signatures))
}

/// Whether arrays and objects outside strings nest deeper than the limit.
/// Scans bytes, so it runs before decoding and gives the same answer in every
/// runtime, whatever else is wrong with the body.
fn too_deep(body: &[u8]) -> bool {
    // Signed like the other runtimes' counters, so a stray closer counts the same.
    let mut depth = 0isize;
    let (mut in_string, mut escaped) = (false, false);
    for &byte in body {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'[' || byte == b'{' {
            depth += 1;
            if depth > MAX_WEBHOOK_JSON_DEPTH as isize {
                return true;
            }
        } else if byte == b']' || byte == b'}' {
            depth -= 1;
        }
    }
    false
}

fn mac(secret: &str, timestamp: &str, body: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// Verify a Stripe-compatible signed delivery and parse its JSON payload as
/// `T` (use [`Value`] for an untyped event). A payload that is not JSON or
/// does not match `T` is rejected as `payload_invalid`, as is one whose arrays
/// and objects nest deeper than [`MAX_WEBHOOK_JSON_DEPTH`], one with a `\u`
/// escape that is a lone surrogate, or one with a number outside the `f64`
/// range (the TypeScript and Python runtimes reject the same payloads).
///
/// The outer `Err` is only for invalid options; a bad delivery is the inner
/// `Err`.
pub fn verify_webhook<T: DeserializeOwned>(
    body: &[u8],
    signature_header: Option<&str>,
    options: &VerifyWebhookOptions<'_>,
) -> Outcome<T> {
    verify_webhook_with(body, signature_header, options, |value| {
        serde_json::from_value(value).ok()
    })
}

/// [`verify_webhook`] with custom typed decoding of the parsed JSON payload,
/// run before the replay claim: `decode` returns `None` to reject the
/// delivery as `payload_invalid` (a decode that also checks constraints, for
/// example). The same step as the `parse` option of the TypeScript and Python
/// runtimes.
pub fn verify_webhook_with<T>(
    body: &[u8],
    signature_header: Option<&str>,
    options: &VerifyWebhookOptions<'_>,
    decode: impl FnOnce(Value) -> Option<T>,
) -> Outcome<T> {
    let tolerance = match options.tolerance_seconds {
        None => DEFAULT_WEBHOOK_TOLERANCE_SECONDS,
        Some(t) if t > 0 && t <= MAX_SAFE_INTEGER => t,
        Some(_) => {
            return Err(WebhookOptionsError(
                "tolerance_seconds must be a positive integer",
            ));
        }
    };
    let max_body = match options.max_body_bytes {
        None => DEFAULT_WEBHOOK_MAX_BODY_BYTES,
        Some(m) if m > 0 && m as u64 <= MAX_SAFE_INTEGER => m,
        Some(_) => {
            return Err(WebhookOptionsError(
                "max_body_bytes must be a positive integer",
            ));
        }
    };
    if body.len() > max_body {
        return reject(
            WebhookRejection::BodyTooLarge,
            format!("the body exceeds {max_body} bytes"),
        );
    }
    let secrets: Vec<&str> = options
        .secrets
        .iter()
        .copied()
        .filter(|s| !s.is_empty())
        .collect();
    if secrets.is_empty() {
        return reject(
            WebhookRejection::SecretMissing,
            "no webhook secret is configured",
        );
    }
    let header = match signature_header {
        Some(h) if !js_trim(h).is_empty() => h,
        _ => {
            return reject(
                WebhookRejection::SignatureMissing,
                "the request has no signature header",
            );
        }
    };
    let (t, signatures) = match parse_header(header) {
        Ok(parsed) => parsed,
        Err(message) => return reject(WebhookRejection::SignatureMalformed, message),
    };
    let mut matched = None;
    for secret in &secrets {
        let mac = mac(secret, t, body);
        for signature in &signatures {
            // `verify_slice` compares in constant time.
            if mac.clone().verify_slice(signature).is_ok() && matched.is_none() {
                matched = Some(*signature);
            }
        }
    }
    let Some(matched) = matched else {
        return reject(
            WebhookRejection::SignatureMismatch,
            "no signature matches a configured secret",
        );
    };
    let timestamp: u64 = t.parse().expect("validated digits");
    let now = options.now.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });
    if now.abs_diff(timestamp) > tolerance {
        return reject(
            WebhookRejection::TimestampOutOfTolerance,
            format!("the signed timestamp is more than {tolerance} seconds from now"),
        );
    }
    if too_deep(body) {
        return reject(
            WebhookRejection::PayloadInvalid,
            format!("the payload nests deeper than {MAX_WEBHOOK_JSON_DEPTH} levels"),
        );
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return reject(
            WebhookRejection::PayloadInvalid,
            "the body is not UTF-8 JSON",
        );
    };
    let field = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_owned);
    let (id, r#type) = (field("id"), field("type"));
    let Some(event) = decode(value) else {
        return reject(
            WebhookRejection::PayloadInvalid,
            "the payload does not match the event type",
        );
    };
    if let Some(claim) = options.claim {
        let key = id
            .clone()
            .unwrap_or_else(|| format!("{t}.{}", to_hex(&matched)));
        if !claim(&key) {
            return reject(
                WebhookRejection::Duplicate,
                "the delivery was already accepted",
            );
        }
    }
    Ok(Ok(VerifiedWebhook {
        event,
        timestamp,
        r#type,
        id,
    }))
}

/// Build a Stripe-compatible signature header for `body` at `timestamp`
/// (Unix seconds). Intended for local examples and outbound test deliveries.
pub fn sign_webhook(body: &[u8], secret: &str, timestamp: u64) -> String {
    let t = timestamp.to_string();
    let digest = mac(secret, &t, body).finalize().into_bytes();
    format!("t={t},v1={}", to_hex(&digest))
}
