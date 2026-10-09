// SPDX-License-Identifier: AGPL-3.0-only
//! Shared build state: the workspace, the manifest, the type builder and
//! the diagnostics, with helpers to read the normalized documents and to
//! label diagnostics with their source location.

use std::collections::BTreeSet;

use serde_json::Value;
use tungsten_config::TungstenConfig;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_ir::Doc;
use tungsten_openapi::{RefTarget, Workspace, join_pointer};

use crate::types::TypeBuilder;

/// Everything the build steps share.
pub(crate) struct Ctx<'a> {
    pub ws: &'a Workspace,
    pub cfg: &'a TungstenConfig,
    pub tb: TypeBuilder<'a>,
    pub diags: Diagnostics,
    /// Display name of the manifest, used in labels of manifest diagnostics.
    manifest: &'a str,
    /// (code, file, pointer, message) of every diagnostic recorded, so a
    /// problem in a shared node (a path-level parameter) is reported once.
    seen: BTreeSet<(String, String, String, String)>,
}

impl<'a> Ctx<'a> {
    pub fn new(ws: &'a Workspace, cfg: &'a TungstenConfig, manifest: &'a str) -> Self {
        Self {
            ws,
            cfg,
            tb: TypeBuilder::new(ws, &cfg.types.break_cycles),
            diags: Diagnostics::new(),
            manifest,
            seen: BTreeSet::new(),
        }
    }

    /// The value at a target.
    pub fn get(&self, target: &RefTarget) -> Option<&'a Value> {
        self.ws.get(target)
    }

    /// Follow `$ref`s from `target` to the object they name. `None` for a
    /// dangling or circular chain, which the frontend has already reported.
    pub fn deref(&self, target: &RefTarget) -> Option<RefTarget> {
        self.ws.deref(target)
    }

    /// The value behind `target` after following `$ref`s.
    pub fn deref_value(&self, target: &RefTarget) -> Option<(RefTarget, &'a Value)> {
        let target = self.deref(target)?;
        let value = self.get(&target)?;
        Some((target, value))
    }

    /// Record a diagnostic located at a spec node.
    pub fn report(&mut self, diagnostic: Diagnostic, at: &RefTarget) {
        let d = diagnostic.at(self.ws.name(at.doc), &at.pointer, self.ws.span(at));
        self.push(d);
    }

    /// Record a diagnostic located at a manifest node. The driver attaches
    /// spans for the manifest's text.
    pub fn report_manifest(&mut self, diagnostic: Diagnostic, pointer: &str) {
        let d = diagnostic.at(self.manifest, pointer, None);
        self.push(d);
    }

    /// Record a diagnostic unless an identical one (same code, message and
    /// first location) was recorded already.
    fn push(&mut self, d: Diagnostic) {
        let (file, pointer) = d
            .labels
            .first()
            .map(|l| (l.file.clone(), l.pointer.clone()))
            .unwrap_or_default();
        if self
            .seen
            .insert((d.code.clone(), file, pointer, d.message.clone()))
        {
            self.diags.push(d);
        }
    }
}

/// The child of `target` named by one unescaped reference token.
pub(crate) fn child(target: &RefTarget, token: &str) -> RefTarget {
    RefTarget {
        doc: target.doc,
        pointer: join_pointer(&target.pointer, token),
    }
}

/// A JSON Pointer from unescaped reference tokens.
pub(crate) fn pointer<'t>(tokens: impl IntoIterator<Item = &'t str>) -> String {
    tokens
        .into_iter()
        .fold(String::new(), |acc, token| join_pointer(&acc, token))
}

/// A string member of an object.
pub(crate) fn str_of<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value.get(key).and_then(Value::as_str)
}

/// A boolean member of an object; absent or non-boolean is `false`.
pub(crate) fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Documentation from an optional summary and description. The summary
/// falls back to the first sentence of the description; both are trimmed
/// and empty strings count as absent.
pub(crate) fn doc(summary: Option<&str>, description: Option<&str>) -> Option<Doc> {
    let description = non_empty(description);
    let summary = non_empty(summary).or_else(|| description.map(first_sentence));
    if summary.is_none() && description.is_none() {
        return None;
    }
    Some(Doc {
        summary: summary.map(str::to_string),
        description: description.map(str::to_string),
    })
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// The `summary` and `description` members of an object as a [`Doc`].
pub(crate) fn doc_of(value: &Value) -> Option<Doc> {
    doc(str_of(value, "summary"), str_of(value, "description"))
}

/// Text up to and including the first sentence end (a period, question or
/// exclamation mark followed by whitespace) or the first blank line.
fn first_sentence(text: &str) -> &str {
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let next = bytes.get(i + 1).copied();
        if matches!(b, b'.' | b'?' | b'!') && next.is_none_or(|n| n.is_ascii_whitespace()) {
            return text[..=i].trim();
        }
        if b == b'\n' && next == Some(b'\n') {
            return text[..i].trim();
        }
    }
    text.trim()
}
