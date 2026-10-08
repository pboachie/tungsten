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

/// The meaning and fix texts of every extended explanation.
pub fn explained_texts() -> Vec<String> {
    crate::explain_table::EXPLANATIONS
        .iter()
        .flat_map(|e| [e.meaning.to_string(), e.fix.to_string()])
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

/// The unified diff `tungsten diff` shows for a changed file (`a/<path>`
/// to `b/<path>`): the text, lines added, lines removed and hunk lines cut
/// after `max_lines`.
pub fn unified_diff(
    path: &str,
    old: &str,
    new: &str,
    max_lines: usize,
) -> (String, usize, usize, usize) {
    use crate::textdiff::{Side, unified};
    let p = unified(Side::File(path), Side::File(path), old, new, max_lines);
    (p.text, p.added, p.removed, p.truncated)
}

/// Escape text for the HTML report.
pub fn html_escape(text: &str) -> String {
    crate::html::escape(text)
}

/// The report's MCP budgets for an MCP target's files (`None`: no MCP
/// manifest among them).
pub fn report_mcp_budgets(
    files: &tungsten_emit::FileSet,
    schema_budget: u32,
) -> Option<crate::output::McpBudgets> {
    crate::commands::report::mcp_budgets(files, schema_budget)
}

/// The report's tools.json budgets for a docs target's files.
pub fn report_tool_budgets(
    files: &tungsten_emit::FileSet,
    schema_budget: u32,
) -> Option<crate::output::ToolBudgets> {
    crate::commands::report::tool_budgets(files, schema_budget)
}
