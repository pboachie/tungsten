// SPDX-License-Identifier: AGPL-3.0-only
//! Machine-first documentation emitter (planning/05 "Docs"): `llms.txt`,
//! `llms-full.txt`, `tools.json` and a README.
//!
//! Honesty (NFR-8): planned operations are only ever listed under
//! "Planned (not available)" and are never tools; gated operations name
//! their environment variable wherever they appear. Agent metadata is
//! rendered as the IR carries it (method defaults when no agent manifest
//! was applied).

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
    fn supports(&self, _ir: &Ir) -> Diagnostics {
        Diagnostics::new()
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
