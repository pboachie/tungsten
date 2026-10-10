// SPDX-License-Identifier: AGPL-3.0-only
//! The `--json` output contract.
//!
//! Every invocation with `--json` prints exactly one [`CliOutput`] on
//! stdout. The JSON Schema returned by [`cli_output_schema`] is generated
//! from these types and published as `cli-output.schema.json`. Output never
//! contains timings, timestamps or other run-dependent values, so two runs
//! over the same inputs print identical bytes.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use tungsten_core::{Diagnostic, Severity, SourceMap};

pub use crate::args::SchemaName;

/// `$id` of the published output schema.
pub const SCHEMA_ID: &str = "https://tungsten.dev/schemas/cli-output-v1.json";

/// One `--json` document.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CliOutput {
    pub tool: Tool,
    /// tungsten version.
    pub version: String,
    /// The command that ran; null when the command line could not be
    /// parsed, or asked for help or the version.
    pub command: Option<CommandName>,
    /// True exactly when `exit_code` is 0.
    pub ok: bool,
    /// The process exit code: 0 ok, 1 input errors, stale output or item
    /// not found, 2 usage, 3 I/O or internal failure, 4 refused without
    /// confirmation.
    pub exit_code: i32,
    /// Diagnostics about the input, in deterministic order.
    pub diagnostics: Vec<JsonDiagnostic>,
    /// An invocation-level failure that is not about the input.
    pub error: Option<CliError>,
    /// The command-specific result; null when the command produced none.
    pub result: Option<CommandResult>,
}

impl CliOutput {
    pub fn new(
        command: Option<CommandName>,
        exit_code: i32,
        diagnostics: Vec<JsonDiagnostic>,
        error: Option<CliError>,
        result: Option<CommandResult>,
    ) -> Self {
        Self {
            tool: Tool::Tungsten,
            version: tungsten_build::TUNGSTEN_VERSION.to_string(),
            command,
            ok: exit_code == crate::exit::OK,
            exit_code,
            diagnostics,
            error,
            result,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub enum Tool {
    #[serde(rename = "tungsten")]
    Tungsten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub enum CommandName {
    #[serde(rename = "check")]
    Check,
    #[serde(rename = "ir dump")]
    IrDump,
    #[serde(rename = "explain")]
    Explain,
    #[serde(rename = "schema")]
    Schema,
    #[serde(rename = "init")]
    Init,
    #[serde(rename = "doctor")]
    Doctor,
    #[serde(rename = "emitters")]
    Emitters,
    #[serde(rename = "generate")]
    Generate,
    #[serde(rename = "mock")]
    Mock,
    #[serde(rename = "report")]
    Report,
    #[serde(rename = "diff")]
    Diff,
    #[serde(rename = "overlay suggest")]
    OverlaySuggest,
}

/// A diagnostic flattened to its first label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct JsonDiagnostic {
    /// Stable `TGxxxx` code; `tungsten explain <code>` describes it.
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub help: Option<String>,
    /// Input file as named in the manifest or on the command line.
    pub file: Option<String>,
    /// RFC 6901 JSON Pointer inside `file` (`""` is the document root).
    pub pointer: Option<String>,
    /// 1-based line, when the location could be mapped to source text.
    pub line: Option<u32>,
    /// 1-based column in characters, when `line` is known.
    pub column: Option<u32>,
}

/// Convert a diagnostic for JSON output, resolving its first label's span.
pub fn json_diagnostic(d: &Diagnostic, sources: &SourceMap) -> JsonDiagnostic {
    let label = d.labels.first();
    let location = label.and_then(|l| crate::render::locate(l, sources));
    JsonDiagnostic {
        code: d.code.clone(),
        severity: d.severity,
        message: d.message.clone(),
        help: d.help.clone(),
        file: label.map(|l| l.file.clone()),
        pointer: label.map(|l| l.pointer.clone()),
        line: location.as_ref().map(|l| l.line),
        column: location.as_ref().map(|l| l.column),
    }
}

/// A failure of the invocation itself, as opposed to a diagnostic about the
/// input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CliError {
    pub kind: ErrorKind,
    pub message: String,
    pub help: Option<String>,
}

impl CliError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            help: None,
        }
    }
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The command line could not be parsed (exit 2).
    Usage,
    /// The command refused to act without confirmation, for example to
    /// overwrite files (exit 4).
    Refused,
    /// The requested item does not exist (exit 1).
    NotFound,
    /// Reading or writing a file failed (exit 3).
    Io,
    /// A bug in tungsten (exit 3).
    Internal,
}

/// Command-specific results. Each variant is a closed object with distinct
/// required keys; `command` tells which one is present.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum CommandResult {
    Check(CheckResult),
    IrDump(IrDumpResult),
    Explain(ExplainResult),
    Schema(SchemaResult),
    Init(InitResult),
    Doctor(DoctorResult),
    Emitters(EmittersResult),
    Generate(GenerateResult),
    Mock(MockResult),
    Report(Box<ReportResult>),
    Diff(DiffResult),
    OverlaySuggest(OverlaySuggestResult),
    Help(HelpResult),
}

/// `--help` or `--version` under `--json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct HelpResult {
    /// The help or version text, as printed without `--json`.
    pub help: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CheckResult {
    /// Entry documents in manifest order, as named in the manifest.
    pub inputs: Vec<String>,
    /// The OpenAPI version each input declares (`3.0.3`, `3.1.0`), in the
    /// order of `inputs`; null for an input that was not loaded. 3.0
    /// documents are normalized to 3.1 semantics.
    pub openapi_versions: Vec<Option<String>>,
    /// Documents loaded, including files reached only through `$ref`.
    pub documents: usize,
    /// Diagnostic counts after `--strict` promotion.
    pub counts: DiagnosticCounts,
    /// Warnings promoted to errors by `--strict`.
    pub promoted: usize,
    pub strict: bool,
    pub ci: bool,
    /// The `$ref` graph of the loaded documents.
    pub refs: RefStats,
    /// Summary of the IR; null when errors prevented building it.
    pub stats: Option<IrStats>,
    /// With `--ci`: every configured target, its generated output compared
    /// with what the emitters produce now. Empty without `--ci`.
    pub outputs: Vec<TargetReport>,
}

/// `tungsten generate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct GenerateResult {
    /// The selected targets in manifest order.
    pub targets: Vec<TargetReport>,
    /// `--dry-run`: nothing was written.
    pub dry_run: bool,
    /// `--check`: compared with the disk, nothing was written.
    pub check: bool,
    /// `--strict`: warnings fail the run; when any target fails, no target
    /// is written (the others are `withheld`).
    pub strict: bool,
    /// Warnings promoted to errors by `--strict`.
    pub promoted: usize,
}

/// One target of `generate`, or of `check --ci`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TargetReport {
    /// Target id (`typescript`, `docs`, ...).
    pub target: String,
    pub status: TargetStatus,
    /// Output directory: the target's `out`, resolved against the
    /// manifest's directory as given on the command line.
    pub out: String,
    /// Files the emitter produced.
    pub files: usize,
    /// Created or replaced (with `--dry-run`: would be). Paths are relative
    /// to `out`, sorted, here and below.
    pub written: Vec<String>,
    /// Already up to date.
    pub unchanged: Vec<String>,
    /// From the previous generation, no longer generated (with
    /// `--dry-run`: would be removed).
    pub removed: Vec<String>,
    /// Custom files that exist and were left untouched.
    pub preserved: Vec<String>,
    /// With `--check` or `check --ci`: files that differ from what would
    /// be written.
    pub stale: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetStatus {
    /// Written to disk.
    Generated,
    /// `--dry-run`: computed, nothing written.
    DryRun,
    /// `--check` / `check --ci`: the output matches.
    Fresh,
    /// `--check` / `check --ci`: the output differs (TG0901).
    Stale,
    /// No emitter for this target in this version (TG0702).
    Skipped,
    /// The emitter reported errors, or writing failed.
    Failed,
    /// Not written: the directory was not generated by tungsten (TG0703).
    Refused,
    /// `--strict`: emitted cleanly but not written, because another target
    /// failed (a strict run writes every target or none).
    Withheld,
}

/// `tungsten mock`, printed once the server is listening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct MockResult {
    /// `http://127.0.0.1:<port>`.
    pub base_url: String,
    /// Runtime gates served as on (`--gate`), in the order given.
    pub enabled_gates: Vec<String>,
    /// Cap on recorded calls (`--max-recorded-calls`).
    pub max_recorded_calls: usize,
    /// Cap on stored idempotent responses (`--max-idempotent-responses`).
    pub max_idempotent_responses: usize,
}

/// The `$ref` graph: schema nodes (every `$ref` target and every component
/// schema), edges (a node references or contains another) and cycles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RefStats {
    pub schemas: usize,
    pub edges: usize,
    /// Edges whose ends are in different documents.
    pub cross_document: usize,
    /// Strongly connected components with more than one schema, or a
    /// self-edge.
    pub cycles: usize,
    /// Schemas that take part in a cycle.
    pub recursive: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DiagnosticCounts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct IrStats {
    /// API machine name.
    pub api: String,
    pub namespaces: usize,
    /// Resources at every depth.
    pub resources: usize,
    pub operations: OperationCounts,
    pub types: usize,
    pub auth_schemes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OperationCounts {
    pub total: usize,
    pub implemented: usize,
    /// Documented but not callable (`planned_from` or planned status).
    pub planned: usize,
    /// Callable only when a runtime gate is on.
    pub gated: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct IrDumpResult {
    /// The IR (see `tungsten schema ir`); null when written to `out`.
    pub ir: Option<Value>,
    /// The file written with `--out`, as given.
    pub out: Option<String>,
    /// Bytes written to `out`.
    pub bytes: Option<usize>,
    pub stats: IrStats,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schemars(deny_unknown_fields)]
pub enum ExplainResult {
    /// A diagnostic code from the registry.
    DiagnosticCode {
        code: String,
        /// The one-line registry description.
        summary: String,
        /// The code range the diagnostic belongs to.
        area: String,
        meaning: String,
        fix: String,
    },
    /// An operation of the compiled project.
    Operation {
        id: String,
        /// Dotted resource path; null for planned operations.
        resource: Option<String>,
        planned: bool,
        /// The IR `Operation` object.
        fragment: Value,
    },
    /// A named type of the compiled project.
    Type {
        id: String,
        /// The IR `NamedType` object.
        fragment: Value,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SchemaResult {
    pub name: SchemaName,
    pub schema: Value,
}

/// `overlay suggest`: the overlay for one input.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OverlaySuggestResult {
    /// The input (namespace) the overlay is for; null when nothing was
    /// suggested.
    pub namespace: Option<String>,
    /// That input's `spec`, as written in tungsten.yml.
    pub spec: Option<String>,
    pub actions: Vec<SuggestedAction>,
    /// Diagnostics (or codes) that got no action; each is also reported as
    /// TG0920 or TG0921.
    pub skipped: usize,
    /// The file written by `--write`; null otherwise.
    pub out: Option<String>,
    /// The overlay document as YAML; null when it was written to `out` or
    /// nothing was suggested.
    pub overlay: Option<String>,
}

/// One action of a suggested overlay.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SuggestedAction {
    /// The diagnostics the action fixes.
    pub codes: Vec<String>,
    /// The Overlay target (JSONPath).
    pub target: String,
    /// JSON Pointer of the target node in the input document.
    pub pointer: String,
    /// The object merged into the target.
    pub update: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InitResult {
    /// Target directory, as given.
    pub dir: String,
    pub api_name: String,
    pub namespace: String,
    /// Spec path written into tungsten.yml, relative to `dir`.
    pub spec: String,
    /// False when no spec was given or found and a placeholder was written.
    pub spec_found: bool,
    pub files: Vec<InitFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InitFile {
    pub path: String,
    pub action: InitAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InitAction {
    Created,
    Overwritten,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DoctorResult {
    pub tools: Vec<ToolReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ToolReport {
    pub name: String,
    pub found: bool,
    /// Version reported by `<tool> --version`, when it could be parsed.
    pub version: Option<String>,
    /// What tungsten uses the tool for.
    pub purpose: String,
}

/// `tungsten emitters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct EmittersResult {
    /// The configured external targets in manifest order, then the
    /// emitters found only on `PATH`, sorted by name.
    pub emitters: Vec<EmitterReport>,
}

/// One external emitter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct EmitterReport {
    /// The target name (`go` for `tungsten-emit-go`).
    pub name: String,
    pub source: EmitterSource,
    /// The command as configured, or `tungsten-emit-<name>` (paths are
    /// machine-specific and not reported).
    pub command: String,
    pub status: EmitterStatus,
    /// The protocol version the emitter reports.
    pub protocol: Option<u32>,
    /// The emitter's own version.
    pub version: Option<String>,
    /// The emitter describes its options with a JSON Schema.
    pub has_options_schema: bool,
    /// Why the emitter could not be described (not `ok`).
    pub message: Option<String>,
}

/// Where an emitter was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmitterSource {
    /// A `tungsten-emit-<name>` executable on `PATH`.
    Path,
    /// An `external` target of `tungsten.yml`.
    Config,
}

/// What `--describe` showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmitterStatus {
    /// It described itself and speaks protocol 1.
    Ok,
    /// The executable does not exist or cannot be started (TG0801).
    NotFound,
    /// It speaks another protocol version (TG0802).
    ProtocolMismatch,
    /// It failed, timed out or printed an invalid description.
    Failed,
}

/// `tungsten diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DiffResult {
    /// The selected targets in manifest order.
    pub changes: Vec<TargetDiff>,
    /// `--semver` was given: each target with an emitter carries `semver`.
    pub semver: bool,
    /// With `--semver`: the highest level over the targets whose last
    /// generation left a snapshot; null when none did (or without
    /// `--semver`).
    pub level: Option<SemverLevel>,
}

/// What regenerating one target would change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TargetDiff {
    pub target: String,
    pub status: DiffStatus,
    /// Output directory, as in `TargetReport.out`.
    pub out: String,
    /// Files regeneration would create (also custom files that are
    /// missing).
    pub added: usize,
    /// Generated files whose content would change.
    pub changed: usize,
    /// Files of the previous generation that would be removed.
    pub removed: usize,
    /// Every added, changed and removed file, sorted by path.
    pub files: Vec<FileDiff>,
    /// The API surface change since the last generation: with `diff
    /// --semver` and in `report`; null otherwise and for targets without an
    /// emitter.
    pub semver: Option<SemverReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    /// Regeneration would write the same bytes.
    Unchanged,
    /// Regeneration would add, change or remove files.
    Changed,
    /// The directory has no `.tungsten/manifest.json`: never generated (or
    /// not by tungsten).
    NotGenerated,
    /// No emitter for this target in this version (TG0702).
    Skipped,
    /// The emitter reported errors.
    Failed,
}

/// One file regeneration would touch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FileDiff {
    /// Relative to the target's `out`.
    pub path: String,
    pub change: FileChange,
    /// Lines only in the regenerated file.
    pub lines_added: usize,
    /// Lines only in the file on disk.
    pub lines_removed: usize,
    /// Unified diff (`--- a/<path>`, `+++ b/<path>`, hunks with three lines
    /// of context), cut after a fixed number of lines. Null in `report`
    /// and for files that are not UTF-8 text.
    pub patch: Option<String>,
    /// Diff lines left out of `patch`.
    pub truncated_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Changed,
    Removed,
}

/// Semantic versioning of the API surface change since a target's last
/// generation (`.tungsten/surface.json` against the current IR).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SemverReport {
    pub snapshot: SnapshotState,
    /// The highest level of `changes` (`none` when empty); null when there
    /// is no usable snapshot.
    pub level: Option<SemverLevel>,
    /// Every classified difference, most severe first.
    pub changes: Vec<SurfaceChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotState {
    /// The last generation left a snapshot; it was compared.
    Present,
    /// No snapshot: never generated, or generated before snapshots were
    /// written (TG0902).
    Missing,
    /// The snapshot could not be read or parsed (TG0902).
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemverLevel {
    /// The surfaces are equal.
    None,
    /// Documentation, wire bindings, gates or macro steps changed; the
    /// signatures are the same.
    Patch,
    /// Additive: callers keep working.
    Minor,
    /// Callers can break.
    Major,
}

/// One classified surface difference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SurfaceChange {
    pub level: SemverLevel,
    /// Stable rule id (`operation_removed`, `required_arg_added`,
    /// `enum_value_removed`, ...).
    pub rule: String,
    /// An operation id, `operation(arg)`, `operation response`, a type or
    /// macro id, followed by a path into the type (`.field`, `[]`, `{}`).
    pub subject: String,
    pub detail: String,
}

/// `tungsten report`: the generation report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ReportResult {
    /// Version, API and IR counts; `stats` is null when errors prevented
    /// building the IR.
    pub summary: ReportSummary,
    pub coverage: Coverage,
    /// One row per callable operation, in IR order.
    pub safety: Vec<SafetyRow>,
    pub budgets: Budgets,
    /// Diagnostics of the compiler and of the configured targets' emitters,
    /// grouped by code (sorted).
    pub diagnostic_groups: Vec<DiagnosticGroup>,
    /// What regenerating each configured target would change since its
    /// last generation (file lists without patches) and the API surface
    /// change.
    pub changes: Vec<TargetDiff>,
    /// The HTML file written with `--html`, as given.
    pub html: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ReportSummary {
    /// API title; null when no IR was built.
    pub title: Option<String>,
    /// The first input document's `info.version`; null when no IR was
    /// built.
    pub api_version: Option<String>,
    /// Documents loaded.
    pub documents: usize,
    pub stats: Option<IrStats>,
    pub counts: DiagnosticCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct Coverage {
    /// Operations by namespace and status, in IR order.
    pub namespaces: Vec<NamespaceCoverage>,
    /// Every known target and what this version generates for it.
    pub targets: Vec<TargetCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NamespaceCoverage {
    pub namespace: String,
    pub implemented: usize,
    pub planned: usize,
    pub gated: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TargetCoverage {
    pub target: String,
    /// Listed under `targets` in tungsten.yml.
    pub configured: bool,
    /// This version has an emitter for it.
    pub emitter: bool,
    pub status: CoverageStatus,
    /// Callable operations the output covers: the tools of `docs` and
    /// `mcp` (agents' hidden operations excluded), every callable
    /// operation for the SDKs.
    pub operations: usize,
    /// Macros the output covers.
    pub macros: usize,
    /// Files the emitter produces.
    pub files: usize,
    /// Warnings of the emitter (feature gaps, budgets).
    pub warnings: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// The emitter produced output (computed in memory).
    Generated,
    /// The emitter exists but produced no files in this version.
    NotGenerated,
    /// No emitter for this target in this version.
    NoEmitter,
    /// The emitter reported errors, or the project has errors.
    Failed,
}

/// The safety metadata of one operation (the report's safety matrix).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SafetyRow {
    pub operation: String,
    pub namespace: String,
    pub method: String,
    pub path: String,
    /// Callable only when a runtime gate is on.
    pub gated: bool,
    /// `read_only`, `mutating`, `destructive` or `irreversible`.
    pub tier: SafetyCell,
    /// The idempotency policy (`caller_owned (required)`, `none`, ...).
    pub idempotency: SafetyCell,
    /// The preview mode (`local`, `header`, `endpoint <op>`, `none`).
    pub preview: SafetyCell,
    /// `required` (with summary fields) for destructive and irreversible
    /// operations, else `not required`.
    pub confirmation: SafetyCell,
    /// The verification operation, or `none`.
    pub verify: SafetyCell,
}

/// One cell of the safety matrix and where its value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SafetyCell {
    pub value: String,
    pub origin: CellOrigin,
    /// The file the value was written in: the agent manifest or a spec
    /// document, as named in diagnostics; null for built-in defaults and
    /// inferred values.
    pub file: Option<String>,
    /// JSON Pointer of the node inside `file`.
    pub pointer: Option<String>,
    /// 1-based line of the node, when known.
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellOrigin {
    /// An agent manifest `tools` entry.
    Tool,
    /// An `x-agent-*` extension of the operation in the spec.
    Extension,
    /// The agent manifest's `defaults`.
    Defaults,
    /// Inferred from the spec (a required `Idempotency-Key` header) or from
    /// the tier (confirmation).
    Inferred,
    /// tungsten's built-in method defaults.
    BuiltIn,
}

/// Token budgets of the generated agent surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct Budgets {
    /// The counter of every count below (`tungsten-estimate-v2`).
    pub counter: String,
    /// agent.yml `disclosure.schema_budget_tokens`.
    pub schema_budget: u32,
    /// agent.yml `disclosure.description_budget_tokens`.
    pub description_budget: u32,
    /// agent.yml `disclosure.threshold`.
    pub threshold: u32,
    /// The docs target's agent-facing files (`llms.txt`, `llms-full.txt`,
    /// `tools.json`); empty when the docs emitter did not run.
    pub documents: Vec<DocumentBudget>,
    /// `tools.json`, one entry per tool; null when the docs emitter did not
    /// run.
    pub tools: Option<ToolBudgets>,
    /// The MCP manifest; null when the MCP emitter produced none ("not
    /// generated").
    pub mcp: Option<McpBudgets>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DocumentBudget {
    pub file: String,
    pub bytes: usize,
    pub tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ToolBudgets {
    /// Tools in file order: name, description and parameters as compact
    /// JSON, as a function-calling API receives them.
    pub tools: Vec<ToolCost>,
    /// All tools as one JSON array (a discrete tool list).
    pub list_tokens: usize,
    pub largest: usize,
    pub median: usize,
    /// Tools over `schema_budget`.
    pub over_budget: usize,
    /// Tool counts by cost, in 100-token buckets up to 600 and above.
    pub histogram: Vec<HistogramBucket>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ToolCost {
    pub name: String,
    /// The IR operation id or macro name the tool calls.
    pub target: String,
    pub kind: ToolKind,
    pub tokens: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Operation,
    Macro,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct HistogramBucket {
    /// Lowest cost in the bucket.
    pub min: usize,
    /// Highest cost in the bucket; null for the last, open bucket.
    pub max: Option<usize>,
    pub tools: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct McpBudgets {
    /// `discrete` or `progressive`, as the manifest selected.
    pub mode: String,
    pub threshold: u32,
    /// The manifest's own counter (`tokenCounter`).
    pub manifest_counter: String,
    /// Tools in manifest order: `tokens` is the manifest's `schemaTokens`.
    pub tools: Vec<ToolCost>,
    /// What a client receives in discrete mode before its first call: the
    /// `tools/list` tools `@tungsten/mcp` serves (every tool with its
    /// input and advertised output schema and annotations, and `preview`)
    /// as compact JSON, plus that mode's `initialize` instructions.
    pub discrete_tokens: usize,
    /// The same in progressive mode: the meta tools (`search_tools`,
    /// `describe_tool`, `invoke`, `preview`, `list_clusters`) plus the
    /// instructions, which carry the cluster index (budget 2,000 tokens).
    pub progressive_tokens: usize,
    /// Tools over `schema_budget`.
    pub over_budget: usize,
}

/// The diagnostics of one code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DiagnosticGroup {
    pub code: String,
    /// The most severe severity in the group.
    pub severity: Severity,
    /// The registry's one-line description of the code.
    pub summary: Option<String>,
    pub diagnostics: Vec<JsonDiagnostic>,
}

/// Which `$defs` entry describes `result` for each command.
const RESULT_DEFS: [(CommandName, &str); 12] = [
    (CommandName::Check, "CheckResult"),
    (CommandName::IrDump, "IrDumpResult"),
    (CommandName::Explain, "ExplainResult"),
    (CommandName::Schema, "SchemaResult"),
    (CommandName::Init, "InitResult"),
    (CommandName::Doctor, "DoctorResult"),
    (CommandName::Emitters, "EmittersResult"),
    (CommandName::Generate, "GenerateResult"),
    (CommandName::Mock, "MockResult"),
    (CommandName::Report, "ReportResult"),
    (CommandName::Diff, "DiffResult"),
    (CommandName::OverlaySuggest, "OverlaySuggestResult"),
];

/// The JSON Schema (draft 2020-12) of [`CliOutput`]. Beyond the generated
/// shape, it ties each `command` to the result object it produces.
pub fn cli_output_schema() -> Value {
    let generated = schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<CliOutput>()
        .to_value();
    let Value::Object(mut generated) = generated else {
        return generated;
    };
    // Identification first; the generated title is the Rust type name.
    generated.remove("title");
    let mut schema = serde_json::Map::new();
    if let Some(dialect) = generated.remove("$schema") {
        schema.insert("$schema".into(), dialect);
    }
    schema.insert("$id".into(), json!(SCHEMA_ID));
    schema.insert("title".into(), json!("tungsten CLI --json output v1"));
    schema.append(&mut generated);
    let rules: Vec<Value> = RESULT_DEFS
        .iter()
        .map(|(command, def)| {
            json!({
                "if": { "properties": { "command": { "const": command } }, "required": ["command"] },
                "then": { "properties": { "result": {
                    "anyOf": [{ "$ref": format!("#/$defs/{def}") }, { "type": "null" }]
                } } }
            })
        })
        .collect();
    schema.insert("allOf".into(), Value::Array(rules));
    Value::Object(schema)
}
