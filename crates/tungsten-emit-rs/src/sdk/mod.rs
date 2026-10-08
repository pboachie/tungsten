// SPDX-License-Identifier: AGPL-3.0-only
//! The SDK half of the `rust` target (owned by the SDK emitter work
//! package).

use tungsten_core::Diagnostics;
use tungsten_emit::{FileSet, TargetConfig};
use tungsten_ir::Ir;

pub(crate) fn supports(ir: &Ir) -> Diagnostics {
    let _ = ir;
    Diagnostics::new()
}

pub(crate) fn emit(ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
    let _ = (ir, cfg, out);
    Diagnostics::new()
}
