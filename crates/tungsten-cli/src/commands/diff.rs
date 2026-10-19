// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten diff`: what regenerating each target would add, change or
//! remove, with unified diffs, and with `--semver` how the API surface
//! changed since the target's last generation.
//!
//! The emitters run in memory; nothing is written. Each target directory
//! is compared with its file set the way `generate --check` compares it
//! (`tungsten_emit::stale_files`). The surface comparison reads the
//! snapshot `generate` left in `.tungsten/surface.json` and classifies the
//! differences with the rules documented on `tungsten_emit::surface`.
//!
//! Exit codes: 0 when the comparison ran, whether or not regeneration
//! would change anything; 1 when the project has errors, an emitter failed
//! or a requested target is not configured.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use tungsten_core::{Diagnostic, Severity};
use tungsten_emit::surface::{self, ApiSurface, Level};
use tungsten_emit::{StaleReason, stale_files};
use tungsten_ir::Ir;

use crate::args::DiffArgs;
use crate::commands::generate::{Emitted, Project, emit_target, prepare, select};
use crate::output::{
    CommandName, CommandResult, DiffResult, DiffStatus, FileChange, FileDiff, MigrationAction,
    MigrationReport, SemverLevel, SemverReport, SnapshotState, SurfaceChange, TargetDiff,
};
use crate::stats::{headline, ir_stats, plural};
use crate::textdiff::{Side, unified};
use crate::{CliEnv, Report, exit, input, targets};

/// Diff lines kept per file; the rest is counted in `truncated_lines`.
pub(crate) const MAX_PATCH_LINES: usize = 200;

/// What to compute for each target.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DiffOptions {
    /// Keep the unified diff text of each file.
    pub patches: bool,
    /// Compare the API surface snapshot.
    pub semver: bool,
}

pub(crate) fn run(args: &DiffArgs, env: &CliEnv, no_external: bool) -> Report {
    let mut compiled = input::compile_with_policy(&args.input.path, env.policy.clone());
    let mut report = Report::new(CommandName::Diff);
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
    let project = Project::of(&args.input.path, env).with_no_external(no_external);
    let opts = DiffOptions {
        patches: true,
        semver: args.semver || args.migration,
    };
    let mut changes = vec![];
    let prepared = prepare(ir, &names);
    for name in &names {
        let emitted = emit_target(&compiled, &prepared, &project, name, false);
        report
            .diagnostics
            .extend(emitted.diagnostics.0.iter().cloned());
        changes.push(target_diff(
            name,
            &emitted,
            ir,
            &project,
            opts,
            &mut report.diagnostics,
        ));
    }
    if args.migration {
        for target in &mut changes {
            if let Some(semver) = &mut target.semver {
                let previous = ApiSurface::read(Path::new(&target.out)).ok().flatten();
                let known = previous.as_ref().is_some_and(|s| s.target.is_some());
                let mut actions: Vec<MigrationAction> = semver
                    .changes
                    .iter()
                    .map(|change| MigrationAction {
                        rule: change.rule.clone(),
                        subject: change.subject.clone(),
                        action: migration_action(change),
                    })
                    .collect();
                if !known {
                    actions.push(MigrationAction {
                        rule: "runtime_requirements_unknown".into(),
                        subject: target.target.clone(),
                        action: "The previous snapshot does not record dependency requirements; inspect the previous generated package manifest and installed runtimes before upgrading.".into(),
                    });
                }
                semver.migration = Some(MigrationReport {
                    actions,
                    runtime_requirements_known: known,
                    preserves_custom_files: true,
                });
            }
        }
    }
    let level = (args.semver || args.migration)
        .then(|| overall_level(&changes))
        .flatten();
    report.exit = if report
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        exit::FAILED
    } else {
        exit::OK
    };
    let result = DiffResult {
        changes,
        semver: args.semver || args.migration,
        level,
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
    report.result = Some(CommandResult::Diff(result));
    report
}

/// The highest level over the targets that had a snapshot.
pub(crate) fn overall_level(changes: &[TargetDiff]) -> Option<SemverLevel> {
    changes
        .iter()
        .filter_map(|t| t.semver.as_ref().and_then(|s| s.level))
        .max()
}

/// Compare one target's directory with what its emitter produced
/// (`emitted`). TG0702 (no emitter) and TG0902 (no usable snapshot) are
/// appended to `out`; the emitter's own diagnostics are the caller's.
pub(crate) fn target_diff(
    name: &str,
    emitted: &Emitted,
    ir: &Ir,
    project: &Project,
    opts: DiffOptions,
    out: &mut Vec<Diagnostic>,
) -> TargetDiff {
    let out_dir = &emitted.cfg.out_dir;
    let mut diff = TargetDiff {
        target: name.to_string(),
        status: DiffStatus::Skipped,
        out: out_dir.display().to_string(),
        added: 0,
        changed: 0,
        removed: 0,
        files: vec![],
        semver: None,
    };
    let Some(files) = &emitted.files else {
        out.push(if emitted.external_skipped {
            targets::external_skipped(name, &project.manifest)
        } else {
            targets::not_implemented(name, &project.manifest)
        });
        return diff;
    };
    if emitted.failed() {
        diff.status = DiffStatus::Failed;
        return diff;
    }
    let mut manifest_missing = false;
    for stale in stale_files(files, out_dir) {
        let on_disk = || std::fs::read(out_dir.join(&stale.path)).unwrap_or_default();
        let new = files.get(&stale.path).unwrap_or_default();
        let (change, old, new) = match stale.reason {
            StaleReason::ManifestMissing => {
                manifest_missing = true;
                continue;
            }
            StaleReason::Missing => (FileChange::Added, Vec::new(), new.to_vec()),
            StaleReason::Changed => (FileChange::Changed, on_disk(), new.to_vec()),
            StaleReason::Removed => (FileChange::Removed, on_disk(), Vec::new()),
        };
        match change {
            FileChange::Added => diff.added += 1,
            FileChange::Changed => diff.changed += 1,
            FileChange::Removed => diff.removed += 1,
        }
        diff.files
            .push(file_diff(&stale.path, change, &old, &new, opts.patches));
    }
    diff.status = if manifest_missing {
        DiffStatus::NotGenerated
    } else if diff.files.is_empty() {
        DiffStatus::Unchanged
    } else {
        DiffStatus::Changed
    };
    if opts.semver {
        diff.semver = Some(semver(ir, &emitted.tools, files, out_dir, out));
    }
    diff
}

fn file_diff(path: &str, change: FileChange, old: &[u8], new: &[u8], patches: bool) -> FileDiff {
    let text = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .ok()
            .filter(|t| !t.contains('\0'))
            .map(str::to_string)
    };
    let mut entry = FileDiff {
        path: path.to_string(),
        change,
        lines_added: 0,
        lines_removed: 0,
        patch: None,
        truncated_lines: 0,
    };
    let (Some(old_text), Some(new_text)) = (text(old), text(new)) else {
        return entry;
    };
    let (old_side, new_side) = match change {
        FileChange::Added => (Side::Absent, Side::File(path)),
        FileChange::Changed => (Side::File(path), Side::File(path)),
        FileChange::Removed => (Side::File(path), Side::Absent),
    };
    let patch = unified(old_side, new_side, &old_text, &new_text, MAX_PATCH_LINES);
    entry.lines_added = patch.added;
    entry.lines_removed = patch.removed;
    if patches {
        entry.patch = Some(patch.text);
        entry.truncated_lines = patch.truncated;
    }
    entry
}

/// The surface change since the snapshot in `out_dir`.
fn semver(
    ir: &Ir,
    tools: &BTreeMap<String, String>,
    files: &tungsten_emit::FileSet,
    out_dir: &Path,
    out: &mut Vec<Diagnostic>,
) -> SemverReport {
    let shown = out_dir.join(surface::SURFACE_PATH).display().to_string();
    match ApiSurface::read(out_dir) {
        Ok(Some(previous)) => {
            let changes = surface::compare(
                &previous,
                &ApiSurface::of(ir)
                    .with_tools(tools.clone())
                    .with_generated_files(files),
            );
            SemverReport {
                snapshot: SnapshotState::Present,
                migration: None,
                level: Some(level(surface::classify(&changes))),
                changes: changes
                    .into_iter()
                    .map(|c| SurfaceChange {
                        level: level(c.level),
                        rule: c.rule,
                        subject: c.subject,
                        detail: c.detail,
                    })
                    .collect(),
            }
        }
        Ok(None) => {
            out.push(
                Diagnostic::info(
                    "TG0902",
                    format!("no API surface snapshot at {shown}: the change since the last generation is not classified"),
                )
                .at(shown, "", None)
                .with_help("run `tungsten generate`; it records the snapshot that the next `tungsten diff --semver` compares with"),
            );
            unclassified(SnapshotState::Missing)
        }
        Err(e) => {
            out.push(
                Diagnostic::warning(
                    "TG0902",
                    format!("the API surface snapshot is unreadable: {e}; the change since the last generation is not classified"),
                )
                .at(shown, "", None)
                .with_help("run `tungsten generate` to write a new snapshot"),
            );
            unclassified(SnapshotState::Unreadable)
        }
    }
}

fn migration_action(change: &SurfaceChange) -> String {
    let action = match change.rule.as_str() {
        "operation_renamed" => {
            "Update SDK method accessors at call sites using the before/after names above."
        }
        "operation_removed" | "macro_removed" => {
            "Replace calls to the removed operation or macro with a supported replacement; remove obsolete calls if no replacement exists."
        }
        "tool_removed" | "tool_retargeted" => {
            "Update agent tool allowlists, stored tool calls and prompts; refresh the discovered tool catalogue."
        }
        "package_renamed" => {
            "Update package installation declarations and imports to the new package identity."
        }
        "package_version_changed" => {
            "Update the released package version as intended; version metadata alone does not imply an API break."
        }
        "runtime_dependency_changed" => {
            "Review the old and new dependency requirements above, update installed runtimes and lockfiles, and run SDK integration tests before deploying."
        }
        "required_arg_added" | "arg_became_required" => {
            "Supply the newly required input at every affected call site and update request fixtures."
        }
        "arg_removed" => "Remove the obsolete argument from calls and request fixtures.",
        "confirmation_required" => {
            "Add the required confirmation flow before invoking this operation."
        }
        "deprecation_changed" => {
            "Review deprecated calls and schedule migration to the API provider's supported replacement."
        }
        "response_removed" | "response_field_removed" => {
            "Update response consumers and fixtures to handle the removed response data."
        }
        _ if change.level == SemverLevel::Major => {
            "Update affected input or response types, call sites and fixtures to match the new contract; run consumer tests."
        }
        _ if change.level == SemverLevel::Minor => {
            "Review the additive contract change and regenerate consumer types and fixtures as needed."
        }
        _ => "Review the reported change and regenerate generated files when ready.",
    };
    action.to_string()
}

fn unclassified(snapshot: SnapshotState) -> SemverReport {
    SemverReport {
        snapshot,
        migration: None,
        level: None,
        changes: vec![],
    }
}

fn level(l: Level) -> SemverLevel {
    match l {
        Level::None => SemverLevel::None,
        Level::Patch => SemverLevel::Patch,
        Level::Minor => SemverLevel::Minor,
        Level::Major => SemverLevel::Major,
    }
}

pub(crate) fn level_name(l: SemverLevel) -> &'static str {
    match l {
        SemverLevel::None => "none",
        SemverLevel::Patch => "patch",
        SemverLevel::Minor => "minor",
        SemverLevel::Major => "major",
    }
}

pub(crate) fn change_name(c: FileChange) -> &'static str {
    match c {
        FileChange::Added => "added",
        FileChange::Changed => "changed",
        FileChange::Removed => "removed",
    }
}

/// One line describing a target's comparison.
pub(crate) fn describe(t: &TargetDiff) -> String {
    let counts = format!(
        "{} added, {} changed, {} removed",
        t.added, t.changed, t.removed
    );
    match t.status {
        DiffStatus::Unchanged => format!("{} · up to date", t.out),
        DiffStatus::Changed => format!("{} · {counts}", t.out),
        DiffStatus::NotGenerated => format!("{} · not generated yet · {counts}", t.out),
        DiffStatus::Skipped => "skipped: no emitter in this version".to_string(),
        DiffStatus::Failed => format!("{} · failed (see diagnostics)", t.out),
    }
}

/// One line describing a semver report.
pub(crate) fn describe_semver(s: &SemverReport) -> String {
    match (s.snapshot, s.level) {
        (SnapshotState::Present, Some(l)) => format!(
            "{} · {}",
            level_name(l),
            plural(s.changes.len(), "change", "changes")
        ),
        (SnapshotState::Unreadable, _) => "not classified: the snapshot is unreadable".into(),
        _ => "not classified: no snapshot from the last generation".into(),
    }
}

fn human(headline: &str, r: &DiffResult) -> String {
    let mut out = format!("{headline}\n");
    for t in &r.changes {
        let _ = writeln!(out, "  {:<12}{}", t.target, describe(t));
        for f in &t.files {
            let _ = writeln!(
                out,
                "    {:<8} {} (+{} -{})",
                change_name(f.change),
                f.path,
                f.lines_added,
                f.lines_removed
            );
        }
        if let Some(s) = &t.semver {
            let _ = writeln!(out, "  {:<12}{}", "semver", describe_semver(s));
            for c in &s.changes {
                let _ = writeln!(
                    out,
                    "    {:<6} {} · {}: {}",
                    level_name(c.level),
                    c.rule,
                    c.subject,
                    c.detail
                );
            }
            if let Some(migration) = &s.migration {
                let _ = writeln!(
                    out,
                    "  migration   custom files are preserved; dependency requirements {}",
                    if migration.runtime_requirements_known {
                        "recorded"
                    } else {
                        "unknown for previous generation"
                    }
                );
                for action in &migration.actions {
                    let _ = writeln!(
                        out,
                        "    {} · {}: {}",
                        action.rule, action.subject, action.action
                    );
                }
            }
        }
    }
    if let Some(level) = r.level {
        let _ = writeln!(out, "  {:<12}{}", "result", level_name(level));
    }
    for t in &r.changes {
        for f in &t.files {
            match &f.patch {
                Some(patch) if !patch.is_empty() => {
                    let _ = writeln!(out, "\n{}: {}", t.target, f.path);
                    out.push_str(patch);
                    if f.truncated_lines > 0 {
                        let lines = if f.truncated_lines == 1 {
                            "line"
                        } else {
                            "lines"
                        };
                        let _ = writeln!(out, "… {} more diff {lines}", f.truncated_lines);
                    }
                }
                Some(_) => {}
                None => {
                    let _ = writeln!(out, "\n{}: {} (binary file differs)", t.target, f.path);
                }
            }
        }
    }
    out
}
