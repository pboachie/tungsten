// SPDX-License-Identifier: AGPL-3.0-only
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EVIDENCE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceDocument {
    pub version: u32,
    pub frontend: Frontend,
    pub operations: Vec<HttpEvidence>,
}

/// Other protocol frontends must define their own versioned evidence model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Frontend {
    Http,
}

/// An opaque adapter-assigned document ID and numeric location, never a URL,
/// filename, header, captured value, or source excerpt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct SourceLocation {
    pub document: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpEvidence {
    pub method: String,
    /// Explicit adapter-approved template. No heuristic grouping of path values.
    pub path: String,
    pub source: SourceLocation,
    #[serde(default)]
    pub parameters: Vec<ParameterEvidence>,
    #[serde(default)]
    pub request: Vec<MediaEvidence>,
    #[serde(default)]
    pub responses: Vec<ResponseEvidence>,
    /// Names of unsupported/unavailable structural facts; values never belong here.
    #[serde(default)]
    pub unknown: BTreeSet<UnknownFact>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeEvidence {
    #[serde(default)]
    pub samples: Vec<Value>,
    /// Structural schema subset only; value-bearing annotations are rejected.
    #[serde(default)]
    pub declared_schema: Option<Value>,
}

impl std::fmt::Debug for ShapeEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShapeEvidence")
            .field("sample_count", &self.samples.len())
            .field("has_declaration", &self.declared_schema.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterEvidence {
    pub name: String,
    pub location: ParameterLocation,
    pub shape: ShapeEvidence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ParameterLocation {
    Path,
    Query,
    Header,
    Cookie,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaEvidence {
    pub media_type: String,
    pub shape: ShapeEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEvidence {
    pub status: u16,
    #[serde(default)]
    pub content: Vec<MediaEvidence>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UnknownFact {
    Authentication,
    Authorization,
    Safety,
    Requiredness,
    Completeness,
    UnsupportedSourceField,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Declared,
    Observed,
    Unknown,
    UserReviewed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactProvenance {
    pub states: BTreeSet<EvidenceState>,
    pub sources: BTreeSet<SourceLocation>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SafetyDecision {
    Unknown,
    ReadOnly,
    Mutating,
    Destructive,
    Irreversible,
}

/// Persist separately from observations and pass unchanged on each import.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewedOverrides {
    /// OpenAPI body schema pointers, or stable `parameter_schema_pointer` keys.
    #[serde(default)]
    pub schemas: BTreeMap<String, Value>,
    /// Stable operation IDs to explicit human safety decisions.
    #[serde(default)]
    pub safety: BTreeMap<String, SafetyDecision>,
    /// Trusted exact origins reviewed for each operation; never inferred.
    #[serde(default)]
    pub reviewed_origins: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewDiff {
    pub pointer: String,
    pub generated: Option<Value>,
    pub reviewed: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceReport {
    pub version: u32,
    pub facts: BTreeMap<String, FactProvenance>,
    pub review_diff: Vec<ReviewDiff>,
    pub unknown: BTreeMap<String, BTreeSet<UnknownFact>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractOutput {
    pub openapi: Value,
    pub report: InferenceReport,
    pub overrides: ReviewedOverrides,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: String,
    /// Constant actionable text; never includes untrusted input fragments.
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Diagnostic {}

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_operations: usize,
    pub max_samples: usize,
    pub max_sources: usize,
    pub max_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_depth: 64,
            max_operations: 10_000,
            max_samples: 50_000,
            max_sources: 100,
            max_nodes: 500_000,
        }
    }
}
