// SPDX-License-Identifier: AGPL-3.0-only
//! The agent manifest (`agent.yml`) and its compilation into
//! the IR's agent metadata (`Operation.agent`, `Ir.agent`, ambiguous
//! responses, manifest-declared gates, sensitive fields).
//!
//! Precedence for every agent-facing property: `agent.yml` > spec
//! `x-agent-*` extension (in `Operation.extensions` and schema fields) >
//! inferred default (a required `Idempotency-Key` header makes an operation
//! with side effects `caller_owned`, `persist_required`) > the manifest's
//! `defaults` > the built-in method defaults
//! (`OperationAgentMeta::default_for`).
//!
//! Loading reports, with pointer and line: YAML problems (TG0601), shape
//! and rule violations (TG0602). [`apply`] reports, as warnings, references
//! the compiled API does not have (TG0603, TG0604), unusable verification,
//! poll or pagination targets (TG0605), unknown error codes (TG0606),
//! field paths that name nothing (TG0607), unknown gates (TG0608), macro
//! adjustments (TG0609) and unknown or malformed `x-agent-*` extensions
//! (TG0610, TG0611). A rule that cannot apply is dropped; the rest of the
//! manifest still applies.
//!
//! Stability: `load`, `parse_str`, `apply` and `json_schema` are shared by the
//! build driver and the CLI. Changes are additive.

mod apply;
mod check;
mod clusters;
mod errors;
mod expr;
mod extensions;
mod fields;
mod index;
mod macros;
mod model;
mod prune;
mod report;

use std::path::Path;

use tungsten_config::ManifestSource;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_ir::Ir;

pub use check::AGENT_MANIFEST_VERSION;
pub use model::*;

/// File name of the agent manifest looked up next to `tungsten.yml`.
pub const DEFAULT_AGENT_MANIFEST: &str = "agent.yml";

impl Default for AgentConfig {
    /// An empty manifest: version 1, built-in defaults, no rules.
    fn default() -> Self {
        Self {
            agent: AGENT_MANIFEST_VERSION,
            defaults: DefaultsConfig::default(),
            tools: vec![],
            errors: ErrorsConfig::default(),
            gates: Default::default(),
            macros: vec![],
            disclosure: DisclosureConfig::default(),
        }
    }
}

/// A parsed, schema-valid agent manifest.
#[derive(Debug, Clone)]
pub struct AgentManifest {
    raw: serde_json::Value,
    config: AgentConfig,
    source: ManifestSource,
}

impl PartialEq for AgentManifest {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw && self.config == other.config
    }
}

impl AgentManifest {
    /// The manifest as JSON (after YAML conversion).
    pub fn as_json(&self) -> &serde_json::Value {
        &self.raw
    }

    /// The typed manifest.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// The manifest text and node positions.
    pub fn source(&self) -> &ManifestSource {
        &self.source
    }
}

/// Result of loading a manifest: the manifest when it is usable, its
/// diagnostics, and its text so callers can map spans.
#[derive(Debug, Clone)]
pub struct ParsedAgentManifest {
    pub name: String,
    pub text: String,
    /// `None` whenever `diagnostics` has errors.
    pub manifest: Option<AgentManifest>,
    pub diagnostics: Diagnostics,
    /// Node positions; [`ManifestSource::attach_spans`] gives the
    /// diagnostics spans once the text is in a source map.
    pub source: ManifestSource,
}

/// Parse an agent manifest from text. `name` is used in diagnostics.
pub fn parse_str(name: &str, text: &str) -> ParsedAgentManifest {
    let (value, source) = tungsten_config::parse_yaml(name, text);
    let mut diagnostics = Diagnostics::new();
    let manifest = match value {
        Err(d) => {
            diagnostics.push(d);
            None
        }
        Ok(serde_json::Value::Null) => {
            diagnostics.push(
                Diagnostic::error(
                    "TG0602",
                    "the agent manifest is empty; it needs at least `agent: 1`",
                )
                .at(name, "", None),
            );
            None
        }
        Ok(raw) => {
            match tungsten_config::deserialize_manifest::<AgentConfig>(&raw, name, &source) {
                Err(d) => {
                    diagnostics.push(d);
                    None
                }
                Ok(config) => {
                    let mut r = report::Reporter::new(name, Some(&source));
                    check::check(&config, &mut r);
                    diagnostics.extend(r.out);
                    (!diagnostics.has_errors()).then(|| AgentManifest {
                        raw,
                        config,
                        source: source.clone(),
                    })
                }
            }
        }
    };
    ParsedAgentManifest {
        name: name.to_string(),
        text: text.to_string(),
        manifest,
        diagnostics,
        source,
    }
}

/// Read and parse an agent manifest file, named in diagnostics by its file
/// name.
pub fn load(path: &Path) -> ParsedAgentManifest {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    load_as(path, &name)
}

/// Read and parse an agent manifest file, named in diagnostics by `name`.
/// Only regular files are read; anything else is TG0101.
pub fn load_as(path: &Path, name: &str) -> ParsedAgentManifest {
    let read = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => std::fs::read_to_string(path),
        Ok(_) => Err(std::io::Error::other("not a regular file")),
        Err(e) => Err(e),
    };
    match read {
        Ok(text) => parse_str(name, &text),
        Err(e) => {
            let mut diagnostics = Diagnostics::new();
            diagnostics.push(
                Diagnostic::error("TG0101", format!("cannot read {name}: {e}")).at(name, "", None),
            );
            let (_, source) = tungsten_config::parse_yaml(name, "");
            ParsedAgentManifest {
                name: name.to_string(),
                text: String::new(),
                manifest: None,
                diagnostics,
                source,
            }
        }
    }
}

/// Compile agent metadata into `ir`: the manifest (when given) and the
/// spec's `x-agent-*` extensions, over the method defaults already set by
/// the builder. Diagnostics about the manifest name `manifest_name`;
/// diagnostics about extensions name the spec file and pointer of the
/// operation. Applying twice gives the same IR.
pub fn apply(ir: &mut Ir, manifest: Option<&AgentManifest>, manifest_name: &str) -> Diagnostics {
    let empty = AgentConfig::default();
    let (config, source) = match manifest {
        Some(m) => (&m.config, Some(&m.source)),
        None => (&empty, None),
    };
    let mut r = report::Reporter::new(manifest_name, source);
    apply::run(ir, config, &mut r);
    r.out
}

/// JSON Schema of `agent.yml`, published as `specs/agent-manifest.schema.json`.
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(AgentConfig)).unwrap_or_default()
}

/// Internals exposed to the private test harness.
#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod __testing {
    use tungsten_ir::Doc;

    /// The compact description of `doc` under the given pruning rules.
    pub fn compact_doc(
        doc: Option<&Doc>,
        max_sentences: usize,
        drop_phrases: &[String],
        budget_tokens: usize,
    ) -> String {
        crate::prune::compact(
            doc,
            &crate::prune::Prune {
                max_sentences,
                drop_phrases,
                budget_tokens,
            },
        )
    }

    /// Approximate tokens of a text (characters / 4, rounded up).
    pub fn tokens(text: &str) -> usize {
        crate::prune::tokens(text)
    }

    /// The canonical text of a boolean expression, or why it is invalid.
    pub fn canonical_expr(text: &str) -> Result<String, String> {
        crate::expr::BoolExpr::parse(text).map(|e| e.canonical())
    }
}
