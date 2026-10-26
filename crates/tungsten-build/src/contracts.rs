// SPDX-License-Identifier: AGPL-3.0-only
//! Opt-in quality audit of JSON success response contracts. Reads the same
//! normalized, overlaid workspace as the compiler; never infers payload types.

use std::collections::BTreeSet;

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_ir::ResponseKind;
use tungsten_openapi::{DocId, RefTarget, Workspace};

use crate::Compiled;
use crate::ctx::child;
use crate::operations::METHODS;
use crate::responses::{kind_of, parse_status};
use crate::types::{JsonType, declared_types, is_inert, is_structural};

/// A nonempty review note on a schema or media-type entry acknowledges that
/// node and its subtree. Use an overlay to keep the original spec untouched.
pub const REVIEWED_CONTRACT_EXTENSION: &str = "x-tungsten-reviewed-contract";

/// Bound reference/nested-schema walks without recursive Rust calls. Exhaustion
/// is itself a warning, so `--strict` cannot silently accept an incomplete audit.
const MAX_VISITS: usize = 100_000;

/// Audit JSON success responses, reporting each weak source node once. Findings
/// are warnings; the caller decides whether to promote them for CI enforcement.
pub fn audit(compiled: &Compiled) -> Diagnostics {
    if compiled.ir.is_none() || compiled.has_errors() {
        return Diagnostics::new();
    }
    let mut walk = Walk {
        ws: &compiled.workspace,
        visited: BTreeSet::new(),
        walked: BTreeSet::new(),
        findings: Diagnostics::new(),
        budget: MAX_VISITS,
    };
    for doc in compiled.workspace.entry_docs.iter().flatten() {
        walk.entry(*doc);
    }
    walk.findings.sort();
    walk.findings
}

struct Site {
    operation: RefTarget,
    name: String,
    status: String,
    media: String,
}

impl Site {
    fn description(&self) -> String {
        format!("{} ({}, {})", self.name, self.status, self.media)
    }
}

struct Walk<'a> {
    ws: &'a Workspace,
    visited: BTreeSet<(DocId, String)>,
    walked: BTreeSet<(DocId, String, bool)>,
    findings: Diagnostics,
    budget: usize,
}

impl Walk<'_> {
    fn entry(&mut self, doc: DocId) {
        let Some(paths) = self.ws.documents[doc]
            .get("/paths")
            .and_then(Value::as_object)
        else {
            return;
        };
        for path in paths.keys() {
            let local = child(
                &RefTarget {
                    doc,
                    pointer: "/paths".into(),
                },
                path,
            );
            let Some(base) = self.ws.deref(&local) else {
                continue;
            };
            // Match the compiler's PathItem semantics: a local operation beside
            // `$ref` wins, otherwise retain the referenced operation's location.
            let query = crate::pathitem::reads_3_2(self.ws, &local);
            for (method, _) in METHODS {
                if method == "query" && !query {
                    continue;
                }
                let own = child(&local, method);
                let target = if self.ws.get(&own).is_some() {
                    own
                } else {
                    child(&base, method)
                };
                self.operation(target, method, path);
            }
            if query {
                let extra = |at: &RefTarget| {
                    self.ws
                        .get(&child(at, "additionalOperations"))
                        .and_then(Value::as_object)
                        .map(|m| m.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default()
                };
                let mut seen = BTreeSet::new();
                for at in [local.clone(), base.clone()] {
                    for token in extra(&at) {
                        if tungsten_openapi::custom_method_problem(&token).is_none()
                            && seen.insert(token.clone())
                        {
                            let target = child(&child(&at, "additionalOperations"), &token);
                            self.operation(target, &token, path);
                        }
                    }
                }
            }
        }
    }

    fn operation(&mut self, operation: RefTarget, method: &str, path: &str) {
        let Some(value) = self.ws.get(&operation) else {
            return;
        };
        let Some(responses) = value.get("responses").and_then(Value::as_object) else {
            return;
        };
        let name = value
            .get("operationId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} {path}", method.to_ascii_uppercase()));
        for status in responses.keys() {
            if parse_status(status).map(kind_of) != Some(ResponseKind::Success) {
                continue;
            }
            let at = child(&child(&operation, "responses"), status);
            let Some(response) = self.ws.deref(&at) else {
                continue;
            };
            let Some(content) = self
                .ws
                .get(&response)
                .and_then(|v| v.get("content"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            for (media, entry) in content {
                if !is_json_media(media) || reviewed(entry) {
                    continue;
                }
                let site = Site {
                    operation: operation.clone(),
                    name: name.clone(),
                    status: status.clone(),
                    media: media.clone(),
                };
                let at = child(&child(&response, "content"), media);
                if entry.get("schema").is_none() {
                    if self.visited.insert((at.doc, at.pointer.clone())) {
                        self.finding("TG0930", "declares JSON content without a schema; the generated response accepts any JSON value", &at, &site);
                    }
                } else {
                    self.schemas(child(&at, "schema"), &site);
                }
            }
        }
    }

    fn schemas(&mut self, root: RefTarget, site: &Site) {
        // `classify` is false for allOf intersections and constrained $ref
        // siblings: a weak conjunct alone does not make the result weak. Their
        // nested fields are still audited independently.
        let mut pending = vec![(root, true)];
        while let Some((at, classify)) = pending.pop() {
            if !self.walked.insert((at.doc, at.pointer.clone(), classify)) {
                continue;
            }
            if self.budget == 0 {
                if !self.findings.iter().any(|d| d.code == "TG0932") {
                    self.finding("TG0932", "could not be completely audited: the schema traversal budget was exhausted", &at, site);
                }
                return;
            }
            self.budget -= 1;
            let Some(value) = self.ws.get(&at) else {
                continue;
            };
            if reviewed(value) {
                continue;
            }
            let Some(map) = value.as_object() else {
                if classify && value == &Value::Bool(true) {
                    self.weak("TG0305", "has an unconstrained `true` schema; the generated value accepts any JSON value", &at, site);
                }
                continue;
            };
            let reference = map.get("$ref").and_then(Value::as_str);
            let composed = ["oneOf", "anyOf", "allOf"]
                .iter()
                .any(|k| map.contains_key(*k));
            if let Some(reference) = reference.and_then(|r| self.ws.resolve(at.doc, r)) {
                let constrained = map.keys().any(|k| k != "$ref" && is_structural(k));
                pending.push((reference, classify && !constrained));
            }
            if classify && reference.is_none() && is_inert(map) {
                self.weak(
                    "TG0305",
                    "has an unconstrained schema; the generated value accepts any JSON value",
                    &at,
                    site,
                );
                continue;
            }
            let properties = map.get("properties").and_then(Value::as_object);
            let has_properties = properties.is_some_and(|p| !p.is_empty());
            let additional = map.get("additionalProperties");
            let typed_map = matches!(additional, Some(Value::Object(m)) if !is_inert(m));
            let closed = additional == Some(&Value::Bool(false));
            let object = declared_types(map)
                .ok()
                .flatten()
                .map(|types| types.contains(&JsonType::Object))
                .unwrap_or_else(|| {
                    [
                        "properties",
                        "additionalProperties",
                        "required",
                        "minProperties",
                        "maxProperties",
                    ]
                    .iter()
                    .any(|key| map.contains_key(*key))
                });
            if classify
                && object
                && !composed
                && reference.is_none()
                && !has_properties
                && !typed_map
                && !closed
            {
                self.weak("TG0931", "has a free-form object schema; the generated value is a mapping of arbitrary JSON values", &at, site);
                continue;
            }
            if let Some(properties) = properties {
                for name in properties.keys().rev() {
                    pending.push((child(&child(&at, "properties"), name), true));
                }
            }
            if typed_map {
                pending.push((child(&at, "additionalProperties"), true));
            }
            if map.contains_key("items") {
                pending.push((child(&at, "items"), true));
            }
            for key in ["oneOf", "anyOf", "allOf"] {
                if let Some(members) = map.get(key).and_then(Value::as_array) {
                    for index in (0..members.len()).rev() {
                        pending.push((
                            child(&child(&at, key), &index.to_string()),
                            classify && key != "allOf",
                        ));
                    }
                }
            }
        }
    }

    fn weak(&mut self, code: &str, message: &str, at: &RefTarget, site: &Site) {
        if self.visited.insert((at.doc, at.pointer.clone())) {
            self.finding(code, message, at, site);
        }
    }

    fn finding(&mut self, code: &str, limitation: &str, at: &RefTarget, site: &Site) {
        self.findings.push(Diagnostic::warning(code, format!("{} response {limitation}", site.description()))
            .at(self.ws.name(at.doc), &at.pointer, self.ws.span(at))
            .at(self.ws.name(site.operation.doc), &site.operation.pointer, self.ws.span(&site.operation))
            .with_help("declare reviewed response schemas in the spec or a standard OpenAPI overlay; for an intentional gap, add a nonempty x-tungsten-reviewed-contract review note to this schema or media-type entry. Shared source nodes are reported once, at the first operation that reaches them"));
    }
}

fn reviewed(value: &Value) -> bool {
    value
        .get(REVIEWED_CONTRACT_EXTENSION)
        .and_then(Value::as_str)
        .is_some_and(|note| !note.trim().is_empty())
}

fn is_json_media(media: &str) -> bool {
    let essence = media
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    essence == "application/json" || essence.ends_with("+json")
}
