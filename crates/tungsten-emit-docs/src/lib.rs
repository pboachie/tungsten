// SPDX-License-Identifier: AGPL-3.0-only
//! Machine-first documentation emitter: [`DocsEmitter`] turns the IR into the
//! `docs` target, a small set of files meant to be read by language models
//! and by people.
//!
//! # Output
//!
//! - `llms.txt`: the llms.txt index of the API (title, summary, auth,
//!   operations per namespace and resource with their safety tier and
//!   idempotency rule);
//! - `llms-full.txt`: the full reference: arguments, responses, errors,
//!   remediation and types of every operation;
//! - `tools.json`: a provider-neutral function-calling manifest with one tool
//!   per callable operation and macro (name, compacted description and the
//!   JSON Schema of the arguments object as the SDK takes it);
//! - `README.md`: the human index, with tables per namespace and the error
//!   categories every SDK returns.
//!
//! # Usage
//!
//! The emitter implements [`tungsten_emit::Emitter`]. The `tungsten` CLI
//! runs it for the `docs` target of `tungsten.yml`; embedding it looks like:
//!
//! ```text
//! let diags = DocsEmitter.supports(&ir);          // budget warnings (TG0713)
//! let mut files = FileSet::default();
//! let diags = DocsEmitter.emit(&ir, &cfg, &mut files);
//! // then tungsten_emit::write_output(...) puts the files on disk
//! ```
//!
//! # Guarantees
//!
//! Output is deterministic. The docs are honest about what can be called:
//! planned operations are only ever listed under "Planned (not available)"
//! and are never tools, and gated operations name their environment variable
//! wherever they appear. Agent metadata is rendered as the IR carries it
//! (method defaults when no agent manifest was applied).

mod compact;
mod llms;
mod model;
mod readme;
mod tools;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{Emitter, FileSet, TargetConfig};
use tungsten_ir::Ir;

/// Emits the `docs` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct DocsEmitter;

impl Emitter for DocsEmitter {
    fn id(&self) -> &'static str {
        "docs"
    }
    /// TG0713 for each tool over the schema budget; `emit` does not repeat it.
    fn supports(&self, ir: &Ir) -> Diagnostics {
        tools::budget_warnings(&model::Model::new(ir))
    }
    fn emit(&self, ir: &Ir, _cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let model = model::Model::new(ir);
        let files = [
            ("README.md", readme::readme(&model)),
            ("llms-full.txt", llms::llms_full(&model)),
            ("llms.txt", llms::llms(&model)),
            ("tools.json", tools::tools_json(&model)),
        ];
        let mut d = Diagnostics::new();
        for (path, text) in files {
            if let Err(e) = out.add(path, text) {
                d.push(Diagnostic::error(
                    "TG0701",
                    format!("docs output {path} could not be added: {e}"),
                ));
            }
        }
        d
    }
}

/// Internals exposed to the private test harness.
#[cfg(feature = "testing")]
pub mod __testing {
    use tungsten_ir::Ir;

    /// `(operation id or macro name, tool name)` in manifest order.
    pub fn tool_names(ir: &Ir) -> Vec<(String, String)> {
        let model = crate::model::Model::new(ir);
        let mut out: Vec<(String, String)> = model
            .callable()
            .map(|(_, c)| (c.op.id.0.clone(), c.tool.clone()))
            .collect();
        out.extend(
            model
                .macros
                .iter()
                .map(|m| (m.mac.name.0.clone(), m.tool.clone())),
        );
        out
    }

    /// Compact notation of a type reference.
    pub fn compact(ty: &tungsten_ir::TypeRef) -> String {
        crate::compact::Compact::default().type_ref(ty)
    }
}
