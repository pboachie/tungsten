// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten support-bundle --preview`: the exact, metadata-only manifest of
//! what a diagnostic support bundle would contain.
//!
//! The preview is assembled only from existing, already redacted sources:
//! build constants, the capability catalog, config-only effective-configuration
//! provenance and declared project prerequisites. It never compiles the project
//! or builds an IR (a broken project still previews), never starts a program
//! (no tool-version probes, no emitters, no manifest-provided commands), never
//! contacts a network. Only explicit `--output` publishes this manifest, using
//! the separate bounded, no-clobber Linux writer. Entries carry names, states and
//! counts only: no configuration values, digests, project names, environment
//! values, URLs, paths or free-form diagnostic text. Every entry and the whole
//! document are size-bounded.

use std::fmt::Write as _;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;
use tungsten_config::capabilities;
use tungsten_config::inspection::{InspectedOrigin, InspectedTarget, InspectionTargetStatus};
use tungsten_core::Severity;
use tungsten_emit::external::find_executable;

use crate::args::SupportBundleArgs;
use crate::output::{CliError, CommandName, CommandResult, ErrorKind, json_diagnostic};
use crate::{CliEnv, Report, exit, input};

use super::support_bundle_file::{self as export, ExportError, Failure};

/// Version of the preview/bundle manifest schema.
pub(crate) const BUNDLE_SCHEMA_VERSION: u32 = 1;
/// Largest serialized payload of one entry; a larger one is withheld.
pub(super) const MAX_ENTRY_BYTES: usize = 16 * 1024;
/// Largest total of all entry payloads; later entries are withheld past it.
pub(super) const MAX_TOTAL_BYTES: usize = 64 * 1024;
const MAX_DIAGNOSTICS: usize = 50;
const MAX_TARGETS: usize = 64;
const MAX_FIELDS_PER_TARGET: usize = 64;
const MAX_TOOLS: usize = 64;

/// Entry ids in manifest order; `--exclude` accepts exactly these.
pub(super) const ENTRY_IDS: [&str; 6] = [
    "tool.versions",
    "capabilities.catalog",
    "project.configuration",
    "project.diagnostics",
    "project.prerequisites",
    "reproduction.recipe",
];

/// Categories never collected by default, reported without any content.
const OMITTED: [(&str, &str); 13] = [
    ("api_documents", "OpenAPI and other API input documents"),
    ("source_payloads", "source text and generated output"),
    ("credentials", "tokens, keys and passwords"),
    (
        "auth_and_idempotency_stores",
        "stored authentication and idempotency state",
    ),
    ("environment_values", "environment variable values"),
    ("raw_urls", "URLs of any kind"),
    ("absolute_paths", "private absolute and project paths"),
    (
        "diagnostic_text",
        "diagnostic messages, help and source excerpts",
    ),
    (
        "configuration_values",
        "configured option values and project names",
    ),
    (
        "hashes_and_identifiers",
        "content digests and project identifiers; they are not anonymous",
    ),
    (
        "tool_version_probes",
        "versions of installed tools; probing starts programs",
    ),
    ("logs", "command and tool logs"),
    ("external_emitter_data", "external emitter names and output"),
];

/// The preview document (`--json` result).
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SupportBundlePreview {
    /// Manifest schema version; a reader must reject versions it does not know.
    pub bundle_schema_version: u32,
    pub mode: BundlePreviewMode,
    /// Whether the inspection phase wrote files; always false. An export
    /// receipt separately records publication of this unchanged manifest.
    pub wrote_files: bool,
    /// True only if running the command started a program or extension.
    pub executed_programs: bool,
    /// Every datum is an observation of the local installation or of the
    /// project's declared configuration. Nothing here is user supplied.
    pub privacy: BundlePrivacyDefaults,
    pub limits: BundleLimits,
    /// What a bundle would contain, in order, with the exact payloads.
    pub entries: Vec<BundleEntry>,
    /// Sum of the included entries' serialized payload sizes.
    pub total_bytes: usize,
    /// Categories excluded by default; names only, never content.
    pub omitted: Vec<OmittedCategory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundlePreviewMode {
    Preview,
}

/// An explicit export's command result. The file contains exactly `manifest`
/// serialized as compact JSON plus one newline, not this receipt. Manifest
/// mode and side-effect flags describe inspection, before publication.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SupportBundleExportReceipt {
    pub manifest: SupportBundlePreview,
    /// Complete serialized file size, including the final newline (at most
    /// 131072 bytes); distinct from the manifest's sum of entry payload sizes.
    pub artifact_bytes: usize,
    pub published: bool,
    pub temporary_cleanup: BundleTemporaryCleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleTemporaryCleanup {
    Complete,
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BundlePrivacyDefaults {
    /// `metadata_only`: names, states, counts and versions.
    pub level: String,
    pub redaction: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BundleLimits {
    pub max_entry_bytes: usize,
    pub max_total_bytes: usize,
    pub max_diagnostics: usize,
    pub max_targets: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BundleEntry {
    /// Stable id; pass it to `--exclude` to remove the entry.
    pub id: String,
    pub origin: BundleEntryOrigin,
    pub sensitivity: BundleSensitivity,
    pub status: BundleEntryStatus,
    /// Serialized size of `payload`; 0 when the entry has none.
    pub bytes: usize,
    /// The exact content a bundle would hold; null unless `included`.
    pub payload: Option<BundlePayload>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleEntryOrigin {
    /// Observed by tungsten; not supplied by the user.
    Observed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleSensitivity {
    /// Names, states, counts and versions only.
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleEntryStatus {
    Included,
    /// Removed with `--exclude`.
    ExcludedByRequest,
    /// Larger than a size limit; its content is not shown.
    WithheldSizeLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OmittedCategory {
    pub category: String,
    pub description: String,
    /// Always `excluded_by_default`; opting in is not available yet.
    pub reason: String,
}

/// Entry payloads; `id` tells which one an entry holds.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum BundlePayload {
    ToolVersions(ToolVersions),
    Capabilities(CapabilitySummary),
    Configuration(ConfigurationSummary),
    Diagnostics(DiagnosticSummary),
    Prerequisites(PrerequisiteSummary),
    Reproduction(ReproductionRecipe),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ToolVersions {
    pub tungsten_version: String,
    pub cli_output_schema: String,
    pub os: String,
    pub arch: String,
    /// `not_performed`: installed tools are never started to read a version.
    pub tool_version_probes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CapabilitySummary {
    pub capability_version: u32,
    pub execution_policy_version: u32,
    pub targets: Vec<CapabilityTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CapabilityTarget {
    pub id: String,
    pub source_support: capabilities::SourceSupport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleProjectState {
    /// The configuration loaded; see the diagnostics for findings.
    Loaded,
    /// The manifest or a profile is invalid; no provenance is available.
    Invalid,
    /// No manifest was found at the given path.
    ManifestNotFound,
    /// The path names an API document or URL rather than a project manifest.
    NotAProjectManifest,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ConfigurationSummary {
    pub state: BundleProjectState,
    /// The effective-configuration contract version, when it loaded.
    pub configuration_version: Option<u32>,
    /// Captured profiles; their sources and digests are not listed.
    pub profiles: usize,
    pub targets: Vec<TargetProvenance>,
    pub targets_omitted: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TargetProvenance {
    /// A built-in target id, or `external[N]`; external names are not listed.
    pub target: String,
    pub status: InspectionTargetStatus,
    /// Supported top-level options with where their winning declaration is
    /// from. Values are never listed.
    pub fields: Vec<FieldProvenance>,
    /// Map entries and fields past the limit, not listed.
    pub fields_omitted: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FieldProvenance {
    pub path: String,
    pub state: tungsten_config::inspection::InspectionValueState,
    pub applicable: bool,
    pub winner: Option<BundleOrigin>,
    /// Earlier declarations that were overridden.
    pub shadowed: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BundleOrigin {
    pub kind: tungsten_config::inspection::InspectionSourceKind,
    /// `project` or `profile[N]`; physical paths are never exported.
    pub source: String,
    pub pointer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DiagnosticSummary {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    /// Configuration diagnostics only, by code and location; no messages.
    pub listed: Vec<DiagnosticRef>,
    pub omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DiagnosticRef {
    pub code: String,
    pub severity: Severity,
    /// `project` or `profile[N]`.
    pub source: Option<String>,
    /// Pointer into the configuration schema; unknown keys are `[redacted]`.
    pub pointer: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrerequisiteState {
    /// Requirements of the configured built-in targets were checked locally.
    Checked,
    /// The configuration did not load, so requirements are unknown.
    Unavailable,
    /// The configured targets' version bounds cannot all be met.
    ConflictingRequirements,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PrerequisiteSummary {
    pub state: PrerequisiteState,
    pub tools: Vec<PrerequisiteTool>,
    pub tools_omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PrerequisiteTool {
    pub tool: String,
    /// The combined version requirement, such as `>=22.0`.
    pub requirement: String,
    /// Whether an executable with this name is on the search path; the
    /// program is not started and its version is not read.
    pub found: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ReproductionRecipe {
    /// Fixed command templates; `<project>` stands for the project path.
    pub steps: Vec<String>,
    /// Global flags the preview ran with.
    pub offline: bool,
    pub no_external: bool,
}

pub(crate) fn run(
    args: &SupportBundleArgs,
    env: &CliEnv,
    cli: &crate::args::Cli,
    cancellation: &tungsten_mock::RunCancellation,
) -> Report {
    let mut report = Report::new(CommandName::SupportBundle);
    if let Some(file) = &args.inspect {
        return inspect_file(report, file);
    }
    if !args.preview && args.output.is_none() {
        return report.failed(
            exit::USAGE,
            CliError::new(
                ErrorKind::Usage,
                "choose a support bundle preview or explicit export",
            )
            .with_help(
                "use --preview, --output <PATH> to create a new metadata-only file on Linux, or --inspect <FILE> to review an existing bundle",
            ),
        );
    }
    if args.output.is_some() && !export::supported() {
        return export_failed(report, Failure::Unsupported.into());
    }
    for id in &args.exclude {
        if !ENTRY_IDS.contains(&id.as_str()) {
            return report.failed(
                exit::USAGE,
                CliError::new(ErrorKind::Usage, "unknown support bundle entry id")
                    .with_help(format!("valid entry ids: {}", ENTRY_IDS.join(", "))),
            );
        }
    }
    if args.output.is_some() && (cancellation.0)() {
        return export_failed(report, Failure::Cancelled.into());
    }
    let manifest = build(args, env, cli.offline, cli.no_external);
    let Some(destination) = &args.output else {
        report.human = human(&manifest);
        report.result = Some(CommandResult::SupportBundle(Box::new(manifest)));
        return report;
    };
    let cancelled = || (cancellation.0)();
    let bytes = match export::serialize(&manifest, &cancelled) {
        Ok(bytes) => bytes,
        Err(error) => return export_failed(report, error),
    };
    let publication = match export::publish(destination, &bytes, &cancelled) {
        Ok(publication) => publication,
        Err(error) => return export_failed(report, error),
    };
    report.human = format!(
        "Published a metadata-only support bundle ({} bytes). Nothing was uploaded or executed.\n",
        bytes.len()
    );
    report.result = Some(CommandResult::SupportBundleExport(Box::new(
        SupportBundleExportReceipt {
            manifest,
            artifact_bytes: bytes.len(),
            published: true,
            temporary_cleanup: if publication.temporary_cleanup {
                BundleTemporaryCleanup::Complete
            } else {
                BundleTemporaryCleanup::Unconfirmed
            },
        },
    )));
    if !publication.temporary_cleanup {
        // Report::failed would discard the published receipt. A cleanup error
        // must preserve the commit-point outcome, with a nonzero status.
        report.exit = exit::INTERNAL;
        report.error = Some(CliError::new(
            ErrorKind::Io,
            "support bundle was published but temporary-file cleanup is unconfirmed",
        ));
    }
    report
}

fn inspect_file(mut report: Report, file: &Path) -> Report {
    use super::support_bundle_inspect::{self as inspection, InspectError};
    let error = match inspection::inspect(file) {
        Ok(result) => {
            report.human = inspection::human(&result);
            report.result = Some(CommandResult::SupportBundleInspection(Box::new(result)));
            return report;
        }
        Err(error) => error,
    };
    // Messages name schema locations and numbers only, never file content.
    let (status, error) = match error {
        InspectError::NotFound => (
            exit::FAILED,
            CliError::new(
                ErrorKind::NotFound,
                "no regular, nonsymlink support bundle file at the given path",
            ),
        ),
        InspectError::TooLarge => (
            exit::REFUSED,
            CliError::new(
                ErrorKind::Refused,
                "support bundle file exceeds the complete 128 KiB artifact limit",
            ),
        ),
        InspectError::Io => (
            exit::INTERNAL,
            CliError::new(ErrorKind::Io, "reading the support bundle file failed"),
        ),
        InspectError::NotJson => (
            exit::REFUSED,
            CliError::new(ErrorKind::Refused, "support bundle file is not a JSON object"),
        ),
        InspectError::UnsupportedVersion(version) => (
            exit::REFUSED,
            CliError::new(
                ErrorKind::Refused,
                format!("unsupported support bundle schema version {version}"),
            )
            .with_help(format!(
                "this tungsten reads schema version {BUNDLE_SCHEMA_VERSION}; use the tungsten release that wrote the bundle"
            )),
        ),
        InspectError::Invalid(problem) => (
            exit::REFUSED,
            CliError::new(
                ErrorKind::Refused,
                format!("support bundle does not match schema version {BUNDLE_SCHEMA_VERSION}: {problem}"),
            ),
        ),
    };
    report.failed(status, error)
}

fn export_failed(report: Report, error: ExportError) -> Report {
    let (status, kind, message) = match error.kind {
        Failure::Unsupported => (
            exit::REFUSED,
            ErrorKind::Refused,
            "support bundle export is supported only on Linux",
        ),
        Failure::Cancelled => (
            exit::REFUSED,
            ErrorKind::Refused,
            "support bundle export was cancelled before publication",
        ),
        Failure::SizeLimit => (
            exit::REFUSED,
            ErrorKind::Refused,
            "support bundle exceeds the complete 128 KiB artifact limit",
        ),
        Failure::UnsafeDestination => (
            exit::REFUSED,
            ErrorKind::Refused,
            "support bundle destination requires trusted, nonsymlink directories and a private directory owned by the effective user",
        ),
        Failure::AlreadyExists => (
            exit::REFUSED,
            ErrorKind::Refused,
            "support bundle destination already exists; no entry was replaced",
        ),
        Failure::Io => (
            exit::INTERNAL,
            ErrorKind::Io,
            "support bundle export failed before publication",
        ),
        Failure::Serialization => (
            exit::INTERNAL,
            ErrorKind::Internal,
            "support bundle serialization failed before publication",
        ),
    };
    let error = if error.temporary_cleanup {
        CliError::new(kind, message)
    } else {
        CliError::new(kind, message)
            .with_help("temporary-file cleanup is unconfirmed; no destination was published")
    };
    report.failed(status, error)
}

fn build(
    args: &SupportBundleArgs,
    env: &CliEnv,
    offline: bool,
    no_external: bool,
) -> SupportBundlePreview {
    let project = inspect(&args.path, env);
    let search_path = env
        .search_path
        .clone()
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    let payloads = [
        BundlePayload::ToolVersions(ToolVersions {
            tungsten_version: tungsten_build::TUNGSTEN_VERSION.to_string(),
            cli_output_schema: crate::output::SCHEMA_ID.to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            tool_version_probes: "not_performed".into(),
        }),
        BundlePayload::Capabilities(CapabilitySummary {
            capability_version: capabilities::VERSION,
            execution_policy_version: capabilities::execution_policy().policy_version,
            targets: capabilities::TARGETS
                .iter()
                .map(|target| CapabilityTarget {
                    id: target.id.to_string(),
                    source_support: target.source_support,
                })
                .collect(),
        }),
        BundlePayload::Configuration(project.configuration),
        BundlePayload::Diagnostics(project.diagnostics),
        BundlePayload::Prerequisites(prerequisites(&args.path, project.loaded, &search_path)),
        BundlePayload::Reproduction(ReproductionRecipe {
            steps: [
                "tungsten --version",
                "tungsten doctor <project> --json",
                "tungsten config <project> --json",
                "tungsten check <project> --json",
            ]
            .map(String::from)
            .to_vec(),
            offline,
            no_external,
        }),
    ];
    let mut total = 0usize;
    let entries: Vec<BundleEntry> = ENTRY_IDS
        .iter()
        .zip(payloads)
        .map(|(id, payload)| {
            let (status, bytes, payload) = if args.exclude.iter().any(|e| e == id) {
                (BundleEntryStatus::ExcludedByRequest, 0, None)
            } else {
                let bytes = serde_json::to_string(&payload).map_or(usize::MAX, |s| s.len());
                if bytes > MAX_ENTRY_BYTES || total.saturating_add(bytes) > MAX_TOTAL_BYTES {
                    (BundleEntryStatus::WithheldSizeLimit, 0, None)
                } else {
                    total += bytes;
                    (BundleEntryStatus::Included, bytes, Some(payload))
                }
            };
            BundleEntry {
                id: (*id).to_string(),
                origin: BundleEntryOrigin::Observed,
                sensitivity: BundleSensitivity::Metadata,
                status,
                bytes,
                payload,
            }
        })
        .collect();
    SupportBundlePreview {
        bundle_schema_version: BUNDLE_SCHEMA_VERSION,
        mode: BundlePreviewMode::Preview,
        wrote_files: false,
        executed_programs: false,
        privacy: BundlePrivacyDefaults {
            level: "metadata_only".into(),
            redaction: "names, states, counts and versions only; no values, digests, paths, URLs or text from the project".into(),
        },
        limits: BundleLimits {
            max_entry_bytes: MAX_ENTRY_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
            max_diagnostics: MAX_DIAGNOSTICS,
            max_targets: MAX_TARGETS,
        },
        entries,
        total_bytes: total,
        omitted: OMITTED
            .iter()
            .map(|(category, description)| OmittedCategory {
                category: (*category).to_string(),
                description: (*description).to_string(),
                reason: "excluded_by_default".into(),
            })
            .collect(),
    }
}

struct ProjectView {
    configuration: ConfigurationSummary,
    diagnostics: DiagnosticSummary,
    /// The manifest resolved and its configuration loaded.
    loaded: bool,
}

/// Config-only inspection. Never compiles, reads API documents or starts a
/// program; its diagnostics are already redacted by the inspector.
fn inspect(path: &Path, env: &CliEnv) -> ProjectView {
    let empty = |state| ProjectView {
        configuration: ConfigurationSummary {
            state,
            configuration_version: None,
            profiles: 0,
            targets: vec![],
            targets_omitted: 0,
        },
        diagnostics: DiagnosticSummary {
            errors: 0,
            warnings: 0,
            infos: 0,
            listed: vec![],
            omitted: 0,
        },
        loaded: false,
    };
    let manifest = match input::resolve(path) {
        Ok(input::Input::Project(manifest)) => manifest,
        Ok(input::Input::Spec(_)) => return empty(BundleProjectState::NotAProjectManifest),
        Err(_) => return empty(BundleProjectState::ManifestNotFound),
    };
    let inspected = tungsten_build::inspection::inspect_project(
        &manifest,
        &tungsten_build::CompileOptions {
            policy: env.policy.clone(),
            ..Default::default()
        },
    );
    let mut counts = [0usize; 3];
    let mut listed = vec![];
    let mut omitted = 0;
    for diagnostic in &inspected.diagnostics.0 {
        counts[match diagnostic.severity {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Info => 2,
        }] += 1;
        if listed.len() == MAX_DIAGNOSTICS {
            omitted += 1;
            continue;
        }
        let flat = json_diagnostic(diagnostic, &inspected.sources);
        // Only the redacted location labels the inspector produces are kept.
        let source = flat.file.filter(|file| safe_source(file));
        let pointer = source
            .as_ref()
            .and(flat.pointer)
            .filter(|pointer| pointer.len() <= 200);
        let known = source.is_some();
        listed.push(DiagnosticRef {
            code: if safe_code(&diagnostic.code) {
                diagnostic.code.clone()
            } else {
                "TG????".into()
            },
            severity: diagnostic.severity,
            source,
            pointer,
            line: flat.line.filter(|_| known),
            column: flat.column.filter(|_| known),
        });
    }
    let diagnostics = DiagnosticSummary {
        errors: counts[0],
        warnings: counts[1],
        infos: counts[2],
        listed,
        omitted,
    };
    let (state, version, profiles, targets, targets_omitted) = match &inspected.configuration {
        Some(configuration) => (
            BundleProjectState::Loaded,
            Some(configuration.configuration_version),
            configuration.profiles.len(),
            configuration
                .targets
                .iter()
                .take(MAX_TARGETS)
                .map(target_provenance)
                .collect(),
            configuration.targets.len().saturating_sub(MAX_TARGETS),
        ),
        None => (BundleProjectState::Invalid, None, 0, vec![], 0),
    };
    ProjectView {
        configuration: ConfigurationSummary {
            state,
            configuration_version: version,
            profiles,
            targets,
            targets_omitted,
        },
        diagnostics,
        loaded: state == BundleProjectState::Loaded,
    }
}

fn target_provenance(target: &InspectedTarget) -> TargetProvenance {
    // The inspector names built-ins by id and everything else `external[N]`;
    // anything unexpected is not echoed.
    let name =
        if capabilities::target(&target.target).is_some() || is_external_ordinal(&target.target) {
            target.target.clone()
        } else {
            "unknown".into()
        };
    let top_level = format!("/targets/{name}/");
    let mut fields = vec![];
    let mut omitted = 0;
    for field in &target.fields {
        // Map entries have user-chosen keys; only direct options are listed.
        let direct = field
            .path
            .strip_prefix(&top_level)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
        if !direct || fields.len() == MAX_FIELDS_PER_TARGET {
            omitted += 1;
            continue;
        }
        fields.push(FieldProvenance {
            path: field.path.clone(),
            state: field.state,
            applicable: field.applicable,
            winner: field.winner.as_ref().and_then(origin),
            shadowed: field.shadowed.len(),
        });
    }
    TargetProvenance {
        target: name,
        status: target.status,
        fields,
        fields_omitted: omitted,
    }
}

fn origin(origin: &InspectedOrigin) -> Option<BundleOrigin> {
    (safe_source(&origin.source) && origin.pointer.matches('/').count() <= 3).then(|| {
        BundleOrigin {
            kind: origin.kind,
            source: origin.source.clone(),
            pointer: origin.pointer.clone(),
        }
    })
}

fn is_external_ordinal(text: &str) -> bool {
    text.strip_prefix("external[")
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `project` or `profile[N]`.
fn safe_source(text: &str) -> bool {
    text == "project"
        || text
            .strip_prefix("profile[")
            .and_then(|rest| rest.strip_suffix(']'))
            .is_some_and(|n| !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn safe_code(code: &str) -> bool {
    code.len() == 6 && code.starts_with("TG") && code[2..].bytes().all(|b| b.is_ascii_digit())
}

/// Declared requirements of the configured built-in targets, each checked
/// only for an executable of that name on the search path.
fn prerequisites(path: &Path, loaded: bool, search_path: &std::ffi::OsStr) -> PrerequisiteSummary {
    let unavailable = |state| PrerequisiteSummary {
        state,
        tools: vec![],
        tools_omitted: 0,
    };
    let manifest = match input::resolve(path) {
        Ok(input::Input::Project(manifest)) if loaded => manifest,
        _ => return unavailable(PrerequisiteState::Unavailable),
    };
    let parsed = tungsten_build::driver::load_project_config(
        &manifest,
        &tungsten_build::CompileOptions::default(),
    );
    let Some(config) = parsed.config else {
        return unavailable(PrerequisiteState::Unavailable);
    };
    let mut requirements = vec![];
    for (name, options) in &config.targets {
        let external = tungsten_config::external_target(options)
            .ok()
            .flatten()
            .is_some();
        if !external && let Some(target) = capabilities::target(name) {
            for requirement in target.prerequisites {
                requirements.push(requirement.for_options(options));
            }
        }
    }
    let combined = match super::doctor::aggregate_requirements(requirements) {
        Ok(combined) => combined,
        Err(_) => return unavailable(PrerequisiteState::ConflictingRequirements),
    };
    let total = combined.len();
    let tools = combined
        .into_iter()
        .take(MAX_TOOLS)
        .map(|(tool, requirement)| PrerequisiteTool {
            tool: tool.to_string(),
            requirement: requirement
                .bounds
                .description_with_exclusions(&requirement.excluded_versions),
            found: find_executable(tool, search_path).is_some(),
        })
        .collect::<Vec<_>>();
    PrerequisiteSummary {
        state: PrerequisiteState::Checked,
        tools_omitted: total - tools.len(),
        tools,
    }
}

fn human(preview: &SupportBundlePreview) -> String {
    let mut out = format!(
        "tungsten {} · support-bundle preview (schema {})\n",
        tungsten_build::TUNGSTEN_VERSION,
        preview.bundle_schema_version
    );
    out.push_str(
        "metadata only; nothing is written, uploaded or executed. These are the exact entries a bundle would hold.\n",
    );
    let _ = writeln!(
        out,
        "entries: {} bytes of {} allowed",
        preview.total_bytes, preview.limits.max_total_bytes
    );
    for entry in &preview.entries {
        let _ = writeln!(
            out,
            "  {:<22} {:?} ({} bytes)",
            entry.id, entry.status, entry.bytes
        );
        if let Some(payload) = &entry.payload
            && let Ok(text) = serde_json::to_string_pretty(payload)
        {
            for line in text.lines() {
                let _ = writeln!(out, "    {line}");
            }
        }
    }
    out.push_str("excluded by default:\n");
    for item in &preview.omitted {
        let _ = writeln!(out, "  {:<28} {}", item.category, item.description);
    }
    out.push_str("Remove an entry with --exclude <id>. Export to a new file on Linux with --output <PATH>; review a bundle file with --inspect <FILE>.\n");
    out
}
