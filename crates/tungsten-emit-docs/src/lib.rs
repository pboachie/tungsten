// SPDX-License-Identifier: AGPL-3.0-only
//! Machine-first documentation emitter (planning/05 "Docs"): `llms.txt`,
//! `llms-full.txt`, `tools.json` and a README.
//!
//! PHASE-2 STUB: emits nothing. The emit-core work package implements it.

use tungsten_core::Diagnostics;
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
    fn emit(&self, _ir: &Ir, _cfg: &TargetConfig, _out: &mut FileSet) -> Diagnostics {
        Diagnostics::new()
    }
}
