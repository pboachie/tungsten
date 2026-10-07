// SPDX-License-Identifier: AGPL-3.0-only
//! Loading. PHASE-1 STUB (JSON only, local refs, no spans/overlays/normalization).

use tungsten_core::{Diagnostic, Digest};

use crate::{DocId, Document, LoadEntry, LoadOptions, RefTarget, SpecVersion, Workspace};

/// Load entry documents and everything they reference.
pub fn load(entries: &[LoadEntry], opts: &LoadOptions) -> Workspace {
    let mut ws = Workspace::default();
    for e in entries {
        match std::fs::read_to_string(&e.path) {
            Ok(text) => add(&mut ws, &e.display_name, text, opts),
            Err(err) => {
                ws.diagnostics.push(
                    Diagnostic::error("TG0101", format!("cannot read {}: {err}", e.path.display()))
                        .at(e.display_name.clone(), "", None),
                )
            }
        }
    }
    ws
}

/// Load one document from memory (used by tests and `--stdin`).
pub fn load_str(name: &str, text: &str, opts: &LoadOptions) -> Workspace {
    let mut ws = Workspace::default();
    add(&mut ws, name, text.to_string(), opts);
    ws
}

fn add(ws: &mut Workspace, name: &str, text: String, _opts: &LoadOptions) {
    let digest = Digest::of(text.as_bytes());
    let source = ws.sources.add(name, text.clone());
    let root: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(err) => {
            ws.diagnostics.push(
                Diagnostic::error("TG0102", format!("invalid JSON: {err}")).at(name, "", None),
            );
            return;
        }
    };
    let version = match root.get("openapi").and_then(|v| v.as_str()) {
        Some(v) if v.starts_with("3.1") => SpecVersion::V31(v.into()),
        Some(v) if v.starts_with("3.0") => SpecVersion::V30(v.into()),
        _ => {
            ws.diagnostics.push(
                Diagnostic::error("TG0103", "unsupported or missing openapi version")
                    .at(name, "", None),
            );
            return;
        }
    };
    let id: DocId = ws.documents.len();
    ws.documents.push(Document::new(
        source,
        name.to_string(),
        version,
        root,
        digest,
    ));
    ws.entries.push(id);
}

pub(crate) fn resolve(ws: &Workspace, from: DocId, reference: &str) -> Option<RefTarget> {
    let pointer = reference.strip_prefix('#')?;
    let t = RefTarget {
        doc: from,
        pointer: pointer.to_string(),
    };
    ws.get(&t).map(|_| t)
}
