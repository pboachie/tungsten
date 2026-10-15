// SPDX-License-Identifier: AGPL-3.0-only
//! Diagnostics with stable codes.
//!
//! Code ranges: `TG01xx` parsing, `TG02xx` refs and cycles, `TG03xx` type
//! normalization, `TG04xx` naming, `TG05xx` pagination/auth inference,
//! `TG06xx` manifests, `TG07xx` emitter limits, `TG08xx` external emitters,
//! `TG09xx` staleness/CI and overlay suggestions, `TG10xx` the SDK emitters
//! built on `tungsten_emit::sdk` in blocks of ten: `TG1000`-`TG1009` the
//! shared SDK plan and descriptor document (reserved), `TG1010` Java,
//! `TG1020` C#, `TG1030` Kotlin, `TG1040` Swift, `TG1050` PHP, `TG1060` Ruby,
//! `TG1070` Dart, `TG1080` F#, `TG1090`-`TG1099` reserved for later
//! languages; `TG12xx` request import (saved cURL and `.http`). The
//! importers, the request-contract engine and the Go emitter use their own
//! `PM`, `HR`, `TC` and `GO` prefixes. Within a language block (base B):
//! B+0 invalid target option, B+1 schema without a native type, B+2 macro not
//! in the canonical form, B+3 OpenID Connect sent as a bearer token, B+4
//! success bodies that differ by status, B+5 discriminated variant without a
//! tag value, B+6 to B+9
//! language-specific. The registry of codes lives in [`codes`].

use crate::source::{SourceMap, Span};
use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A source location attached to a diagnostic. `pointer` is the JSON Pointer
/// inside the document (RFC 6901), always present when the location is in an
/// OpenAPI or manifest document; `span` is present when the frontend could
/// map the pointer to bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Label {
    pub file: String,
    pub pointer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn new(code: &str, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            severity,
            message: message.into(),
            labels: vec![],
            help: None,
        }
    }
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self::new(code, Severity::Error, message)
    }
    pub fn warning(code: &str, message: impl Into<String>) -> Self {
        Self::new(code, Severity::Warning, message)
    }
    pub fn info(code: &str, message: impl Into<String>) -> Self {
        Self::new(code, Severity::Info, message)
    }
    pub fn at(
        mut self,
        file: impl Into<String>,
        pointer: impl Into<String>,
        span: Option<Span>,
    ) -> Self {
        self.labels.push(Label {
            file: file.into(),
            pointer: pointer.into(),
            span,
            message: None,
        });
        self
    }
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// One-line plain rendering: `warning TG0501: message (file:line:col)`.
    pub fn render_short(&self, sources: Option<&SourceMap>) -> String {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        };
        let loc = self.labels.first().map(|l| match (l.span, sources) {
            (Some(span), Some(map)) => map.location(span),
            _ => format!("{}#{}", l.file, l.pointer),
        });
        match loc {
            Some(loc) => format!("{sev} {}: {} ({loc})", self.code, self.message),
            None => format!("{sev} {}: {}", self.code, self.message),
        }
    }
}

/// An ordered collection of diagnostics. Order is insertion order; callers
/// that need deterministic output call [`Diagnostics::sort`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Diagnostics(pub Vec<Diagnostic>);

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, d: Diagnostic) {
        self.0.push(d);
    }
    pub fn extend(&mut self, other: Diagnostics) {
        self.0.extend(other.0);
    }
    pub fn has_errors(&self) -> bool {
        self.0.iter().any(Diagnostic::is_error)
    }
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.0.iter()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Deterministic order: severity, code, first label file, pointer, message.
    pub fn sort(&mut self) {
        self.0.sort_by(|a, b| {
            let ka = (
                a.severity,
                &a.code,
                a.labels.first().map(|l| (&l.file, &l.pointer)),
                &a.message,
            );
            let kb = (
                b.severity,
                &b.code,
                b.labels.first().map(|l| (&l.file, &l.pointer)),
                &b.message,
            );
            ka.cmp(&kb)
        });
    }
}

/// Registry of diagnostic codes. Every code emitted anywhere must be listed
/// here with a one-line meaning; `tungsten explain TGxxxx` reads this table.
pub mod codes {
    pub const REGISTRY: &[(&str, &str)] = &[
        ("TG0101", "input file could not be read"),
        ("TG0102", "input is not valid JSON or YAML"),
        (
            "TG0103",
            "unsupported OpenAPI version (convert Swagger 2.0 to OpenAPI 3 first)",
        ),
        ("TG0104", "document is missing a required OpenAPI field"),
        ("TG0105", "input exceeds a size or nesting limit"),
        (
            "TG0110",
            "document has no info.version; version 0.0.0 is used",
        ),
        (
            "TG0111",
            "path key carries a query string; its pairs are sent on every call",
        ),
        (
            "TG0112",
            "apiKey header scheme inferred from a credential header parameter",
        ),
        ("TG0201", "unresolvable $ref"),
        (
            "TG0202",
            "remote $ref blocked (remote fetching is not available)",
        ),
        (
            "TG0203",
            "circular $ref through named types (recursion preserved)",
        ),
        ("TG0204", "$ref to an external file outside the input root"),
        (
            "TG0205",
            "circular $ref without a named type to break the cycle",
        ),
        ("TG0206", "overlay target matched nothing"),
        ("TG0207", "overlay document or action is invalid"),
        (
            "TG0301",
            "untagged union: runtime will sniff candidates in order",
        ),
        (
            "TG0302",
            "allOf members conflict and could not be flattened",
        ),
        ("TG0303", "unsupported or unknown schema keyword ignored"),
        ("TG0304", "unknown string format treated as plain string"),
        (
            "TG0305",
            "schema type could not be determined; treated as any",
        ),
        ("TG0306", "discriminator could not be applied as written"),
        ("TG0307", "schema admits no value (empty enum or union)"),
        (
            "TG0308",
            "types.break_cycles entry names no component property",
        ),
        ("TG0401", "identifier collision disambiguated"),
        (
            "TG0402",
            "identifier escaped (reserved keyword or invalid start)",
        ),
        ("TG0403", "operation has no operationId; name synthesized"),
        ("TG0501", "pagination inferred by heuristic"),
        ("TG0502", "security scheme referenced but not defined"),
        (
            "TG0503",
            "composite auth profile satisfies an undefined security scheme",
        ),
        (
            "TG0504",
            "operation excluded by include predicate (planned)",
        ),
        ("TG0505", "runtime gate recorded from x-runtime-gate"),
        (
            "TG0506",
            "rpc_unflatten target not found or not a const-discriminated oneOf",
        ),
        (
            "TG0507",
            "path template parameter without a definition; assumed a required string",
        ),
        ("TG0508", "malformed operation element ignored"),
        ("TG0509", "unsupported security scheme ignored"),
        (
            "TG0510",
            "path item field defined both beside its $ref and in the target",
        ),
        (
            "TG0530",
            "event stream declares no schema; events decoded as any JSON value",
        ),
        (
            "TG0531",
            "event stream declared for more than one success status; the first is streamed",
        ),
        (
            "TG0570",
            "invalid MCP embedding index configuration (search.embeddings); the index is off",
        ),
        (
            "TG0571",
            "embedding provider could not be run or did not answer in time; BM25 only",
        ),
        (
            "TG0572",
            "embedding provider returned an invalid response; BM25 only",
        ),
        (
            "TG0573",
            "embedding endpoint refused the request, needs a missing API key or uses https; BM25 only",
        ),
        ("TG0574", "embedding index written next to the MCP manifest"),
        ("TG0601", "manifest is not valid YAML"),
        ("TG0602", "manifest does not match its schema"),
        ("TG0603", "manifest references an unknown operation"),
        ("TG0604", "manifest references an unknown namespace or path"),
        (
            "TG0605",
            "agent rule targets an operation that cannot serve it",
        ),
        ("TG0606", "remediation names an unknown error code"),
        (
            "TG0607",
            "agent rule names a field the operation does not have",
        ),
        (
            "TG0608",
            "gate names no runtime gate or conflicts with the spec",
        ),
        ("TG0609", "macro dropped or its safety tier raised"),
        ("TG0610", "unknown x-agent-* extension ignored"),
        ("TG0611", "x-agent-* extension value is malformed; ignored"),
        ("TG0612", "targets share or nest their output directories"),
        (
            "TG0613",
            "agent.yml option not applied by this version; ignored",
        ),
        ("TG0614", "gates entry no operation is gated by"),
        ("TG0701", "generated output could not be written"),
        ("TG0702", "target has no emitter in this version; skipped"),
        (
            "TG0703",
            "output directory was not generated by tungsten; refusing to write",
        ),
        (
            "TG0704",
            "output path leaves the output directory or is reserved",
        ),
        (
            "TG0705",
            "target does not declare enforced imported-operation policy support; not generated",
        ),
        (
            "TG0710",
            "macro not emitted by the TypeScript SDK (not in the canonical form)",
        ),
        (
            "TG0711",
            "invalid TypeScript target option or custom dependency",
        ),
        (
            "TG0712",
            "OpenID Connect scheme sent as a bearer token by the TypeScript SDK",
        ),
        ("TG0713", "tool schema over the agent schema token budget"),
        ("TG0720", "invalid MCP target option; default used"),
        (
            "TG0721",
            "MCP tool schema over the agent schema token budget",
        ),
        (
            "TG0722",
            "progressive MCP tool list and index summary over the 2,000-token budget",
        ),
        (
            "TG0723",
            "macro not exposed as an MCP tool (not emitted by the TypeScript SDK)",
        ),
        (
            "TG0724",
            "MCP tool names collide; each is named with a digest of its operation",
        ),
        (
            "TG0725",
            "disclosure.mode auto chose the MCP disclosure mode from the measured token size of the discrete tool list",
        ),
        (
            "TG0726",
            "the MCP server's SDK dependency defaults to the typescript target's package",
        ),
        (
            "TG0727",
            "MCP tool output schema over the output token budget even without the types it references",
        ),
        (
            "TG0730",
            "macro not emitted by the Python SDK (not in the canonical form)",
        ),
        ("TG0731", "invalid Python target option; default used"),
        (
            "TG0732",
            "OpenID Connect scheme sent as a bearer token by the Python SDK",
        ),
        (
            "TG0733",
            "inline record typed as a plain mapping by the Python SDK",
        ),
        ("TG0740", "invalid Rust target option; default used"),
        (
            "TG0741",
            "type or schema typed as serde_json::Value by the Rust SDK",
        ),
        (
            "TG0742",
            "macro not emitted by the Rust SDK (not in the canonical form)",
        ),
        (
            "TG0743",
            "OpenID Connect scheme sent as a bearer token by the Rust SDK",
        ),
        (
            "TG0744",
            "operation with different non-JSON or ranged success bodies returns serde_json::Value in the Rust SDK",
        ),
        (
            "TG0745",
            "tagged union emitted by the Rust SDK as an untagged one",
        ),
        (
            "TG0746",
            "macro output typed as serde_json::Value by the Rust SDK",
        ),
        (
            "TG0750",
            "argument given as JSON text on the command line of the generated CLI",
        ),
        (
            "TG0751",
            "resource named like a command of the generated CLI; renamed",
        ),
        (
            "TG0752",
            "flag renamed in the generated CLI because its name is taken",
        ),
        (
            "TG0753",
            "two commands of the generated CLI share a path; the later one is numbered",
        ),
        (
            "TG0760",
            "types no operation reaches were not generated (types.prune_unreferenced)",
        ),
        ("TG0790", "invalid WebMCP target option; default used"),
        ("TG0791", "WebMCP generated file could not be added"),
        (
            "TG0792",
            "WebMCP SDK dependency defaults to the TypeScript target package",
        ),
        (
            "TG0793",
            "WebMCP preview tool name collides with an invocation tool",
        ),
        ("TG0801", "external emitter not found or cannot be started"),
        (
            "TG0802",
            "external emitter speaks a different protocol version",
        ),
        (
            "TG0803",
            "external emitter failed: non-zero exit, timeout or output over the cap",
        ),
        ("TG0804", "external emitter returned an invalid response"),
        (
            "TG0805",
            "external emitter returned a path outside the output directory",
        ),
        ("TG0806", "diagnostic reported by an external emitter"),
        (
            "TG0807",
            "external target not run: external emitters are off (--no-external)",
        ),
        ("TG0901", "generated output is stale relative to its inputs"),
        (
            "TG0902",
            "API surface snapshot of the last generation is missing or unreadable; the change is not classified",
        ),
        (
            "TG0920",
            "overlay suggest: a diagnostic has an overlay fix but its value cannot be derived; skipped",
        ),
        (
            "TG0921",
            "overlay suggest: no overlay action can fix this diagnostic code",
        ),
        ("TG1010", "invalid java target option; the default is used"),
        (
            "TG1011",
            "schema has no Java type in the java target; the runtime's JSON value is used",
        ),
        (
            "TG1012",
            "macro is not in the canonical form; the java target leaves it out",
        ),
        (
            "TG1013",
            "OpenID Connect scheme is sent as a bearer token by the java target",
        ),
        (
            "TG1014",
            "different success bodies by status: the java typed result is the JSON value",
        ),
        (
            "TG1015",
            "discriminated union variant without a tag value: the java target tries variants in order",
        ),
        (
            "TG1020",
            "invalid csharp target option; the default is used",
        ),
        (
            "TG1021",
            "schema has no C# type in the csharp target; the runtime's JsonNode is used",
        ),
        (
            "TG1022",
            "macro is not in the canonical form; the csharp target leaves it out",
        ),
        (
            "TG1023",
            "OpenID Connect scheme is sent as a bearer token by the csharp target",
        ),
        (
            "TG1024",
            "different success bodies by status: the csharp typed result is the JsonNode",
        ),
        (
            "TG1025",
            "discriminated union variant without a tag value: the csharp target tries variants in order",
        ),
        (
            "TG1030",
            "invalid kotlin target option; the default is used",
        ),
        (
            "TG1031",
            "schema has no Kotlin type in the kotlin target; the runtime's JSON value is used",
        ),
        (
            "TG1032",
            "macro is not in the canonical form; the kotlin target leaves it out",
        ),
        (
            "TG1033",
            "OpenID Connect scheme is sent as a bearer token by the kotlin target",
        ),
        (
            "TG1034",
            "different success bodies by status: the kotlin typed result is the JSON value",
        ),
        (
            "TG1035",
            "discriminated union variant without a tag value: the kotlin target tries variants in order",
        ),
        ("TG1060", "invalid Ruby target option; default used"),
        (
            "TG1061",
            "allOf type that cannot be merged is a JSON value in the Ruby SDK",
        ),
        (
            "TG1062",
            "macro not in the canonical form; left out of the Ruby SDK",
        ),
        (
            "TG1063",
            "OpenID Connect scheme sent as a bearer token by the Ruby SDK",
        ),
        (
            "TG1064",
            "different success bodies by status; the Ruby SDK's typed result is the JSON value",
        ),
        (
            "TG1065",
            "discriminated union variant without a tag value; the Ruby SDK tries the variants in order",
        ),
        ("TG1070", "invalid dart target option; the default is used"),
        (
            "TG1071",
            "schema has no Dart type in the dart target; the JSON value (Object?) is used",
        ),
        (
            "TG1072",
            "macro is not in the canonical form; the dart target leaves it out",
        ),
        (
            "TG1073",
            "OpenID Connect scheme is sent as a bearer token by the dart target",
        ),
        (
            "TG1074",
            "different success bodies by status: the dart typed result is the JSON value (Object?)",
        ),
        (
            "TG1075",
            "discriminated union variant without a tag value: the dart target tries variants in order",
        ),
        (
            "TG1080",
            "invalid fsharp target option; the default is used",
        ),
        (
            "TG1081",
            "schema has no F# type in the fsharp target; the runtime's JsonNode is used",
        ),
        (
            "TG1082",
            "macro is not in the canonical form; the fsharp target leaves it out",
        ),
        (
            "TG1083",
            "OpenID Connect scheme is sent as a bearer token by the fsharp target",
        ),
        (
            "TG1084",
            "different success bodies by status: the fsharp typed result is the JsonNode",
        ),
        (
            "TG1085",
            "discriminated union variant without a tag value: the fsharp target tries variants in order",
        ),
        (
            "TG1201",
            "saved cURL or .http request could not be imported",
        ),
    ];

    /// Codes of the importers (Postman `PM`, HAR `HR`), the request-contract
    /// engine (`TC`) and the Go emitter (`GO`). They do not carry the `TG`
    /// prefix; `tungsten explain` reads this table next to [`REGISTRY`].
    /// Sorted by code.
    pub const SUBSYSTEM_REGISTRY: &[(&str, &str)] = &[
        ("GO001", "macro is not emitted by the Go SDK"),
        (
            "GO002",
            "OpenID Connect scheme is sent as a bearer token by the Go SDK",
        ),
        (
            "GO003",
            "different success bodies by status: the Go typed result is json.RawMessage",
        ),
        ("GO004", "invalid go target option"),
        ("GO005", "the generated Go source is invalid"),
        (
            "GO006",
            "pagination items path does not resolve to an array in the success schema",
        ),
        (
            "HR001",
            "HAR capture exceeds the byte, nesting or node limit",
        ),
        ("HR002", "HAR capture is malformed or unsupported"),
        ("HR003", "selected HAR origin is absent or noncanonical"),
        (
            "HR006",
            "reviewed HAR route templates are ambiguous or not whole-segment",
        ),
        (
            "PM001",
            "Postman collection exceeds an import budget or nesting limit",
        ),
        ("PM002", "not a Postman Collection v2.1 item tree"),
        (
            "PM003",
            "Postman API key authentication lacks an explicit name or location",
        ),
        (
            "PM004",
            "Postman dynamic variable cannot be resolved offline",
        ),
        (
            "PM005",
            "Postman request URL or variable cannot be resolved to a fixed shape",
        ),
        ("PM006", "Postman body or response example is not JSON"),
        (
            "PM007",
            "duplicate Postman operations mix unknown and explicit authentication",
        ),
        ("PM008", "every Postman request was omitted as unsupported"),
        ("TC001", "request evidence exceeds the byte limit"),
        ("TC002", "request evidence is not valid JSON"),
        (
            "TC003",
            "request evidence or reviewed overrides are invalid",
        ),
        (
            "TC004",
            "request evidence has an unsupported method or duplicate path placeholders",
        ),
        ("TC005", "evidence source id is not an opaque identifier"),
        ("TC006", "unsupported schema keyword in structural evidence"),
        (
            "TC007",
            "reviewed override uses an unstable parameter index pointer",
        ),
    ];

    pub fn describe(code: &str) -> Option<&'static str> {
        REGISTRY
            .iter()
            .chain(SUBSYSTEM_REGISTRY)
            .find(|(c, _)| *c == code)
            .map(|(_, d)| *d)
    }
}
