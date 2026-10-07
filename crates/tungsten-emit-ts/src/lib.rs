// SPDX-License-Identifier: AGPL-3.0-only
//! TypeScript SDK emitter (planning/05 "TypeScript SDK").
//!
//! PHASE-2 STUB: emits nothing. The emit-ts work package implements the SDK
//! against the `@tungsten/runtime` contract in `runtimes/ts/src/types.ts`.

use tungsten_core::Diagnostics;
use tungsten_emit::{Emitter, FileSet, TargetConfig};
use tungsten_ir::Ir;

/// Emits the `typescript` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct TypeScriptEmitter;

impl Emitter for TypeScriptEmitter {
    fn id(&self) -> &'static str {
        "typescript"
    }
    fn supports(&self, _ir: &Ir) -> Diagnostics {
        Diagnostics::new()
    }
    fn emit(&self, _ir: &Ir, _cfg: &TargetConfig, _out: &mut FileSet) -> Diagnostics {
        Diagnostics::new()
    }
}
