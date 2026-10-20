// SPDX-License-Identifier: AGPL-3.0-only
//! Host-selected external-emitter constraints. These are lifecycle/resource
//! restrictions, never executable approval or a filesystem/network sandbox.
//!
//! A host captures reviewed bytes with an expected digest, then explicitly
//! selects a new invocation context. Neither inspection exports nor prepared IR
//! can be converted into execution authority. Revalidate host selections for
//! every new invocation; a running invocation consistently uses its capture.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::de::{MapAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Diagnostic, Digest};

pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
pub const MAX_DOCUMENTS: usize = 16;
pub const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
pub const MAX_NESTING: usize = 8;
pub const MAX_TIMEOUT_MS: u64 = 3_600_000;
pub const MAX_OUTPUT_BYTES: u64 = 1 << 40;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
pub const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(5);
pub const DESCRIBE_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// The strict JSON document schema. Typed values alone are not a capture.
#[derive(Debug, Clone)]
pub struct PolicyDocument {
    pub tungsten_policy: u32,
    pub external: ExternalConstraints,
}

#[derive(Debug, Clone, Default)]
pub struct ExternalConstraints {
    pub deny: Option<bool>,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

// deserialize_map is intentional: serde's derived struct decoder also accepts
// positional arrays. The wire contract accepts objects only, including nested
// constraints, and derived helpers reject duplicate and unknown members.
impl<'de> Deserialize<'de> for PolicyDocument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            tungsten_policy: u32,
            external: ExternalConstraints,
        }
        struct Object;
        impl<'de> Visitor<'de> for Object {
            type Value = PolicyDocument;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a policy object")
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                let fields = Fields::deserialize(MapAccessDeserializer::new(map))?;
                Ok(PolicyDocument {
                    tungsten_policy: fields.tungsten_policy,
                    external: fields.external,
                })
            }
        }
        deserializer.deserialize_map(Object)
    }
}

impl<'de> Deserialize<'de> for ExternalConstraints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Object;
        impl<'de> Visitor<'de> for Object {
            type Value = ExternalConstraints;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a nonempty external constraints object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut out = ExternalConstraints::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "deny" if out.deny.is_none() => out.deny = Some(map.next_value::<bool>()?),
                        "timeout_ms" if out.timeout_ms.is_none() => {
                            out.timeout_ms = Some(map.next_value::<u64>()?);
                        }
                        "max_output_bytes" if out.max_output_bytes.is_none() => {
                            out.max_output_bytes = Some(map.next_value::<u64>()?);
                        }
                        _ => return Err(serde::de::Error::custom("invalid constraint member")),
                    }
                }
                if out.deny.is_none() && out.timeout_ms.is_none() && out.max_output_bytes.is_none()
                {
                    return Err(serde::de::Error::custom("missing constraint"));
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(Object)
    }
}

/// Exact immutable bytes plus their validated typed meaning. No Deserialize or
/// Serialize implementation exists: exported configuration cannot create one.
#[derive(Clone)]
pub struct CapturedPolicy {
    bytes: Arc<[u8]>,
    digest: Digest,
    document: PolicyDocument,
    // Local provenance only; never displayed or exported, and not launch proof.
    identity: Option<PathBuf>,
}

impl fmt::Debug for CapturedPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedPolicy")
            .field("bytes", &self.bytes.len())
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl CapturedPolicy {
    /// Validate bounds, strict JSON and an exact lowercase core digest before
    /// creating a reusable document. The caller must deliberately select the pin.
    pub fn from_bytes(bytes: &[u8], expected: &Digest) -> Result<Self, Diagnostic> {
        let document = validate(bytes)?;
        let digest = Digest::of(bytes);
        if !valid_pin(&expected.0) || digest != *expected {
            return Err(invalid().with_help("select the exact digest of reviewed policy bytes; pins are never updated automatically"));
        }
        Ok(Self {
            bytes: bytes.into(),
            digest,
            document,
            identity: None,
        })
    }

    /// Attach an adapter's canonical local identity to the same byte capture.
    /// This is provenance, not a trust or replacement-safe launch assertion.
    pub fn with_local_identity(mut self, identity: PathBuf) -> Self {
        self.identity = Some(identity);
        self
    }
}

/// A deliberate, immutable selection for one invocation. It is never retained
/// by compiled/prepared IR. Hosts may share it inside that invocation via Arc.
#[derive(Debug)]
pub struct InvocationPolicy {
    documents: Vec<CapturedPolicy>,
    inspection: PolicyInspection,
}

impl InvocationPolicy {
    /// Explicitly select the legacy, absent-policy behavior for this invocation.
    pub fn absent() -> Self {
        Self {
            documents: Vec::new(),
            inspection: PolicyInspection::absent(),
        }
    }

    /// Select and revalidate every submitted capture, including duplicates.
    pub fn select(documents: &[CapturedPolicy]) -> Result<Self, Diagnostic> {
        if documents.len() > MAX_DOCUMENTS
            || documents
                .iter()
                .try_fold(0usize, |sum, d| sum.checked_add(d.bytes.len()))
                .is_none_or(|sum| sum > MAX_CAPTURE_BYTES)
        {
            return Err(invalid());
        }
        let mut inspection = PolicyInspection::absent();
        let mut captured = Vec::with_capacity(documents.len());
        for (ordinal, document) in documents.iter().enumerate() {
            let checked = CapturedPolicy::from_bytes(&document.bytes, &document.digest)?
                .with_optional_identity(document.identity.clone());
            let source = format!("policy[{ordinal}]");
            inspection.sources.push(PolicySource {
                source: source.clone(),
                digest: checked.digest.0.clone(),
            });
            let constraints = &checked.document.external;
            inspection.deny |= constraints.deny == Some(true);
            inspection.timeout_ms = minimum(inspection.timeout_ms, constraints.timeout_ms);
            inspection.max_output_bytes =
                minimum(inspection.max_output_bytes, constraints.max_output_bytes);
            for (pointer, value) in [
                (
                    "/external/deny",
                    constraints.deny.map(ConstraintValue::Deny),
                ),
                (
                    "/external/timeout_ms",
                    constraints.timeout_ms.map(ConstraintValue::Ceiling),
                ),
                (
                    "/external/max_output_bytes",
                    constraints.max_output_bytes.map(ConstraintValue::Ceiling),
                ),
            ] {
                if let Some(value) = value {
                    inspection.constraints.push(ConstraintOrigin {
                        source: source.clone(),
                        digest: checked.digest.0.clone(),
                        pointer: pointer.into(),
                        value,
                    });
                }
            }
            captured.push(checked);
        }
        if !captured.is_empty() {
            inspection.state = PolicyState::Present;
        }
        Ok(Self {
            documents: captured,
            inspection,
        })
    }

    /// Add one CLI selection by intersection, never replacing the host context.
    pub fn intersect(&self, document: CapturedPolicy) -> Result<Self, Diagnostic> {
        if self.documents.len() >= MAX_DOCUMENTS {
            return Err(invalid());
        }
        let mut documents = self.documents.clone();
        documents.push(document);
        Self::select(&documents)
    }

    pub fn document_count(&self) -> usize {
        self.documents.len()
    }

    /// Redacted inspection is descriptive only, without captured bytes/identity.
    pub fn inspection(&self) -> PolicyInspection {
        self.inspection.clone()
    }

    /// Validate authored project values separately from execution denial. Missing
    /// and null values are represented by None and retain default/clamp semantics.
    pub fn validate_request(&self, requested: RequestedExternalLimits) -> Result<(), Diagnostic> {
        let valid = requested
            .timeout_ms
            .is_none_or(|v| (1..=MAX_TIMEOUT_MS).contains(&v))
            && requested
                .max_output_bytes
                .is_none_or(|v| (1..=MAX_OUTPUT_BYTES).contains(&v) && usize::try_from(v).is_ok());
        if !valid {
            return Err(Diagnostic::error(
                "TG0810",
                "external target constraints are invalid or unrepresentable on this platform",
            ));
        }
        for (pointer, requested, ceiling) in [
            (
                "/external/timeout_ms",
                requested.timeout_ms,
                self.inspection.timeout_ms,
            ),
            (
                "/external/max_output_bytes",
                requested.max_output_bytes,
                self.inspection.max_output_bytes,
            ),
        ] {
            if let (Some(requested), Some(ceiling)) = (requested, ceiling)
                && requested > ceiling
            {
                return Err(self.constraint_error(
                    "TG0810",
                    "external target explicitly exceeds a protected ceiling",
                    pointer,
                ));
            }
        }
        Ok(())
    }

    /// Shared gate used before request serialization, process admission or spawn
    /// for BOTH run and descriptor negotiation. Callers cannot replace it with a
    /// precomputed permissive limit. Ceilings apply per operation, not per command.
    pub fn resolve(
        &self,
        requested: RequestedExternalLimits,
        operation: ExternalOperation,
    ) -> Result<EffectiveExternalLimits, Diagnostic> {
        self.validate_request(requested)?;
        self.allow_execution()?;
        let (timeout, output) = match operation {
            ExternalOperation::Run => (
                requested
                    .timeout_ms
                    .unwrap_or(DEFAULT_TIMEOUT.as_millis() as u64),
                requested
                    .max_output_bytes
                    .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES as u64),
            ),
            ExternalOperation::Describe => (
                DESCRIBE_TIMEOUT.as_millis() as u64,
                DESCRIBE_MAX_OUTPUT_BYTES as u64,
            ),
        };
        Ok(EffectiveExternalLimits {
            timeout: Duration::from_millis(
                self.inspection
                    .timeout_ms
                    .map_or(timeout, |v| v.min(timeout)),
            ),
            max_output_bytes: usize::try_from(
                self.inspection
                    .max_output_bytes
                    .map_or(output, |v| v.min(output)),
            )
            .map_err(|_| invalid())?,
        })
    }

    /// Discovery uses this before scanning/locating external programs.
    pub fn allow_execution(&self) -> Result<(), Diagnostic> {
        if self.inspection.deny {
            Err(self.constraint_error(
                "TG0811",
                "external execution is denied by the selected policy",
                "/external/deny",
            ))
        } else {
            Ok(())
        }
    }

    fn constraint_error(&self, code: &str, message: &str, pointer: &str) -> Diagnostic {
        let mut diagnostic = Diagnostic::error(code, message);
        let relevant: Vec<_> = self
            .inspection
            .constraints
            .iter()
            .filter(|origin| {
                origin.pointer == pointer
                    && match origin.value {
                        ConstraintValue::Deny(value) => value,
                        ConstraintValue::Ceiling(value) => {
                            Some(value)
                                == if pointer == "/external/timeout_ms" {
                                    self.inspection.timeout_ms
                                } else {
                                    self.inspection.max_output_bytes
                                }
                        }
                    }
            })
            .collect();
        for origin in &relevant {
            diagnostic = diagnostic.at(&origin.source, &origin.pointer, None);
        }
        diagnostic.with_help(
            relevant
                .iter()
                .map(|origin| format!("{}#{} {}", origin.source, origin.pointer, origin.digest))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

impl CapturedPolicy {
    fn with_optional_identity(mut self, identity: Option<PathBuf>) -> Self {
        self.identity = identity;
        self
    }
}

fn minimum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RequestedExternalLimits {
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub enum ExternalOperation {
    Run,
    Describe,
}

#[derive(Debug, Clone, Copy)]
pub struct EffectiveExternalLimits {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PolicyInspection {
    pub state: PolicyState,
    pub deny: bool,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
    pub sources: Vec<PolicySource>,
    /// Every declaration, including weaker values, ties and repeated captures.
    pub constraints: Vec<ConstraintOrigin>,
}

impl PolicyInspection {
    pub fn absent() -> Self {
        Self {
            state: PolicyState::Absent,
            deny: false,
            timeout_ms: None,
            max_output_bytes: None,
            sources: Vec::new(),
            constraints: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyState {
    Absent,
    Present,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PolicySource {
    pub source: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ConstraintOrigin {
    pub source: String,
    pub digest: String,
    pub pointer: String,
    pub value: ConstraintValue,
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum ConstraintValue {
    Deny(bool),
    Ceiling(u64),
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PolicyDigestResult {
    pub policy_version: u32,
    pub bytes: usize,
    pub digest: Digest,
}

/// Pure validation/hash result; this deliberately does not return a capture.
pub fn digest_document(bytes: &[u8]) -> Result<PolicyDigestResult, Diagnostic> {
    validate(bytes)?;
    Ok(PolicyDigestResult {
        policy_version: 1,
        bytes: bytes.len(),
        digest: Digest::of(bytes),
    })
}

pub fn json_schema() -> serde_json::Value {
    // Optional constraints are non-nullable on the wire. Keep this publication
    // explicit: ordinary Option schemas would incorrectly admit explicit null.
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "PolicyDocument", "type": "object", "additionalProperties": false,
        "required": ["tungsten_policy", "external"],
        "properties": {
            "tungsten_policy": {"type": "integer", "const": 1},
            "external": {
                "type": "object", "additionalProperties": false, "minProperties": 1,
                "properties": {
                    "deny": {"type": "boolean"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS},
                    "max_output_bytes": {"type": "integer", "minimum": 1, "maximum": MAX_OUTPUT_BYTES}
                }
            }
        }
    })
}

/// Constant, export-safe diagnostic for host reading, shape, pin and budget errors.
pub fn invalid() -> Diagnostic {
    Diagnostic::error("TG0809", "selected host policy is invalid, unavailable, over its bounds, or does not match its digest")
        .with_help("select a regular local strict JSON policy of at most 64 KiB and its reviewed exact lowercase blake3 digest; paths and authored values are omitted")
}

fn valid_pin(pin: &str) -> bool {
    pin.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn validate(bytes: &[u8]) -> Result<PolicyDocument, Diagnostic> {
    if bytes.len() > MAX_DOCUMENT_BYTES || std::str::from_utf8(bytes).is_err() {
        return Err(invalid());
    }
    // Bound nesting before serde recursively decodes anything. Escapes and
    // delimiters inside strings do not contribute to structural depth.
    let (mut depth, mut string, mut escape) = (0usize, false, false);
    for &byte in bytes {
        if string {
            if escape {
                escape = false;
            } else if byte == b'\\' {
                escape = true;
            } else if byte == b'"' {
                string = false;
            }
        } else {
            match byte {
                b'"' => string = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_NESTING {
                        return Err(invalid());
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    let document: PolicyDocument = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let external = &document.external;
    if document.tungsten_policy != 1
        || external
            .timeout_ms
            .is_some_and(|v| !(1..=MAX_TIMEOUT_MS).contains(&v))
        || external
            .max_output_bytes
            .is_some_and(|v| !(1..=MAX_OUTPUT_BYTES).contains(&v) || usize::try_from(v).is_err())
    {
        return Err(invalid());
    }
    Ok(document)
}
