// SPDX-License-Identifier: AGPL-3.0-only
//! Identifiers: wire name plus normalized words. Target casings and keyword
//! escaping are computed by [`crate::naming`].

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct Ident {
    /// The original name as it appears on the wire or in the spec.
    pub wire: String,
    /// Lowercase words from [`crate::naming::split_words`]. Disambiguation
    /// suffixes are appended here, never to `wire`.
    pub words: Vec<String>,
}

impl Ident {
    pub fn new(wire: impl Into<String>) -> Self {
        let wire = wire.into();
        let words = crate::naming::split_words(&wire);
        Self { wire, words }
    }
    pub fn snake(&self) -> String {
        crate::naming::to_case(&self.words, crate::naming::Case::Snake)
    }
    pub fn camel(&self) -> String {
        crate::naming::to_case(&self.words, crate::naming::Case::Camel)
    }
    pub fn pascal(&self) -> String {
        crate::naming::to_case(&self.words, crate::naming::Case::Pascal)
    }
    pub fn screaming(&self) -> String {
        crate::naming::to_case(&self.words, crate::naming::Case::ScreamingSnake)
    }
    pub fn kebab(&self) -> String {
        crate::naming::to_case(&self.words, crate::naming::Case::Kebab)
    }
}
