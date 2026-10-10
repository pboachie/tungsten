// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten generate`: compile, run each configured target's emitter and
//! write (or, with `--check`, compare) its output directory. Also runs the
//! emitters in memory for `check --ci`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tungsten_build::Compiled;
use tungsten_core::{Diagnostic, Diagnostics, Severity};
use tungsten_emit::external;
use tungsten_emit::{FileSet, TargetConfig, WriteOptions, stale_files, write_output};
use tungsten_ir::Ir;

use crate::args::GenerateArgs;
use crate::commands::check;
use crate::input::{self, Input};
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, GenerateResult, TargetReport, TargetStatus,
};
use crate::stats::{headline, ir_stats, plural};
use crate::{CliEnv, Report, exit, targets};

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
    /// Where external emitters are looked up: the environment's search
    /// path, by default the process `PATH`.
    pub search_path: OsString,
}

impl Project {
    /// The manifest's directory as a working directory for a child process
    /// (`.` when the manifest is named without a directory).
    pub fn working_dir(&self) -> PathBuf {
        if self.base_dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            self.base_dir.clone()
        }
    }

    pub fn of(path: &Path, env: &CliEnv) -> Project {
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
            search_path: env
                .search_path
                .clone()
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default(),
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
    /// Emitter warnings promoted to errors (`strict`).
    pub promoted: usize,
}

/// One target's emitter run in memory.
pub(crate) struct Emitted {
    pub cfg: TargetConfig,
    /// The emitter's files; `None` when this version has no emitter for
    /// the target.
    pub files: Option<FileSet>,
    /// `supports` and `emit` diagnostics.
    pub diagnostics: Diagnostics,
    /// The agent tools the target serves (tool name to operation id or
    /// macro name), recorded in its API surface snapshot.
    pub tools: BTreeMap<String, String>,
}

impl Emitted {
    /// Whether the emitter reported errors.
    pub fn failed(&self) -> bool {
        self.diagnostics.has_errors()
    }
}

/// Run the emitter of target `name` over `ir` without touching the disk:
/// the built-in one, or the external emitter its `tungsten.yml` entry
/// declares.
pub(crate) fn emit_target(compiled: &Compiled, ir: &Ir, project: &Project, name: &str) -> Emitted {
    let cfg = targets::target_config(compiled.config.as_ref(), name, &project.base_dir);
    let Some(emitter) = targets::emitter(name) else {
        return match tungsten_config::external_target(&cfg.options) {
            Ok(Some(external)) => emit_external(ir, project, cfg, &external),
            _ => Emitted {
                cfg,
                files: None,
                diagnostics: Diagnostics::new(),
                tools: BTreeMap::new(),
            },
        };
    };
    let mut files = FileSet::new();
    let mut diagnostics = emitter.supports(ir);
    diagnostics.extend(emitter.emit(ir, &cfg, &mut files));
    Emitted {
        tools: targets::tool_names(name, ir),
        cfg,
        files: Some(files),
        diagnostics,
    }
}

/// Run an external emitter (protocol: `tungsten_emit::external`). Its
/// diagnostics that name no location point at the target in the manifest.
fn emit_external(
    ir: &Ir,
    project: &Project,
    cfg: TargetConfig,
    external: &tungsten_config::ExternalTarget,
) -> Emitted {
    let name = cfg.name.clone();
    let mut limits = external::Limits::default();
    if let Some(ms) = external.timeout_ms {
        limits.timeout = std::time::Duration::from_millis(ms);
    }
    if let Some(bytes) = external.max_output_bytes {
        limits.max_output_bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
    }
    let options: serde_json::Map<String, serde_json::Value> = cfg
        .options
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, _)| !tungsten_config::EXTERNAL_RESERVED_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut out = external::Emitted::default();
    match external::locate(
        &name,
        external.command.as_deref(),
        &project.base_dir,
        &project.search_path,
    ) {
        Ok(command) => {
            out = external::run(
                &name,
                &command,
                serde_json::Value::Object(options),
                ir,
                &project.working_dir(),
                limits,
            );
        }
        Err(d) => out.diagnostics.push(d),
    }
    for d in &mut out.diagnostics.0 {
        if d.labels.is_empty() {
            *d = d
                .clone()
                .at(project.manifest.clone(), format!("/targets/{name}"), None);
        }
    }
    Emitted {
        cfg,
        files: Some(out.files),
        diagnostics: out.diagnostics,
        tools: out.tools,
    }
}

/// Run `names` (target ids in manifest order) over `ir`. With `strict`,
/// an emitter's warnings are errors: the target fails, and every target is
/// emitted before anything is written, so when one fails none is written
/// (the others are `withheld`).
pub(crate) fn run_targets(
    compiled: &Compiled,
    ir: &Ir,
    project: &Project,
    names: &[String],
    mode: Mode,
    strict: bool,
) -> Outcome {
    let mut outcome = Outcome {
        reports: vec![],
        diagnostics: vec![],
        io_failed: false,
        refused: false,
        promoted: 0,
    };
    // The surface snapshot is written next to every target's manifest.
    let shared = matches!(mode, Mode::Write { .. }).then(|| Arc::new(ir.clone()));
    let mut emitted: Vec<(&String, Emitted)> = names
        .iter()
        .map(|name| {
            let mut e = emit_target(compiled, ir, project, name);
            if strict && e.files.is_some() {
                outcome.promoted += check::promote_warnings(&mut e.diagnostics.0);
            }
            (name, e)
        })
        .collect();
    // A strict run writes every target or none.
    let withhold = strict
        && matches!(mode, Mode::Write { .. })
        && emitted.iter().any(|(_, e)| e.files.is_some() && e.failed());
    for (name, e) in emitted.drain(..) {
        let Emitted {
            cfg,
            files,
            diagnostics,
            tools,
        } = e;
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
        let Some(files) = files else {
            outcome
                .diagnostics
                .push(targets::not_implemented(name, &project.manifest));
            outcome.reports.push(report);
            continue;
        };
        report.files = files.len();
        let failed = diagnostics.has_errors();
        outcome.diagnostics.extend(diagnostics.0);
        if failed {
            report.status = TargetStatus::Failed;
            outcome.reports.push(report);
            continue;
        }
        if withhold {
            report.status = TargetStatus::Withheld;
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
                    ir: shared.clone(),
                    tools,
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

pub(crate) fn run(args: &GenerateArgs, env: &CliEnv) -> Report {
    let mut compiled = input::compile(&args.input.path);
    let mut report = Report::new(CommandName::Generate);
    report.diagnostics = compiled.diagnostics.0.clone();
    report.sources = std::mem::take(&mut compiled.workspace.sources);
    let mut promoted = 0;
    if args.strict {
        promoted = check::promote_warnings(&mut report.diagnostics);
    }
    let clean = !report
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    let Some(ir) = compiled.ir.as_ref().filter(|_| clean) else {
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
    let project = Project::of(&args.input.path, env);
    let outcome = run_targets(&compiled, ir, &project, &names, mode, args.strict);
    report
        .diagnostics
        .extend(outcome.diagnostics.iter().cloned());
    report.exit = exit_code(&report.diagnostics, &outcome);
    let result = GenerateResult {
        targets: outcome.reports,
        dry_run: args.dry_run,
        check: args.check,
        strict: args.strict,
        promoted: promoted + outcome.promoted,
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
pub(crate) fn select(compiled: &Compiled, wanted: &[String]) -> Result<Vec<String>, CliError> {
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
    let count = |statuses: &[TargetStatus]| {
        r.targets
            .iter()
            .filter(|t| statuses.contains(&t.status))
            .count()
    };
    // Only targets that were checked or (would be) written count as done.
    let done = count(&[
        TargetStatus::Generated,
        TargetStatus::DryRun,
        TargetStatus::Fresh,
        TargetStatus::Stale,
    ]);
    let failed = count(&[TargetStatus::Failed, TargetStatus::Refused]);
    let withheld = count(&[TargetStatus::Withheld]);
    let not_done = match (failed, withheld) {
        (0, 0) => String::new(),
        (f, 0) => format!(" · {f} failed"),
        (f, w) => format!(" · {f} failed, {w} withheld (--strict writes all or none)"),
    };
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
            "dry run · {} · nothing written{not_done}",
            plural(done, "target", "targets")
        )
    } else if done == 0 {
        format!("nothing written{not_done}")
    } else {
        let written: usize = r.targets.iter().map(|t| t.written.len()).sum();
        let unchanged: usize = r.targets.iter().map(|t| t.unchanged.len()).sum();
        format!(
            "wrote {} · {written} written · {unchanged} unchanged{not_done}",
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
        TargetStatus::Withheld => format!(
            "{} · {files} · not written: another target failed (--strict)",
            t.out
        ),
    }
}

fn preserved(t: &TargetReport) -> String {
    if t.preserved.is_empty() {
        String::new()
    } else {
        format!(", {} custom kept", t.preserved.len())
    }
}
