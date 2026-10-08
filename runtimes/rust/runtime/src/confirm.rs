// SPDX-License-Identifier: Apache-2.0
//! Confirmation tokens (planning/06 "Preview and confirmation"):
//! `tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, op id, args digest, expiry))>`.
//! A token is valid only for the operation and the exact arguments it was
//! issued for, until its expiry.

use serde_json::Value;

use crate::util::{base64url, canonical_json, hmac_sha256, sha256_hex, timing_safe_equal};

/// Lifetime of a confirmation token.
pub const CONFIRMATION_TTL_MS: u64 = 5 * 60 * 1000;

const PREFIX: &str = "tgc1";

/// SHA-256 of the canonical JSON of the arguments.
pub fn args_digest(args: &Value) -> String {
    sha256_hex(canonical_json(args).as_bytes())
}

/// The text a token's signature covers.
pub fn token_payload(operation: &str, digest: &str, expiry: u64) -> String {
    format!("{operation}\n{digest}\n{expiry}")
}

fn signature(key: &[u8], operation: &str, digest: &str, expiry: u64) -> String {
    base64url(&hmac_sha256(
        key,
        token_payload(operation, digest, expiry).as_bytes(),
    ))
}

pub fn issue_token(key: &[u8], operation: &str, args: &Value, now: u64) -> String {
    let expiry = now.saturating_add(CONFIRMATION_TTL_MS);
    format!(
        "{PREFIX}.{expiry}.{}",
        signature(key, operation, &args_digest(args), expiry)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenCheck {
    Valid,
    Expired,
    Mismatch,
    Malformed,
}

/// The expiry a token claims, when it has the token shape.
pub fn token_expiry(token: &str) -> Option<(u64, &str)> {
    let rest = token.strip_prefix(PREFIX)?.strip_prefix('.')?;
    let (expiry, signature) = rest.split_once('.')?;
    let digits =
        !expiry.is_empty() && expiry.len() <= 16 && expiry.bytes().all(|b| b.is_ascii_digit());
    let shaped = signature.len() == 43
        && signature
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if digits && shaped {
        Some((expiry.parse().ok()?, signature))
    } else {
        None
    }
}

pub fn check_token(key: &[u8], token: &str, operation: &str, args: &Value, now: u64) -> TokenCheck {
    let Some((expiry, given)) = token_expiry(token) else {
        return TokenCheck::Malformed;
    };
    let expected = signature(key, operation, &args_digest(args), expiry);
    if !timing_safe_equal(&expected, given) {
        TokenCheck::Mismatch
    } else if expiry > now {
        TokenCheck::Valid
    } else {
        TokenCheck::Expired
    }
}
