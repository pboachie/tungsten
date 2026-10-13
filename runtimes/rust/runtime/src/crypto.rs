// SPDX-License-Identifier: Apache-2.0
//! Cryptographic building blocks for hand-written SDK extensions.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Compute HMAC-SHA256 over exact bytes. The key and message are never serialized.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    crate::util::hmac_sha256(key, message)
}

/// Verify HMAC-SHA256 with the cryptography library's constant-time comparison.
/// A digest whose length differs from SHA256's 32 bytes is rejected.
pub fn verify_hmac_sha256(key: &[u8], message: &[u8], digest: &[u8]) -> bool {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return false;
    };
    mac.update(message);
    mac.verify_slice(digest).is_ok()
}
