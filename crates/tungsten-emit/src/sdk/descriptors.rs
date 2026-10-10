// SPDX-License-Identifier: AGPL-3.0-only
//! The SDK descriptor document v1: everything a runtime needs to call an
//! API's operations and run its macros, as one JSON document per API
//! (`tungsten-descriptors.json` in a generated SDK, or a string constant
//! where a language cannot load resources). `tungsten schema
//! sdk-descriptors` prints its JSON Schema (`specs/sdk-descriptors.schema.json`).
//!
//! # Document
//!
//! `{format: "tungsten-sdk-descriptors/1", target, api, operations, macros,
//! defs}`:
//!
//! - `api` ([`ApiDescriptor`]), `operations` ([`OperationDescriptor`] by
//!   operation id) and `macros` ([`MacroDescriptor`] by name) carry the
//!   fields of the TypeScript runtime's `ApiDescriptor`,
//!   `OperationDescriptor` and `MacroDescriptor` (`runtimes/ts/src/types.ts`,
//!   the contract), with snake_case member names. Where the TypeScript SDK
//!   names an argument (a parameter's `name`, a merged body field, a
//!   pagination parameter, a step argument key, a sensitive request path)
//!   the document has the arguments layout key ([`crate::args`]); parameters
//!   and merged fields also carry `name`, the target's spelling (from the
//!   [`super::NameMap`] the emitter passes), and `wire`.
//! - Schemas (`request`, `response`, `page_item`, a stream's `event`) are in
//!   the runtime schema form ([`RuntimeSchema`]); named types are `{"kind":
//!   "ref", "ref": "<type id>"}` and their schemas are in `defs`, keyed by
//!   type id. `request` validates the arguments object as the target names
//!   it (its fields' `name`); `response` validates a success body when one
//!   arrives (several JSON bodies are an untagged `union`).
//! - Member order is fixed by the types below and maps are sorted, so the
//!   same IR, names and options always give the same bytes.
//!
//! # Validation rules (every runtime)
//!
//! A runtime checks a JSON value against a schema as follows; the issues of
//! one check are reported structure first, then constraint violations, then
//! unknown members.
//!
//! - Types are strict: no coercion between strings, numbers and booleans.
//!   A JSON number without a fractional part and below 2^53 in magnitude is
//!   an integer, in arguments and in decoded bodies alike (`5` and `5.0`
//!   are both the integer 5); numbers must be finite. `bits: 32` limits an
//!   integer to the signed 32-bit range.
//! - `string`: `min_length`/`max_length` count UTF-16 code units (as the
//!   TypeScript runtime does); `pattern` is an ECMAScript regular
//!   expression searched anywhere in the string (a runtime whose engine
//!   cannot compile it skips the check); `format` checks `uuid`, `email`,
//!   `date-time`, `date`, `ipv4` and `ipv6` with Zod's expressions (the Go
//!   runtime's `formatPatterns`), anchored; other formats are not checked.
//! - Numbers: `minimum`, `maximum`, `exclusive_minimum`,
//!   `exclusive_maximum` inclusive and exclusive bounds; `multiple_of` with
//!   a relative tolerance of 1e-9.
//! - `enum`/`const`: JSON equality (object members compared regardless of
//!   order, numbers by value).
//! - `array`: every item, then `min_items`, `max_items`, `unique`.
//! - `map`: every member value against `items`.
//! - `object`: each field in order: a missing `required` field is an issue
//!   at its path, a `null` value is accepted when `nullable`, any other
//!   value is checked; then each member not listed: refused with `closed`
//!   (an issue at the member's own path, after every other issue), checked
//!   against `extra` with `schema`, ignored with `open`.
//! - `union`: with `tag`, the value must be an object whose `tag` member is
//!   a string; the variant whose `tags` entry equals it is checked (an
//!   unknown tag is an issue at the tag's path); without `tag`, variants are
//!   tried left to right and the first that accepts the value wins.
//! - `all`: every variant. `nullable`: `null`, else `inner`. `ref`: the
//!   `defs` entry. `limits`: the constraints applied to the value's own
//!   kind. `any` accepts everything, `never` nothing. `bytes` accepts the
//!   runtime's byte type, or a string in JSON.
//!
//! The `expected` texts of the envelope are each runtime's own; the contract
//! suite compares them for presence only.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tungsten_ir::naming::{self, Role, Target};
use tungsten_ir::{
    ApiKeyIn, AuthScheme, BodyEncoding, CompositePart, HttpMethod, IdempotencyKind, Ident, Jitter,
    OperationAgentMeta, OperationStatus, PaginationStyle, ParamRole, ParamStyle, PreviewMode,
    Remediation, ResponseKind, RetryPolicy, Retryable, Safety, Shape, StatusMatch, TypeId, TypeRef,
};

use super::macros::{InputSource, MacroPlan};
use super::namer::NameMap;
use super::plan::{ArgSource, OpPlan, SdkPlan};
use super::schema_form::{AdditionalMembers, RuntimeSchema, SchemaFormBuilder, SchemaKind};
use crate::args::{self, BodyArg};

/// The `format` of a v1 document.
pub const FORMAT: &str = "tungsten-sdk-descriptors/1";

const SCHEMA_ID: &str = "https://tungsten.dev/schemas/sdk-descriptors-v1.json";

/// The error categories of the runtime contract (`Category`).
pub const CATEGORIES: &[&str] = &[
    "VALIDATION_FAILED",
    "MALFORMED_REQUEST",
    "REQUEST_TOO_LARGE",
    "AUTH_FAILED",
    "NOT_FOUND",
    "CONFLICT",
    "PRECONDITION_FAILED",
    "RATE_LIMITED",
    "UPSTREAM_UNAVAILABLE",
    "OUTCOME_UNKNOWN",
    "TRANSPORT_FAILED",
    "CONFIRMATION_REQUIRED",
    "GATE_DISABLED",
    "UNEXPECTED_RESPONSE",
];

/// What the document depends on besides the plan and the names.
#[derive(Debug, Clone)]
pub struct DocumentOptions {
    /// The SDK package version (`api.version`).
    pub version: String,
}

/// The SDK descriptor document v1.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SdkDescriptors {
    /// Always `tungsten-sdk-descriptors/1`.
    pub format: String,
    /// The target the names are spelled for (`java`, `csharp`, ...).
    pub target: String,
    pub api: ApiDescriptor,
    /// Every callable operation by id.
    pub operations: BTreeMap<String, OperationDescriptor>,
    /// Every macro in the canonical form by name.
    pub macros: BTreeMap<String, MacroDescriptor>,
    /// The schema of every named type the document's schemas reference, by
    /// type id.
    pub defs: BTreeMap<String, RuntimeSchema>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ApiDescriptor {
    /// Machine name (`zrotext`).
    pub name: String,
    /// SDK package version.
    pub version: String,
    pub tungsten_version: String,
    pub servers: Vec<String>,
    pub auth: Vec<AuthSchemeDescriptor>,
    /// Global remediation by API error code.
    pub error_codes: BTreeMap<String, RemediationEntry>,
    /// Statuses that make a mutation's outcome unknown.
    pub ambiguous_statuses: Vec<u16>,
    pub non_json: Vec<NonJsonDescriptor>,
    /// Runtime gate environment variable to its explanation.
    pub gates: BTreeMap<String, String>,
    pub retries: RetriesDescriptor,
}

/// An auth scheme. OpenID Connect schemes are `http_bearer`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthSchemeDescriptor {
    ApiKey {
        name: String,
        #[serde(rename = "in")]
        location: String,
        wire: String,
    },
    HttpBearer {
        name: String,
        /// The token must start with it; checked before sending.
        prefix: Option<String>,
    },
    HttpBasic {
        name: String,
    },
    Oauth2 {
        name: String,
        token_url: Option<String>,
        scopes: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization_code: Option<AuthorizationCodeDescriptor>,
    },
    Composite {
        name: String,
        satisfies: Vec<String>,
        parts: Vec<CompositePartDescriptor>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct AuthorizationCodeDescriptor {
    pub authorization_url: Option<String>,
    pub token_url: Option<String>,
    /// Falls back to `token_url` when null.
    pub refresh_url: Option<String>,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompositePartDescriptor {
    Cookie {
        name: String,
    },
    Header {
        name: String,
        equals_cookie: Option<String>,
        from_config: Option<String>,
        mutation_only: bool,
    },
    Bearer {
        prefix: Option<String>,
    },
}

/// A remediation entry; absent members are omitted.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RemediationEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct NonJsonDescriptor {
    pub status: u16,
    /// A media type, or `none` for a bare status.
    pub media: String,
    pub category: String,
    pub retryable: String,
    pub text: Option<String>,
}

/// Retry defaults by tier: `read_only` for read-only operations, `mutating`
/// for the others.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RetriesDescriptor {
    pub read_only: RetryDescriptor,
    pub mutating: RetryDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RetryDescriptor {
    /// Attempts after the first.
    pub max: u32,
    pub base_ms: u64,
    pub max_ms: u64,
    /// `none`, `full` or `equal`.
    pub jitter: String,
    pub honor_retry_after: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct OperationDescriptor {
    pub id: String,
    pub method: String,
    /// Path template as in the spec; placeholders are wire names.
    pub path: String,
    /// Arguments first, then the supplied parameters (layout order).
    pub params: Vec<ParamDescriptor>,
    pub body: Option<BodyDescriptor>,
    pub responses: Vec<ResponseDescriptor>,
    /// OR of AND-sets of auth scheme names.
    pub security: Vec<Vec<String>>,
    pub pagination: Option<PaginationDescriptor>,
    pub rpc: Option<RpcDescriptor>,
    /// Field path of the API error code in error bodies.
    pub error_code_field: Option<String>,
    pub status: OperationStatusDescriptor,
    pub agent: AgentDescriptor,
    /// Validates the arguments object before any network call.
    pub request: RuntimeSchema,
    /// Validates a success body, when every success body is JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<RuntimeSchema>,
    /// Validates one item of a page, when the items are typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_item: Option<RuntimeSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<StreamDescriptor>,
    /// One-line summary.
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ParamDescriptor {
    /// Key in the arguments object (the arguments layout).
    pub key: String,
    /// The target's argument name; informational for supplied parameters.
    pub name: String,
    /// Name on the wire.
    pub wire: String,
    #[serde(rename = "in")]
    pub location: String,
    pub required: bool,
    pub style: String,
    pub explode: bool,
    /// `plain`, `dry_run`, or a supplied role: `idempotency_key`, `origin`,
    /// `auth`, `constant`.
    pub role: String,
    /// The header value of a `constant` parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct BodyDescriptor {
    pub media_type: String,
    /// `json`, `form`, `multipart`, `bytes` or `text`.
    pub encoding: String,
    pub required: bool,
    pub shape: BodyShapeDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BodyShapeDescriptor {
    /// A JSON object whose fields are keys of the arguments object.
    Merged { fields: Vec<ArgName> },
    /// The whole body is the argument `arg` (its key), named `name`.
    Arg { arg: String, name: String },
}

/// An argument's key, target name and wire name.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ArgName {
    pub key: String,
    pub name: String,
    pub wire: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ResponseDescriptor {
    /// An exact status, `1XX` to `5XX`, or `default`.
    pub status: StatusDescriptor,
    /// `success`, `error` or `ambiguous`.
    pub kind: String,
    /// First media type, or null for a bare status.
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum StatusDescriptor {
    Exact(u16),
    Text(String),
}

/// How an operation pages; parameters are arguments keys.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum PaginationDescriptor {
    Cursor {
        request_param: String,
        response_field: String,
        items_field: String,
        page_size_param: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_more_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor_item_field: Option<String>,
    },
    Offset {
        offset_param: String,
        limit_param: String,
        items_field: String,
    },
    Page {
        page_param: String,
        size_param: String,
        items_field: String,
    },
    LinkHeader {
        items_field: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RpcDescriptor {
    pub field: String,
    pub value: String,
    pub params_field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constants: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationStatusDescriptor {
    Implemented,
    Gated {
        env_var: String,
        disabled_status: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct AgentDescriptor {
    pub safety: String,
    pub idempotency: IdempotencyDescriptor,
    pub preview: PreviewDescriptor,
    pub confirmation: Option<ConfirmationDescriptor>,
    pub verify: Option<VerifyDescriptor>,
    /// Operation-specific remediation by API error code.
    pub remediation: BTreeMap<String, RemediationEntry>,
    pub remediation_note: Option<String>,
    pub sensitive_response_fields: Vec<String>,
    /// Arguments marked sensitive, as dotted paths of arguments keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensitive_request_fields: Vec<String>,
    pub shown_once: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct IdempotencyDescriptor {
    pub policy: String,
    pub header: Option<String>,
    pub format: Option<String>,
    pub persist_required: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PreviewDescriptor {
    Local,
    Header { header: String, value: String },
    Endpoint { operation: String },
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ConfirmationDescriptor {
    pub summary_fields: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct VerifyDescriptor {
    pub operation: String,
    pub args: Value,
    pub expect: Value,
    pub terminal: Value,
    pub poll_interval_ms: Option<u64>,
    pub poll_budget_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct StreamDescriptor {
    /// Validates every event's decoded `data`.
    pub event: RuntimeSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MacroDescriptor {
    pub name: String,
    pub summary: String,
    pub safety: String,
    pub steps: Vec<MacroStepDescriptor>,
    pub output: Value,
    pub input: MacroInputDescriptor,
    pub sensitive_response_fields: Vec<String>,
    pub shown_once: bool,
    pub cluster: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MacroStepDescriptor {
    /// `call`, `poll` or `paginate`.
    pub kind: String,
    pub operation: String,
    pub args: Value,
    #[serde(rename = "as")]
    pub as_name: Option<String>,
    pub until: Option<Map<String, Value>>,
    pub interval_ms: Option<u64>,
    pub budget_ms: Value,
    pub max_pages: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MacroInputDescriptor {
    pub extends: Option<String>,
    /// Added fields as JSON Schemas; a `default` is applied when the input
    /// omits the field.
    pub add: Map<String, Value>,
    /// Every input field: its key, the target's name and whether it must
    /// be given.
    pub fields: Vec<MacroInputFieldDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MacroInputFieldDescriptor {
    pub key: String,
    pub name: String,
    pub required: bool,
}

/// The JSON Schema of the document.
pub fn json_schema() -> Value {
    let generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let mut schema = generator
        .into_root_schema_for::<SdkDescriptors>()
        .to_value();
    if let Some(obj) = schema.as_object_mut() {
        obj.insert("$id".into(), json!(SCHEMA_ID));
        obj.insert("title".into(), json!("tungsten SDK descriptor document v1"));
    }
    schema
}

/// The lowercase name of a target (`typescript`, `csharp`, ...).
pub fn target_name(target: Target) -> &'static str {
    match target {
        Target::TypeScript => "typescript",
        Target::Python => "python",
        Target::Rust => "rust",
        Target::Java => "java",
        Target::CSharp => "csharp",
        Target::Kotlin => "kotlin",
        Target::Swift => "swift",
        Target::Php => "php",
        Target::Ruby => "ruby",
        Target::Dart => "dart",
    }
}

/// The document of a plan, its names for one target and the options.
pub fn document(plan: &SdkPlan<'_>, names: &NameMap, options: &DocumentOptions) -> SdkDescriptors {
    let mut schemas = SchemaFormBuilder::new(plan.ir);
    let mut operations = BTreeMap::new();
    for (i, op) in plan.operations.iter().enumerate() {
        let d = operation(plan, names, i, op, &mut schemas);
        operations.insert(op.op.id.0.clone(), d);
    }
    let macros = plan
        .macros
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name().to_string(), macro_descriptor(plan, names, i, m)))
        .collect();
    SdkDescriptors {
        format: FORMAT.to_string(),
        target: target_name(names.target).to_string(),
        api: api(plan, options),
        operations,
        macros,
        defs: schemas.finish(),
    }
}

/// [`document`] as JSON.
pub fn document_value(plan: &SdkPlan<'_>, names: &NameMap, options: &DocumentOptions) -> Value {
    serde_json::to_value(document(plan, names, options)).unwrap_or(Value::Null)
}

// ------------------------------------------------------------------ api

fn api(plan: &SdkPlan<'_>, options: &DocumentOptions) -> ApiDescriptor {
    let ir = plan.ir;
    let retries = &ir.agent.retries;
    ApiDescriptor {
        name: ir.api.name.wire.clone(),
        version: options.version.clone(),
        tungsten_version: ir.generator.tungsten_version.clone(),
        servers: ir.api.servers.iter().map(|s| s.url.clone()).collect(),
        auth: ir.auth.iter().map(auth_scheme).collect(),
        error_codes: ir
            .agent
            .error_codes
            .iter()
            .map(|(code, r)| (code.clone(), remediation(r)))
            .collect(),
        ambiguous_statuses: ir.agent.ambiguous_statuses.clone(),
        non_json: ir
            .agent
            .non_json
            .iter()
            .filter(|e| CATEGORIES.contains(&e.category.as_str()))
            .map(|e| NonJsonDescriptor {
                status: e.status,
                media: e.media.clone(),
                category: e.category.clone(),
                retryable: retryable_str(e.retryable).to_string(),
                text: e.text.clone(),
            })
            .collect(),
        gates: ir.agent.gates.clone(),
        retries: RetriesDescriptor {
            read_only: retry(&retries.read_only, retries.honor_retry_after),
            mutating: retry(&retries.mutating, retries.honor_retry_after),
        },
    }
}

fn retry(policy: &RetryPolicy, honor_retry_after: bool) -> RetryDescriptor {
    RetryDescriptor {
        max: policy.max,
        base_ms: policy.base_ms,
        max_ms: policy.max_ms,
        jitter: match policy.jitter {
            Jitter::None => "none",
            Jitter::Full => "full",
            Jitter::Equal => "equal",
        }
        .to_string(),
        honor_retry_after,
    }
}

fn auth_scheme(s: &AuthScheme) -> AuthSchemeDescriptor {
    match s {
        AuthScheme::ApiKey {
            name,
            location,
            wire_name,
            ..
        } => AuthSchemeDescriptor::ApiKey {
            name: name.clone(),
            location: match location {
                ApiKeyIn::Header => "header",
                ApiKeyIn::Query => "query",
                ApiKeyIn::Cookie => "cookie",
            }
            .to_string(),
            wire: wire_name.clone(),
        },
        AuthScheme::HttpBearer { name, prefix, .. } => AuthSchemeDescriptor::HttpBearer {
            name: name.clone(),
            prefix: prefix.clone(),
        },
        AuthScheme::OpenIdConnect { name, .. } => AuthSchemeDescriptor::HttpBearer {
            name: name.clone(),
            prefix: None,
        },
        AuthScheme::HttpBasic { name, .. } => {
            AuthSchemeDescriptor::HttpBasic { name: name.clone() }
        }
        AuthScheme::OAuth2 { name, flows, .. } => {
            let token_url = flows
                .iter()
                .find(|f| f.kind == "clientCredentials" && f.token_url.is_some())
                .or_else(|| flows.iter().find(|f| f.token_url.is_some()))
                .and_then(|f| f.token_url.clone());
            let scopes: std::collections::BTreeSet<String> = flows
                .iter()
                .flat_map(|f| f.scopes.keys().cloned())
                .collect();
            AuthSchemeDescriptor::Oauth2 {
                name: name.clone(),
                token_url,
                scopes: scopes.into_iter().collect(),
                authorization_code: s.authorization_code().map(|flow| {
                    AuthorizationCodeDescriptor {
                        authorization_url: flow.authorization_url.clone(),
                        token_url: flow.token_url.clone(),
                        refresh_url: flow.refresh_url.clone(),
                        scopes: flow.scopes.keys().cloned().collect(),
                    }
                }),
            }
        }
        AuthScheme::Composite {
            name,
            satisfies,
            parts,
        } => AuthSchemeDescriptor::Composite {
            name: name.clone(),
            satisfies: satisfies.clone(),
            parts: parts
                .iter()
                .map(|p| match p {
                    CompositePart::Cookie { name } => {
                        CompositePartDescriptor::Cookie { name: name.clone() }
                    }
                    CompositePart::Header {
                        name,
                        equals_cookie,
                        from_config,
                        mutation_only,
                    } => CompositePartDescriptor::Header {
                        name: name.clone(),
                        equals_cookie: equals_cookie.clone(),
                        from_config: from_config.clone(),
                        mutation_only: *mutation_only,
                    },
                    CompositePart::Bearer { prefix, .. } => CompositePartDescriptor::Bearer {
                        prefix: prefix.clone(),
                    },
                })
                .collect(),
        },
    }
}

/// A remediation entry; a category outside the closed set is dropped.
fn remediation(r: &Remediation) -> RemediationEntry {
    RemediationEntry {
        category: r
            .category
            .clone()
            .filter(|c| CATEGORIES.contains(&c.as_str())),
        text: r.text.clone(),
        retryable: r.retryable.map(|x| retryable_str(x).to_string()),
        next_action: r.next_action.clone(),
    }
}

/// The wire spelling of an HTTP method.
pub fn method_str(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Put => "PUT",
        HttpMethod::Post => "POST",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Head => "HEAD",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Trace => "TRACE",
    }
}

/// The contract spelling of a safety tier.
pub fn safety_str(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "read_only",
        Safety::Mutating => "mutating",
        Safety::Destructive => "destructive",
        Safety::Irreversible => "irreversible",
    }
}

fn retryable_str(r: Retryable) -> &'static str {
    match r {
        Retryable::Never => "never",
        Retryable::AfterDelay => "after_delay",
        Retryable::SameKeyOnly => "same_key_only",
        Retryable::AfterRemediation => "after_remediation",
    }
}

/// The contract spelling of an idempotency policy.
pub fn idempotency_str(k: IdempotencyKind) -> &'static str {
    match k {
        IdempotencyKind::None => "none",
        IdempotencyKind::Auto => "auto",
        IdempotencyKind::CallerOwned => "caller_owned",
        IdempotencyKind::ContentHash => "content_hash",
        IdempotencyKind::ContentIdentity => "content_identity",
    }
}

fn style_str(s: ParamStyle) -> &'static str {
    match s {
        ParamStyle::Simple => "simple",
        ParamStyle::Form => "form",
        ParamStyle::Label => "label",
        ParamStyle::Matrix => "matrix",
        ParamStyle::SpaceDelimited => "space_delimited",
        ParamStyle::PipeDelimited => "pipe_delimited",
        ParamStyle::DeepObject => "deep_object",
    }
}

fn role_str(r: ParamRole) -> &'static str {
    match r {
        ParamRole::Plain => "plain",
        ParamRole::IdempotencyKey => "idempotency_key",
        ParamRole::DryRun => "dry_run",
        ParamRole::Origin => "origin",
        ParamRole::Auth => "auth",
        ParamRole::Constant => "constant",
    }
}

fn encoding_str(e: BodyEncoding) -> &'static str {
    match e {
        BodyEncoding::Json => "json",
        BodyEncoding::Form => "form",
        BodyEncoding::Multipart => "multipart",
        // JSON Lines is a response encoding: a request body of that media
        // type is bytes.
        BodyEncoding::Bytes | BodyEncoding::Jsonl => "bytes",
        BodyEncoding::Text => "text",
    }
}

fn status(s: StatusMatch) -> Option<StatusDescriptor> {
    match s {
        StatusMatch::Exact(n) => Some(StatusDescriptor::Exact(n)),
        StatusMatch::Range(d @ 1..=5) => Some(StatusDescriptor::Text(format!("{d}XX"))),
        StatusMatch::Range(_) => None,
        StatusMatch::Default => Some(StatusDescriptor::Text("default".into())),
    }
}

// ----------------------------------------------------------- operations

fn operation(
    plan: &SdkPlan<'_>,
    names: &NameMap,
    index: usize,
    p: &OpPlan<'_>,
    schemas: &mut SchemaFormBuilder<'_>,
) -> OperationDescriptor {
    let op = p.op;
    let arg_names = names
        .operations
        .get(index)
        .map(|o| o.arguments.as_slice())
        .unwrap_or(&[]);
    let name_of = |key: &str| -> String {
        p.arguments
            .iter()
            .position(|a| a.key == key)
            .and_then(|i| arg_names.get(i).cloned())
            .unwrap_or_else(|| key.to_string())
    };
    let mut params: Vec<ParamDescriptor> = vec![];
    for a in &p.layout.params {
        params.push(param(a.key.clone(), name_of(&a.key), a.location, a.param));
    }
    for s in &p.supplied {
        let name = naming::render(
            &Ident {
                wire: s.param.wire_name.clone(),
                words: s.param.name.words.clone(),
            },
            names.target,
            Role::Param,
        );
        params.push(param(s.key.clone(), name, s.location, s.param));
    }
    let body = p.layout.body.as_ref().map(|b| {
        let (content, shape) = match b {
            BodyArg::Merged {
                content, fields, ..
            } => (
                *content,
                BodyShapeDescriptor::Merged {
                    fields: fields
                        .iter()
                        .map(|f| ArgName {
                            key: f.wire_name.clone(),
                            name: name_of(&f.wire_name),
                            wire: f.wire_name.clone(),
                        })
                        .collect(),
                },
            ),
            BodyArg::Arg { key, content } => (
                *content,
                BodyShapeDescriptor::Arg {
                    arg: key.clone(),
                    name: name_of(key),
                },
            ),
        };
        BodyDescriptor {
            media_type: content.media_type.clone(),
            encoding: encoding_str(content.encoding).to_string(),
            required: p.layout.body_required,
            shape,
        }
    });
    let responses = op
        .responses
        .iter()
        .filter_map(|r| {
            Some(ResponseDescriptor {
                status: status(r.status)?,
                kind: match r.kind {
                    ResponseKind::Success => "success",
                    ResponseKind::Error => "error",
                    ResponseKind::Ambiguous => "ambiguous",
                }
                .to_string(),
                media_type: r.content.first().map(|c| c.media_type.clone()),
            })
        })
        .collect();
    let security = op
        .security
        .iter()
        .map(|req| req.all_of.iter().map(|s| s.scheme.clone()).collect())
        .collect();
    let rpc = op.rpc.as_ref().map(|r| RpcDescriptor {
        field: r.discriminator_field.clone(),
        value: r.discriminator_value.clone(),
        params_field: r.params_field.clone(),
        constants: (!r.constants.is_empty()).then(|| {
            r.constants
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }),
    });
    let status = match &op.status {
        OperationStatus::Gated { gate } => OperationStatusDescriptor::Gated {
            env_var: gate.env_var.clone(),
            disabled_status: gate.disabled_status,
        },
        OperationStatus::Implemented | OperationStatus::Planned { .. } => {
            OperationStatusDescriptor::Implemented
        }
    };
    let summary = op_summary(op);
    OperationDescriptor {
        id: op.id.0.clone(),
        method: method_str(op.method).to_string(),
        path: op.path.raw.clone(),
        params,
        body,
        responses,
        security,
        pagination: pagination(p),
        rpc,
        error_code_field: plan
            .ir
            .namespaces
            .iter()
            .find(|n| n.name.wire == p.namespace)
            .and_then(|n| n.errors.code_field.clone()),
        status,
        agent: agent(&op.agent, sensitive_request_fields(plan, p)),
        request: request_schema(p, arg_names, schemas),
        response: response_schema(p, schemas),
        page_item: p
            .page
            .as_ref()
            .and_then(|pg| pg.item.as_ref())
            .map(|t| schemas.type_ref(t)),
        stream: p.stream.as_ref().map(|s| StreamDescriptor {
            event: schemas.type_ref(&s.event),
            done: s.done.clone(),
            flag: s.flag.clone(),
        }),
        summary: (!summary.is_empty()).then_some(summary),
    }
}

fn param(
    key: String,
    name: String,
    location: args::ParamLocation,
    p: &tungsten_ir::Param,
) -> ParamDescriptor {
    ParamDescriptor {
        key,
        name,
        wire: p.wire_name.clone(),
        location: location.as_str().to_string(),
        required: p.required,
        style: style_str(p.style).to_string(),
        explode: p.explode,
        role: role_str(p.role).to_string(),
        constant: args::constant_text(p),
    }
}

/// `summary`: the pruned agent doc (`compact_doc`), else the spec summary
/// (else the first line of the description), collapsed to one line.
pub fn op_summary(op: &tungsten_ir::Operation) -> String {
    let text = if op.agent.compact_doc.trim().is_empty() {
        match op.doc.as_ref() {
            None => String::new(),
            Some(doc) => match doc.summary.as_deref().map(str::trim) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => doc
                    .description
                    .as_deref()
                    .and_then(|d| d.trim().lines().next())
                    .unwrap_or("")
                    .to_string(),
            },
        }
    } else {
        op.agent.compact_doc.clone()
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pagination(p: &OpPlan<'_>) -> Option<PaginationDescriptor> {
    let pg = p.op.pagination.as_ref()?;
    // Request parameters are named by their arguments key.
    let arg = |wire: &str| -> String {
        p.layout
            .params
            .iter()
            .chain(&p.layout.supplied)
            .find(|a| a.param.wire_name == wire && a.location == args::ParamLocation::Query)
            .map_or_else(|| wire.to_string(), |a| a.key.clone())
    };
    let items_field = pg.items_field.clone();
    Some(match &pg.style {
        PaginationStyle::Cursor {
            request_param,
            response_field,
        } => PaginationDescriptor::Cursor {
            request_param: arg(request_param),
            response_field: response_field.clone(),
            items_field,
            page_size_param: pg.page_size_param.as_deref().map(arg),
            has_more_field: pg.has_more_field.clone(),
            cursor_item_field: pg.cursor_item_field.clone(),
        },
        PaginationStyle::Offset {
            offset_param,
            limit_param,
        } => PaginationDescriptor::Offset {
            offset_param: arg(offset_param),
            limit_param: arg(limit_param),
            items_field,
        },
        PaginationStyle::Page {
            page_param,
            size_param,
        } => PaginationDescriptor::Page {
            page_param: arg(page_param),
            size_param: arg(size_param),
            items_field,
        },
        PaginationStyle::LinkHeader => PaginationDescriptor::LinkHeader { items_field },
    })
}

fn agent(a: &OperationAgentMeta, sensitive_request: Vec<String>) -> AgentDescriptor {
    let idem = &a.idempotency;
    AgentDescriptor {
        safety: safety_str(a.safety).to_string(),
        idempotency: IdempotencyDescriptor {
            policy: idempotency_str(idem.policy).to_string(),
            header: idem.header.clone(),
            format: idem.format.clone(),
            persist_required: idem.persist_required,
            note: idem.note.clone(),
        },
        preview: match &a.preview {
            PreviewMode::Local => PreviewDescriptor::Local,
            PreviewMode::Header { header, value } => PreviewDescriptor::Header {
                header: header.clone(),
                value: value.clone(),
            },
            PreviewMode::Endpoint { operation } => PreviewDescriptor::Endpoint {
                operation: operation.0.clone(),
            },
            PreviewMode::None => PreviewDescriptor::None,
        },
        confirmation: a.confirmation.as_ref().map(|c| ConfirmationDescriptor {
            summary_fields: c.summary_fields.clone(),
            message: c.message.clone(),
        }),
        verify: a.verify.as_ref().map(|v| VerifyDescriptor {
            operation: v.operation.0.clone(),
            args: json_object(&v.args),
            expect: json_object(&v.expect),
            terminal: json_object(&v.terminal),
            poll_interval_ms: v.poll_interval_ms,
            poll_budget_ms: v.poll_budget_ms,
        }),
        remediation: a
            .remediation
            .iter()
            .map(|(code, r)| (code.clone(), remediation(r)))
            .collect(),
        remediation_note: a.remediation_note.clone(),
        sensitive_response_fields: a.sensitive_response_fields.clone(),
        sensitive_request_fields: sensitive_request,
        shown_once: a.shown_once,
    }
}

fn json_object(v: &Value) -> Value {
    match v {
        Value::Object(_) => v.clone(),
        _ => Value::Object(Map::new()),
    }
}

/// Dotted arguments paths of the request body fields marked sensitive:
/// merged fields by their key, an argument-shaped body under its key. Each
/// named type is walked once per path, so cycles end; array items are not
/// indexed.
pub fn sensitive_request_fields(plan: &SdkPlan<'_>, p: &OpPlan<'_>) -> Vec<String> {
    let mut out = vec![];
    let mut active: Vec<TypeId> = vec![];
    match &p.layout.body {
        None => return out,
        Some(BodyArg::Merged { fields, .. }) => {
            for f in fields {
                if f.sensitive {
                    out.push(f.wire_name.clone());
                } else {
                    sensitive_paths(plan, &f.ty, &f.wire_name, &mut active, &mut out);
                }
            }
        }
        Some(BodyArg::Arg { key, content }) => {
            if matches!(
                content.encoding,
                BodyEncoding::Bytes | BodyEncoding::Text | BodyEncoding::Jsonl
            ) {
                return out;
            }
            sensitive_paths(plan, &content.ty, key, &mut active, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn sensitive_paths(
    plan: &SdkPlan<'_>,
    ty: &TypeRef,
    prefix: &str,
    active: &mut Vec<TypeId>,
    out: &mut Vec<String>,
) {
    // Nesting is bounded so wide type graphs that share types stay cheap.
    if prefix.matches('.').count() >= 16 || out.len() >= 256 {
        return;
    }
    if let TypeRef::Named(id) = ty {
        if active.contains(id) {
            return;
        }
        active.push(id.clone());
    }
    match plan.resolve(ty) {
        Some(Shape::Record { fields, .. }) => {
            for f in fields {
                let path = format!("{prefix}.{}", f.wire_name);
                if f.sensitive {
                    out.push(path);
                } else {
                    sensitive_paths(plan, &f.ty, &path, active, out);
                }
            }
        }
        Some(Shape::Nullable { inner }) => sensitive_paths(plan, inner, prefix, active, out),
        Some(Shape::Array { items, .. }) => sensitive_paths(plan, items, prefix, active, out),
        Some(Shape::Union(u)) => {
            for v in &u.variants {
                sensitive_paths(plan, &v.ty, prefix, active, out);
            }
        }
        Some(Shape::Intersection { members }) => {
            for m in members {
                sensitive_paths(plan, m, prefix, active, out);
            }
        }
        _ => {}
    }
    if let TypeRef::Named(id) = ty
        && let Some(at) = active.iter().position(|a| a == id)
    {
        active.remove(at);
    }
}

/// The schema of the arguments object: parameters (no constraints of their
/// own), merged fields (with their presence and field constraints) or the
/// whole body; closed, except for an rpc operation without a body.
fn request_schema(
    p: &OpPlan<'_>,
    arg_names: &[String],
    schemas: &mut SchemaFormBuilder<'_>,
) -> RuntimeSchema {
    let mut fields = vec![];
    for (i, a) in p.arguments.iter().enumerate() {
        let name = arg_names.get(i).cloned().unwrap_or_else(|| a.key.clone());
        let field = match &a.source {
            ArgSource::Param { param, .. } => super::schema_form::SchemaField {
                name,
                key: Some(a.key.clone()),
                wire: a.wire.clone(),
                schema: schemas.type_ref(&param.ty),
                required: param.required,
                nullable: false,
            },
            ArgSource::Field(f) => {
                let schema = schemas.type_ref(&f.ty);
                schemas.field(name, Some(a.key.clone()), a.wire.clone(), schema, f)
            }
            ArgSource::Body { encoding, .. } => super::schema_form::SchemaField {
                name,
                key: Some(a.key.clone()),
                wire: a.wire.clone(),
                schema: match (encoding, &a.ty) {
                    (BodyEncoding::Bytes | BodyEncoding::Jsonl, _) => {
                        RuntimeSchema::of(SchemaKind::Bytes)
                    }
                    (BodyEncoding::Text, _) => RuntimeSchema::of(SchemaKind::String),
                    (_, Some(ty)) => schemas.type_ref(ty),
                    (_, None) => RuntimeSchema::of(SchemaKind::Any),
                },
                required: p.layout.body_required,
                nullable: false,
            },
        };
        fields.push(field);
    }
    let open = p.op.rpc.is_some() && p.op.body.is_none();
    RuntimeSchema {
        fields: Some(fields),
        additional: Some(if open {
            AdditionalMembers::Open
        } else {
            AdditionalMembers::Closed
        }),
        ..RuntimeSchema::of(SchemaKind::Object)
    }
}

/// The schema of a success body, when every success body is JSON (or JSON
/// Lines): the one type, or an untagged union of the distinct ones.
fn response_schema(p: &OpPlan<'_>, schemas: &mut SchemaFormBuilder<'_>) -> Option<RuntimeSchema> {
    let mut json: Vec<TypeRef> = vec![];
    for r in
        p.op.responses
            .iter()
            .filter(|r| r.kind == ResponseKind::Success)
    {
        let Some(c) = r
            .content
            .iter()
            .find(|c| c.encoding == BodyEncoding::Json)
            .or_else(|| r.content.first())
        else {
            continue;
        };
        match c.encoding {
            BodyEncoding::Json | BodyEncoding::Jsonl => {
                let ty = c.value_type();
                if !json.contains(&ty) {
                    json.push(ty);
                }
            }
            _ => return None,
        }
    }
    match json.as_slice() {
        [] => None,
        [one] => Some(schemas.type_ref(one)),
        many => Some(RuntimeSchema {
            variants: Some(many.iter().map(|t| schemas.type_ref(t)).collect()),
            ..RuntimeSchema::of(SchemaKind::Union)
        }),
    }
}

// --------------------------------------------------------------- macros

fn macro_descriptor(
    plan: &SdkPlan<'_>,
    names: &NameMap,
    index: usize,
    m: &MacroPlan<'_>,
) -> MacroDescriptor {
    let src = m.source;
    let input_names = names
        .macros
        .get(index)
        .map(|n| n.input.as_slice())
        .unwrap_or(&[]);
    MacroDescriptor {
        name: src.name.0.clone(),
        summary: src.summary.clone(),
        safety: safety_str(src.safety).to_string(),
        steps: m
            .steps
            .iter()
            .map(|s| MacroStepDescriptor {
                kind: s.kind.as_str().to_string(),
                operation: plan.operations[s.operation].op.id.0.clone(),
                args: s.args.clone(),
                as_name: s.as_name.clone(),
                until: s.until.clone(),
                interval_ms: s.interval_ms,
                budget_ms: s.budget.clone(),
                max_pages: s.max_pages,
            })
            .collect(),
        output: m.output.clone(),
        input: MacroInputDescriptor {
            extends: m.base.map(|b| plan.operations[b].op.id.0.clone()),
            add: m.add.clone(),
            fields: m
                .input
                .iter()
                .enumerate()
                .map(|(i, f)| MacroInputFieldDescriptor {
                    key: f.key.clone(),
                    name: input_names.get(i).cloned().unwrap_or_else(|| f.key.clone()),
                    required: match &f.source {
                        InputSource::Argument(_) => {
                            f.presence == tungsten_ir::Presence::Required
                                || f.presence == tungsten_ir::Presence::RequiredNullable
                        }
                        InputSource::Added { default, .. } => default.is_none(),
                    },
                })
                .collect(),
        },
        sensitive_response_fields: src.sensitive_response_fields.clone(),
        shown_once: src.shown_once,
        cluster: src.cluster.clone(),
    }
}
