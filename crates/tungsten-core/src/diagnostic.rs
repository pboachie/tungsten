// SPDX-License-Identifier: AGPL-3.0-only
//! Diagnostics with stable codes.
//!
//! Code ranges (planning/03): `TG01xx` parsing, `TG02xx` refs and cycles,
//! `TG03xx` type normalization, `TG04xx` naming, `TG05xx` pagination/auth
//! inference, `TG06xx` manifests, `TG07xx` emitter limits, `TG09xx`
//! staleness/CI. The registry of codes lives in [`codes`].

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
            "TG0710",
            "macro not emitted by the TypeScript SDK (not in the canonical form)",
        ),
        ("TG0711", "invalid TypeScript target option; default used"),
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
        ("TG0901", "generated output is stale relative to its inputs"),
        (
            "TG0902",
            "API surface snapshot of the last generation is missing or unreadable; the change is not classified",
        ),
    ];

    pub fn describe(code: &str) -> Option<&'static str> {
        REGISTRY.iter().find(|(c, _)| *c == code).map(|(_, d)| *d)
    }
}
