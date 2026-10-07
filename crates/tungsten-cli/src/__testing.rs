// SPDX-License-Identifier: AGPL-3.0-only
//! Internals exposed to the private test harness (feature `testing`).

use std::path::{Path, PathBuf};

use tungsten_core::Diagnostic;
use tungsten_ir::Ir;

use crate::output::{DiagnosticCounts, ExplainResult};

/// Codes with an extended explanation, in table order.
pub fn explained_codes() -> Vec<&'static str> {
    crate::explain_table::EXPLANATIONS
        .iter()
        .map(|e| e.code)
        .collect()
}

/// Explain an operation or type id against an in-memory IR. Returns the
/// JSON result and the human text, or the error message.
pub fn explain_in_ir(ir: &Ir, target: &str) -> Result<(ExplainResult, String), String> {
    crate::commands::explain::explain_in_ir(ir, target).map_err(|e| match e.help {
        Some(help) => format!("{} ({help})", e.message),
        None => e.message,
    })
}

/// Apply `--strict` and count, as `check` does. Returns the number of
/// promoted warnings and the counts after promotion.
pub fn strict_counts(diagnostics: &mut [Diagnostic]) -> (usize, DiagnosticCounts) {
    let promoted = crate::commands::check::promote_warnings(diagnostics);
    (promoted, crate::commands::check::count(diagnostics))
}

pub fn parse_version(text: &str) -> Option<String> {
    crate::commands::doctor::parse_version(text)
}

pub fn relative_path(target: &Path, base: &Path) -> Option<PathBuf> {
    crate::commands::init::relative_path(target, base)
}

pub fn yaml_scalar(s: &str) -> String {
    crate::commands::init::yaml_scalar(s)
}
