// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten refresh-import`: re-convert a converted import in place.
//!
//! A converted input (Postman, HAR, saved cURL, HTTP file, Swagger or an
//! endpoint manifest saved by `init` as `imported.openapi.json`) stays tied to
//! the source bytes it was reviewed from. This command converts a new revision
//! of that source with the importer settings given again on the command line
//! and, without writing anything, shows:
//!
//! - a unified diff of the converted document;
//! - each added, removed or changed operation, keyed by method and path
//!   template, with the facts that changed (origin, authentication, policy,
//!   parameters, request, responses, operationId);
//! - which approved imported operations need renewed review. An approval is
//!   carried over silently only when the operation keeps its method, path,
//!   id and every fact; a changed, moved or newly matched approval must be
//!   confirmed with `--accept-review`;
//! - what regenerating each target would change, with the API surface level;
//! - whether the generation lock would be updated.
//!
//! `--apply` writes the conversion, the import report, `source_sha256` and
//! the remote pin, then rewrites `tungsten.lock.json` when it matched the
//! project before the refresh. It refuses unresolved reviews, a mismatched
//! `--expect` digest, compile errors and files that changed during the run.
//! Overlays, agent.yml, profiles and generated outputs are never touched; run
//! `tungsten generate` afterwards.
//!
//! Application is not a transaction across directories. Before replacing
//! anything it backs every file up under `.tungsten/refresh-import/` and
//! writes a journal; an interrupted run leaves that journal behind, every
//! later run refuses until `--rollback` restores the backups or `--resume`
//! completes the staged refresh, and the journal is removed only after the
//! last write succeeded.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tungsten_build::{CompileOptions, Compiled, compile_project};
use tungsten_core::Digest;
use tungsten_ir::{Operation, Resource};

use super::diff::{DiffOptions, describe, describe_semver, target_diff};
use super::generate::{Project, emit_target, prepare, select};
use super::init_input::{ConvertOptions, PreparedImport, convert};
use super::lock::{self, Existing, LOCK_FILE};
use crate::args::RefreshImportArgs;
use crate::output::{CliError, CommandName, CommandResult, ErrorKind, TargetDiff};
use crate::textdiff::{Side, unified};
use crate::{CliEnv, Report, exit, input};

/// Project-relative directory holding the journal and backups of an apply.
const STATE_DIR: &str = ".tungsten/refresh-import";
const JOURNAL: &str = "journal.json";
const REPORT_FILE: &str = "import-report.json";
const MAX_SPEC_BYTES: u64 = 8 * 1024 * 1024;
/// Hunk lines of the converted-document diff kept in the result.
const MAX_SPEC_PATCH_LINES: usize = 400;
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// `tungsten refresh-import`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RefreshImportResult {
    pub state: RefreshState,
    /// Namespace of the refreshed input.
    pub namespace: String,
    /// The converted document, as named in the manifest.
    pub spec: String,
    /// Source converted, with any URL shown without credentials; null for a
    /// rollback.
    pub source: Option<String>,
    /// `source_sha256` before and after; null for local sources.
    pub source_sha256_before: Option<String>,
    pub source_sha256_after: Option<String>,
    /// Identity of this exact refresh (manifest, current conversion, new
    /// source and conversion). Pass it to `--apply --expect`.
    pub digest: Option<String>,
    /// Operations whose facts or approval changed, sorted by key.
    pub operations: Vec<OperationChange>,
    /// Ids of approved operations that need renewed review before --apply.
    pub review_required: Vec<String>,
    /// Unified diff of the converted document, cut after 400 hunk lines.
    pub spec_patch: Option<String>,
    pub spec_patch_truncated_lines: usize,
    /// What regenerating each target would change after the refresh.
    pub targets: Vec<TargetDiff>,
    pub lock: RefreshLock,
    /// Project-relative files written (or restored by --rollback).
    pub written: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefreshState {
    /// Nothing was written; the result describes the refresh.
    Preview,
    /// The new conversion and source are identical to the current ones.
    Unchanged,
    /// --apply wrote the refresh.
    Applied,
    /// --apply refused; nothing was written.
    Refused,
    /// --rollback restored an interrupted apply.
    RolledBack,
    /// --resume finished an interrupted apply from its staged content.
    Resumed,
}

/// How the refresh affects `tungsten.lock.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefreshLock {
    /// The project has no lock.
    Absent,
    /// The lock matched the project and is rewritten by --apply.
    WouldUpdate,
    /// The lock was rewritten with the refreshed identities.
    Updated,
    /// The lock was stale or corrupt before the refresh and is left alone;
    /// review it with `tungsten lock --preview`.
    LeftStale,
}

/// One operation of the converted input that changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OperationChange {
    /// `METHOD /path/template`.
    pub key: String,
    /// The compiled operation id (`namespace.name`), new id first.
    pub id: Option<String>,
    pub change: OperationChangeKind,
    /// Changed facts: origin, authentication, policy, parameters, request,
    /// responses, operation_id, moved_from:<key>, approval.
    pub facts: Vec<String>,
    /// Whether an imported-operation approval applied before and after.
    pub approved_before: bool,
    pub approved_after: bool,
    pub review_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationChangeKind {
    Added,
    Removed,
    Changed,
}

/// `given` unless it is empty, then `saved`.
fn pick<'a>(given: &'a [String], saved: &'a [String]) -> &'a [String] {
    if given.is_empty() { saved } else { given }
}

/// Importer settings saved under `settings` in import-report.json.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Recorded {
    input_format: Option<String>,
    route: Vec<String>,
    shell_dialect: Option<String>,
    har_origin: Option<String>,
    har_paths: Option<BTreeMap<String, String>>,
    include_prefix: Vec<String>,
    exclude_prefix: Vec<String>,
    redact_property: Vec<String>,
}

/// Written before the first replacement and removed after the last.
#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u32,
    entries: Vec<JournalEntry>,
    /// Manifest file name in the project, for --resume.
    #[serde(default)]
    manifest: Option<String>,
    /// Remote source to pin from the staged bytes `staged/source`.
    #[serde(default)]
    pin: Option<String>,
    /// Whether the generation lock is recaptured after the writes.
    #[serde(default)]
    relock: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalEntry {
    /// Project-relative path.
    path: String,
    /// Backup file under the state directory; null when the file was absent.
    backup: Option<String>,
    /// New content under the state directory; null removes the file. Unused
    /// for `derived` entries (pin and lock), which later steps produce.
    #[serde(default)]
    staged: Option<String>,
    #[serde(default)]
    derived: bool,
}

fn usage(message: impl Into<String>, help: &str) -> CliError {
    CliError::new(ErrorKind::Usage, message).with_help(help)
}

pub(crate) fn run(args: &RefreshImportArgs, env: &CliEnv, no_external: bool) -> Report {
    let report = Report::new(CommandName::RefreshImport);
    let manifest = match input::resolve(&args.path) {
        Ok(input::Input::Project(manifest)) => manifest,
        _ => {
            return report.failed(
                exit::FAILED,
                usage(
                    "refresh-import requires a local project directory or tungsten manifest",
                    "pass the directory containing tungsten.yml",
                ),
            );
        }
    };
    let base = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let root = if base.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        base.clone()
    };
    let state_dir = root.join(STATE_DIR);
    let journal = state_dir.join(JOURNAL);
    let pending = std::fs::symlink_metadata(&journal).is_ok();
    if args.rollback || args.resume {
        if !pending {
            return report.failed(
                exit::FAILED,
                CliError::new(ErrorKind::NotFound, "no interrupted refresh to recover").with_help(
                    format!("{STATE_DIR}/{JOURNAL} does not exist; the project is not mid-refresh"),
                ),
            );
        }
        return if args.rollback {
            rollback(report, &root, &state_dir)
        } else {
            resume(report, &root, &state_dir)
        };
    }
    if pending {
        return report.failed(
            exit::REFUSED,
            CliError::new(
                ErrorKind::Refused,
                format!("an earlier refresh-import --apply was interrupted ({STATE_DIR}/{JOURNAL} exists)"),
            )
            .with_help("run `tungsten refresh-import --resume` to finish the reviewed refresh, or --rollback to restore the backed-up files"),
        );
    }
    match Plan::build(args, &manifest, &root) {
        Ok(plan) => plan.finish(report, args, env, no_external, &manifest, &root),
        Err(error) => report.failed(exit::FAILED, error),
    }
}

/// TG0124 when `path` names a project with an interrupted --apply.
pub(crate) fn pending_diagnostic(path: &Path) -> Option<tungsten_core::Diagnostic> {
    let Ok(input::Input::Project(manifest)) = input::resolve(path) else {
        return None;
    };
    let base = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let journal = format!("{STATE_DIR}/{JOURNAL}");
    std::fs::symlink_metadata(input::join(&base, &journal))
        .is_ok()
        .then(|| {
            tungsten_core::Diagnostic::error(
                "TG0124",
                "an interrupted `refresh-import --apply` left the converted input partly refreshed",
            )
            .at(journal, "", None)
            .with_help("run `tungsten refresh-import --resume` to finish the reviewed refresh, or --rollback to restore the backed-up files")
        })
}

/// Everything read and converted before any comparison.
struct Plan {
    namespace: String,
    spec: String,
    spec_path: PathBuf,
    report_path: PathBuf,
    manifest_text: String,
    old_text: String,
    old_report: Option<Vec<u8>>,
    new_text: String,
    new_report: Option<String>,
    /// Shown form of the source.
    source: String,
    /// The URL and fetched bytes of a remote source.
    remote: Option<(String, Vec<u8>)>,
    sha_before: Option<String>,
    sha_after: Option<String>,
}

impl Plan {
    fn build(args: &RefreshImportArgs, manifest: &Path, root: &Path) -> Result<Plan, CliError> {
        let manifest_text = read_text(manifest).ok_or_else(|| {
            usage(
                "cannot read the project manifest",
                "pass a readable regular tungsten.yml",
            )
        })?;
        let (config, _) = tungsten_config::parse_str("tungsten.yml", &manifest_text);
        let config = config.ok_or_else(|| {
            usage(
                "the project manifest is invalid",
                "run `tungsten check` and correct tungsten.yml first",
            )
        })?;
        let converted: Vec<_> = config
            .inputs
            .iter()
            .filter(|i| args.namespace.as_ref().is_none_or(|ns| *ns == i.namespace))
            .filter(|i| {
                read_text(&root.join(&i.spec))
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .is_some_and(|doc| is_converted(&doc))
            })
            .collect();
        let input = match converted.as_slice() {
            [one] => *one,
            [] => {
                return Err(usage(
                    "no converted import found in this project",
                    "refresh-import applies to inputs saved by `tungsten init` from Postman, HAR, cURL, HTTP, Swagger or endpoint manifests; refresh OpenAPI inputs by editing them, or remote ones with --refresh-inputs",
                ));
            }
            _ => {
                return Err(usage(
                    "several inputs are converted imports",
                    "choose one with --namespace",
                ));
            }
        };
        let spec_path = root.join(&input.spec);
        let old_text = read_text(&spec_path)
            .ok_or_else(|| usage("cannot read the converted input", "restore the file"))?;
        let report_path = spec_path
            .parent()
            .map_or_else(|| PathBuf::from(REPORT_FILE), |dir| dir.join(REPORT_FILE));
        let old_report = read_bytes(&report_path);
        let from = args
            .from
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| input.source_url.clone())
            .ok_or_else(|| {
                usage(
                    "the converted input records no source to refresh from",
                    "pass the new revision of the original Postman, HAR, cURL, HTTP, Swagger or endpoint manifest file with --from",
                )
            })?;
        // Command-line settings win; anything left at its default falls back
        // to the reviewed settings the previous conversion recorded.
        let recorded: Recorded = old_report
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .and_then(|report| serde_json::from_value(report.get("settings")?.clone()).ok())
            .unwrap_or_default();
        let options = ConvertOptions {
            input_format: match (args.input_format.as_str(), &recorded.input_format) {
                ("auto", Some(saved)) => saved,
                (given, _) => given,
            },
            route: pick(&args.route, &recorded.route),
            shell_dialect: match (args.shell_dialect.as_str(), &recorded.shell_dialect) {
                ("posix", Some(saved)) => saved,
                (given, _) => given,
            },
            har_origin: args
                .har_origin
                .as_deref()
                .or(recorded.har_origin.as_deref()),
            har_paths: args.har_paths.as_deref(),
            har_path_map: recorded.har_paths.as_ref(),
            include_prefix: pick(&args.include_prefix, &recorded.include_prefix),
            exclude_prefix: pick(&args.exclude_prefix, &recorded.exclude_prefix),
            redact_property: pick(&args.redact_property, &recorded.redact_property),
        };
        let (prepared, remote, source) = if tungsten_openapi::acquire::is_remote(&from) {
            let bytes = tungsten_openapi::acquire::fetch(
                &from,
                &tungsten_openapi::acquire::current_options(),
            )
            .map_err(|d| {
                let mut error = CliError::new(ErrorKind::Io, d.message.clone());
                error.help = d.help.clone();
                error
            })?;
            let prepared = convert_bytes(&from, &bytes, &options)?;
            let shown = tungsten_openapi::acquire::display_source(&from);
            (prepared, Some((from, bytes)), shown)
        } else {
            let path = PathBuf::from(&from);
            let prepared = convert(&path, &options).map_err(|e| {
                usage(
                    e,
                    "repeat the importer settings used by `init` (--route, --har-paths, --har-origin and prefixes)",
                )
            })?;
            (prepared, None, from)
        };
        let prepared = prepared.ok_or_else(|| {
            usage(
                "the new source is an OpenAPI document, not a convertible import",
                "point the input's `spec` at the OpenAPI document instead of refreshing a conversion",
            )
        })?;
        let new_text = serde_json::to_string_pretty(prepared.document())
            .map_err(|_| CliError::new(ErrorKind::Internal, "cannot encode converted input"))?;
        let new_report = prepared
            .report()
            .map(serde_json::to_string_pretty)
            .transpose()
            .map_err(|_| CliError::new(ErrorKind::Internal, "cannot encode import report"))?;
        let sha_before = input.source_sha256.clone();
        let sha_after = match (&remote, &sha_before) {
            (Some((_, bytes)), _) => Some(tungsten_openapi::acquire::sha256(bytes)),
            (None, Some(_)) => {
                return Err(usage(
                    "this input was converted from a remote source; refresh it from that URL",
                    "omit --from to fetch the recorded source_url, or pass the same URL",
                ));
            }
            (None, None) => None,
        };
        if remote.is_some() && sha_before.is_none() {
            return Err(usage(
                "this input was converted from a local file, not a URL",
                "pass the new revision of the local source with --from",
            ));
        }
        Ok(Plan {
            namespace: input.namespace.clone(),
            spec: input.spec.clone(),
            spec_path,
            report_path,
            manifest_text,
            old_text,
            old_report,
            new_text,
            new_report,
            source,
            remote,
            sha_before,
            sha_after,
        })
    }

    fn digest(&self) -> String {
        let mut bytes = vec![];
        for part in [
            self.manifest_text.as_bytes(),
            self.old_text.as_bytes(),
            self.new_text.as_bytes(),
            self.new_report.as_deref().unwrap_or("").as_bytes(),
            self.sha_after.as_deref().unwrap_or("").as_bytes(),
        ] {
            bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
            bytes.extend_from_slice(part);
        }
        Digest::of(&bytes).0
    }

    fn finish(
        self,
        mut report: Report,
        args: &RefreshImportArgs,
        env: &CliEnv,
        no_external: bool,
        manifest: &Path,
        root: &Path,
    ) -> Report {
        let mut result = RefreshImportResult {
            state: RefreshState::Preview,
            namespace: self.namespace.clone(),
            spec: self.spec.clone(),
            source: Some(self.source.clone()),
            source_sha256_before: self.sha_before.clone(),
            source_sha256_after: self.sha_after.clone(),
            digest: Some(self.digest()),
            operations: vec![],
            review_required: vec![],
            spec_patch: None,
            spec_patch_truncated_lines: 0,
            targets: vec![],
            lock: RefreshLock::Absent,
            written: vec![],
        };
        let report_unchanged = self.new_report.as_deref().map(str::as_bytes)
            == self.old_report.as_deref()
            || self.new_report.is_none();
        if self.old_text == self.new_text && self.sha_before == self.sha_after && report_unchanged {
            result.state = RefreshState::Unchanged;
            report.human = human(&result);
            report.result = Some(CommandResult::RefreshImport(Box::new(result)));
            return report;
        }
        let patch = unified(
            Side::File(&self.spec),
            Side::File(&self.spec),
            &self.old_text,
            &self.new_text,
            MAX_SPEC_PATCH_LINES,
        );
        result.spec_patch = (!patch.text.is_empty()).then_some(patch.text);
        result.spec_patch_truncated_lines = patch.truncated;

        let old = lock::compile_offline(manifest);
        let new = compile_with(manifest, &self.spec_path, &self.new_text);
        report.sources = new.workspace.sources.clone();
        report.diagnostics = new.diagnostics.0.clone();
        let old_doc: Value = serde_json::from_str(&self.old_text).unwrap_or(Value::Null);
        let new_doc: Value = serde_json::from_str(&self.new_text).unwrap_or(Value::Null);
        // An old project that does not compile has no known approvals, so
        // every approval after the refresh needs review.
        result.operations = compare_operations(
            &old_doc,
            &new_doc,
            &approvals(&old, &self.namespace),
            &approvals(&new, &self.namespace),
        );
        result.review_required = result
            .operations
            .iter()
            .filter(|o| o.review_required)
            .filter_map(|o| o.id.clone())
            .collect();

        if let Some(ir) = new.ir.as_ref().filter(|_| !new.has_errors()) {
            let project = Project::of(manifest, env).with_no_external(no_external);
            if let Ok(names) = select(&new, &[]) {
                let prepared = prepare(ir, &names);
                for name in &names {
                    let emitted = emit_target(&new, &prepared, &project, name, false);
                    report
                        .diagnostics
                        .extend(emitted.diagnostics.0.iter().cloned());
                    result.targets.push(target_diff(
                        name,
                        &emitted,
                        ir,
                        &project,
                        DiffOptions {
                            patches: true,
                            semver: true,
                        },
                        &mut report.diagnostics,
                    ));
                }
            }
        }

        let lock_path = root.join(LOCK_FILE);
        let lock_current = match lock::read_lock(&lock_path) {
            Existing::Missing => None,
            Existing::Corrupt => Some(false),
            Existing::Valid(locked) => Some(
                lock::capture(manifest, &old)
                    .is_some_and(|recipe| lock::compare(&locked, &recipe).is_empty()),
            ),
        };
        result.lock = match lock_current {
            None => RefreshLock::Absent,
            Some(true) => RefreshLock::WouldUpdate,
            Some(false) => RefreshLock::LeftStale,
        };

        if !args.apply {
            report.human = human(&result);
            report.result = Some(CommandResult::RefreshImport(Box::new(result)));
            return report;
        }
        if let Some(error) = self.refusal(args, &new, &result, manifest) {
            result.state = RefreshState::Refused;
            report.exit = exit::REFUSED;
            report.error = Some(error);
            report.human = human(&result);
            report.result = Some(CommandResult::RefreshImport(Box::new(result)));
            return report;
        }
        match self.apply(manifest, root, lock_current == Some(true)) {
            Ok((written, lock_updated)) => {
                result.state = RefreshState::Applied;
                result.written = written;
                if lock_updated {
                    result.lock = RefreshLock::Updated;
                }
                report.human = human(&result);
                report.result = Some(CommandResult::RefreshImport(Box::new(result)));
                report
            }
            Err(error) => report.failed(exit::INTERNAL, error),
        }
    }

    /// Why --apply must not write, if anything.
    fn refusal(
        &self,
        args: &RefreshImportArgs,
        new: &Compiled,
        result: &RefreshImportResult,
        manifest: &Path,
    ) -> Option<CliError> {
        let refuse = |message: String, help: &str| {
            Some(CliError::new(ErrorKind::Refused, message).with_help(help))
        };
        if let Some(expected) = &args.expect
            && Some(expected) != result.digest.as_ref()
        {
            return refuse(
                "the project, source or conversion changed since the previewed refresh (--expect does not match)".into(),
                "preview again, review the new result and pass its digest",
            );
        }
        if new.has_errors() {
            return refuse(
                "the project does not compile with the refreshed conversion".into(),
                "fix the reported diagnostics (for example overlays that target removed operations) and preview again; nothing was written",
            );
        }
        let accepted: BTreeSet<&str> = args.accept_review.iter().map(String::as_str).collect();
        let required: BTreeSet<&str> = result.review_required.iter().map(String::as_str).collect();
        if let Some(extra) = accepted.difference(&required).next() {
            return refuse(
                format!("--accept-review {extra} names no operation awaiting review"),
                "pass only ids listed under review_required",
            );
        }
        let missing: Vec<&str> = required.difference(&accepted).copied().collect();
        if !missing.is_empty() {
            return refuse(
                format!(
                    "approved imported operations changed and need renewed review: {}",
                    missing.join(", ")
                ),
                "review each operation's new origin, authentication and policy, update or remove its approval, then confirm with --accept-review <id>; or keep the current pinned conversion by not applying",
            );
        }
        // Stale-source detection: the files compared must still be current.
        if read_text(manifest).as_deref() != Some(self.manifest_text.as_str())
            || read_text(&self.spec_path).as_deref() != Some(self.old_text.as_str())
        {
            return refuse(
                "tungsten.yml or the converted input changed during the refresh".into(),
                "preview again; nothing was written",
            );
        }
        None
    }

    /// Back up, journal, replace, pin and relock. Returns the written files
    /// and whether the lock was rewritten.
    fn apply(
        &self,
        manifest: &Path,
        root: &Path,
        relock: bool,
    ) -> Result<(Vec<String>, bool), CliError> {
        let io = |what: &str, e: &dyn std::fmt::Display| {
            CliError::new(ErrorKind::Io, format!("{what}: {e}")).with_help(
                "run `tungsten refresh-import --rollback` to restore the backed-up files",
            )
        };
        let relative = |path: &Path| -> String {
            path.strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        };
        // Planned replacements: project-relative path and new content (None removes).
        let mut files: Vec<(String, Option<Vec<u8>>)> = vec![(
            relative(&self.spec_path),
            Some(self.new_text.clone().into_bytes()),
        )];
        match &self.new_report {
            Some(text) => {
                files.push((relative(&self.report_path), Some(text.clone().into_bytes())))
            }
            None if self.old_report.is_some() => files.push((relative(&self.report_path), None)),
            None => {}
        }
        if let (Some(before), Some(after)) = (&self.sha_before, &self.sha_after)
            && before != after
        {
            let updated = replace_sha(&self.manifest_text, before, after).ok_or_else(|| {
                CliError::new(
                    ErrorKind::Refused,
                    "cannot locate this input's source_sha256 in tungsten.yml exactly once",
                )
                .with_help(format!(
                    "set source_sha256 to {after} by hand after applying, or remove duplicate hashes"
                ))
            })?;
            files.push((relative(manifest), Some(updated.into_bytes())));
        }
        let mut derived = vec![];
        if let Some((url, _)) = &self.remote {
            derived.push(
                tungsten_openapi::acquire::pin_file(url)
                    .map_err(|d| io("cannot pin", &d.message))?,
            );
        }
        if relock {
            derived.push(LOCK_FILE.to_string());
        }

        // Stage everything first: backups, new content and the journal. From
        // here on --rollback restores and --resume completes the refresh.
        let state_dir = root.join(STATE_DIR);
        for dir in ["backup", "staged"] {
            std::fs::create_dir_all(state_dir.join(dir))
                .map_err(|e| io("cannot create the refresh journal directory", &e))?;
        }
        let planned = files
            .iter()
            .map(|(path, content)| (path, Some(content), false))
            .chain(derived.iter().map(|path| (path, None, true)));
        let mut entries = vec![];
        for (index, (path, content, derived)) in planned.enumerate() {
            let backup = match read_bytes(&root.join(path)) {
                Some(bytes) => {
                    let name = format!("backup/{index}");
                    write_atomic(&state_dir.join(&name), &bytes)
                        .map_err(|e| io("cannot back up a file", &e))?;
                    Some(name)
                }
                None => None,
            };
            let staged = match content.and_then(Option::as_ref) {
                Some(bytes) => {
                    let name = format!("staged/{index}");
                    write_atomic(&state_dir.join(&name), bytes)
                        .map_err(|e| io("cannot stage a file", &e))?;
                    Some(name)
                }
                None => None,
            };
            entries.push(JournalEntry {
                path: path.clone(),
                backup,
                staged,
                derived,
            });
        }
        if let Some((_, bytes)) = &self.remote {
            write_atomic(&state_dir.join("staged/source"), bytes)
                .map_err(|e| io("cannot stage the fetched source", &e))?;
        }
        let journal = Journal {
            version: 1,
            entries,
            manifest: Some(relative(manifest)),
            pin: self.remote.as_ref().map(|(url, _)| url.clone()),
            relock,
        };
        let encoded = serde_json::to_vec_pretty(&journal)
            .map_err(|e| io("cannot encode the refresh journal", &e))?;
        write_atomic(&state_dir.join(JOURNAL), &encoded)
            .map_err(|e| io("cannot write the refresh journal", &e))?;
        complete(root, &state_dir, &journal)
    }
}

/// Carry out a staged refresh: replace the files, pin the source, recapture
/// the lock, then drop the journal. Every step rewrites whole contents, so
/// running it again after an interruption (--resume) is safe.
fn complete(
    root: &Path,
    state_dir: &Path,
    journal: &Journal,
) -> Result<(Vec<String>, bool), CliError> {
    let io = |what: &str, e: &dyn std::fmt::Display| {
        CliError::new(ErrorKind::Io, format!("{what}: {e}")).with_help(
            "fix the cause and run `tungsten refresh-import --resume`, or --rollback to restore the backed-up files",
        )
    };
    let mut written = vec![];
    for entry in journal.entries.iter().filter(|e| !e.derived) {
        let target = root.join(&entry.path);
        match &entry.staged {
            Some(staged) => match read_bytes(&state_dir.join(staged)) {
                Some(bytes) => write_atomic(&target, &bytes),
                None => Err(std::io::Error::other("staged content missing")),
            },
            None => match std::fs::remove_file(&target) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        }
        .map_err(|e| io(&format!("cannot replace {}", entry.path), &e))?;
        written.push(entry.path.clone());
    }
    if let Some(url) = &journal.pin {
        let bytes = read_bytes(&state_dir.join("staged/source"))
            .ok_or_else(|| io("cannot pin the refreshed source", &"staged source missing"))?;
        tungsten_openapi::acquire::pin(url, root, &bytes)
            .map_err(|d| io("cannot pin the refreshed source", &d.message))?;
        written.extend(tungsten_openapi::acquire::pin_file(url).ok());
    }
    if journal.relock {
        let manifest = root.join(journal.manifest.as_deref().unwrap_or("tungsten.yml"));
        let compiled = lock::compile_offline(&manifest);
        let recipe = lock::capture(&manifest, &compiled).ok_or_else(|| {
            CliError::new(
                ErrorKind::Internal,
                "the refreshed project no longer compiles offline; the lock was not updated",
            )
            .with_help("run `tungsten refresh-import --rollback` to restore the backed-up files")
        })?;
        lock::write_lock(&root.join(LOCK_FILE), &recipe)
            .map_err(|e| io("cannot write the generation lock", &e))?;
        written.push(LOCK_FILE.to_string());
    }
    // Only now is the refresh complete; drop the journal last.
    std::fs::remove_file(state_dir.join(JOURNAL))
        .map_err(|e| io("cannot remove the refresh journal", &e))?;
    let _ = std::fs::remove_dir_all(state_dir);
    // `.tungsten` itself only when nothing else lives there.
    let _ = state_dir.parent().map(std::fs::remove_dir);
    Ok((written, journal.relock))
}

/// Read a journal written by this command, refusing paths that leave the
/// project or the state directory.
fn read_journal(state_dir: &Path) -> Result<Journal, CliError> {
    let fail = |message: &str| {
        CliError::new(ErrorKind::Io, message.to_string()).with_help(format!(
            "restore the project from version control, then delete {STATE_DIR}"
        ))
    };
    let journal: Journal = read_bytes(&state_dir.join(JOURNAL))
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|j: &Journal| j.version == 1)
        .ok_or_else(|| fail("the refresh journal is unreadable"))?;
    let unsafe_path =
        |p: &str| Path::new(p).is_absolute() || p.split(['/', '\\']).any(|part| part == "..");
    let paths = journal
        .entries
        .iter()
        .flat_map(|e| [Some(&e.path), e.backup.as_ref(), e.staged.as_ref()])
        .flatten()
        .chain(journal.manifest.as_ref());
    for path in paths {
        if unsafe_path(path) {
            return Err(fail("the refresh journal names a path outside the project"));
        }
    }
    Ok(journal)
}

/// Finish an interrupted --apply from its staged content.
fn resume(mut report: Report, root: &Path, state_dir: &Path) -> Report {
    let journal = match read_journal(state_dir) {
        Ok(journal) => journal,
        Err(error) => return report.failed(exit::FAILED, error),
    };
    if journal.manifest.is_none() {
        return report.failed(
            exit::REFUSED,
            CliError::new(
                ErrorKind::Refused,
                "the interrupted refresh was journaled without staged content and cannot be resumed",
            )
            .with_help("run `tungsten refresh-import --rollback`, then refresh again"),
        );
    }
    match complete(root, state_dir, &journal) {
        Ok((written, relocked)) => {
            let result = RefreshImportResult {
                state: RefreshState::Resumed,
                namespace: String::new(),
                spec: String::new(),
                source: None,
                source_sha256_before: None,
                source_sha256_after: None,
                digest: None,
                operations: vec![],
                review_required: vec![],
                spec_patch: None,
                spec_patch_truncated_lines: 0,
                targets: vec![],
                lock: if relocked {
                    RefreshLock::Updated
                } else {
                    RefreshLock::Absent
                },
                written,
            };
            report.human = human(&result);
            report.result = Some(CommandResult::RefreshImport(Box::new(result)));
            report
        }
        Err(error) => report.failed(exit::INTERNAL, error),
    }
}

/// Restore every journaled file from its backup (or remove a file that did
/// not exist before), then drop the journal.
fn rollback(mut report: Report, root: &Path, state_dir: &Path) -> Report {
    let journal = match read_journal(state_dir) {
        Ok(journal) => journal,
        Err(error) => return report.failed(exit::FAILED, error),
    };
    let mut restored = vec![];
    for entry in &journal.entries {
        let target = root.join(&entry.path);
        let outcome = match &entry.backup {
            Some(backup) => match read_bytes(&state_dir.join(backup)) {
                Some(bytes) => write_atomic(&target, &bytes),
                None => Err(std::io::Error::other("backup missing")),
            },
            None => match std::fs::remove_file(&target) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        if let Err(error) = outcome {
            return report.failed(
                exit::INTERNAL,
                CliError::new(
                    ErrorKind::Io,
                    format!("cannot restore {}: {error}", entry.path),
                )
                .with_help(
                    "fix the file permissions and run --rollback again; the journal is kept",
                ),
            );
        }
        restored.push(entry.path.clone());
    }
    if let Err(error) = std::fs::remove_dir_all(state_dir) {
        return report.failed(
            exit::INTERNAL,
            CliError::new(ErrorKind::Io, format!("cannot remove {STATE_DIR}: {error}"))
                .with_help("the files were restored; delete the directory by hand"),
        );
    }
    let result = RefreshImportResult {
        state: RefreshState::RolledBack,
        namespace: String::new(),
        spec: String::new(),
        source: None,
        source_sha256_before: None,
        source_sha256_after: None,
        digest: None,
        operations: vec![],
        review_required: vec![],
        spec_patch: None,
        spec_patch_truncated_lines: 0,
        targets: vec![],
        lock: RefreshLock::Absent,
        written: restored,
    };
    report.human = human(&result);
    report.result = Some(CommandResult::RefreshImport(Box::new(result)));
    report
}

/// Convert fetched bytes through a private scratch file, so detection by
/// extension behaves as for a local file.
fn convert_bytes(
    url: &str,
    bytes: &[u8],
    options: &ConvertOptions<'_>,
) -> Result<Option<PreparedImport>, CliError> {
    let extension = url
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext)
        .filter(|ext| ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or("json");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("tungsten-refresh-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| {
        CliError::new(
            ErrorKind::Io,
            format!("cannot create scratch directory: {e}"),
        )
    })?;
    let path = dir.join(format!("source.{extension}"));
    let converted = write_atomic(&path, bytes)
        .map_err(|e| CliError::new(ErrorKind::Io, format!("cannot stage fetched source: {e}")))
        .and_then(|()| {
            convert(&path, options).map_err(|e| {
                usage(
                    e,
                    "repeat the importer settings used by `init` (--route, --har-paths, --har-origin and prefixes)",
                )
            })
        });
    let _ = std::fs::remove_dir_all(&dir);
    converted
}

/// Compile the project offline with `spec` read as `text`.
fn compile_with(manifest: &Path, spec: &Path, text: &str) -> Compiled {
    let mut opts = CompileOptions::default();
    opts.load.acquisition = tungsten_openapi::acquire::current_options();
    opts.load.acquisition.offline = true;
    opts.load.acquisition.refresh = false;
    opts.load.sources = tungsten_openapi::SourceProvider::new(BTreeMap::from([(
        spec.to_path_buf(),
        text.to_string(),
    )]));
    compile_project(manifest, &opts)
}

/// Whether a document holds operations marked by an importer.
fn is_converted(doc: &Value) -> bool {
    operations(doc)
        .values()
        .any(|op| op.get("x-tungsten-imported").is_some())
}

/// `METHOD /path` to the operation object.
fn operations(doc: &Value) -> BTreeMap<String, &Value> {
    let mut out = BTreeMap::new();
    if let Some(paths) = doc.get("paths").and_then(Value::as_object) {
        for (path, item) in paths {
            for method in METHODS {
                if let Some(op) = item.get(method) {
                    out.insert(format!("{} {path}", method.to_uppercase()), op);
                }
            }
        }
    }
    out
}

/// The comparable facts of one operation, with path- and document-level
/// servers, parameters and security resolved as OpenAPI applies them.
fn facts(doc: &Value, key: &str) -> BTreeMap<&'static str, Value> {
    let (method, path) = key.split_once(' ').unwrap_or_default();
    let item = &doc["paths"][path];
    let op = &item[method.to_lowercase()];
    let servers = [&op["servers"], &item["servers"], &doc["servers"]]
        .into_iter()
        .find(|s| s.as_array().is_some_and(|a| !a.is_empty()))
        .cloned()
        .unwrap_or(Value::Null);
    let security = if op.get("security").is_some() {
        op["security"].clone()
    } else {
        doc["security"].clone()
    };
    let schemes: BTreeMap<String, Value> = security
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .flat_map(|req| req.keys())
        .map(|name| {
            (
                name.clone(),
                doc["components"]["securitySchemes"][name].clone(),
            )
        })
        .collect();
    let mut parameters: BTreeMap<String, Value> = BTreeMap::new();
    for p in [&item["parameters"], &op["parameters"]]
        .into_iter()
        .filter_map(Value::as_array)
        .flatten()
    {
        let id = format!(
            "{}:{}",
            p["in"].as_str().unwrap_or(""),
            p["name"].as_str().unwrap_or("")
        );
        parameters.insert(id, p.clone());
    }
    let policy: BTreeMap<&String, &Value> = op
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| k.starts_with("x-tungsten-") && *k != "x-tungsten-imported")
        .collect();
    BTreeMap::from([
        ("origin", servers),
        (
            "authentication",
            serde_json::json!({"security": security, "schemes": schemes}),
        ),
        ("policy", serde_json::json!(policy)),
        ("parameters", serde_json::json!(parameters)),
        ("request", op["requestBody"].clone()),
        ("responses", op["responses"].clone()),
        ("operation_id", op["operationId"].clone()),
    ])
}

/// `METHOD /path` to (operation id, approved) for one namespace's
/// imported operations.
fn approvals(compiled: &Compiled, namespace: &str) -> BTreeMap<String, (String, bool)> {
    fn walk(r: &Resource, out: &mut Vec<Operation>) {
        out.extend(r.operations.iter().cloned());
        for c in &r.children {
            walk(c, out);
        }
    }
    let mut out = BTreeMap::new();
    let Some(ir) = compiled.ir.as_ref().filter(|_| !compiled.has_errors()) else {
        return out;
    };
    for ns in ir.namespaces.iter().filter(|ns| ns.name.wire == namespace) {
        let mut ops = vec![];
        for r in &ns.resources {
            walk(r, &mut ops);
        }
        for op in ops {
            let reviewed = op
                .agent
                .import_policy
                .as_ref()
                .is_some_and(|policy| policy.reviewed);
            out.insert(
                format!("{} {}", op.wire_method().to_uppercase(), op.path.raw),
                (op.id.0.clone(), reviewed),
            );
        }
    }
    out
}

/// Every operation whose facts or approval changed. An approval after the
/// refresh needs renewed review unless the same operation (method, path and
/// id) was approved before and kept every fact.
fn compare_operations(
    old_doc: &Value,
    new_doc: &Value,
    old_approved: &BTreeMap<String, (String, bool)>,
    new_approved: &BTreeMap<String, (String, bool)>,
) -> Vec<OperationChange> {
    let old_ops = operations(old_doc);
    let new_ops = operations(new_doc);
    let operation_id = |op: &Value| op["operationId"].as_str().map(str::to_owned);
    let removed_ids: BTreeMap<String, String> = old_ops
        .iter()
        .filter(|(k, _)| !new_ops.contains_key(*k))
        .filter_map(|(k, op)| Some((operation_id(op)?, k.clone())))
        .collect();
    let keys: BTreeSet<&String> = old_ops.keys().chain(new_ops.keys()).collect();
    let mut out = vec![];
    for key in keys {
        let before = old_approved.get(key);
        let after = new_approved.get(key);
        let approved_before = before.is_some_and(|(_, r)| *r);
        let approved_after = after.is_some_and(|(_, r)| *r);
        let id = after.or(before).map(|(id, _)| id.clone());
        let (change, mut changed) = match (old_ops.get(key), new_ops.get(key)) {
            (Some(_), None) => (OperationChangeKind::Removed, vec![]),
            (None, Some(op)) => {
                let moved = operation_id(op)
                    .and_then(|oid| removed_ids.get(&oid))
                    .map(|from| format!("moved_from:{from}"));
                (OperationChangeKind::Added, moved.into_iter().collect())
            }
            _ => {
                let (a, b) = (facts(old_doc, key), facts(new_doc, key));
                let changed: Vec<String> = a
                    .iter()
                    .filter(|(name, value)| b.get(*name) != Some(*value))
                    .map(|(name, _)| (*name).to_string())
                    .collect();
                (OperationChangeKind::Changed, changed)
            }
        };
        let same_identity = before.map(|(id, _)| id) == after.map(|(id, _)| id);
        let review_required = approved_after
            && (change == OperationChangeKind::Added
                || !changed.is_empty()
                || !approved_before
                || !same_identity);
        if change == OperationChangeKind::Changed && approved_before != approved_after {
            changed.push("approval".into());
        }
        if change == OperationChangeKind::Changed && changed.is_empty() && !review_required {
            continue;
        }
        out.push(OperationChange {
            key: key.clone(),
            id,
            change,
            facts: changed,
            approved_before,
            approved_after,
            review_required,
        });
    }
    out
}

/// `text` with the one occurrence of `before` on a `source_sha256` line
/// replaced; `None` unless exactly one such line exists.
fn replace_sha(text: &str, before: &str, after: &str) -> Option<String> {
    let matches: Vec<usize> = text
        .match_indices(before)
        .map(|(at, _)| at)
        .filter(|at| {
            let line_start = text[..*at].rfind('\n').map_or(0, |i| i + 1);
            text[line_start..*at]
                .trim_start()
                .starts_with("source_sha256:")
        })
        .collect();
    let [at] = matches.as_slice() else {
        return None;
    };
    Some(format!(
        "{}{after}{}",
        &text[..*at],
        &text[at + before.len()..]
    ))
}

fn read_bytes(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_SPEC_BYTES {
        return None;
    }
    let mut bytes = vec![];
    std::fs::File::open(path)
        .ok()?
        .take(MAX_SPEC_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_SPEC_BYTES).then_some(bytes)
}

fn read_text(path: &Path) -> Option<String> {
    String::from_utf8(read_bytes(path)?).ok()
}

/// Write through a fresh sibling and rename; an existing link at `path` is
/// replaced, never followed.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!(".{name}.{}.refresh.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    {
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

fn human(r: &RefreshImportResult) -> String {
    let mut text = String::new();
    match r.state {
        RefreshState::RolledBack => {
            let _ = writeln!(
                text,
                "rolled back an interrupted refresh; restored {} file(s)",
                r.written.len()
            );
            for path in &r.written {
                let _ = writeln!(text, "  {path}");
            }
            return text;
        }
        RefreshState::Resumed => {
            let _ = writeln!(
                text,
                "resumed an interrupted refresh; wrote {}",
                r.written.join(", ")
            );
            let _ = writeln!(text, "run `tungsten generate` to regenerate the outputs");
            return text;
        }
        RefreshState::Unchanged => {
            let _ = writeln!(
                text,
                "{} ({}): conversion of {} is unchanged; nothing to refresh",
                r.spec,
                r.namespace,
                r.source.as_deref().unwrap_or("")
            );
            return text;
        }
        _ => {}
    }
    let _ = writeln!(
        text,
        "{} ({}) refreshed from {}",
        r.spec,
        r.namespace,
        r.source.as_deref().unwrap_or("")
    );
    if let (Some(a), Some(b)) = (&r.source_sha256_before, &r.source_sha256_after) {
        let _ = writeln!(text, "  source_sha256 {a} -> {b}");
    }
    if let Some(patch) = &r.spec_patch {
        let _ = writeln!(text);
        text.push_str(patch);
        if r.spec_patch_truncated_lines > 0 {
            let _ = writeln!(text, "... {} more diff lines", r.spec_patch_truncated_lines);
        }
    }
    let _ = writeln!(text, "\noperations: {} changed", r.operations.len());
    for op in &r.operations {
        let change = match op.change {
            OperationChangeKind::Added => "added",
            OperationChangeKind::Removed => "removed",
            OperationChangeKind::Changed => "changed",
        };
        let mut line = format!("  {change:<8} {}", op.key);
        if let Some(id) = &op.id {
            let _ = write!(line, " ({id})");
        }
        if !op.facts.is_empty() {
            let _ = write!(line, ": {}", op.facts.join(", "));
        }
        if op.review_required {
            line.push_str(" [approval needs renewed review]");
        } else if op.approved_before && !op.approved_after {
            line.push_str(" [approval no longer applies]");
        }
        let _ = writeln!(text, "{line}");
    }
    if !r.targets.is_empty() {
        let _ = writeln!(text, "\ngenerated outputs after refresh:");
        for t in &r.targets {
            let _ = writeln!(text, "  {}", describe(t));
            if let Some(semver) = &t.semver {
                let _ = writeln!(text, "    {}", describe_semver(semver));
            }
            for f in &t.files {
                let _ = writeln!(
                    text,
                    "    {} {} (+{} -{})",
                    super::diff::change_name(f.change),
                    f.path,
                    f.lines_added,
                    f.lines_removed
                );
            }
        }
    }
    let lock = match r.lock {
        RefreshLock::Absent => "no tungsten.lock.json",
        RefreshLock::WouldUpdate => "tungsten.lock.json is current and is updated by --apply",
        RefreshLock::Updated => "tungsten.lock.json updated",
        RefreshLock::LeftStale => {
            "tungsten.lock.json was already stale or corrupt and is left unchanged; review it with `tungsten lock --preview`"
        }
    };
    let _ = writeln!(text, "\nlock: {lock}");
    match r.state {
        RefreshState::Preview => {
            let _ = writeln!(text, "digest: {}", r.digest.as_deref().unwrap_or(""));
            if r.review_required.is_empty() {
                let _ = writeln!(
                    text,
                    "preview only; nothing written. Apply with --apply --expect {}; not applying keeps the current pinned conversion",
                    r.digest.as_deref().unwrap_or("")
                );
            } else {
                let accepts: String = r
                    .review_required
                    .iter()
                    .map(|id| format!(" --accept-review {id}"))
                    .collect();
                let _ = writeln!(
                    text,
                    "preview only; nothing written. Review the marked approvals, then apply with --apply --expect {}{accepts}",
                    r.digest.as_deref().unwrap_or("")
                );
            }
        }
        RefreshState::Applied => {
            let _ = writeln!(text, "applied; wrote {}", r.written.join(", "));
            let _ = writeln!(text, "run `tungsten generate` to regenerate the outputs");
        }
        RefreshState::Refused => {
            let _ = writeln!(text, "refused; nothing written");
        }
        _ => {}
    }
    text
}
