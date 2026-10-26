// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten upgrade apply`: copy an accepted rehearsal into the project.
//!
//! Apply is the only upgrade step that writes the project, and it acts only
//! on a complete rehearsal record whose plan is applicable. Before writing
//! anything it re-establishes everything the rehearsal assumed: the same
//! compiler version, the same recipe digest (so no input, profile or option
//! changed), the same lock, unmodified staged files, and every path it will
//! write or remove still holding the bytes recorded at rehearsal time. Any
//! difference is a conflict and nothing is written.
//!
//! Each such path is backed up into the workspace and an apply journal is
//! written before the first change. Target output is not transactional: an
//! interrupted apply leaves the journal in `started`, every later apply
//! refuses, and `--restore` puts the recorded files back (and removes the
//! files the apply added), making the original project usable again.
//! Custom files are never overwritten, as in `generate`.

use std::io::Read as _;
use std::path::{Component, Path};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tungsten_build::TUNGSTEN_VERSION;
use tungsten_core::Digest;
use tungsten_emit::surface::SURFACE_PATH;
use tungsten_emit::{FileSet, MANIFEST_PATH, OutputManifest, WriteOptions};
use tungsten_ir::GeneratorStamp;

use crate::args::UpgradeApplyArgs;
use crate::commands::lock::{self, Existing, LOCK_FILE};
use crate::commands::upgrade::{
    ApplyPlan, Conflict, ConflictReason, PlannedTarget, REHEARSAL_FILE, REHEARSAL_VERSION,
    STAGED_DIR, describe_conflicts, disk_digest, local_manifest, plan_digest, reason_name,
    write_atomic,
};
use crate::output::{CliError, CommandName, CommandResult, ErrorKind};
use crate::{Report, exit, input, targets};

/// The apply journal at the workspace root.
const JOURNAL_FILE: &str = "tungsten-apply.json";
/// Backups of the files apply replaces, by planned target index.
const BACKUP_DIR: &str = "backup";
const JOURNAL_VERSION: u32 = 1;
/// Largest rehearsal record read back.
const MAX_RECORD_BYTES: u64 = 512 * 1024 * 1024;

/// `tungsten upgrade apply` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct UpgradeApplyResult {
    pub workspace: String,
    pub action: ApplyAction,
    pub targets: Vec<AppliedTarget>,
    /// Whether the project's lock was replaced (applied) or put back
    /// (restored).
    pub lock_updated: bool,
    /// Why apply refused; empty otherwise.
    pub conflicts: Vec<TargetConflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApplyAction {
    Applied,
    Restored,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AppliedTarget {
    pub target: String,
    pub out: String,
    /// Paths written (applied) or put back (restored).
    pub written: Vec<String>,
    pub removed: Vec<String>,
    /// Custom files left as they were.
    pub preserved: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TargetConflict {
    pub target: String,
    pub conflict: Conflict,
}

#[derive(Debug, Deserialize)]
struct StoredRecord {
    tungsten_rehearsal: u32,
    plan_digest: String,
    plan: ApplyPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Started,
    Completed,
    Restored,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    tungsten_apply: u32,
    plan_digest: String,
    state: JournalState,
    /// The project's lock was backed up before it was replaced.
    lock_backed_up: bool,
}

pub(crate) fn run(args: &UpgradeApplyArgs) -> Report {
    let mut report = Report::new(CommandName::UpgradeApply);
    let workspace = args.workspace.display().to_string();
    let manifest = match local_manifest(&args.path, "upgrade apply") {
        Ok(manifest) => manifest,
        Err(error) => return report.failed(exit::FAILED, error),
    };
    let base = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let record = match read_record(&args.workspace) {
        Ok(record) => record,
        Err(error) => return report.failed(exit::REFUSED, error),
    };
    let digest = record.plan_digest.clone();
    let plan = record.plan;
    let journal = read_journal(&args.workspace);
    let mut result = UpgradeApplyResult {
        workspace: workspace.clone(),
        action: ApplyAction::Refused,
        targets: vec![],
        lock_updated: false,
        conflicts: vec![],
    };
    let restore_help = format!(
        "run `tungsten upgrade apply {} --workspace {workspace} --restore` to put the previous output back",
        args.path.display()
    );
    if args.restore {
        return match journal {
            Ok(Some(journal)) if journal.plan_digest == digest => {
                restore(report, &args.workspace, &base, &plan, journal, result)
            }
            Ok(Some(_)) | Err(_) => report.failed(
                exit::REFUSED,
                CliError::new(
                    ErrorKind::Refused,
                    format!("the apply journal in {workspace} does not belong to its rehearsal"),
                ),
            ),
            Ok(None) => report.failed(
                exit::REFUSED,
                CliError::new(
                    ErrorKind::Refused,
                    format!("nothing to restore: {workspace} was never applied"),
                ),
            ),
        };
    }
    match journal {
        Ok(None) => {}
        Ok(Some(journal)) if journal.state == JournalState::Started => {
            return report.failed(
                exit::REFUSED,
                CliError::new(
                    ErrorKind::Refused,
                    "a previous apply of this rehearsal was interrupted; the project output may be partly upgraded",
                )
                .with_help(restore_help),
            );
        }
        Ok(Some(_)) | Err(_) => {
            return report.failed(
                exit::REFUSED,
                CliError::new(
                    ErrorKind::Refused,
                    format!("{workspace} was already applied or restored; rehearse again into a new workspace"),
                ),
            );
        }
    }
    let refuse = |report: Report, message: String| {
        report.failed(
            exit::REFUSED,
            CliError::new(ErrorKind::Refused, message)
                .with_help("nothing was written; rehearse again into a new workspace"),
        )
    };
    if !plan.applicable {
        return refuse(
            report,
            "the rehearsal reported blocking findings; resolve them and rehearse again".into(),
        );
    }
    if plan.compiler_version != TUNGSTEN_VERSION {
        return refuse(
            report,
            format!(
                "the rehearsal was staged by tungsten {}; apply it with that binary, not tungsten {TUNGSTEN_VERSION}",
                plan.compiler_version
            ),
        );
    }
    let manifest_name = manifest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if plan.manifest != manifest_name {
        return refuse(
            report,
            format!(
                "the rehearsal is for {}, not {manifest_name}",
                plan.manifest
            ),
        );
    }
    let mut compiled = lock::compile_offline(&manifest);
    report.sources = std::mem::take(&mut compiled.workspace.sources);
    report.diagnostics = compiled.diagnostics.0.clone();
    let Some(recipe) = lock::capture(&manifest, &compiled) else {
        return refuse(
            report,
            "the project no longer compiles offline with this compiler".into(),
        );
    };
    if recipe.recipe_digest != plan.proposed_recipe {
        return refuse(
            report,
            "the project's inputs or configuration changed since the rehearsal".into(),
        );
    }
    let lock_path = input::join(&base, LOCK_FILE);
    let current_lock = match lock::read_lock(&lock_path) {
        Existing::Valid(locked) => Some(locked.recipe_digest.clone()),
        Existing::Missing => None,
        Existing::Corrupt => Some(String::new()),
    };
    if current_lock != plan.base_lock {
        return refuse(report, format!("{LOCK_FILE} changed since the rehearsal"));
    }

    // Load and verify every staged target, and check the baseline.
    let mut loaded = vec![];
    for target in &plan.targets {
        let out_dir =
            targets::target_config_for(compiled.config.as_ref(), &target.target, &base, false)
                .out_dir;
        if out_dir.display().to_string() != target.out {
            return refuse(
                report,
                format!(
                    "the output directory of {} changed since the rehearsal",
                    target.target
                ),
            );
        }
        let staged = match load_staged(&args.workspace, target) {
            Ok(staged) => staged,
            Err(message) => return refuse(report, message),
        };
        for planned in &target.baseline {
            if !confined(&planned.path) {
                return refuse(
                    report,
                    format!("the rehearsal lists the invalid path {:?}", planned.path),
                );
            }
            let now = disk_digest(&out_dir.join(&planned.path));
            if now.as_ref().ok() != Some(&planned.digest) {
                result.conflicts.push(TargetConflict {
                    target: target.target.clone(),
                    conflict: Conflict {
                        path: planned.path.clone(),
                        reason: match now {
                            Err(reason) => reason,
                            Ok(_) => ConflictReason::ChangedSinceRehearsal,
                        },
                    },
                });
            }
        }
        loaded.push((target, out_dir, staged));
    }
    if !result.conflicts.is_empty() {
        let listed: Vec<_> = result
            .conflicts
            .iter()
            .map(|c| (c.target.clone(), c.conflict.clone()))
            .collect();
        report.exit = exit::REFUSED;
        report.error = Some(
            CliError::new(
                ErrorKind::Refused,
                format!(
                    "the project output changed since the rehearsal: {}",
                    describe_conflicts(&listed)
                ),
            )
            .with_help(
                "nothing was written; review those files, then rehearse again into a new workspace",
            ),
        );
        report.human = human(&result);
        report.result = Some(CommandResult::UpgradeApply(Box::new(result)));
        return report;
    }

    // Back up, journal, then write.
    let io_failure = |report: Report, message: String, help: &str| {
        report.failed(
            exit::INTERNAL,
            CliError::new(ErrorKind::Io, message).with_help(help.to_string()),
        )
    };
    for (index, (target, out_dir, _)) in loaded.iter().enumerate() {
        if let Err(error) = backup(&args.workspace, index, target, out_dir) {
            return io_failure(
                report,
                format!("cannot back up the output of {}: {error}", target.target),
                "nothing was written to the project",
            );
        }
    }
    let lock_backed_up = plan.write_lock && current_lock.is_some();
    if lock_backed_up
        && let Err(error) = copy_file(&lock_path, &args.workspace.join(BACKUP_DIR).join(LOCK_FILE))
    {
        return io_failure(
            report,
            format!("cannot back up {LOCK_FILE}: {error}"),
            "nothing was written to the project",
        );
    }
    let mut journal = Journal {
        tungsten_apply: JOURNAL_VERSION,
        plan_digest: digest,
        state: JournalState::Started,
        lock_backed_up,
    };
    if let Err(error) = write_journal(&args.workspace, &journal) {
        return io_failure(
            report,
            format!("cannot write the apply journal: {error}"),
            "nothing was written to the project",
        );
    }
    for (target, out_dir, staged) in loaded {
        let Staged {
            files,
            generator,
            surface,
        } = staged;
        let opts = WriteOptions {
            dry_run: false,
            force: false,
            generator: Some(generator),
            ir: None,
            tools: Default::default(),
        };
        let written = tungsten_emit::write_output(&files, &out_dir, &opts).and_then(|w| {
            if let Some(surface) = &surface {
                write_atomic(&out_dir.join(SURFACE_PATH), surface).map_err(|e| {
                    tungsten_core::Diagnostics(vec![tungsten_core::Diagnostic::error(
                        "TG0701",
                        format!("cannot write {}: {e}", out_dir.join(SURFACE_PATH).display()),
                    )])
                })?;
            }
            Ok(w)
        });
        match written {
            Ok(w) => result.targets.push(AppliedTarget {
                target: target.target.clone(),
                out: target.out.clone(),
                written: w.written,
                removed: w.removed,
                preserved: w.preserved,
            }),
            Err(d) => {
                report.diagnostics.extend(d.0);
                return io_failure(
                    report,
                    format!(
                        "applying the output of {} failed; the project output may be partly upgraded",
                        target.target
                    ),
                    &restore_help,
                );
            }
        }
    }
    if plan.write_lock {
        if let Err(error) = lock::write_lock(&lock_path, &recipe) {
            return io_failure(
                report,
                format!("cannot write {LOCK_FILE}: {error}"),
                &restore_help,
            );
        }
        result.lock_updated = true;
    }
    journal.state = JournalState::Completed;
    if let Err(error) = write_journal(&args.workspace, &journal) {
        return io_failure(
            report,
            format!(
                "the upgrade was applied but the apply journal could not be completed: {error}"
            ),
            "the project is upgraded; keep the workspace until you have reviewed it",
        );
    }
    result.action = ApplyAction::Applied;
    report.exit = exit::OK;
    report.human = human(&result);
    report.result = Some(CommandResult::UpgradeApply(Box::new(result)));
    report
}

/// Put back every recorded path of every planned target, then the lock.
fn restore(
    mut report: Report,
    workspace: &Path,
    base: &Path,
    plan: &ApplyPlan,
    mut journal: Journal,
    mut result: UpgradeApplyResult,
) -> Report {
    let config = tungsten_config::load(&input::join(base, &plan.manifest)).0;
    for (index, target) in plan.targets.iter().enumerate() {
        let out_dir =
            targets::target_config_for(config.as_ref(), &target.target, base, false).out_dir;
        if out_dir.display().to_string() != target.out {
            return report.failed(
                exit::REFUSED,
                CliError::new(
                    ErrorKind::Refused,
                    format!("the output directory of {} changed since the rehearsal; restore {} by hand from {}", target.target, target.out, workspace.join(BACKUP_DIR).join(index.to_string()).display()),
                ),
            );
        }
        let mut entry = AppliedTarget {
            target: target.target.clone(),
            out: target.out.clone(),
            written: vec![],
            removed: vec![],
            preserved: vec![],
        };
        for planned in &target.baseline {
            if !confined(&planned.path) {
                continue;
            }
            let path = out_dir.join(&planned.path);
            let outcome = match &planned.digest {
                Some(_) => copy_file(
                    &workspace
                        .join(BACKUP_DIR)
                        .join(index.to_string())
                        .join(&planned.path),
                    &path,
                )
                .map(|()| entry.written.push(planned.path.clone())),
                None => match std::fs::remove_file(&path) {
                    Ok(()) => {
                        entry.removed.push(planned.path.clone());
                        Ok(())
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                },
            };
            if let Err(error) = outcome {
                return report.failed(
                    exit::INTERNAL,
                    CliError::new(
                        ErrorKind::Io,
                        format!("cannot restore {}: {error}", path.display()),
                    )
                    .with_help("fix the problem and run --restore again; it is safe to repeat"),
                );
            }
        }
        result.targets.push(entry);
    }
    if journal.lock_backed_up {
        let lock_path = input::join(base, LOCK_FILE);
        if let Err(error) = copy_file(&workspace.join(BACKUP_DIR).join(LOCK_FILE), &lock_path) {
            return report.failed(
                exit::INTERNAL,
                CliError::new(
                    ErrorKind::Io,
                    format!("cannot restore {LOCK_FILE}: {error}"),
                )
                .with_help("fix the problem and run --restore again; it is safe to repeat"),
            );
        }
        result.lock_updated = true;
    }
    journal.state = JournalState::Restored;
    if let Err(error) = write_journal(workspace, &journal) {
        return report.failed(
            exit::INTERNAL,
            CliError::new(
                ErrorKind::Io,
                format!(
                    "the output was restored but the apply journal could not be updated: {error}"
                ),
            ),
        );
    }
    result.action = ApplyAction::Restored;
    report.exit = exit::OK;
    report.human = human(&result);
    report.result = Some(CommandResult::UpgradeApply(Box::new(result)));
    report
}

struct Staged {
    files: FileSet,
    generator: GeneratorStamp,
    surface: Option<Vec<u8>>,
}

/// Read a staged target back and check every file against the plan.
fn load_staged(workspace: &Path, target: &PlannedTarget) -> Result<Staged, String> {
    let index = target
        .staged
        .strip_prefix(&format!("{STAGED_DIR}/"))
        .filter(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()));
    if index.is_none() {
        return Err(format!(
            "the rehearsal lists the invalid staged directory {:?}",
            target.staged
        ));
    }
    let dir = workspace.join(&target.staged);
    let modified = |path: &str| {
        format!(
            "the staged file {} was modified after the rehearsal",
            dir.join(path).display()
        )
    };
    let mut files = FileSet::new();
    for planned in &target.files {
        let path = dir.join(&planned.path);
        let bytes = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {
                std::fs::read(&path).map_err(|_| modified(&planned.path))?
            }
            _ => return Err(modified(&planned.path)),
        };
        if Some(Digest::of(&bytes).0) != planned.digest {
            return Err(modified(&planned.path));
        }
        files
            .add(planned.path.clone(), bytes)
            .map_err(|_| format!("the rehearsal lists the invalid path {:?}", planned.path))?;
    }
    let manifest = std::fs::read(dir.join(MANIFEST_PATH))
        .ok()
        .and_then(|bytes| OutputManifest::parse(&bytes).ok())
        .ok_or_else(|| modified(MANIFEST_PATH))?;
    let expected = OutputManifest::new(&files, None);
    if manifest.files != expected.files || manifest.custom != expected.custom {
        return Err(modified(MANIFEST_PATH));
    }
    let surface = std::fs::read(dir.join(SURFACE_PATH)).ok();
    Ok(Staged {
        files,
        generator: GeneratorStamp {
            tungsten_version: manifest.tungsten_version,
            inputs: manifest.inputs,
        },
        surface,
    })
}

/// Copy every recorded path that exists into the workspace backup.
fn backup(
    workspace: &Path,
    index: usize,
    target: &PlannedTarget,
    out_dir: &Path,
) -> std::io::Result<()> {
    let dir = workspace.join(BACKUP_DIR).join(index.to_string());
    for planned in target.baseline.iter().filter(|p| p.digest.is_some()) {
        copy_file(&out_dir.join(&planned.path), &dir.join(&planned.path))?;
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    let bytes = std::fs::read(from)?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(to, &bytes)
}

/// A plain relative path with `/` separators that stays inside its root.
fn confined(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn read_record(workspace: &Path) -> Result<StoredRecord, CliError> {
    let path = workspace.join(REHEARSAL_FILE);
    let shown = path.display().to_string();
    let refuse = |message: String| {
        CliError::new(ErrorKind::Refused, message).with_help(
            "pass the --workspace of a completed `tungsten upgrade rehearse`; an interrupted rehearsal has no record and cannot be applied",
        )
    };
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|e| refuse(format!("cannot read {shown}: {e}")))?;
    if !meta.is_file() || meta.len() > MAX_RECORD_BYTES {
        return Err(refuse(format!("{shown} is not a rehearsal record")));
    }
    let mut bytes = vec![];
    std::fs::File::open(&path)
        .and_then(|f| f.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| refuse(format!("cannot read {shown}: {e}")))?;
    let record: StoredRecord = serde_json::from_slice(&bytes)
        .map_err(|e| refuse(format!("{shown} is not a rehearsal record: {e}")))?;
    if record.tungsten_rehearsal != REHEARSAL_VERSION {
        return Err(refuse(format!(
            "{shown} has rehearsal format {}, expected {REHEARSAL_VERSION}",
            record.tungsten_rehearsal
        )));
    }
    if plan_digest(&record.plan) != record.plan_digest {
        return Err(refuse(format!(
            "the plan in {shown} was modified after the rehearsal"
        )));
    }
    Ok(record)
}

fn read_journal(workspace: &Path) -> Result<Option<Journal>, ()> {
    match std::fs::read(workspace.join(JOURNAL_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|_| ()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

fn write_journal(workspace: &Path, journal: &Journal) -> std::io::Result<()> {
    let mut text = serde_json::to_string_pretty(journal).map_err(std::io::Error::other)?;
    text.push('\n');
    write_atomic(&workspace.join(JOURNAL_FILE), text.as_bytes())
}

fn human(r: &UpgradeApplyResult) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for target in &r.targets {
        let _ = writeln!(
            out,
            "  {} ({}): {} written, {} removed, {} custom files preserved",
            target.target,
            target.out,
            target.written.len(),
            target.removed.len(),
            target.preserved.len()
        );
    }
    for c in &r.conflicts {
        let _ = writeln!(
            out,
            "  conflict {}: {} ({})",
            c.target,
            if c.conflict.path.is_empty() {
                "."
            } else {
                &c.conflict.path
            },
            reason_name(c.conflict.reason)
        );
    }
    let _ = writeln!(
        out,
        "{}",
        match r.action {
            ApplyAction::Applied => format!(
                "applied the rehearsal{}; backups stay in {} until you remove it",
                if r.lock_updated {
                    " and updated the lock"
                } else {
                    ""
                },
                r.workspace
            ),
            ApplyAction::Restored => "restored the output recorded before the apply".to_string(),
            ApplyAction::Refused => "refused; nothing was written".to_string(),
        }
    );
    out
}
