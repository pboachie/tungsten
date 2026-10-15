// SPDX-License-Identifier: AGPL-3.0-only
//! Path Items that combine `$ref` with fields written beside it.
//!
//! A Path Item `$ref` names a shared item, and the item that holds the
//! `$ref` may also carry operations, `parameters`, `servers`, `summary` and
//! `description` of its own. The two are merged: the referenced item is the
//! base and what is written beside the `$ref` is laid over it, per
//! operation, so a sibling operation is added and a sibling that repeats an
//! operation of the target replaces it. Parameters are merged by name and
//! location with the sibling winning. The merge is the same for OpenAPI 3.0
//! and 3.1 documents. Every field that both sides define is reported as
//! TG0510 and names the side that won.

use serde_json::Value;
use tungsten_core::Diagnostic;
use tungsten_openapi::RefTarget;

use crate::ctx::{Ctx, child, str_of};
use crate::operations::METHODS;

/// Path Item fields besides the operations that a sibling can override.
const MERGED_FIELDS: [&str; 3] = ["summary", "description", "servers"];

/// A Path Item after its `$ref` was followed.
pub(crate) struct PathItem<'a> {
    /// The item the `$ref` chain ends at (the item itself without `$ref`).
    pub at: RefTarget,
    pub item: &'a Value,
    /// The item that holds the `$ref` when fields are written beside it.
    pub local: Option<(RefTarget, &'a Value)>,
}

impl<'a> PathItem<'a> {
    /// Resolve the Path Item at `at`; `None` for a dangling or circular
    /// `$ref`, which the frontend has already reported.
    pub fn resolve(cx: &Ctx<'a>, at: &RefTarget) -> Option<Self> {
        let (resolved, item) = cx.deref_value(at)?;
        let local = cx
            .get(at)
            .filter(|v| v.get("$ref").is_some() && *at != resolved)
            .filter(|v| {
                v.as_object()
                    .is_some_and(|m| m.keys().any(|k| k != "$ref" && !k.starts_with("x-")))
            })
            .map(|v| (at.clone(), v));
        Some(Self {
            at: resolved,
            item,
            local,
        })
    }

    /// The operation `word` and where it is written: beside the `$ref` when
    /// the sibling defines it, else in the referenced item.
    pub fn operation(&self, word: &str) -> Option<(RefTarget, &'a Value)> {
        if let Some((at, local)) = &self.local
            && let Some(op) = local.get(word)
        {
            return Some((child(at, word), op));
        }
        self.item.get(word).map(|op| (child(&self.at, word), op))
    }

    /// The items that own path-level `parameters`, base first. A later
    /// owner overrides an earlier one.
    pub fn owners(&self) -> Vec<RefTarget> {
        let mut owners = vec![self.at.clone()];
        if let Some((at, _)) = &self.local {
            owners.push(at.clone());
        }
        owners
    }

    /// Report each field that the target and the sibling both define.
    pub fn report_conflicts(&self, cx: &mut Ctx<'a>, path: &str) {
        let Some((local_at, local)) = &self.local else {
            return;
        };
        let words = METHODS.iter().map(|(w, _)| *w).chain(MERGED_FIELDS);
        for word in words {
            if local.get(word).is_some() && self.item.get(word).is_some() {
                self.report(
                    cx,
                    path,
                    word,
                    &child(&self.at, word),
                    &child(local_at, word),
                );
            }
        }
        // Parameters merge by name and location; only a repeated key is a
        // conflict.
        let base_list = child(&self.at, "parameters");
        let local_list = child(local_at, "parameters");
        let (Some(Value::Array(base)), Some(Value::Array(own))) =
            (cx.get(&base_list), cx.get(&local_list))
        else {
            return;
        };
        let keys = |cx: &Ctx<'a>, list: &RefTarget, n: usize| {
            (0..n)
                .filter_map(|i| {
                    let (at, p) = cx.deref_value(&child(list, &i.to_string()))?;
                    let key = (str_of(p, "name")?.to_string(), str_of(p, "in")?.to_string());
                    Some((key, child(list, &i.to_string()), at))
                })
                .collect::<Vec<_>>()
        };
        let base_keys = keys(cx, &base_list, base.len());
        for (key, own_at, _) in keys(cx, &local_list, own.len()) {
            if let Some((_, base_at, _)) = base_keys.iter().find(|(k, _, _)| *k == key) {
                let what = format!("parameter `{}` in {}", key.0, key.1);
                self.report(cx, path, &what, base_at, &own_at);
            }
        }
    }

    fn report(&self, cx: &mut Ctx<'a>, path: &str, what: &str, base: &RefTarget, own: &RefTarget) {
        cx.report(
            Diagnostic::warning(
                "TG0510",
                format!(
                    "path item {path}: `{what}` is defined both by the `$ref` target ({}) and beside the `$ref` ({}); the one beside the `$ref` wins",
                    base.pointer, own.pointer
                ),
            )
            .with_help("define the field on one side only"),
            own,
        );
    }
}
