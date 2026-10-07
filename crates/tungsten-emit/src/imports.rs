// SPDX-License-Identifier: AGPL-3.0-only
//! Import collection: deduplicated, sorted, rendered per target by the
//! emitter that owns the syntax.

use std::collections::{BTreeMap, BTreeSet};

/// Names imported per module, sorted and deduplicated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Imports {
    by_module: BTreeMap<String, BTreeSet<String>>,
    type_only: BTreeMap<String, BTreeSet<String>>,
}

impl Imports {
    pub fn new() -> Self {
        Self::default()
    }

    /// Import `name` from `module`.
    pub fn add(&mut self, module: &str, name: &str) {
        self.by_module
            .entry(module.to_string())
            .or_default()
            .insert(name.to_string());
    }

    /// Import `name` from `module` for types only (`import type` in TS).
    pub fn add_type(&mut self, module: &str, name: &str) {
        self.type_only
            .entry(module.to_string())
            .or_default()
            .insert(name.to_string());
    }

    /// (module, value names) in sorted order.
    pub fn values(&self) -> impl Iterator<Item = (&str, Vec<&str>)> {
        self.by_module
            .iter()
            .map(|(m, n)| (m.as_str(), n.iter().map(String::as_str).collect()))
    }

    /// (module, type-only names) in sorted order, excluding names that are
    /// also imported as values from the same module.
    pub fn types(&self) -> impl Iterator<Item = (&str, Vec<&str>)> {
        self.type_only.iter().filter_map(|(m, n)| {
            let values = self.by_module.get(m);
            let names: Vec<&str> = n
                .iter()
                .filter(|x| values.is_none_or(|v| !v.contains(*x)))
                .map(String::as_str)
                .collect();
            (!names.is_empty()).then_some((m.as_str(), names))
        })
    }

    pub fn is_empty(&self) -> bool {
        self.by_module.is_empty() && self.type_only.is_empty()
    }
}
