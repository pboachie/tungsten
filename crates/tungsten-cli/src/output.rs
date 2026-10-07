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
    #[serde(rename = "generate")]
    Generate,
    #[serde(rename = "mock")]
    Mock,
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
    Generate(GenerateResult),
    Mock(MockResult),
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
}

/// `tungsten mock`, printed once the server is listening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct MockResult {
    /// `http://127.0.0.1:<port>`.
    pub base_url: String,
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

/// Which `$defs` entry describes `result` for each command.
const RESULT_DEFS: [(CommandName, &str); 8] = [
    (CommandName::Check, "CheckResult"),
    (CommandName::IrDump, "IrDumpResult"),
    (CommandName::Explain, "ExplainResult"),
    (CommandName::Schema, "SchemaResult"),
    (CommandName::Init, "InitResult"),
    (CommandName::Doctor, "DoctorResult"),
    (CommandName::Generate, "GenerateResult"),
    (CommandName::Mock, "MockResult"),
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
