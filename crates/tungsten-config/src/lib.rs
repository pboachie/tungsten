// SPDX-License-Identifier: AGPL-3.0-only
//! The `tungsten.yml` shape manifest (planning/04).
//!
//! The structs here are the contract the builder consumes. The loader turns
//! YAML into these structs and reports TG0601/TG0602 with JSON Pointers and
//! line numbers. `TungstenConfig::json_schema()` is published as
//! `specs/tungsten.schema.json`.
//!
//! PHASE-1 STUB loader: the config work package replaces `load`/`parse_str`
//! internals (span-aware YAML conversion, schema validation with pointers,
//! unknown-key errors) without changing the signatures.

mod yaml;

use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tungsten_core::{Diagnostic, Diagnostics};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TungstenConfig {
    /// Manifest format version. Must be 1.
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// Machine name, lowercase (`zrotext`).
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypesConfig {
    /// `Type.field` edges to box explicitly when a cycle has no named type.
    #[serde(default)]
    pub break_cycles: Vec<String>,
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
        }
    }
}

/// Parse a manifest from text. `name` is used in diagnostics.
pub fn parse_str(name: &str, text: &str) -> (Option<TungstenConfig>, Diagnostics) {
    let mut diags = Diagnostics::new();
    let value = match yaml::to_json(text) {
        Ok(v) => v,
        Err(msg) => {
            diags.push(Diagnostic::error("TG0601", msg).at(name, "", None));
            return (None, diags);
        }
    };
    match serde_json::from_value::<TungstenConfig>(value) {
        Ok(cfg) => {
            if cfg.tungsten != 1 {
                diags.push(
                    Diagnostic::error(
                        "TG0602",
                        format!("unsupported manifest version {}", cfg.tungsten),
                    )
                    .at(name, "/tungsten", None),
                );
                return (None, diags);
            }
            (Some(cfg), diags)
        }
        Err(err) => {
            diags.push(Diagnostic::error("TG0602", err.to_string()).at(name, "", None));
            (None, diags)
        }
    }
}

/// Read and parse a manifest file.
pub fn load(path: &Path) -> (Option<TungstenConfig>, Diagnostics) {
    let name = path.display().to_string();
    match std::fs::read_to_string(path) {
        Ok(text) => parse_str(&name, &text),
        Err(err) => {
            let mut d = Diagnostics::new();
            d.push(
                Diagnostic::error("TG0101", format!("cannot read {name}: {err}"))
                    .at(name, "", None),
            );
            (None, d)
        }
    }
}
