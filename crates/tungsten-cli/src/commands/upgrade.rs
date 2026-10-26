// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten upgrade rehearse`: preview a compiler/runtime upgrade.
//!
//! Run with the candidate release's binary. The project is compiled offline
//! from its pinned inputs (never fetched or refreshed) and every built-in
//! target is generated in memory with the running compiler. The report
//! compares the identities recorded in `tungsten.lock.json` with the
//! candidate's (compiler, emitters, consumer toolchains, runtime families,
//! effective options), lists the file changes and the API surface change with
//! migration guidance (the `diff --migration` engine), and names conflicts:
//! generated files edited since their generation, or files tungsten does not
//! own that regeneration would replace or remove.
//!
//! The project is never written. With `--workspace`, the complete generated
//! output, the proposed lock and the rehearsal record (`tungsten-rehearsal.json`)
//! go into a new scratch directory for review; `tungsten upgrade apply` is the
//! separate, explicit step that copies an accepted rehearsal into the project
//! (see `upgrade_apply`). External emitters are never started by a rehearsal.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tungsten_build::TUNGSTEN_VERSION;
use tungsten_core::{Digest, Severity};
use tungsten_emit::surface::SURFACE_PATH;
use tungsten_emit::{FileSet, MANIFEST_PATH, OutputManifest, WriteOptions, is_custom_path};

use crate::args::UpgradeRehearseArgs;
use crate::commands::diff::{self, DiffOptions};
use crate::commands::generate::{Project, emit_target, prepare, select};
use crate::commands::lock::{
    self, CompilerIdentity, DriftKind, Existing, GenerationRecipe, LOCK_FILE, LockDrift,
    LockedTarget,
};
use crate::output::{CliError, CommandName, CommandResult, DiffStatus, ErrorKind, TargetDiff};
use crate::{CliEnv, Report, exit, input};

/// Format version of `tungsten-rehearsal.json`.
pub(crate) const REHEARSAL_VERSION: u32 = 1;
/// The rehearsal record at the workspace root; written last.
pub(crate) const REHEARSAL_FILE: &str = "tungsten-rehearsal.json";
/// Staged target output, one numbered directory per planned target.
pub(crate) const STAGED_DIR: &str = "targets";

/// `tungsten upgrade rehearse` result, also stored as `report` in the
/// rehearsal record.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RehearsalResult {
    /// The workspace directory, as given; null without `--workspace`.
    pub workspace: Option<String>,
    /// Whether the staged output and rehearsal record were written.
    pub staged: bool,
    /// The compiler recorded in the project's lock; null when there is no
    /// valid lock.
    pub from: Option<CompilerIdentity>,
    /// The running (candidate) compiler.
    pub to: CompilerIdentity,
    pub verdict: RehearsalVerdict,
    /// Recorded identities that the candidate changes.
    pub identity_changes: Vec<IdentityChange>,
    /// Changes to inputs, profiles or the manifest since the lock. A compiler
    /// upgrade never carries them along; each one blocks the rehearsal.
    pub input_drift: Vec<LockDrift>,
    pub targets: Vec<RehearsedTarget>,
    /// Blocking problems and unsupported combinations, in discovery order.
    pub findings: Vec<RehearsalFinding>,
    pub checks: RehearsalChecks,
    /// Commands that reproduce or continue this rehearsal.
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalVerdict {
    /// Nothing would change.
    NoChanges,
    /// The candidate's output can be applied with `upgrade apply`.
    Ready,
    /// A blocking finding must be resolved first.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct IdentityChange {
    pub kind: IdentityKind,
    /// `compiler`, or the target name.
    pub subject: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    Compiler,
    /// The tungsten version that wrote the target's current output.
    GeneratedBy,
    Emitter,
    /// Tools required to build the generated consumer (dependency floors).
    ConsumerToolchain,
    RuntimeFamily,
    /// The digest of the effective target options (option defaults or
    /// profile resolution changed).
    Options,
    Out,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RehearsedTarget {
    pub target: String,
    pub status: RehearsedStatus,
    /// File and API surface changes with migration guidance, as reported by
    /// `diff --migration`.
    pub diff: TargetDiff,
    /// Files whose replacement or removal would lose content tungsten did
    /// not write.
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RehearsedStatus {
    Unchanged,
    /// Changes can be applied.
    Changed,
    Conflict,
    Failed,
    /// Not rehearsed: no built-in emitter in this version, or an external
    /// emitter (never started by a rehearsal).
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    /// Path relative to the output directory; empty for the directory.
    pub path: String,
    pub reason: ConflictReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConflictReason {
    /// A generated file whose bytes differ from what tungsten recorded.
    EditedAfterGeneration,
    /// A file tungsten did not generate at a path the candidate writes.
    NotOwned,
    /// A non-empty output directory without a tungsten output manifest.
    NotGenerated,
    /// A symbolic link at a path the candidate writes.
    Link,
    /// The file changed after the rehearsal recorded it (`upgrade apply`).
    ChangedSinceRehearsal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RehearsalFinding {
    pub kind: FindingKind,
    pub subject: String,
    pub detail: String,
    /// Whether it prevents `upgrade apply`.
    pub blocking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// `--from`/`--to` do not match the recorded or running compiler.
    IdentityMismatch,
    /// The lock is unreadable or its digest does not match.
    LockCorrupt,
    /// Inputs changed since the lock.
    InputDrift,
    Conflict,
    TargetFailed,
    /// An external target is not rehearsed and keeps its current output.
    ExternalTarget,
    /// A configured target has no emitter in the candidate.
    UnsupportedTarget,
    /// The API surface change is major; review the migration actions.
    BreakingChange,
    /// The workspace could not be written.
    Staging,
}

/// What this rehearsal established, kept apart so that a compatible
/// generated source is never mistaken for an installable, working consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RehearsalChecks {
    /// Classified from the API surface snapshots of the rehearsed targets.
    pub generated_source: SourceCompatibility,
    /// Registry availability of the candidate runtime packages.
    pub package_availability: CheckState,
    /// Building and running the generated consumers against the candidate
    /// runtimes.
    pub runtime_acceptance: CheckState,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceCompatibility {
    Unchanged,
    /// Additive or patch-level changes only.
    Compatible,
    Breaking,
    /// A target had no usable surface snapshot.
    Unclassified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    NotChecked,
}

/// The part of the rehearsal record `upgrade apply` acts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyPlan {
    /// Compiler version that staged the output; apply requires the same.
    pub compiler_version: String,
    /// Manifest file name.
    pub manifest: String,
    /// `recipe_digest` of the lock when rehearsed; null when there was none.
    pub base_lock: Option<String>,
    /// `recipe_digest` of the project as the candidate sees it. Apply
    /// recomputes it, so any change to inputs or configuration is detected.
    pub proposed_recipe: String,
    /// Replace the project's lock with the proposed one (only when the
    /// project had a lock).
    pub write_lock: bool,
    /// False when a blocking finding was reported.
    pub applicable: bool,
    pub targets: Vec<PlannedTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlannedTarget {
    pub target: String,
    /// The output directory, as reported.
    pub out: String,
    /// Staged directory, relative to the workspace.
    pub staged: String,
    /// Every staged file (generated and custom) with its digest, sorted.
    pub files: Vec<PlannedFile>,
    /// Every path apply may write or remove in the output directory, with the
    /// digest it had when rehearsed (null: absent). Apply refuses unless each
    /// still matches, and backs each existing one up first.
    pub baseline: Vec<PlannedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlannedFile {
    pub path: String,
    pub digest: Option<String>,
}

/// `tungsten-rehearsal.json`.
#[derive(Debug, Serialize)]
struct RehearsalRecord<'a> {
    tungsten_rehearsal: u32,
    /// `blake3:` digest of `plan` as canonical JSON.
    plan_digest: String,
    plan: &'a ApplyPlan,
    report: &'a RehearsalResult,
}

/// The digest recorded for a plan.
pub(crate) fn plan_digest(plan: &ApplyPlan) -> String {
    let value = serde_json::to_value(plan).unwrap_or(Value::Null);
    Digest::of(value.to_string().as_bytes()).0
}

/// The digest of a file on disk; `Ok(None)` when absent, `Err` for a link
/// or a file that cannot be read.
pub(crate) fn disk_digest(path: &Path) -> Result<Option<String>, ConflictReason> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(meta) if meta.file_type().is_symlink() => Err(ConflictReason::Link),
        Ok(meta) if meta.is_file() => std::fs::read(path)
            .map(|bytes| Some(Digest::of(&bytes).0))
            .map_err(|_| ConflictReason::NotOwned),
        _ => Err(ConflictReason::NotOwned),
    }
}

/// Resolve a local project manifest or fail with a usage error.
pub(crate) fn local_manifest(path: &Path, command: &str) -> Result<PathBuf, CliError> {
    match input::resolve(path) {
        Ok(input::Input::Project(manifest)) => Ok(manifest),
        _ => Err(CliError::new(
            ErrorKind::Usage,
            format!("{command} requires a local project directory or tungsten manifest"),
        )
        .with_help("pass a directory containing tungsten.yml or a local tungsten*.yml manifest")),
    }
}

pub(crate) fn rehearse(args: &UpgradeRehearseArgs, env: &CliEnv) -> Report {
    let mut report = Report::new(CommandName::UpgradeRehearse);
    let manifest = match local_manifest(&args.path, "upgrade rehearse") {
        Ok(manifest) => manifest,
        Err(error) => return report.failed(exit::FAILED, error),
    };
    let base = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut compiled = lock::compile_offline(&manifest);
    report.sources = std::mem::take(&mut compiled.workspace.sources);
    report.diagnostics = compiled.diagnostics.0.clone();
    let recipe = lock::capture(&manifest, &compiled);
    let (Some(ir), Some(recipe)) = (compiled.ir.as_ref(), recipe) else {
        report.exit = exit::FAILED;
        report.error = Some(
            CliError::new(
                ErrorKind::Usage,
                "the project does not compile offline with this compiler, so the upgrade cannot be rehearsed",
            )
            .with_help("fix the reported diagnostics; remote inputs must already be pinned in .tungsten-inputs; a rehearsal never fetches or refreshes inputs"),
        );
        return report;
    };
    let names = match select(&compiled, &args.target) {
        Ok(names) => names,
        Err(error) => return report.failed(exit::FAILED, error),
    };

    let mut findings = vec![];
    let existing = lock::read_lock(&input::join(&base, LOCK_FILE));
    let locked = match &existing {
        Existing::Valid(locked) => Some(locked.as_ref()),
        Existing::Corrupt => {
            findings.push(blocking(
                FindingKind::LockCorrupt,
                LOCK_FILE,
                "the lock is unreadable, has an unknown version or its digest does not match; restore it from version control before rehearsing",
            ));
            None
        }
        Existing::Missing => None,
    };
    let from = locked.map(|l| l.compiler.clone());
    let to = recipe.compiler.clone();
    if let Some(wanted) = &args.from {
        match &from {
            Some(from) if &from.version == wanted => {}
            Some(from) => findings.push(blocking(
                FindingKind::IdentityMismatch,
                "--from",
                &format!(
                    "{LOCK_FILE} records tungsten {}, not {wanted}",
                    from.version
                ),
            )),
            None => findings.push(blocking(
                FindingKind::IdentityMismatch,
                "--from",
                &format!("--from {wanted} needs a valid {LOCK_FILE}; run `tungsten lock` with the current release first"),
            )),
        }
    }
    if let Some(wanted) = &args.to
        && &to.version != wanted
    {
        findings.push(blocking(
            FindingKind::IdentityMismatch,
            "--to",
            &format!(
                "this binary is tungsten {}; run the rehearsal with the binary of tungsten {wanted}",
                to.version
            ),
        ));
    }

    let mut identity_changes = vec![];
    let mut input_drift = vec![];
    if let Some(locked) = locked {
        identity_changes = identities(locked, &recipe);
        input_drift = lock::compare(locked, &recipe)
            .into_iter()
            .filter(|d| !matches!(d.kind, DriftKind::Compiler | DriftKind::Target))
            .collect();
        for drift in &input_drift {
            findings.push(blocking(
                FindingKind::InputDrift,
                &drift.subject,
                &format!(
                    "{} {} since {LOCK_FILE}; review it and run `tungsten lock --update` with the current release before rehearsing an upgrade",
                    format!("{:?}", drift.kind).to_lowercase(),
                    format!("{:?}", drift.change).to_lowercase(),
                ),
            ));
        }
    }

    // External emitters are never started: a rehearsal label is no sandbox.
    let project = Project::of(&args.path, env).with_no_external(true);
    let prepared = prepare(ir, &names);
    let opts = DiffOptions {
        patches: true,
        semver: true,
    };
    let mut targets = vec![];
    let mut staged = vec![];
    for name in &names {
        let emitted = emit_target(&compiled, &prepared, &project, name, false);
        report
            .diagnostics
            .extend(emitted.diagnostics.0.iter().cloned());
        let mut target_diags = vec![];
        let mut target_diff =
            diff::target_diff(name, &emitted, ir, &project, opts, &mut target_diags);
        diff::add_migration(&mut target_diff);
        report.diagnostics.extend(target_diags);
        let out_dir = emitted.cfg.out_dir.clone();
        if let Some(before) = generated_by(&out_dir)
            && before != TUNGSTEN_VERSION
        {
            identity_changes.push(IdentityChange {
                kind: IdentityKind::GeneratedBy,
                subject: name.clone(),
                before: Some(before),
                after: Some(TUNGSTEN_VERSION.to_string()),
            });
        }
        let mut entry = RehearsedTarget {
            target: name.clone(),
            status: RehearsedStatus::Skipped,
            diff: target_diff,
            conflicts: vec![],
        };
        let files = match emitted.files {
            None if emitted.external_skipped => {
                findings.push(RehearsalFinding {
                    kind: FindingKind::ExternalTarget,
                    subject: name.clone(),
                    detail: "external emitters are not started by a rehearsal; this target keeps its current output, and `tungsten diff` (with your execution policy) previews it separately".into(),
                    blocking: false,
                });
                targets.push(entry);
                continue;
            }
            None => {
                findings.push(blocking(
                    FindingKind::UnsupportedTarget,
                    name,
                    "this compiler has no emitter for the target; remove it from the manifest or choose a release that supports it",
                ));
                targets.push(entry);
                continue;
            }
            Some(_) if emitted.diagnostics.has_errors() => {
                entry.status = RehearsedStatus::Failed;
                findings.push(blocking(
                    FindingKind::TargetFailed,
                    name,
                    "the candidate emitter reported errors for this target",
                ));
                targets.push(entry);
                continue;
            }
            Some(files) => files,
        };
        entry.conflicts = conflicts(&files, &out_dir);
        entry.status = if !entry.conflicts.is_empty() {
            for conflict in &entry.conflicts {
                findings.push(blocking(
                    FindingKind::Conflict,
                    &format!("{name}: {}", shown_path(&out_dir, &conflict.path)),
                    conflict_detail(conflict.reason),
                ));
            }
            RehearsedStatus::Conflict
        } else if entry.diff.status == DiffStatus::Unchanged {
            RehearsedStatus::Unchanged
        } else {
            RehearsedStatus::Changed
        };
        if let Some(semver) = &entry.diff.semver
            && semver.level == Some(crate::output::SemverLevel::Major)
        {
            findings.push(RehearsalFinding {
                kind: FindingKind::BreakingChange,
                subject: name.clone(),
                detail: "the API surface change is major; update callers using the migration actions before applying".into(),
                blocking: false,
            });
        }
        if entry.status != RehearsedStatus::Unchanged {
            staged.push((
                targets.len(),
                files,
                emitted.tools,
                out_dir,
                entry.diff.out.clone(),
            ));
        }
        targets.push(entry);
    }
    if report
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
        && !findings.iter().any(|f| f.blocking)
    {
        findings.push(blocking(
            FindingKind::TargetFailed,
            "diagnostics",
            "the rehearsal reported errors",
        ));
    }

    let checks = checks(&targets);
    let path = args.path.display().to_string();
    let mut commands = vec![format!(
        "tungsten upgrade rehearse {path} --from {} --to {} --workspace <new directory>",
        from.as_ref()
            .map_or("<locked version>", |f| f.version.as_str()),
        to.version
    )];
    let applicable = !findings.iter().any(|f| f.blocking);
    let mut result = RehearsalResult {
        workspace: args.workspace.as_ref().map(|w| w.display().to_string()),
        staged: false,
        from,
        to,
        verdict: if !applicable {
            RehearsalVerdict::Blocked
        } else if staged.is_empty() {
            RehearsalVerdict::NoChanges
        } else {
            RehearsalVerdict::Ready
        },
        identity_changes,
        input_drift,
        targets,
        findings,
        checks,
        commands: vec![],
    };

    if let Some(workspace) = &args.workspace {
        let plan = ApplyPlan {
            compiler_version: TUNGSTEN_VERSION.to_string(),
            manifest: recipe.project.manifest.clone(),
            base_lock: locked.map(|l| l.recipe_digest.clone()),
            proposed_recipe: recipe.recipe_digest.clone(),
            write_lock: locked.is_some(),
            applicable,
            targets: vec![],
        };
        let shown = workspace.display().to_string();
        commands.push(format!("tungsten upgrade apply {path} --workspace {shown}"));
        result.commands = commands;
        match stage(workspace, plan, &recipe, ir, staged, &mut result) {
            Ok(()) => result.staged = true,
            Err(error) => {
                result
                    .findings
                    .push(blocking(FindingKind::Staging, &shown, &error.message));
                result.verdict = RehearsalVerdict::Blocked;
                report.error = Some(error);
            }
        }
    } else {
        commands.push(format!("tungsten diff {path} --migration"));
        result.commands = commands;
    }

    report.exit = if report.error.is_some() {
        exit::INTERNAL
    } else if result.verdict == RehearsalVerdict::Blocked {
        exit::FAILED
    } else {
        exit::OK
    };
    report.human = human(&result);
    report.result = Some(CommandResult::UpgradeRehearse(Box::new(result)));
    report
}

fn blocking(kind: FindingKind, subject: &str, detail: &str) -> RehearsalFinding {
    RehearsalFinding {
        kind,
        subject: subject.to_string(),
        detail: detail.to_string(),
        blocking: true,
    }
}

fn conflict_detail(reason: ConflictReason) -> &'static str {
    match reason {
        ConflictReason::EditedAfterGeneration => {
            "a generated file was edited after generation; move the edit into a `custom/` file or revert it, then rehearse again"
        }
        ConflictReason::NotOwned => {
            "a file tungsten did not generate is in the way; move it, then rehearse again"
        }
        ConflictReason::NotGenerated => {
            "the output directory was not written by tungsten; choose a new `out` directory or run `tungsten generate --force` with the current release first"
        }
        ConflictReason::Link => {
            "a symbolic link is in the way; replace it with the file it points to"
        }
        ConflictReason::ChangedSinceRehearsal => {
            "the file changed after the rehearsal; rehearse again"
        }
    }
}

fn shown_path(out_dir: &Path, rel: &str) -> String {
    if rel.is_empty() {
        out_dir.display().to_string()
    } else {
        out_dir.join(rel).display().to_string()
    }
}

/// The tungsten version recorded in a target's output manifest.
fn generated_by(out_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(out_dir.join(MANIFEST_PATH)).ok()?;
    OutputManifest::parse(&bytes)
        .ok()
        .map(|m| m.tungsten_version)
}

/// Identity changes between the lock and the candidate's recipe.
fn identities(locked: &GenerationRecipe, current: &GenerationRecipe) -> Vec<IdentityChange> {
    let mut changes = vec![];
    if locked.compiler != current.compiler {
        let show = |c: &CompilerIdentity| format!("{} {} (IR {})", c.name, c.version, c.ir_version);
        changes.push(IdentityChange {
            kind: IdentityKind::Compiler,
            subject: "compiler".into(),
            before: Some(show(&locked.compiler)),
            after: Some(show(&current.compiler)),
        });
    }
    let before: BTreeMap<&str, &LockedTarget> = locked
        .targets
        .iter()
        .map(|t| (t.name.as_str(), t))
        .collect();
    for target in &current.targets {
        let old = before.get(target.name.as_str());
        let mut push = |kind, before: Option<String>, after: Option<String>| {
            if before != after {
                changes.push(IdentityChange {
                    kind,
                    subject: target.name.clone(),
                    before,
                    after,
                });
            }
        };
        let toolchain = |t: &LockedTarget| {
            t.consumer_toolchain
                .iter()
                .map(|r| format!("{} {}", r.tool, r.requirement))
                .collect::<Vec<_>>()
                .join(", ")
        };
        push(
            IdentityKind::Emitter,
            old.and_then(|t| t.emitter.identity.clone()),
            target.emitter.identity.clone(),
        );
        push(
            IdentityKind::ConsumerToolchain,
            old.map(|t| toolchain(t)),
            Some(toolchain(target)),
        );
        push(
            IdentityKind::RuntimeFamily,
            old.and_then(|t| t.runtime_family.clone()),
            target.runtime_family.clone(),
        );
        push(
            IdentityKind::Options,
            old.map(|t| t.options_digest.clone()),
            Some(target.options_digest.clone()),
        );
        push(
            IdentityKind::Out,
            old.and_then(|t| t.out.clone()),
            target.out.clone(),
        );
    }
    changes
}

/// Files whose replacement or removal would lose content tungsten did not
/// write: a generated file edited after generation, or a file at a
/// generated path that the previous output manifest does not list.
fn conflicts(files: &FileSet, out_dir: &Path) -> Vec<Conflict> {
    let conflict = |path: &str, reason| Conflict {
        path: path.to_string(),
        reason,
    };
    let owned: BTreeMap<String, String> = match std::fs::read(out_dir.join(MANIFEST_PATH)) {
        Ok(bytes) => match OutputManifest::parse(&bytes) {
            Ok(m) => m.files.into_iter().map(|f| (f.path, f.digest.0)).collect(),
            Err(_) => return vec![conflict("", ConflictReason::NotGenerated)],
        },
        Err(_) => {
            let empty = std::fs::read_dir(out_dir).map_or(true, |mut d| d.next().is_none());
            return if empty {
                vec![]
            } else {
                vec![conflict("", ConflictReason::NotGenerated)]
            };
        }
    };
    let mut found = vec![];
    for (rel, bytes) in files.iter().filter(|(rel, _)| !is_custom_path(rel)) {
        match disk_digest(&out_dir.join(rel)) {
            Ok(None) => {}
            Ok(Some(digest)) if digest == Digest::of(bytes).0 => {}
            Ok(Some(digest)) => match owned.get(rel) {
                Some(recorded) if *recorded == digest => {}
                Some(_) => found.push(conflict(rel, ConflictReason::EditedAfterGeneration)),
                None => found.push(conflict(rel, ConflictReason::NotOwned)),
            },
            Err(reason) => found.push(conflict(rel, reason)),
        }
    }
    for (rel, recorded) in &owned {
        if files.get(rel).is_some() || is_custom_path(rel) {
            continue;
        }
        match disk_digest(&out_dir.join(rel)) {
            Ok(Some(digest)) if digest != *recorded => {
                found.push(conflict(rel, ConflictReason::EditedAfterGeneration));
            }
            Err(reason) => found.push(conflict(rel, reason)),
            _ => {}
        }
    }
    found
}

fn checks(targets: &[RehearsedTarget]) -> RehearsalChecks {
    let rehearsed: Vec<&RehearsedTarget> = targets
        .iter()
        .filter(|t| t.status != RehearsedStatus::Skipped && t.status != RehearsedStatus::Failed)
        .collect();
    let levels: Vec<_> = rehearsed
        .iter()
        .map(|t| t.diff.semver.as_ref().and_then(|s| s.level))
        .collect();
    use crate::output::SemverLevel;
    let generated_source = if levels.iter().any(Option::is_none) {
        SourceCompatibility::Unclassified
    } else if levels.contains(&Some(SemverLevel::Major)) {
        SourceCompatibility::Breaking
    } else if levels.iter().all(|l| *l == Some(SemverLevel::None)) {
        SourceCompatibility::Unchanged
    } else {
        SourceCompatibility::Compatible
    };
    RehearsalChecks {
        generated_source,
        package_availability: CheckState::NotChecked,
        runtime_acceptance: CheckState::NotChecked,
        notes: vec![
            "Generated-source compatibility compares API surface snapshots only; it does not show that the candidate runtime packages are published or that the generated consumers build and run.".into(),
            "Build the staged output under the workspace's targets/ directory with your consumer toolchain (for example a clean install and your test suite) before `upgrade apply`.".into(),
        ],
    }
}

type Staged = (usize, FileSet, BTreeMap<String, String>, PathBuf, String);

/// Write the staged output, the proposed lock and, last, the rehearsal record
/// into a new workspace directory.
fn stage(
    workspace: &Path,
    mut plan: ApplyPlan,
    recipe: &GenerationRecipe,
    ir: &tungsten_ir::Ir,
    staged: Vec<Staged>,
    result: &mut RehearsalResult,
) -> Result<(), CliError> {
    let shown = workspace.display().to_string();
    let io = |what: &str, error: std::io::Error| {
        CliError::new(ErrorKind::Io, format!("cannot {what} in {shown}: {error}"))
            .with_help("the project was not changed; remove the workspace and rehearse again")
    };
    create_workspace(workspace, staged.iter().map(|s| s.3.as_path()))?;
    let shared = Arc::new(ir.clone());
    for (index, files, tools, out_dir, out) in staged {
        let rel = format!("{STAGED_DIR}/{index}");
        // Planning is a dry run; `force` only lets a conflicting directory
        // that tungsten does not own be planned for review (never applied).
        let opts = WriteOptions {
            dry_run: true,
            force: true,
            generator: Some(ir.generator.clone()),
            ir: Some(shared.clone()),
            tools,
        };
        // The paths `upgrade apply` will write or remove, exactly as the
        // writer decides them against the current output directory.
        let mut paths: Vec<String> = match tungsten_emit::write_output(&files, &out_dir, &opts) {
            Ok(w) => w.written.into_iter().chain(w.removed).collect(),
            Err(d) => {
                let message = d.0.first().map_or_else(String::new, |d| d.message.clone());
                return Err(CliError::new(
                    ErrorKind::Io,
                    format!("cannot plan the output of {out}: {message}"),
                ));
            }
        };
        paths.extend([MANIFEST_PATH.to_string(), SURFACE_PATH.to_string()]);
        paths.sort();
        paths.dedup();
        let mut baseline = vec![];
        for path in paths {
            // A link or unreadable entry is already a blocking conflict.
            let digest = disk_digest(&out_dir.join(&path))
                .unwrap_or_else(|_| Some("unreadable".to_string()));
            baseline.push(PlannedFile { path, digest });
        }
        let opts = WriteOptions {
            dry_run: false,
            ..opts
        };
        tungsten_emit::write_output(&files, &workspace.join(&rel), &opts).map_err(|d| {
            let message = d.0.first().map_or_else(String::new, |d| d.message.clone());
            CliError::new(ErrorKind::Io, format!("cannot stage {out}: {message}"))
                .with_help("the project was not changed; remove the workspace and rehearse again")
        })?;
        plan.targets.push(PlannedTarget {
            target: result.targets[index].target.clone(),
            out,
            staged: rel,
            files: files
                .iter()
                .map(|(path, bytes)| PlannedFile {
                    path: path.to_string(),
                    digest: Some(Digest::of(bytes).0),
                })
                .collect(),
            baseline,
        });
    }
    lock::write_lock(&workspace.join(LOCK_FILE), recipe)
        .map_err(|e| io("write the proposed lock", e))?;
    result.staged = true;
    let record = RehearsalRecord {
        tungsten_rehearsal: REHEARSAL_VERSION,
        plan_digest: plan_digest(&plan),
        plan: &plan,
        report: result,
    };
    let mut text = serde_json::to_string_pretty(&record)
        .map_err(std::io::Error::other)
        .map_err(|e| io("serialize the rehearsal", e))?;
    text.push('\n');
    write_atomic(&workspace.join(REHEARSAL_FILE), text.as_bytes())
        .map_err(|e| io("write the rehearsal record", e))
}

/// Create the workspace: a new directory (or an existing empty one) that is
/// not inside any output directory it stages.
fn create_workspace<'a>(
    workspace: &Path,
    out_dirs: impl Iterator<Item = &'a Path>,
) -> Result<(), CliError> {
    let shown = workspace.display().to_string();
    let refuse = |message: String| {
        CliError::new(ErrorKind::Refused, message).with_help(
            "pass --workspace with a new directory outside the project's output directories",
        )
    };
    match std::fs::create_dir(workspace) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = std::fs::symlink_metadata(workspace)
                .map_err(|e| refuse(format!("cannot inspect {shown}: {e}")))?;
            let empty =
                meta.is_dir() && std::fs::read_dir(workspace).is_ok_and(|mut d| d.next().is_none());
            if !empty {
                return Err(refuse(format!(
                    "{shown} already exists and is not an empty directory"
                )));
            }
        }
        Err(error) => {
            return Err(
                CliError::new(ErrorKind::Io, format!("cannot create {shown}: {error}"))
                    .with_help("the parent directory must exist and be writable"),
            );
        }
    }
    let canonical = std::fs::canonicalize(workspace)
        .map_err(|e| refuse(format!("cannot resolve {shown}: {e}")))?;
    for out_dir in out_dirs {
        if let Ok(out) = std::fs::canonicalize(out_dir)
            && canonical.starts_with(&out)
        {
            let _ = std::fs::remove_dir(workspace);
            return Err(refuse(format!(
                "{shown} is inside the output directory {}",
                out_dir.display()
            )));
        }
    }
    Ok(())
}

/// Write a file through a temporary sibling and rename, so a reader never
/// sees a partial record.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temporary = PathBuf::from(path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    temporary.set_file_name(format!(".{name}.{}.tmp", std::process::id()));
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// Human output: identities, per-target changes, findings and next commands.
fn human(r: &RehearsalResult) -> String {
    let mut out = String::new();
    let from = r.from.as_ref().map_or_else(
        || "an unlocked project".to_string(),
        |f| format!("tungsten {}", f.version),
    );
    let _ = writeln!(
        out,
        "upgrade rehearsal: {from} -> tungsten {}",
        r.to.version
    );
    for change in &r.identity_changes {
        let _ = writeln!(
            out,
            "  {} {}: {} -> {}",
            format!("{:?}", change.kind).to_lowercase(),
            change.subject,
            change.before.as_deref().unwrap_or("none"),
            change.after.as_deref().unwrap_or("none"),
        );
    }
    for target in &r.targets {
        let _ = writeln!(
            out,
            "  {}: {}",
            target.target,
            match target.status {
                RehearsedStatus::Unchanged => "unchanged".to_string(),
                RehearsedStatus::Changed => diff::describe(&target.diff),
                RehearsedStatus::Conflict =>
                    format!("{} conflicting files", target.conflicts.len()),
                RehearsedStatus::Failed => "failed".to_string(),
                RehearsedStatus::Skipped => "not rehearsed".to_string(),
            }
        );
        if let Some(semver) = &target.diff.semver {
            let _ = writeln!(out, "    {}", diff::describe_semver(semver));
            if let Some(migration) = &semver.migration {
                for action in &migration.actions {
                    let _ = writeln!(out, "    - {}: {}", action.subject, action.action);
                }
            }
        }
    }
    for finding in &r.findings {
        let _ = writeln!(
            out,
            "  {} {}: {}",
            if finding.blocking {
                "blocked:"
            } else {
                "note:"
            },
            finding.subject,
            finding.detail
        );
    }
    let _ = writeln!(
        out,
        "  generated source: {}; package availability and runtime acceptance: not checked",
        match r.checks.generated_source {
            SourceCompatibility::Unchanged => "unchanged",
            SourceCompatibility::Compatible => "compatible",
            SourceCompatibility::Breaking => "breaking",
            SourceCompatibility::Unclassified => "unclassified",
        }
    );
    let verdict = match r.verdict {
        RehearsalVerdict::NoChanges => "nothing to upgrade",
        RehearsalVerdict::Ready if r.staged => "ready: review the workspace, then apply",
        RehearsalVerdict::Ready => "ready: rehearse with --workspace to stage the output for apply",
        RehearsalVerdict::Blocked => "blocked: resolve the findings above and rehearse again",
    };
    let _ = writeln!(out, "{verdict}; the project was not changed");
    for command in &r.commands {
        let _ = writeln!(out, "  {command}");
    }
    out
}

/// The snake_case name of a conflict reason, as in `--json` output.
pub(crate) fn reason_name(reason: ConflictReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// A short list of conflicts for an apply refusal.
pub(crate) fn describe_conflicts(conflicts: &[(String, Conflict)]) -> String {
    conflicts
        .iter()
        .take(8)
        .map(|(target, c)| {
            format!(
                "{target}: {} ({})",
                if c.path.is_empty() { "." } else { &c.path },
                reason_name(c.reason)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}
