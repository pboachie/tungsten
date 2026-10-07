// SPDX-License-Identifier: AGPL-3.0-only
//! The agent manifest (`agent.yml`, planning/04) and its compilation into
//! the IR's agent metadata (`Operation.agent`, `Ir.agent`, ambiguous
//! responses).
//!
//! Precedence for every agent-facing property: `agent.yml` > spec
//! `x-agent-*` extension (in `Operation.extensions` and schema fields) >
//! defaults (`OperationAgentMeta::default_for`).
//!
//! PHASE-2 CONTRACT: `load`, `parse_str`, `apply` and `json_schema` are
//! the API used by the driver and the CLI. The stub accepts any YAML and
//! applies nothing; the agent work package implements the manifest model,
//! validation (TG06xx) and every transform without changing these
//! signatures.

use std::path::Path;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_ir::Ir;

/// A parsed, schema-valid agent manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentManifest {
    raw: serde_json::Value,
}

impl AgentManifest {
    /// The manifest as JSON (after YAML conversion).
    pub fn as_json(&self) -> &serde_json::Value {
        &self.raw
    }
}

/// Result of loading a manifest: the manifest when it is usable, its
/// diagnostics, and its text so callers can map spans.
#[derive(Debug, Clone)]
pub struct ParsedAgentManifest {
    pub name: String,
    pub text: String,
    pub manifest: Option<AgentManifest>,
    pub diagnostics: Diagnostics,
}

/// Parse an agent manifest from text. `name` is used in diagnostics.
pub fn parse_str(name: &str, text: &str) -> ParsedAgentManifest {
    let mut diagnostics = Diagnostics::new();
    let manifest = match yaml_rust2::YamlLoader::load_from_str(text) {
        Ok(_) => Some(AgentManifest {
            raw: serde_json::Value::Null,
        }),
        Err(e) => {
            diagnostics.push(Diagnostic::error("TG0601", e.to_string()).at(name, "", None));
            None
        }
    };
    ParsedAgentManifest {
        name: name.to_string(),
        text: text.to_string(),
        manifest,
        diagnostics,
    }
}

/// Read and parse an agent manifest file.
pub fn load(path: &Path) -> ParsedAgentManifest {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match std::fs::read_to_string(path) {
        Ok(text) => parse_str(&name, &text),
        Err(e) => {
            let mut diagnostics = Diagnostics::new();
            diagnostics.push(
                Diagnostic::error("TG0101", format!("cannot read {name}: {e}")).at(
                    name.clone(),
                    "",
                    None,
                ),
            );
            ParsedAgentManifest {
                name,
                text: String::new(),
                manifest: None,
                diagnostics,
            }
        }
    }
}

/// Compile agent metadata into `ir`: the manifest (when given) and the
/// spec's `x-agent-*` extensions, over the method defaults already set by
/// the builder. Unknown operation references are TG0603.
pub fn apply(ir: &mut Ir, manifest: Option<&AgentManifest>, manifest_name: &str) -> Diagnostics {
    let _ = (ir, manifest, manifest_name);
    Diagnostics::new()
}

/// JSON Schema of `agent.yml`, published as `specs/agent-manifest.schema.json`.
pub fn json_schema() -> serde_json::Value {
    serde_json::json!({ "$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object" })
}
