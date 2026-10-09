// SPDX-License-Identifier: AGPL-3.0-only
//! Overlay suggestions: OpenAPI Overlay 1.0 actions that make compiler
//! diagnostics disappear without changing what the compiler built.
//!
//! The engine reads a finished compilation and never invents a value. A
//! diagnostic gets an action only when the fix is already decided by the
//! compiler:
//!
//! - TG0403 (an operation without `operationId`): the action sets
//!   `operationId` to the id the IR already uses for the operation.
//! - TG0401 on an operation id used more than once: the action gives the
//!   renamed operation the unique id the IR already uses for it.
//!
//! Every other diagnostic is reported as skipped, either because its fix
//! needs a value the engine cannot derive ([`SkipKind::Underivable`]) or
//! because no overlay action can fix it ([`SkipKind::NoOverlayFix`]; for
//! example TG0501, which is confirmed in `tungsten.yml`, not in the spec).
//!
//! Actions are grouped by input document: the target paths of one document
//! select nothing in another, so each input gets its own overlay.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use tungsten_core::{Diagnostic, Label, Severity};
use tungsten_ir::{Ir, Operation, Resource};
use tungsten_openapi::overlay_target;

use crate::driver::Compiled;

/// Codes that can get an overlay action.
pub const SUGGESTED_CODES: [&str; 2] = ["TG0401", "TG0403"];

/// One overlay action.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    /// The diagnostic codes the action fixes, sorted.
    pub codes: Vec<String>,
    /// One comment line per diagnostic, in code order.
    pub comments: Vec<String>,
    /// JSON Pointer of the target node in the input document.
    pub pointer: String,
    /// The Overlay target (JSONPath) selecting exactly that node.
    pub target: String,
    /// The object merged into the target node.
    pub update: Value,
}

/// The suggestions for one input document.
#[derive(Debug, Clone, PartialEq)]
pub struct InputSuggestions {
    pub namespace: String,
    /// The `spec` of the input in `tungsten.yml`.
    pub spec: String,
    /// Actions sorted by target pointer.
    pub actions: Vec<Action>,
}

/// Why a diagnostic got no action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipKind {
    /// An overlay could fix it, but the value cannot be derived (TG0920).
    Underivable,
    /// No overlay action can fix it (TG0921).
    NoOverlayFix,
}

/// A diagnostic (or, for [`SkipKind::NoOverlayFix`], all of one code) that
/// got no action.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub kind: SkipKind,
    pub code: String,
    pub reason: String,
    /// How many diagnostics of the code this stands for.
    pub count: usize,
    /// Where the (first) diagnostic points.
    pub label: Option<Label>,
}

/// Everything [`suggest`] found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Suggestions {
    /// Inputs with at least one action, in manifest order.
    pub inputs: Vec<InputSuggestions>,
    pub skipped: Vec<Skipped>,
}

/// Suggest overlay actions for the diagnostics of `compiled`. `only`, when
/// not empty, limits the diagnostics considered to those codes. A
/// compilation without an IR yields nothing.
pub fn suggest(compiled: &Compiled, only: &[String]) -> Suggestions {
    let (Some(ir), Some(config)) = (&compiled.ir, &compiled.config) else {
        return Suggestions::default();
    };
    let wanted = |code: &str| only.is_empty() || only.iter().any(|c| c == code);
    let ws = &compiled.workspace;
    let mut by_file: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, doc) in ws.entry_docs.iter().enumerate() {
        if let Some(doc) = doc.and_then(|d| ws.documents.get(d)) {
            by_file.entry(doc.name.as_str()).or_insert(i);
        }
    }
    let ids = operation_ids(ir);
    let mut pending: BTreeMap<(usize, String), Vec<&Diagnostic>> = BTreeMap::new();
    let mut out = Suggestions::default();
    let mut unfixable: BTreeMap<&str, Skipped> = BTreeMap::new();
    for d in compiled.diagnostics.iter().filter(|d| wanted(&d.code)) {
        let label = d.labels.first();
        let operation_level = d.code == "TG0403" || (d.code == "TG0401" && is_operation_id(d));
        if operation_level {
            let located = label.and_then(|l| Some((l, *by_file.get(l.file.as_str())?)));
            match located {
                Some((l, input)) => pending
                    .entry((input, l.pointer.clone()))
                    .or_default()
                    .push(d),
                None => out.skipped.push(skipped(
                    SkipKind::Underivable,
                    d,
                    "the diagnostic is not in an input document".into(),
                )),
            }
        } else if d.severity == Severity::Warning {
            unfixable
                .entry(&d.code)
                .and_modify(|s| s.count += 1)
                .or_insert_with(|| skipped(SkipKind::NoOverlayFix, d, no_fix_reason(&d.code)));
        }
    }
    let mut actions: BTreeMap<usize, Vec<Action>> = BTreeMap::new();
    for ((input, pointer), diagnostics) in pending {
        let Some(doc) = ws.entry_docs[input].and_then(|d| ws.documents.get(d)) else {
            continue;
        };
        match action(doc, &ids, &pointer, &diagnostics) {
            Ok(action) => actions.entry(input).or_default().push(action),
            Err(reason) => {
                for d in diagnostics {
                    out.skipped
                        .push(skipped(SkipKind::Underivable, d, reason.clone()));
                }
            }
        }
    }
    for (input, mut list) in actions {
        list.sort_by(|a, b| a.pointer.cmp(&b.pointer));
        let entry = &config.inputs[input];
        out.inputs.push(InputSuggestions {
            namespace: entry.namespace.clone(),
            spec: entry.spec.clone(),
            actions: list,
        });
    }
    out.skipped.extend(unfixable.into_values());
    out
}

fn skipped(kind: SkipKind, d: &Diagnostic, reason: String) -> Skipped {
    Skipped {
        kind,
        code: d.code.clone(),
        reason,
        count: 1,
        label: d.labels.first().cloned(),
    }
}

fn no_fix_reason(code: &str) -> String {
    match code {
        "TG0501" => "pagination is confirmed under `pagination` in tungsten.yml; an overlay \
                     cannot declare it"
            .into(),
        "TG0401" => "only repeated operation ids can be fixed by an overlay; a renamed field, \
                     enum value or type needs a naming decision"
            .into(),
        _ => format!("no overlay action derives its fix; see `tungsten explain {code}`"),
    }
}

/// TG0401 about a repeated operation id (the other TG0401 diagnostics are
/// about fields, enum values and types).
fn is_operation_id(d: &Diagnostic) -> bool {
    d.message.starts_with("operation id `")
}

/// The id the IR gives the operation built from each (file, pointer), without
/// its namespace. More than one distinct id means the document is shared.
fn operation_ids(ir: &Ir) -> BTreeMap<(&str, &str), BTreeSet<&str>> {
    fn walk<'a>(r: &'a Resource, out: &mut BTreeMap<(&'a str, &'a str), BTreeSet<&'a str>>) {
        for op in &r.operations {
            record(op, out);
        }
        for c in &r.children {
            walk(c, out);
        }
    }
    fn record<'a>(op: &'a Operation, out: &mut BTreeMap<(&'a str, &'a str), BTreeSet<&'a str>>) {
        if let Some((_, local)) = op.id.0.split_once('.') {
            out.entry((op.source.file.as_str(), op.source.pointer.as_str()))
                .or_default()
                .insert(local);
        }
    }
    let mut out = BTreeMap::new();
    for ns in &ir.namespaces {
        ns.resources.iter().for_each(|r| walk(r, &mut out));
        ns.planned.iter().for_each(|op| record(op, &mut out));
    }
    out
}

/// The action for the operation at `pointer`, or why there is none.
fn action(
    doc: &tungsten_openapi::Document,
    ids: &BTreeMap<(&str, &str), BTreeSet<&str>>,
    pointer: &str,
    diagnostics: &[&Diagnostic],
) -> Result<Action, String> {
    let node = doc
        .get(pointer)
        .and_then(Value::as_object)
        .ok_or("the operation is not an object of the document")?;
    let mut distinct = ids.get(&(doc.name.as_str(), pointer)).into_iter().flatten();
    let (Some(id), None) = (distinct.next(), distinct.next()) else {
        return Err(
            "the operation has no single id in the IR (the document is shared by \
                    several namespaces, or the operation is expanded)"
                .into(),
        );
    };
    let current = node.get("operationId").and_then(Value::as_str);
    if current == Some(*id) {
        return Err("the operation already has the id the IR uses".into());
    }
    let target = overlay_target(&doc.root, pointer)
        .ok_or("the operation cannot be selected by an Overlay target")?;
    let mut diagnostics = diagnostics.to_vec();
    diagnostics.sort_by(|a, b| (&a.code, &a.message).cmp(&(&b.code, &b.message)));
    Ok(Action {
        codes: diagnostics.iter().map(|d| d.code.clone()).collect(),
        comments: diagnostics
            .iter()
            .map(|d| format!("{}: {}", d.code, one_line(&d.message)))
            .collect(),
        pointer: pointer.to_string(),
        target,
        update: json!({ "operationId": id }),
    })
}

fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The overlay document for one input, as YAML. Deterministic: no
/// timestamps, actions in pointer order.
pub fn render_overlay(input: &InputSuggestions) -> String {
    let mut out = format!(
        "# Suggested by `tungsten overlay suggest` for namespace {} (spec {}).\n\
         # Review it, then list this file under `overlays` of that input in tungsten.yml.\n\
         overlay: 1.0.0\n\
         info:\n  title: {}\n  version: 1.0.0\n",
        quoted(&input.namespace),
        quoted(&input.spec),
        quoted(&format!("Suggestions for {}", input.namespace)),
    );
    if input.actions.is_empty() {
        out.push_str("actions: []\n");
        return out;
    }
    out.push_str("actions:\n");
    for action in &input.actions {
        for comment in &action.comments {
            out.push_str("  # ");
            out.push_str(comment);
            out.push('\n');
        }
        out.push_str("  - target: ");
        out.push_str(&quoted(&action.target));
        out.push_str("\n    update:\n");
        if let Some(update) = action.update.as_object() {
            for (key, value) in update {
                out.push_str("      ");
                out.push_str(&key_scalar(key));
                out.push_str(": ");
                out.push_str(&value.to_string());
                out.push('\n');
            }
        }
    }
    out
}

/// A mapping key: plain when it is a simple identifier, quoted otherwise.
fn key_scalar(key: &str) -> String {
    let plain = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain { key.to_string() } else { quoted(key) }
}

/// A JSON string literal is a valid YAML double-quoted scalar.
fn quoted(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}
