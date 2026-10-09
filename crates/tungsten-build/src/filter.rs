// SPDX-License-Identifier: AGPL-3.0-only
//! Which operations are callable: `include` / `planned_from` predicates of
//! a `tungsten.yml` input (FR-I8) and `x-runtime-gate` deployment gates.

use std::collections::BTreeMap;

use serde_json::Value;
use tungsten_config::{InputConfig, Predicate};
use tungsten_core::Diagnostic;
use tungsten_ir::{OperationStatus, RuntimeGate};
use tungsten_openapi::RefTarget;

use crate::ctx::{Ctx, str_of};

/// What happens to an operation before it is built.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Disposition {
    /// Callable.
    Keep,
    /// Not callable; recorded in `Namespace.planned` with this reason.
    Planned(String),
    /// Not callable and not recorded.
    Drop(String),
}

/// Apply the input's predicates to an operation object. Without `include`
/// every operation is included; an included operation that matches
/// `planned_from` (only possible without `include`) is planned.
pub(crate) fn disposition(input: &InputConfig, op: &Value) -> Disposition {
    let planned = input.planned_from.as_ref().filter(|p| matches(p, op));
    match (&input.include, planned) {
        (Some(include), planned) if !matches(include, op) => {
            let reason = format!(
                "excluded by include: {} != {}",
                include.extension, include.equals
            );
            match planned {
                Some(_) => Disposition::Planned(reason),
                None => Disposition::Drop(reason),
            }
        }
        (None, Some(p)) => Disposition::Planned(format!(
            "matched planned_from: {} == {}",
            p.extension, p.equals
        )),
        _ => Disposition::Keep,
    }
}

fn matches(predicate: &Predicate, op: &Value) -> bool {
    op.get(&predicate.extension) == Some(&predicate.equals)
}

/// Report an excluded operation (TG0504, info).
pub(crate) fn report_excluded(
    cx: &mut Ctx<'_>,
    id: &str,
    disposition: &Disposition,
    at: &RefTarget,
) {
    let message = match disposition {
        Disposition::Keep => return,
        Disposition::Planned(reason) => {
            format!("operation `{id}` is not callable ({reason}); recorded as planned")
        }
        Disposition::Drop(reason) => {
            format!("operation `{id}` is not callable ({reason}); omitted")
        }
    };
    cx.report(Diagnostic::info("TG0504", message), at);
}

/// The status of a callable operation: `Gated` when it carries a valid
/// `x-runtime-gate` (TG0505 info), otherwise `Implemented`. A malformed
/// gate is a TG0505 warning and is ignored.
pub(crate) fn status(cx: &mut Ctx<'_>, id: &str, op: &Value, at: &RefTarget) -> OperationStatus {
    let Some(gate) = op.get("x-runtime-gate") else {
        return OperationStatus::Implemented;
    };
    let Some(env_var) = str_of(gate, "environmentVariable") else {
        cx.report(
            Diagnostic::warning(
                "TG0505",
                format!(
                    "x-runtime-gate of `{id}` has no string `environmentVariable`; the operation is treated as ungated"
                ),
            ),
            at,
        );
        return OperationStatus::Implemented;
    };
    let gate = RuntimeGate {
        env_var: env_var.to_string(),
        default_on: gate
            .get("default")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        disabled_status: gate
            .get("disabledStatus")
            .and_then(Value::as_u64)
            .and_then(|s| u16::try_from(s).ok())
            .unwrap_or(404),
    };
    cx.report(
        Diagnostic::info(
            "TG0505",
            format!(
                "operation `{id}` is gated by {} (default {}, {} when disabled)",
                gate.env_var,
                if gate.default_on { "on" } else { "off" },
                gate.disabled_status
            ),
        ),
        at,
    );
    OperationStatus::Gated { gate }
}

/// Every `x-*` member of an operation object, sorted by key.
pub(crate) fn extensions(op: &Value) -> BTreeMap<String, Value> {
    op.as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| k.starts_with("x-"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
