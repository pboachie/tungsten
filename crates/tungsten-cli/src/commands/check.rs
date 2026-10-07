// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten check`: compile and report.

use std::fmt::Write as _;

use tungsten_build::Compiled;
use tungsten_core::{Diagnostic, Severity};

use crate::args::CheckArgs;
use crate::output::{CheckResult, CommandName, CommandResult, DiagnosticCounts};
use crate::stats::{describe, describe_refs, ir_stats, plural, ref_stats};
use crate::{Report, exit, input};

pub(crate) fn run(args: &CheckArgs) -> Report {
    let compiled = input::compile(&args.input.path);
    let mut diagnostics = compiled.diagnostics.0.clone();
    let promoted = if args.strict {
        promote_warnings(&mut diagnostics)
    } else {
        0
    };
    let counts = count(&diagnostics);
    let result = CheckResult {
        inputs: input_names(&compiled),
        documents: compiled.workspace.documents.len(),
        counts,
        promoted,
        strict: args.strict,
        ci: args.ci,
        refs: ref_stats(&compiled.workspace.graph),
        stats: compiled.ir.as_ref().map(ir_stats),
    };
    let mut report = Report::new(CommandName::Check);
    report.exit = if counts.errors > 0 {
        exit::FAILED
    } else {
        exit::OK
    };
    report.human = human(&args.input.path.display().to_string(), &result);
    report.diagnostics = diagnostics;
    report.sources = compiled.workspace.sources;
    report.result = Some(CommandResult::Check(result));
    report
}

/// `--strict`: warnings become errors. Returns how many were promoted.
pub(crate) fn promote_warnings(diagnostics: &mut [Diagnostic]) -> usize {
    let mut n = 0;
    for d in diagnostics
        .iter_mut()
        .filter(|d| d.severity == Severity::Warning)
    {
        d.severity = Severity::Error;
        n += 1;
    }
    n
}

pub(crate) fn count(diagnostics: &[Diagnostic]) -> DiagnosticCounts {
    let mut c = DiagnosticCounts::default();
    for d in diagnostics {
        match d.severity {
            Severity::Error => c.errors += 1,
            Severity::Warning => c.warnings += 1,
            Severity::Info => c.infos += 1,
        }
    }
    c
}

/// Entry documents in manifest order.
fn input_names(compiled: &Compiled) -> Vec<String> {
    compiled
        .config
        .as_ref()
        .map(|c| c.inputs.iter().map(|i| i.spec.clone()).collect())
        .unwrap_or_default()
}

fn human(path: &str, r: &CheckResult) -> String {
    let mut out = String::new();
    let docs = plural(r.documents, "document", "documents");
    match &r.stats {
        Some(s) => {
            let o = &s.operations;
            let _ = writeln!(
                out,
                "tungsten {} · {} · {docs} · {} ({} implemented, {} planned, {} gated)",
                tungsten_build::TUNGSTEN_VERSION,
                s.api,
                plural(o.total, "operation", "operations"),
                o.implemented,
                o.planned,
                o.gated,
            );
        }
        None => {
            let _ = writeln!(
                out,
                "tungsten {} · {path} · {docs}",
                tungsten_build::TUNGSTEN_VERSION
            );
        }
    }
    if !r.inputs.is_empty() {
        let _ = writeln!(out, "  {:<9}{}", "inputs", r.inputs.join("  "));
    }
    let _ = writeln!(out, "  {:<9}{}", "refs", describe_refs(&r.refs));
    let ir = match &r.stats {
        Some(s) => describe(s),
        None => "not built".to_string(),
    };
    let _ = writeln!(out, "  {:<9}{ir}", "ir");
    let c = &r.counts;
    let status = if c.errors > 0 { "failed" } else { "ok" };
    let mut line = format!(
        "{status} · {} · {}",
        plural(c.errors, "error", "errors"),
        plural(c.warnings, "warning", "warnings")
    );
    if r.promoted > 0 {
        let _ = write!(line, " ({} promoted by --strict)", r.promoted);
    }
    let _ = writeln!(out, "  {:<9}{line}", "result");
    out
}
