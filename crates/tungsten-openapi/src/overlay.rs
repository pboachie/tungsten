// SPDX-License-Identifier: AGPL-3.0-only
//! OpenAPI Overlay 1.0 application.
//!
//! Actions run in order. `remove: true` deletes every selected node;
//! otherwise `update` is merged into every selected node: objects merge
//! recursively, arrays are appended to (an array update appends each of its
//! elements), anything else is replaced. A target that selects nothing is
//! reported with TG0206; malformed overlays and actions with TG0207.
//! Spans of inserted values point into the overlay file.

use std::cmp::Ordering;

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};

use crate::jsonpath::JsonPath;
use crate::spans::SpanIndex;
use crate::{join_pointer, split_pointer};

/// A parsed overlay file.
pub(crate) struct Overlay<'a> {
    pub name: &'a str,
    pub value: &'a Value,
    pub spans: &'a SpanIndex,
}

impl Overlay<'_> {
    fn diagnostic(&self, d: Diagnostic, pointer: &str) -> Diagnostic {
        d.at(self.name, pointer, self.spans.nearest(pointer))
    }
}

/// Apply `overlay` to the document `doc` whose spans are `spans`.
pub(crate) fn apply(
    doc: &mut Value,
    spans: &mut SpanIndex,
    overlay: &Overlay<'_>,
    diags: &mut Diagnostics,
) {
    let version = overlay.value.get("overlay");
    if !version
        .and_then(Value::as_str)
        .is_some_and(|v| v == "1" || v.starts_with("1."))
    {
        let pointer = if version.is_some() { "/overlay" } else { "" };
        diags.push(
            overlay.diagnostic(
                Diagnostic::error("TG0207", "not an OpenAPI Overlay 1.x document")
                    .with_help("an overlay needs `overlay: 1.0.0`, `info` and `actions`"),
                pointer,
            ),
        );
        return;
    }
    let Some(actions) = overlay.value.get("actions").and_then(Value::as_array) else {
        diags.push(overlay.diagnostic(
            Diagnostic::error("TG0207", "overlay has no `actions` array"),
            "",
        ));
        return;
    };
    for (i, action) in actions.iter().enumerate() {
        apply_action(doc, spans, overlay, action, &format!("/actions/{i}"), diags);
    }
}

fn apply_action(
    doc: &mut Value,
    spans: &mut SpanIndex,
    overlay: &Overlay<'_>,
    action: &Value,
    at: &str,
    diags: &mut Diagnostics,
) {
    let target_ptr = format!("{at}/target");
    let Some(target) = action.get("target").and_then(Value::as_str) else {
        diags.push(overlay.diagnostic(
            Diagnostic::error("TG0207", "overlay action has no string `target`"),
            at,
        ));
        return;
    };
    let path = match JsonPath::parse(target) {
        Ok(p) => p,
        Err(e) => {
            diags.push(overlay.diagnostic(
                Diagnostic::error("TG0207", format!("invalid JSONPath target `{target}`: {e}")),
                &target_ptr,
            ));
            return;
        }
    };
    let remove = match action.get("remove") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(_) => {
            diags.push(overlay.diagnostic(
                Diagnostic::error("TG0207", "overlay action `remove` must be a boolean"),
                &format!("{at}/remove"),
            ));
            return;
        }
    };
    let update = action.get("update");
    if !remove && update.is_none() {
        diags.push(overlay.diagnostic(
            Diagnostic::warning(
                "TG0207",
                format!("overlay action for `{target}` has neither `update` nor `remove: true`"),
            ),
            at,
        ));
        return;
    }
    let selected = path.select(doc);
    if selected.is_empty() {
        diags.push(overlay.diagnostic(
            Diagnostic::warning(
                "TG0206",
                format!("overlay target `{target}` matched nothing"),
            ),
            &target_ptr,
        ));
        return;
    }
    if remove {
        remove_all(doc, spans, selected, overlay, &target_ptr, diags);
    } else if let Some(update) = update {
        let update_ptr = format!("{at}/update");
        for p in selected {
            if let Some(node) = doc.pointer_mut(&p) {
                merge(node, &p, update, &update_ptr, spans, overlay.spans);
            }
        }
    }
}

/// Order pointers so that later array elements and descendants come first:
/// removing in this order never shifts a pointer still to be removed.
fn removal_order(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let ord = match (x.parse::<usize>(), y.parse::<usize>()) {
            (Ok(i), Ok(j)) => i.cmp(&j),
            _ => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord.reverse();
        }
    }
    b.len().cmp(&a.len())
}

fn remove_all(
    doc: &mut Value,
    spans: &mut SpanIndex,
    selected: Vec<String>,
    overlay: &Overlay<'_>,
    target_ptr: &str,
    diags: &mut Diagnostics,
) {
    let mut paths: Vec<(String, Vec<String>)> = selected
        .into_iter()
        .map(|p| {
            let tokens = split_pointer(&p);
            (p, tokens)
        })
        .collect();
    paths.sort_by(|(_, a), (_, b)| removal_order(a, b));
    for (pointer, tokens) in paths {
        let Some((last, parent_tokens)) = tokens.split_last() else {
            diags.push(overlay.diagnostic(
                Diagnostic::error("TG0207", "an overlay cannot remove the document root"),
                target_ptr,
            ));
            continue;
        };
        let parent_ptr = parent_tokens
            .iter()
            .fold(String::new(), |acc, t| join_pointer(&acc, t));
        match doc.pointer_mut(&parent_ptr) {
            Some(Value::Object(m)) => {
                m.shift_remove(last.as_str());
                spans.take_subtree(&pointer);
            }
            Some(Value::Array(a)) => {
                if let Ok(i) = last.parse::<usize>()
                    && i < a.len()
                {
                    a.remove(i);
                    spans.shift_after_removal(&parent_ptr, i);
                }
            }
            _ => {}
        }
    }
}

/// Merge `update` (at `update_ptr` in the overlay) into `target` (at
/// `target_ptr` in the document).
fn merge(
    target: &mut Value,
    target_ptr: &str,
    update: &Value,
    update_ptr: &str,
    spans: &mut SpanIndex,
    overlay_spans: &SpanIndex,
) {
    match (target, update) {
        (Value::Object(t), Value::Object(u)) => {
            for (k, v) in u {
                let tp = join_pointer(target_ptr, k);
                let up = join_pointer(update_ptr, k);
                match t.get_mut(k) {
                    Some(existing)
                        if (existing.is_object() && v.is_object())
                            || (existing.is_array() && v.is_array()) =>
                    {
                        merge(existing, &tp, v, &up, spans, overlay_spans);
                    }
                    Some(existing) => {
                        *existing = v.clone();
                        replace_spans(spans, &tp, overlay_spans, &up);
                    }
                    None => {
                        t.insert(k.clone(), v.clone());
                        replace_spans(spans, &tp, overlay_spans, &up);
                    }
                }
            }
        }
        (Value::Array(t), Value::Array(u)) => {
            for (i, v) in u.iter().enumerate() {
                let tp = format!("{target_ptr}/{}", t.len());
                t.push(v.clone());
                replace_spans(spans, &tp, overlay_spans, &format!("{update_ptr}/{i}"));
            }
        }
        (Value::Array(t), u) => {
            let tp = format!("{target_ptr}/{}", t.len());
            t.push(u.clone());
            replace_spans(spans, &tp, overlay_spans, update_ptr);
        }
        (t, u) => {
            *t = u.clone();
            replace_spans(spans, target_ptr, overlay_spans, update_ptr);
        }
    }
}

fn replace_spans(
    spans: &mut SpanIndex,
    target_ptr: &str,
    overlay_spans: &SpanIndex,
    update_ptr: &str,
) {
    spans.take_subtree(target_ptr);
    spans.insert_subtree(target_ptr, overlay_spans.subtree(update_ptr));
}
