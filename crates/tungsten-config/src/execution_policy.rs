// SPDX-License-Identifier: AGPL-3.0-only
//! Host constraints are separate from project declarations and profiles.

use tungsten_core::Diagnostics;
use tungsten_core::policy::{InvocationPolicy, RequestedExternalLimits};

use crate::TungstenConfig;

/// Run after ordinary manifest/profile validation, before acquiring inputs or
/// probing emitters. Presence survives extraction: null/missing are None.
pub fn validate(config: &TungstenConfig, policy: &InvocationPolicy) -> Diagnostics {
    let mut diagnostics = Diagnostics::new();
    for (ordinal, (_, options)) in config.targets.iter().enumerate() {
        let Ok(Some(target)) = crate::external_target(options) else {
            continue;
        };
        if let Err(mut diagnostic) = policy.validate_request(RequestedExternalLimits {
            timeout_ms: target.timeout_ms,
            max_output_bytes: target.max_output_bytes,
        }) {
            diagnostic.message = format!("external[{ordinal}]: {}", diagnostic.message);
            diagnostics.push(diagnostic);
        }
    }
    diagnostics
}
