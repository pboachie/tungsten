// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten check`: compile and report.

use std::fmt::Write as _;

use tungsten_build::Compiled;
use tungsten_core::{Diagnostic, Severity};
use tungsten_openapi::SpecVersion;

use crate::args::CheckArgs;
use crate::commands::generate::{Mode, Project, run_targets};
use crate::output::{
    CheckResult, CodeCount, CommandName, CommandResult, DiagnosticCounts, TargetReport,
    TargetStatus,
};
use crate::render::thousands;
use crate::stats::{describe, describe_refs, headline, ir_stats, plural, ref_stats};
use crate::{CliEnv, Collapse, Report, exit, input};

pub(crate) fn run(args: &CheckArgs, env: &CliEnv, no_external: bool) -> Report {
    let compiled = input::compile(&args.input.path);
    let mut diagnostics = compiled.diagnostics.0.clone();
    // Whether the project compiles, before `--strict` turns warnings into
    // errors: the staleness comparison runs whenever the emitters can.
    let compiles = !has_errors(&diagnostics);
    // A promoted warning stays collapsible: only real errors are always shown.
    let mut eligible: Vec<bool> = diagnostics
        .iter()
        .map(|d| d.severity != Severity::Error)
        .collect();
    let mut promoted = if args.strict {
        promote_warnings(&mut diagnostics)
    } else {
        0
    };
    // `--ci`: the emitters run in memory and their output is compared with
    // the target directories (TG0901), once the project itself is clean.
    let mut outputs = vec![];
    let mut io_failed = false;
    if let Some(ir) = compiled.ir.as_ref().filter(|_| args.ci && compiles) {
        let names: Vec<String> = compiled
            .config
            .as_ref()
            .map(|c| c.targets.keys().cloned().collect())
            .unwrap_or_default();
        let project = Project::of(&args.input.path, env).with_no_external(no_external);
        let mut outcome = run_targets(&compiled, ir, &project, &names, Mode::Check, false);
        io_failed = outcome.io_failed;
        // The emitters' warnings (TG07xx, such as TG0713 over the schema
        // budget) are warnings like the compiler's: `--strict` fails on them.
        eligible.extend(
            outcome
                .diagnostics
                .iter()
                .map(|d| d.severity != Severity::Error),
        );
        if args.strict {
            promoted += promote_warnings(&mut outcome.diagnostics);
        }
        diagnostics.extend(outcome.diagnostics);
        outputs = outcome.reports;
    }
    let counts = count(&diagnostics);
    let result = CheckResult {
        inputs: input_names(&compiled),
        openapi_versions: openapi_versions(&compiled),
        documents: compiled.workspace.documents.len(),
        counts,
        by_code: by_code(&diagnostics),
        promoted,
        strict: args.strict,
        ci: args.ci,
        refs: ref_stats(&compiled.workspace.graph),
        stats: compiled.ir.as_ref().map(ir_stats),
        outputs,
    };
    let mut report = Report::new(CommandName::Check);
    report.exit = if io_failed {
        exit::INTERNAL
    } else if counts.errors > 0 {
        exit::FAILED
    } else {
        exit::OK
    };
    report.human = human(&args.input.path.display().to_string(), &result);
    report.diagnostics = diagnostics;
    if !args.all_warnings {
        report.collapse = Some(Collapse {
            limit: COLLAPSE_AFTER,
            eligible,
        });
    }
    report.sources = compiled.workspace.sources;
    report.result = Some(CommandResult::Check(result));
    report
}

/// Diagnostics of one code and severity printed in full before the human
/// output summarizes the rest (`--all-warnings` lifts the limit).
const COLLAPSE_AFTER: usize = 3;

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

fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
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

/// Counts per (code, severity), ordered by code.
fn by_code(diagnostics: &[Diagnostic]) -> Vec<CodeCount> {
    let mut map: std::collections::BTreeMap<(&str, u8), (Severity, usize)> = Default::default();
    for d in diagnostics {
        let rank = match d.severity {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Info => 2,
        };
        map.entry((&d.code, rank)).or_insert((d.severity, 0)).1 += 1;
    }
    map.into_iter()
        .map(|((code, _), (severity, count))| CodeCount {
            code: code.to_string(),
            severity,
            count,
        })
        .collect()
}

/// The OpenAPI version of each entry document, in manifest order.
fn openapi_versions(compiled: &Compiled) -> Vec<Option<String>> {
    let ws = &compiled.workspace;
    ws.entry_docs
        .iter()
        .map(|doc| match &ws.documents.get((*doc)?)?.version {
            SpecVersion::V30(v) | SpecVersion::V31(v) => Some(v.clone()),
            SpecVersion::Fragment => None,
        })
        .collect()
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
    let mut out = headline(path, r.documents, r.stats.as_ref());
    out.push('\n');
    if !r.inputs.is_empty() {
        let inputs: Vec<String> = r
            .inputs
            .iter()
            .enumerate()
            .map(
                |(i, name)| match r.openapi_versions.get(i).cloned().flatten() {
                    Some(v) => format!("{name} (OpenAPI {v})"),
                    None => name.clone(),
                },
            )
            .collect();
        let _ = writeln!(out, "  {:<9}{}", "inputs", inputs.join("  "));
    }
    let _ = writeln!(out, "  {:<9}{}", "refs", describe_refs(&r.refs));
    let ir = match &r.stats {
        Some(s) => describe(s),
        None => "not built".to_string(),
    };
    let _ = writeln!(out, "  {:<9}{ir}", "ir");
    if !r.outputs.is_empty() {
        let outputs: Vec<String> = r
            .outputs
            .iter()
            .map(|t| format!("{} {}", t.target, output_state(t)))
            .collect();
        let _ = writeln!(out, "  {:<9}{}", "output", outputs.join(" · "));
    }
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
    if !r.by_code.is_empty() {
        let codes: Vec<String> = r
            .by_code
            .iter()
            .map(|c| format!("{} {} ×{}", c.code, severity_word(c), thousands(c.count)))
            .collect();
        let _ = writeln!(out, "  {:<9}{}", "codes", codes.join(" · "));
    }
    out
}

fn severity_word(c: &CodeCount) -> &'static str {
    match c.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// `fresh`, `stale (2 files)`, `skipped`, ... for the human `output` line.
fn output_state(t: &TargetReport) -> String {
    match t.status {
        TargetStatus::Fresh => "fresh".into(),
        TargetStatus::Stale => format!("stale ({})", plural(t.stale.len(), "file", "files")),
        TargetStatus::Skipped => "skipped".into(),
        TargetStatus::Failed => "failed".into(),
        TargetStatus::Generated
        | TargetStatus::DryRun
        | TargetStatus::Refused
        | TargetStatus::Withheld => "not checked".into(),
    }
}
