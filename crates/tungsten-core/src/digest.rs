// SPDX-License-Identifier: AGPL-3.0-only
//! Content digests (BLAKE3) used in generated-file headers and staleness checks.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct Digest(pub String);

impl Digest {
    /// `blake3:` followed by the lowercase hex digest of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Digest(format!("blake3:{}", blake3::hash(bytes).to_hex()))
    }
    /// Short form for human output: the first 12 hex characters.
    pub fn short(&self) -> &str {
        let hex = self.0.strip_prefix("blake3:").unwrap_or(&self.0);
        &hex[..hex.len().min(12)]
    }
}
