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

use std::fmt::Write as _;
use std::path::Path;

use tungsten_core::{Diagnostic, Severity};
use tungsten_emit::surface::{self, ApiSurface, Level};
use tungsten_emit::{StaleReason, stale_files};
use tungsten_ir::Ir;

use crate::args::DiffArgs;
use crate::commands::generate::{Emitted, Project, emit_target, select};
use crate::output::{
    CommandName, CommandResult, DiffResult, DiffStatus, FileChange, FileDiff, SemverLevel,
    SemverReport, SnapshotState, SurfaceChange, TargetDiff,
};
use crate::stats::{headline, ir_stats, plural};
use crate::textdiff::{Side, unified};
use crate::{Report, exit, input, targets};

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

pub(crate) fn run(args: &DiffArgs) -> Report {
    let mut compiled = input::compile(&args.input.path);
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
    let project = Project::of(&args.input.path);
    let opts = DiffOptions {
        patches: true,
        semver: args.semver,
    };
    let mut changes = vec![];
    for name in &names {
        let emitted = emit_target(&compiled, ir, &project, name);
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
    let level = args.semver.then(|| overall_level(&changes)).flatten();
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
        semver: args.semver,
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
        out.push(targets::not_implemented(name, &project.manifest));
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
        diff.semver = Some(semver(name, ir, out_dir, out));
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
fn semver(target: &str, ir: &Ir, out_dir: &Path, out: &mut Vec<Diagnostic>) -> SemverReport {
    let shown = out_dir.join(surface::SURFACE_PATH).display().to_string();
    match ApiSurface::read(out_dir) {
        Ok(Some(previous)) => {
            let changes = surface::compare(&previous, &crate::targets::surface(target, ir));
            SemverReport {
                snapshot: SnapshotState::Present,
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

fn unclassified(snapshot: SnapshotState) -> SemverReport {
    SemverReport {
        snapshot,
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
