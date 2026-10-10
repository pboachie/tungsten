// SPDX-License-Identifier: AGPL-3.0-only
//! The documents of protocol version 1 (see the module documentation of
//! [`super`]).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tungsten_core::Severity;
use tungsten_ir::Ir;

use super::{MAX_FILES, PROTOCOL};

/// Schema identifier of the protocol documents.
const SCHEMA_ID: &str = "https://tungsten.dev/schemas/external-emitter-v1.json";

/// The compiler that sends a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompilerInfo {
    /// Always `tungsten`.
    pub name: String,
    /// The compiler version.
    pub version: String,
}

/// What the compiler promises about the files it writes from a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutputPolicy {
    /// Always `relative-paths-only`: the response names files relative to
    /// the output directory and the compiler writes them; the emitter
    /// never receives the output directory.
    pub policy: String,
    /// A directory segment with this name marks hand-editable files:
    /// created once, never overwritten.
    pub custom_segment: String,
    /// The first path segment reserved for the compiler's bookkeeping.
    pub reserved_dir: String,
    /// Most files a response may list.
    pub max_files: usize,
    /// Most bytes a response may have on standard output.
    pub max_total_bytes: usize,
}

/// The document written to the emitter's standard input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "EmitRequest")]
pub struct EmitRequest {
    /// The protocol version, 1.
    pub protocol: u32,
    /// The target name in `tungsten.yml`.
    pub target: String,
    pub compiler: CompilerInfo,
    /// The target's options: its `tungsten.yml` keys except `out`,
    /// `external`, `timeout_ms` and `max_output_bytes`.
    pub options: Value,
    pub output: OutputPolicy,
    /// The intermediate representation, as `tungsten ir dump` prints it.
    pub ir: Ir,
}

impl EmitRequest {
    /// The request for `target` with `options` over `ir`.
    pub fn new(target: &str, options: Value, ir: &Ir, max_total_bytes: usize) -> Self {
        Self {
            protocol: PROTOCOL,
            target: target.to_string(),
            compiler: CompilerInfo {
                name: "tungsten".into(),
                version: ir.generator.tungsten_version.clone(),
            },
            options,
            output: OutputPolicy {
                policy: "relative-paths-only".into(),
                custom_segment: "custom".into(),
                reserved_dir: ".tungsten".into(),
                max_files: MAX_FILES,
                max_total_bytes,
            },
            ir: ir.clone(),
        }
    }
}

/// The kind of file to write. Only regular files exist in version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FileMode {
    /// A regular, non-executable file.
    File,
}

/// One file of a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmitFile {
    /// Relative to the output directory, forward slashes.
    pub path: String,
    /// The file as UTF-8 text. Exactly one of `content` and
    /// `content_base64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// The file as standard padded base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    /// Defaults to `file`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FileMode>,
}

/// Where a diagnostic of the emitter points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmitSpan {
    /// A file name, as shown to the user.
    pub file: String,
    /// An RFC 6901 JSON Pointer inside the file; empty for the whole file.
    #[serde(default)]
    pub pointer: String,
}

/// A diagnostic reported by the emitter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmitDiagnostic {
    /// The emitter's own code (any non-empty string).
    pub code: String,
    pub severity: Severity,
    pub message: String,
    /// What to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<EmitSpan>,
}

/// What the target adds to the API surface snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmitSurface {
    /// Agent tool name to the operation id or macro name it serves.
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
}

/// The document read from the emitter's standard output. Unknown fields
/// are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "EmitResponse")]
pub struct EmitResponse {
    /// The protocol version, 1.
    pub protocol: u32,
    #[serde(default)]
    pub files: Vec<EmitFile>,
    #[serde(default)]
    pub diagnostics: Vec<EmitDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<EmitSurface>,
}

/// The document `tungsten-emit-<name> --describe` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "EmitterDescription")]
pub struct EmitterDescription {
    /// The protocol version, 1.
    pub protocol: u32,
    /// The emitter's name (the target name it serves).
    pub name: String,
    /// The emitter's own version.
    pub version: String,
    /// A JSON Schema of the target options, for tooling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}

/// The JSON Schema (draft 2020-12) of the three documents, published as
/// `specs/external-emitter.schema.json`. They are definitions
/// (`#/$defs/EmitRequest`, `EmitResponse`, `EmitterDescription`); the root
/// accepts any of them.
pub fn json_schema() -> Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let request = generator.subschema_for::<EmitRequest>().to_value();
    let response = generator.subschema_for::<EmitResponse>().to_value();
    let description = generator.subschema_for::<EmitterDescription>().to_value();
    let defs = generator.take_definitions(true);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": SCHEMA_ID,
        "title": "tungsten external emitter protocol v1",
        "description": "The documents exchanged with an external emitter: EmitRequest on its standard input, EmitResponse on its standard output, EmitterDescription for --describe. Validate against the matching #/$defs entry.",
        "anyOf": [request, response, description],
        "$defs": defs,
    })
}
