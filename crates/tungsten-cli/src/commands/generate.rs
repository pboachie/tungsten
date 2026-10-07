// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten generate`: compile, run each configured target's emitter and
//! write (or, with `--check`, compare) its output directory. Also runs the
//! emitters in memory for `check --ci`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tungsten_build::Compiled;
use tungsten_core::{Diagnostic, Diagnostics, Severity};
use tungsten_emit::{FileSet, WriteOptions, stale_files, write_output};
use tungsten_ir::Ir;

use crate::args::GenerateArgs;
use crate::input::{self, Input};
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, GenerateResult, TargetReport, TargetStatus,
};
use crate::stats::{headline, ir_stats, plural};
use crate::{Report, exit, targets};

/// What to do with each target's file set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Write { dry_run: bool, force: bool },
    Check,
}

/// Where a project's targets live: the manifest's directory and the name
/// diagnostics use for the manifest.
pub(crate) struct Project {
    pub base_dir: PathBuf,
    pub manifest: String,
}

impl Project {
    pub fn of(path: &Path) -> Project {
        let (file, default) = match input::resolve(path) {
            Ok(Input::Project(manifest)) => (manifest, None),
            Ok(Input::Spec(spec)) => (spec, Some(tungsten_build::DEFAULT_MANIFEST_NAME)),
            Err(_) => (
                path.to_path_buf(),
                Some(tungsten_build::DEFAULT_MANIFEST_NAME),
            ),
        };
        let manifest = match default {
            Some(name) => name.to_string(),
            None => file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| tungsten_build::DEFAULT_MANIFEST_NAME.to_string()),
        };
        Project {
            base_dir: file.parent().map(Path::to_path_buf).unwrap_or_default(),
            manifest,
        }
    }
}

/// The outcome of running targets.
pub(crate) struct Outcome {
    pub reports: Vec<TargetReport>,
    pub diagnostics: Vec<Diagnostic>,
    /// An emitter or the disk failed (TG0701).
    pub io_failed: bool,
    /// A directory was not written because tungsten does not own it.
    pub refused: bool,
}

/// Run `names` (target ids in manifest order) over `ir`.
pub(crate) fn run_targets(
    compiled: &Compiled,
    ir: &Ir,
    project: &Project,
    names: &[String],
    mode: Mode,
) -> Outcome {
    let mut outcome = Outcome {
        reports: vec![],
        diagnostics: vec![],
        io_failed: false,
        refused: false,
    };
    for name in names {
        let cfg = targets::target_config(compiled.config.as_ref(), name, &project.base_dir);
        let mut report = TargetReport {
            target: name.clone(),
            status: TargetStatus::Skipped,
            out: cfg.out_dir.display().to_string(),
            files: 0,
            written: vec![],
            unchanged: vec![],
            removed: vec![],
            preserved: vec![],
            stale: vec![],
        };
        let Some(emitter) = targets::emitter(name) else {
            outcome
                .diagnostics
                .push(targets::not_implemented(name, &project.manifest));
            outcome.reports.push(report);
            continue;
        };
        let mut files = FileSet::new();
        let mut produced = emitter.supports(ir);
        produced.extend(emitter.emit(ir, &cfg, &mut files));
        report.files = files.len();
        let failed = produced.has_errors();
        outcome.diagnostics.extend(produced.0);
        if failed {
            report.status = TargetStatus::Failed;
            outcome.reports.push(report);
            continue;
        }
        match mode {
            Mode::Check => {
                let stale = stale_files(&files, &cfg.out_dir);
                report.status = if stale.is_empty() {
                    TargetStatus::Fresh
                } else {
                    TargetStatus::Stale
                };
                report.stale = stale.iter().map(|f| f.path.clone()).collect();
                outcome
                    .diagnostics
                    .extend(stale.iter().map(|f| f.diagnostic(&cfg.out_dir)));
            }
            Mode::Write { dry_run, force } => {
                let opts = WriteOptions {
                    dry_run,
                    force,
                    generator: Some(ir.generator.clone()),
                };
                match write_output(&files, &cfg.out_dir, &opts) {
                    Ok(w) => {
                        report.status = if dry_run {
                            TargetStatus::DryRun
                        } else {
                            TargetStatus::Generated
                        };
                        report.written = w.written;
                        report.unchanged = w.unchanged;
                        report.removed = w.removed;
                        report.preserved = w.preserved;
                    }
                    Err(d) => {
                        report.status = classify(&d, &mut outcome);
                        outcome.diagnostics.extend(d.0);
                    }
                }
            }
        }
        outcome.reports.push(report);
    }
    outcome
}

/// The status of a target whose write failed with `d`, noting I/O failures
/// and refusals in `outcome`.
fn classify(d: &Diagnostics, outcome: &mut Outcome) -> TargetStatus {
    if d.iter().any(|x| x.code == "TG0701") {
        outcome.io_failed = true;
        TargetStatus::Failed
    } else if d.iter().any(|x| x.code == "TG0703") {
        outcome.refused = true;
        TargetStatus::Refused
    } else {
        TargetStatus::Failed
    }
}

/// The exit code for diagnostics and an outcome: I/O failures first, then
/// refusals, then errors.
pub(crate) fn exit_code(diagnostics: &[Diagnostic], outcome: &Outcome) -> i32 {
    if outcome.io_failed {
        exit::INTERNAL
    } else if outcome.refused {
        exit::REFUSED
    } else if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        exit::FAILED
    } else {
        exit::OK
    }
}

pub(crate) fn run(args: &GenerateArgs) -> Report {
    let mut compiled = input::compile(&args.input.path);
    let mut report = Report::new(CommandName::Generate);
    report.diagnostics = compiled.diagnostics.0.clone();
    report.sources = std::mem::take(&mut compiled.workspace.sources);
    let Some(ir) = compiled.ir.as_ref().filter(|_| !compiled.has_errors()) else {
        report.exit = exit::FAILED;
        return report;
    };
    let names = match select(&compiled, &args.target) {
        Ok(names) => names,
        Err(error) => return report.failed(exit::FAILED, error),
    };
    let mode = if args.check {
        Mode::Check
    } else {
        Mode::Write {
            dry_run: args.dry_run,
            force: args.force,
        }
    };
    let project = Project::of(&args.input.path);
    let outcome = run_targets(&compiled, ir, &project, &names, mode);
    report
        .diagnostics
        .extend(outcome.diagnostics.iter().cloned());
    report.exit = exit_code(&report.diagnostics, &outcome);
    let result = GenerateResult {
        targets: outcome.reports,
        dry_run: args.dry_run,
        check: args.check,
    };
    let path = args.input.path.display().to_string();
    report.human = human(
        &headline(
            &path,
            compiled.workspace.documents.len(),
            Some(&ir_stats(ir)),
        ),
        &result,
    );
    report.result = Some(CommandResult::Generate(result));
    report
}

/// The targets to run: the requested ones (aliases resolved, manifest
/// order, each once) or every configured one.
fn select(compiled: &Compiled, wanted: &[String]) -> Result<Vec<String>, CliError> {
    let configured: Vec<String> = compiled
        .config
        .as_ref()
        .map(|c| c.targets.keys().cloned().collect())
        .unwrap_or_default();
    if configured.is_empty() {
        return Err(CliError::new(ErrorKind::NotFound, "no targets are configured")
            .with_help("add a `targets:` section to tungsten.yml, for example `targets: { docs: { out: generated/docs } }`"));
    }
    if wanted.is_empty() {
        return Ok(configured);
    }
    let wanted: Vec<&str> = wanted
        .iter()
        .map(|w| targets::canonical(w.trim()))
        .collect();
    if let Some(missing) = wanted.iter().find(|w| !configured.iter().any(|c| c == *w)) {
        return Err(CliError::new(
            ErrorKind::NotFound,
            format!("target `{missing}` is not configured in tungsten.yml"),
        )
        .with_help(format!("configured targets: {}", configured.join(", "))));
    }
    Ok(configured
        .into_iter()
        .filter(|c| wanted.contains(&c.as_str()))
        .collect())
}

fn human(headline: &str, r: &GenerateResult) -> String {
    let mut out = format!("{headline}\n");
    for t in &r.targets {
        let _ = writeln!(out, "  {:<12}{}", t.target, describe_target(t));
    }
    let done = r
        .targets
        .iter()
        .filter(|t| t.status != TargetStatus::Skipped)
        .count();
    let summary = if r.check {
        let stale = r
            .targets
            .iter()
            .filter(|t| t.status == TargetStatus::Stale)
            .count();
        format!(
            "checked {} · {}",
            plural(done, "target", "targets"),
            if stale == 0 {
                "up to date".to_string()
            } else {
                format!("{stale} stale: run `tungsten generate`")
            }
        )
    } else if r.dry_run {
        format!(
            "dry run · {} · nothing written",
            plural(done, "target", "targets")
        )
    } else {
        let written: usize = r.targets.iter().map(|t| t.written.len()).sum();
        let unchanged: usize = r.targets.iter().map(|t| t.unchanged.len()).sum();
        format!(
            "wrote {} · {written} written · {unchanged} unchanged",
            plural(done, "target", "targets")
        )
    };
    let _ = writeln!(out, "  {:<12}{summary}", "result");
    out
}

/// One line per target for human output.
pub(crate) fn describe_target(t: &TargetReport) -> String {
    let files = plural(t.files, "file", "files");
    match t.status {
        TargetStatus::Generated => format!(
            "{} · {files} · {} written, {} unchanged, {} removed{}",
            t.out,
            t.written.len(),
            t.unchanged.len(),
            t.removed.len(),
            preserved(t)
        ),
        TargetStatus::DryRun => format!(
            "{} · {files} · would write {}, {} unchanged, would remove {}{}",
            t.out,
            t.written.len(),
            t.unchanged.len(),
            t.removed.len(),
            preserved(t)
        ),
        TargetStatus::Fresh => format!("{} · {files} · up to date", t.out),
        TargetStatus::Stale => format!(
            "{} · stale: {}",
            t.out,
            plural(t.stale.len(), "file differs", "files differ")
        ),
        TargetStatus::Skipped => "skipped: no emitter in this version".to_string(),
        TargetStatus::Failed => format!("{} · failed (see diagnostics)", t.out),
        TargetStatus::Refused => format!("{} · refused: not generated by tungsten", t.out),
    }
}

fn preserved(t: &TargetReport) -> String {
    if t.preserved.is_empty() {
        String::new()
    } else {
        format!(", {} custom kept", t.preserved.len())
    }
}
