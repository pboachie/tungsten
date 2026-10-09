// SPDX-License-Identifier: AGPL-3.0-only
//! The `tungsten.yml` shape manifest (planning/04).
//!
//! The structs here are the contract the builder consumes. Loading runs in
//! three stages, each reporting diagnostics with the JSON Pointer of the
//! offending node and its line and column in the message:
//!
//! 1. YAML → JSON with node positions ([`ManifestSource`]); YAML problems,
//!    aliases, duplicate keys and multiple documents are `TG0601`.
//! 2. JSON → [`TungstenConfig`]; type mismatches, missing fields and
//!    unknown keys are `TG0602`.
//! 3. Semantic rules ([`validate`]): `TG0602` for invalid values, `TG0604`
//!    for references to undeclared namespaces.
//!
//! `TungstenConfig::json_schema()` is published as
//! `specs/tungsten.schema.json`.
//!
//! Stages 1 and 2 are shared with other YAML manifests (`agent.yml`):
//! [`parse_yaml`] and [`deserialize_manifest`] report `TG0601` and
//! `TG0602` the same way for any manifest type.

pub mod pointer;
mod source;
mod strict;
mod validate;
mod yaml;

use std::fmt;
use std::path::Path;

use indexmap::IndexMap;
use serde::de::{self, DeserializeOwned, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use tungsten_core::{Diagnostic, Diagnostics};

pub use source::ManifestSource;
pub use validate::{KNOWN_TARGETS, MANIFEST_VERSION, is_machine_name, validate};

/// The `tungsten.yml` manifest: shape and naming of the generated SDKs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TungstenConfig {
    /// Manifest format version. Must be 1.
    #[schemars(range(min = 1, max = 1))]
    pub tungsten: u32,
    pub api: ApiConfig,
    pub inputs: Vec<InputConfig>,
    /// Overlays applied to every input, after per-input overlays.
    #[serde(default)]
    pub overlays: Vec<String>,
    #[serde(default)]
    pub servers: Option<ServersConfig>,
    /// namespace → resource name → resource.
    #[serde(default)]
    pub resources: IndexMap<String, IndexMap<String, ResourceConfig>>,
    #[serde(default)]
    pub naming: NamingConfig,
    #[serde(default)]
    pub auth_profiles: IndexMap<String, AuthProfile>,
    /// Operation ref (`namespace.operationId`) → pagination.
    #[serde(default)]
    pub pagination: IndexMap<String, PaginationConfig>,
    #[serde(default)]
    pub types: TypesConfig,
    /// Target name → target options. Typed per target in Phase 2.
    #[serde(default)]
    pub targets: IndexMap<String, serde_json::Value>,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    /// Path of the agent manifest, relative to this file. When absent,
    /// `agent.yml` next to this file is used if it exists.
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// Machine name, lowercase (`zrotext`).
    #[schemars(pattern(r"^[a-z][a-z0-9_]*$"))]
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub docs_url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    /// Namespace of this document's operations and types; unique.
    #[schemars(pattern(r"^[a-z][a-z0-9_]*$"))]
    pub namespace: String,
    /// Path relative to the manifest's directory.
    pub spec: String,
    #[serde(default)]
    pub overlays: Vec<String>,
    /// Keep only operations matching this predicate.
    #[serde(default)]
    pub include: Option<Predicate>,
    /// Operations matching this predicate are recorded as planned.
    #[serde(default)]
    pub planned_from: Option<Predicate>,
    #[serde(default)]
    pub rpc_unflatten: Option<RpcUnflatten>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    /// Operation-object key, usually an `x-` extension.
    pub extension: String,
    pub equals: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RpcUnflatten {
    pub path: String,
    pub method: String,
    pub discriminator: String,
    pub params: String,
    #[serde(default)]
    pub name_strip_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServersConfig {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub allow_override: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    /// Path prefix owning this resource's operations.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub children: IndexMap<String, ResourceConfig>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NamingConfig {
    /// Operation ref → method name.
    #[serde(default)]
    pub operations: IndexMap<String, String>,
    #[serde(default)]
    pub fields: FieldNaming,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldNaming {
    #[serde(default)]
    pub preserve_wire_names_in_ts: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthProfile {
    #[serde(default)]
    pub composite: Option<Vec<CompositePartConfig>>,
    /// Spec security scheme names this profile satisfies together.
    #[serde(default)]
    pub satisfies: Vec<String>,
    #[serde(default)]
    pub bearer: Option<BearerConfig>,
    #[serde(default)]
    pub api_key: Option<ApiKeyConfig>,
    /// Client configuration values the profile reads.
    #[serde(default)]
    pub config: IndexMap<String, ConfigValue>,
}

/// One credential part of a composite profile: a mapping with exactly one
/// key, `cookie`, `header` or `bearer`.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum CompositePartConfig {
    Cookie { cookie: String },
    Header { header: HeaderPartConfig },
    Bearer { bearer: BearerConfig },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HeaderPartConfig {
    pub name: String,
    #[serde(default)]
    pub equals_cookie: Option<String>,
    #[serde(default)]
    pub from_config: Option<String>,
    /// `mutation` sends the header only on non-safe methods.
    #[serde(default)]
    pub when: Option<String>,
}

impl<'de> Deserialize<'de> for CompositePartConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(CompositePartVisitor)
    }
}

/// Reads a composite part as a single-key mapping so errors name the
/// offending key instead of "did not match any variant".
struct CompositePartVisitor;

const COMPOSITE_PART_KEYS: &[&str] = &["cookie", "header", "bearer"];

impl<'de> Visitor<'de> for CompositePartVisitor {
    type Value = CompositePartConfig;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a mapping with exactly one key: cookie, header or bearer")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let Some(key) = map.next_key::<String>()? else {
            return Err(de::Error::invalid_length(0, &self));
        };
        let part = match key.as_str() {
            "cookie" => CompositePartConfig::Cookie {
                cookie: map.next_value()?,
            },
            "header" => CompositePartConfig::Header {
                header: map.next_value()?,
            },
            "bearer" => CompositePartConfig::Bearer {
                bearer: map.next_value()?,
            },
            other => return Err(de::Error::unknown_field(other, COMPOSITE_PART_KEYS)),
        };
        if let Some(extra) = map.next_key::<String>()? {
            return Err(de::Error::custom(format!(
                "a composite part has exactly one key (cookie, header or bearer); found `{key}` and `{extra}`"
            )));
        }
        Ok(part)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BearerConfig {
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyConfig {
    #[serde(default)]
    pub env: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigValue {
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaginationConfig {
    #[serde(default)]
    pub cursor: Option<CursorPagination>,
    #[serde(default)]
    pub offset: Option<OffsetPagination>,
    #[serde(default)]
    pub page: Option<PagePagination>,
    #[serde(default)]
    pub link_header: Option<LinkHeaderPagination>,
    /// Disable inference for this operation.
    #[serde(default)]
    pub none: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CursorPagination {
    pub param: String,
    pub field: String,
    pub items: String,
    #[serde(default)]
    pub page_size_param: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OffsetPagination {
    pub offset_param: String,
    pub limit_param: String,
    pub items: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PagePagination {
    pub page_param: String,
    pub size_param: String,
    pub items: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkHeaderPagination {
    pub items: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypesConfig {
    /// `Type.field` edges to box explicitly when a cycle has no named type.
    #[serde(default)]
    pub break_cycles: Vec<String>,
    /// Drop the types no operation reaches (the schemas only operations
    /// excluded by `include` use, or none): they are not generated and their
    /// diagnostics are not reported; one note (TG0760) counts them. Set to
    /// `false` to generate every schema of the documents. A document
    /// without operations keeps all its types either way.
    #[serde(default = "default_prune_unreferenced")]
    pub prune_unreferenced: bool,
}

fn default_prune_unreferenced() -> bool {
    true
}

impl Default for TypesConfig {
    fn default() -> Self {
        Self {
            break_cycles: vec![],
            prune_unreferenced: true,
        }
    }
}

impl TungstenConfig {
    pub fn json_schema() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(TungstenConfig)).expect("schema serializes")
    }

    /// A default single-input manifest for `tungsten check <spec>` without
    /// a tungsten.yml.
    pub fn for_single_spec(name: &str, spec: &str) -> Self {
        Self {
            tungsten: 1,
            api: ApiConfig {
                name: name.into(),
                title: None,
                docs_url: None,
                description: None,
            },
            inputs: vec![InputConfig {
                namespace: name.into(),
                spec: spec.into(),
                overlays: vec![],
                include: None,
                planned_from: None,
                rpc_unflatten: None,
                note: None,
            }],
            overlays: vec![],
            servers: None,
            resources: IndexMap::new(),
            naming: NamingConfig::default(),
            auth_profiles: IndexMap::new(),
            pagination: IndexMap::new(),
            types: TypesConfig::default(),
            targets: IndexMap::new(),
            output: None,
            agent: None,
        }
    }
}

/// A parsed manifest with everything needed to report locations.
#[derive(Debug, Clone)]
pub struct ParsedManifest {
    /// `None` whenever `diagnostics` has errors.
    pub config: Option<TungstenConfig>,
    /// In document order; callers sort for output.
    pub diagnostics: Diagnostics,
    pub source: ManifestSource,
}

/// Parse a manifest from text. `name` is used in diagnostics.
pub fn parse_str(name: &str, text: &str) -> (Option<TungstenConfig>, Diagnostics) {
    let parsed = parse_with_source(name, text);
    (parsed.config, parsed.diagnostics)
}

/// Read and parse a manifest file.
pub fn load(path: &Path) -> (Option<TungstenConfig>, Diagnostics) {
    let parsed = load_with_source(path);
    (parsed.config, parsed.diagnostics)
}

/// Read and parse a manifest file, keeping its text and node positions.
/// The manifest is named in diagnostics by `path` as given.
pub fn load_with_source(path: &Path) -> ParsedManifest {
    let name = path.display().to_string();
    match std::fs::read_to_string(path) {
        Ok(text) => parse_with_source(&name, &text),
        Err(err) => {
            let diagnostic = Diagnostic::error("TG0101", format!("cannot read {name}: {err}"))
                .at(&name, "", None);
            ParsedManifest::failed(
                diagnostic,
                ManifestSource::new(&name, "", Default::default()),
            )
        }
    }
}

/// Parse a manifest from text, keeping its text and node positions.
pub fn parse_with_source(name: &str, text: &str) -> ParsedManifest {
    let (value, source) = parse_yaml(name, text);
    let value = match value {
        Ok(value) => value,
        Err(diagnostic) => return ParsedManifest::failed(diagnostic, source),
    };
    if value.is_null() {
        let message = "the manifest is empty; it needs at least `tungsten`, `api` and `inputs`";
        let diagnostic = Diagnostic::error("TG0602", message).at(name, "", None);
        return ParsedManifest::failed(diagnostic, source);
    }
    let config = match deserialize_manifest::<TungstenConfig>(&value, name, &source) {
        Ok(config) => config,
        Err(diagnostic) => return ParsedManifest::failed(diagnostic, source),
    };
    let diagnostics = validate(&config, name, Some(&source));
    let config = (!diagnostics.has_errors()).then_some(config);
    ParsedManifest {
        config,
        diagnostics,
        source,
    }
}

/// Stage 1 for any YAML manifest: convert `text` to JSON with node
/// positions (see the module documentation for the accepted YAML). A YAML
/// problem is a `TG0601` error naming `name`, the pointer of the node being
/// read and its line and column. The source is returned either way, with
/// the positions known so far.
pub fn parse_yaml(
    name: &str,
    text: &str,
) -> (Result<serde_json::Value, Diagnostic>, ManifestSource) {
    match yaml::to_json(text) {
        Ok(converted) => (
            Ok(converted.value),
            ManifestSource::new(name, text, converted.positions),
        ),
        Err(err) => {
            let source = ManifestSource::new(name, text, err.positions);
            let (line, col) = source.line_col_at(err.offset);
            let message = format!("{} (line {line}, column {col})", err.message);
            let diagnostic = Diagnostic::error("TG0601", message).at(name, err.pointer, None);
            (Err(diagnostic), source)
        }
    }
}

/// Stage 2 for any manifest type: deserialize `value` strictly (mappings
/// for structs and maps, sequences for sequences). A mismatch is a
/// `TG0602` error with the pointer of the offending node and its line and
/// column in `source`.
pub fn deserialize_manifest<T: DeserializeOwned>(
    value: &serde_json::Value,
    name: &str,
    source: &ManifestSource,
) -> Result<T, Diagnostic> {
    deserialize_value(value).map_err(|(at, message)| {
        let place = if at.is_empty() {
            "the document root"
        } else {
            &at
        };
        let location = source
            .line_col(&at)
            .map(|(line, col)| format!(" (line {line}, column {col})"))
            .unwrap_or_default();
        let message = format!("{message} at {place}{location}");
        Diagnostic::error("TG0602", message).at(name, &at, None)
    })
}

/// Deserialize `value` strictly, as [`deserialize_manifest`] does; a
/// mismatch is the JSON Pointer of the offending node (relative to
/// `value`) and what is wrong with it.
pub fn deserialize_value<T: DeserializeOwned>(
    value: &serde_json::Value,
) -> Result<T, (String, String)> {
    serde_path_to_error::deserialize::<_, T>(strict::Strict(value))
        .map_err(|err| (error_pointer(err.path()), err.inner().to_string()))
}

impl ParsedManifest {
    fn failed(diagnostic: Diagnostic, source: ManifestSource) -> Self {
        let mut diagnostics = Diagnostics::new();
        diagnostics.push(diagnostic);
        Self {
            config: None,
            diagnostics,
            source,
        }
    }
}

/// The JSON Pointer of a deserialization error path. Segments the path
/// tracker could not name end the pointer at their parent.
fn error_pointer(path: &serde_path_to_error::Path) -> String {
    use serde_path_to_error::Segment;
    let mut out = String::new();
    for segment in path.iter() {
        out = match segment {
            Segment::Map { key } => pointer::child(&out, key),
            Segment::Enum { variant } => pointer::child(&out, variant),
            Segment::Seq { index } => pointer::child(&out, &index.to_string()),
            Segment::Unknown => break,
        };
    }
    out
}
