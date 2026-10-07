// SPDX-License-Identifier: AGPL-3.0-only
//! Identifiers: wire name plus normalized words. Target casings and keyword
//! escaping are computed by [`crate::naming`].

use serde::{Deserialize, Serialize};

use crate::naming::{self, Case, Role, Target};

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
        let words = naming::split_words(&wire);
        Self { wire, words }
    }
    pub fn snake(&self) -> String {
        naming::to_case(&self.words, Case::Snake)
    }
    pub fn camel(&self) -> String {
        naming::to_case(&self.words, Case::Camel)
    }
    pub fn pascal(&self) -> String {
        naming::to_case(&self.words, Case::Pascal)
    }
    pub fn screaming(&self) -> String {
        naming::to_case(&self.words, Case::ScreamingSnake)
    }
    pub fn kebab(&self) -> String {
        naming::to_case(&self.words, Case::Kebab)
    }
    /// The name as it must appear in `target` source for `role`
    /// (see [`crate::naming::render`]).
    pub fn render(&self, target: Target, role: Role) -> String {
        naming::render(self, target, role)
    }
}
