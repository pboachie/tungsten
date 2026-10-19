// SPDX-License-Identifier: AGPL-3.0-only
//! Immutable, in-process generation preparation. No disk cache or effects.

use tungsten_core::Diagnostics;
use tungsten_ir::Ir;

use crate::{Emitter, FileSet, TargetConfig, sdk::SdkPlan};

/// Reusable preparation for one immutable IR and a selected set of emitters.
/// Compatible SDK targets share one structural plan across support checks,
/// reports and emission. Other targets retain their existing emitter adapter.
/// The plan borrows its IR, so it cannot outlive or mutate its source snapshot.
#[derive(Debug)]
pub struct PreparedGeneration<'a> {
    ir: &'a Ir,
    sdk: Option<SdkPlan<'a>>,
}

impl<'a> PreparedGeneration<'a> {
    /// Prepare only the services needed by `emitters`. Recreate preparation
    /// when inputs, configuration or target selection change; it is not a
    /// persistent cache or a complete project dependency identity.
    pub fn new(ir: &'a Ir, emitters: &[&dyn Emitter]) -> Self {
        Self {
            ir,
            sdk: emitters
                .iter()
                .any(|emitter| emitter.uses_sdk_plan())
                .then(|| SdkPlan::new(ir).0),
        }
    }

    /// The source IR of this preparation.
    pub fn ir(&self) -> &'a Ir {
        self.ir
    }

    /// The shared structural plan, when a selected emitter needs one.
    pub fn sdk_plan(&self) -> Option<&SdkPlan<'a>> {
        self.sdk.as_ref()
    }

    /// Check support with the same plan later used for emission.
    pub fn supports(&self, emitter: &dyn Emitter) -> Diagnostics {
        match self.sdk.as_ref().filter(|_| emitter.uses_sdk_plan()) {
            Some(plan) => emitter.supports_prepared(self.ir, plan),
            None => emitter.supports(self.ir),
        }
    }

    /// Emit using this preparation and the target's native naming/lowering.
    /// External executable invocation remains the caller's explicit effect.
    pub fn emit(
        &self,
        emitter: &dyn Emitter,
        cfg: &TargetConfig,
        out: &mut FileSet,
    ) -> Diagnostics {
        match self.sdk.as_ref().filter(|_| emitter.uses_sdk_plan()) {
            Some(plan) => emitter.emit_prepared(self.ir, plan, cfg, out),
            None => emitter.emit(self.ir, cfg, out),
        }
    }
}
