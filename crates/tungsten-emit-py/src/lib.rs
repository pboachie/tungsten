// SPDX-License-Identifier: AGPL-3.0-only
//! Python SDK emitter (planning/05 "Python SDK").
//!
//! Emits a package for Python >= 3.12 with Pydantic v2 models, a sync and an
//! async client over `tungsten-runtime`, descriptors as data per the contract
//! in `runtimes/python/src/tungsten_runtime/types.py`, and macros.
//!
//! PHASE-4 STUB: emits nothing. The emit-py work package implements it.

use tungsten_core::Diagnostics;
use tungsten_emit::{Emitter, FileSet, TargetConfig};
use tungsten_ir::Ir;

/// Emits the `python` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct PythonEmitter;

impl Emitter for PythonEmitter {
    fn id(&self) -> &'static str {
        "python"
    }
    fn supports(&self, _ir: &Ir) -> Diagnostics {
        Diagnostics::new()
    }
    fn emit(&self, _ir: &Ir, _cfg: &TargetConfig, _out: &mut FileSet) -> Diagnostics {
        Diagnostics::new()
    }
}
