// SPDX-License-Identifier: AGPL-3.0-only
//! The `agent.yml` model: per-operation tool overrides, API-wide defaults
//! (retries, disclosure, error handling), gates, macros and clusters.
//!
//! Every struct denies unknown keys. Shapes that a JSON Schema can express
//! are declared here and published by [`crate::json_schema`]; the rules it
//! cannot express are checked by `check.rs` (TG0602) before anything is
//! applied. The `x-agent-*` extensions of a spec use the same types.

use std::fmt;

use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::value::{MapAccessDeserializer, StrDeserializer};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use tungsten_ir::{DisclosureMode, IdempotencyKind, Jitter, OutcomeRule, Retryable, Safety};

/// The agent manifest: how agents may use the API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Manifest format version. Must be 1.
    #[schemars(range(min = 1, max = 1))]
    pub agent: u32,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    /// Per-operation rules; at most one entry per operation.
    #[serde(default)]
    pub tools: Vec<ToolConfig>,
    #[serde(default)]
    pub errors: ErrorsConfig,
    /// Deployment gate (environment variable) → what agents are told.
    #[serde(default)]
    pub gates: IndexMap<String, GateConfig>,
    #[serde(default)]
    pub macros: Vec<MacroConfig>,
    #[serde(default)]
    pub disclosure: DisclosureConfig,
}

/// Defaults applied to every operation before extensions and `tools`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    #[serde(default)]
    pub safety: SafetyByMethod,
    /// Policy of operations with side effects that declare none. A
    /// required `Idempotency-Key` header still infers `caller_owned`.
    #[serde(default)]
    pub idempotency: Option<IdempotencyConfig>,
    /// Preview mode of operations with side effects (`endpoint` is per tool).
    #[serde(default)]
    pub preview: Option<PreviewConfig>,
    #[serde(default)]
    pub unknown_outcome: UnknownOutcomeConfig,
    #[serde(default)]
    pub retries: RetriesConfig,
    #[serde(default)]
    pub disclosure: DisclosureDefaultsConfig,
}

/// Safety tier by HTTP method. A missing method keeps the built-in default:
/// safe methods `read_only`, `delete` `destructive`, the rest `mutating`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SafetyByMethod {
    #[serde(default)]
    pub get: Option<Safety>,
    #[serde(default)]
    pub put: Option<Safety>,
    #[serde(default)]
    pub post: Option<Safety>,
    #[serde(default)]
    pub delete: Option<Safety>,
    #[serde(default)]
    pub options: Option<Safety>,
    #[serde(default)]
    pub head: Option<Safety>,
    #[serde(default)]
    pub patch: Option<Safety>,
    #[serde(default)]
    pub trace: Option<Safety>,
}

/// An idempotency policy: a policy name, or an object with details.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum IdempotencyConfig {
    Kind(IdempotencyKind),
    Policy(IdempotencyPolicyConfig),
}

impl IdempotencyConfig {
    /// The object form of the policy.
    pub fn policy(&self) -> IdempotencyPolicyConfig {
        match self {
            IdempotencyConfig::Kind(kind) => IdempotencyPolicyConfig {
                policy: *kind,
                header: None,
                format: None,
                persist: None,
                note: None,
            },
            IdempotencyConfig::Policy(policy) => policy.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyPolicyConfig {
    pub policy: IdempotencyKind,
    /// Header carrying the key (`auto`, `caller_owned`, `content_hash`).
    /// Default: the operation's idempotency-key header, else `Idempotency-Key`.
    #[serde(default)]
    pub header: Option<String>,
    /// Required key format (`caller_owned`).
    #[serde(default)]
    pub format: Option<KeyFormat>,
    /// `required` (`caller_owned` only) makes the runtime refuse a call
    /// without a key.
    #[serde(default)]
    pub persist: Option<Persist>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyFormat {
    UuidV4,
    Uuid,
    Opaque,
}

impl KeyFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyFormat::UuidV4 => "uuid_v4",
            KeyFormat::Uuid => "uuid",
            KeyFormat::Opaque => "opaque",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Persist {
    Required,
    Runtime,
    None,
}

impl<'de> Deserialize<'de> for IdempotencyConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = IdempotencyConfig;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a policy name or a mapping with `policy`")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                IdempotencyKind::deserialize(StrDeserializer::<E>::new(v))
                    .map(IdempotencyConfig::Kind)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                IdempotencyPolicyConfig::deserialize(MapAccessDeserializer::new(map))
                    .map(IdempotencyConfig::Policy)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// A preview mode: `local` or `none`, or an object for `header` and
/// `endpoint` previews.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum PreviewConfig {
    Mode(PreviewMode),
    Detailed(PreviewObject),
}

impl PreviewConfig {
    pub fn object(&self) -> PreviewObject {
        match self {
            PreviewConfig::Mode(mode) => PreviewObject {
                mode: *mode,
                header: None,
                value: None,
                operation: None,
            },
            PreviewConfig::Detailed(object) => object.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    /// Render the request locally; no network.
    Local,
    /// Send with a dry-run header (`header`, `value`).
    Header,
    /// Call a preview operation (`operation`).
    Endpoint,
    /// No preview.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewObject {
    pub mode: PreviewMode,
    /// `header` mode: the dry-run header and its value.
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    /// `endpoint` mode: the operation that previews.
    #[serde(default)]
    pub operation: Option<String>,
}

impl<'de> Deserialize<'de> for PreviewConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = PreviewConfig;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a preview mode or a mapping with `mode`")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                PreviewMode::deserialize(StrDeserializer::<E>::new(v)).map(PreviewConfig::Mode)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                PreviewObject::deserialize(MapAccessDeserializer::new(map))
                    .map(PreviewConfig::Detailed)
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnknownOutcomeConfig {
    #[serde(default)]
    pub on_timeout: Option<OutcomeRule>,
    #[serde(default)]
    pub on_connection_reset: Option<OutcomeRule>,
    /// For the statuses in `errors.ambiguous_statuses`.
    #[serde(default)]
    pub on_ambiguous_status: Option<OutcomeRule>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetriesConfig {
    #[serde(default)]
    pub read_only: Option<RetryPolicyConfig>,
    /// Mutations are retried only with an idempotency key.
    #[serde(default)]
    pub mutating: Option<RetryPolicyConfig>,
    #[serde(default)]
    pub honor_retry_after: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicyConfig {
    /// Attempts after the first.
    #[schemars(range(max = 10))]
    pub max: u32,
    #[serde(default)]
    pub backoff: Option<BackoffConfig>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackoffConfig {
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub base_ms: Option<u64>,
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub max_ms: Option<u64>,
    #[serde(default)]
    pub jitter: Option<Jitter>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DisclosureDefaultsConfig {
    #[serde(default)]
    pub mode: Option<DisclosureMode>,
    /// Tool count, kept for compatibility: `auto` decides by
    /// `list_budget_tokens`.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub threshold: Option<u32>,
    /// Tokens of the discrete MCP tool list above which `auto` selects
    /// progressive disclosure (default 10000).
    #[serde(default)]
    #[schemars(range(min = 100))]
    pub list_budget_tokens: Option<u32>,
    /// Budget of each operation's compact description (tokens ≈ chars / 4).
    #[serde(default)]
    #[schemars(range(min = 10))]
    pub description_budget_tokens: Option<u32>,
    #[serde(default)]
    #[schemars(range(min = 50))]
    pub schema_budget_tokens: Option<u32>,
}

/// Rules for one operation. Every key except `operation` overrides the
/// defaults and the spec's `x-agent-*` extensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolConfig {
    /// Operation id (`public.submitAlphaMessage`, `workflow.action.send`)
    /// or `<namespace>.<resource path>.<method>`.
    pub operation: String,
    #[serde(default)]
    pub safety: Option<Safety>,
    #[serde(default)]
    pub idempotency: Option<IdempotencyConfig>,
    #[serde(default)]
    pub preview: Option<PreviewConfig>,
    #[serde(default)]
    pub confirmation: Option<ConfirmationConfig>,
    #[serde(default)]
    pub verify: Option<VerifyConfig>,
    /// API error code → remediation for this operation.
    #[serde(default)]
    pub remediation: IndexMap<String, RemediationConfig>,
    /// Side-effect note shown in previews.
    #[serde(default)]
    pub remediation_note: Option<String>,
    #[serde(default)]
    pub response: Option<ResponseConfig>,
    /// A runtime gate: an `x-runtime-gate` environment variable or a
    /// `gates` entry.
    #[serde(default)]
    pub gate: Option<String>,
    /// Cluster name; overrides `disclosure.clusters` membership.
    #[serde(default)]
    #[schemars(pattern(r"^[a-z][a-z0-9_]*$"))]
    pub cluster: Option<String>,
    /// Exclude from agent surfaces (MCP, tool lists); SDKs keep it.
    #[serde(default)]
    pub hidden: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationConfig {
    /// Request fields (wire names, dotted for nested fields) shown before
    /// confirming.
    pub summary_fields: Vec<String>,
    /// May interpolate `{field}` from the request.
    #[serde(default)]
    pub message: Option<String>,
}

/// A predicate: field path → `{in: [...]}`, `{equals: x}` or
/// `{contains: {...}}`.
pub type Predicate = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyConfig {
    /// A read-only operation to call after success.
    pub operation: String,
    /// Its arguments; `$response.<path>` and `$args.<path>` refer to the
    /// call's success body and arguments.
    #[serde(default)]
    pub args: Map<String, Value>,
    #[serde(default)]
    #[schemars(schema_with = "optional_predicate_schema")]
    pub expect: Option<Predicate>,
    /// Terminal states; with `poll`, the hook polls until one holds.
    #[serde(default)]
    #[schemars(schema_with = "optional_predicate_schema")]
    pub terminal: Option<Predicate>,
    #[serde(default)]
    pub poll: Option<PollConfig>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PollConfig {
    #[serde(default)]
    #[schemars(range(min = 100))]
    pub interval_ms: Option<u64>,
    #[serde(default)]
    #[schemars(range(min = 100))]
    pub budget_ms: Option<u64>,
}

/// What an agent is told about an error, and whether it may retry.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemediationConfig {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub retryable: Option<Retryable>,
    #[serde(default)]
    pub category: Option<Category>,
    /// What to do next, e.g. a verification operation or macro to call.
    #[serde(default)]
    pub next_action: Option<String>,
    /// The HTTP status the API answers this code with, when the spec does
    /// not say (`4XX` only). Read for `errors.codes` entries only; the mock
    /// uses it to pick the code of an injected status.
    #[serde(default)]
    pub status: Option<StatusCode>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResponseConfig {
    /// Success-body fields that carry secrets (wire names, dotted for
    /// nested fields).
    #[serde(default)]
    pub sensitive_fields: Vec<String>,
    /// The secret is returned exactly once; agents must store it.
    #[serde(default)]
    pub shown_once: bool,
}

/// The runtime's closed set of error categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Category {
    ValidationFailed,
    MalformedRequest,
    RequestTooLarge,
    AuthFailed,
    NotFound,
    Conflict,
    PreconditionFailed,
    RateLimited,
    UpstreamUnavailable,
    OutcomeUnknown,
    TransportFailed,
    ConfirmationRequired,
    GateDisabled,
    UnexpectedResponse,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::ValidationFailed => "VALIDATION_FAILED",
            Category::MalformedRequest => "MALFORMED_REQUEST",
            Category::RequestTooLarge => "REQUEST_TOO_LARGE",
            Category::AuthFailed => "AUTH_FAILED",
            Category::NotFound => "NOT_FOUND",
            Category::Conflict => "CONFLICT",
            Category::PreconditionFailed => "PRECONDITION_FAILED",
            Category::RateLimited => "RATE_LIMITED",
            Category::UpstreamUnavailable => "UPSTREAM_UNAVAILABLE",
            Category::OutcomeUnknown => "OUTCOME_UNKNOWN",
            Category::TransportFailed => "TRANSPORT_FAILED",
            Category::ConfirmationRequired => "CONFIRMATION_REQUIRED",
            Category::GateDisabled => "GATE_DISABLED",
            Category::UnexpectedResponse => "UNEXPECTED_RESPONSE",
        }
    }

    /// The default `retryable` of the category.
    pub fn default_retryable(self) -> Retryable {
        match self {
            Category::RateLimited | Category::UpstreamUnavailable | Category::TransportFailed => {
                Retryable::AfterDelay
            }
            Category::OutcomeUnknown => Retryable::SameKeyOnly,
            _ => Retryable::Never,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorsConfig {
    /// The JSON error schema, when inference picks the wrong one or none.
    #[serde(default)]
    pub envelope: Option<EnvelopeConfig>,
    /// Statuses after which a mutation may have been applied
    /// (`OUTCOME_UNKNOWN`).
    #[serde(default)]
    pub ambiguous_statuses: Vec<StatusCode>,
    /// Error responses without a JSON body.
    #[serde(default)]
    pub non_json: Vec<NonJsonConfig>,
    /// API error code → remediation for every operation.
    #[serde(default)]
    pub codes: IndexMap<String, RemediationConfig>,
}

/// An HTTP status code, 100 to 599.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct StatusCode(#[schemars(range(min = 100, max = 599))] pub u16);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeConfig {
    /// A type id (`public.Error`) or a component name applied to every
    /// namespace that has it (`Error`).
    pub schema: String,
    /// Dotted path of the code field (`code`, `error.code`).
    #[serde(default)]
    pub code_field: Option<String>,
    #[serde(default)]
    pub message_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NonJsonConfig {
    pub status: StatusCode,
    /// A media type (`text/plain`) or `none` for a bare status.
    pub media: String,
    pub category: Category,
    /// Default: the category's default.
    #[serde(default)]
    pub retryable: Option<Retryable>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GateConfig {
    /// What agents are told when the gate is off.
    pub text: String,
    /// Status the API answers when the gate is off (default 404); used when
    /// a tool names this gate and the spec has no `x-runtime-gate`.
    #[serde(default)]
    pub disabled_status: Option<StatusCode>,
    /// Whether the gate is on unless configured (default false).
    #[serde(default)]
    pub default_on: Option<bool>,
}

/// A multi-step workflow exposed like an operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MacroConfig {
    /// `<namespace>.<name>`; must not be an operation id.
    pub name: String,
    pub summary: String,
    /// Default: the strictest step's tier. A weaker declared tier is raised.
    #[serde(default)]
    pub safety: Option<Safety>,
    #[serde(default)]
    pub input: Option<MacroInputConfig>,
    #[schemars(length(min = 1))]
    pub steps: Vec<MacroStepConfig>,
    /// An expression over `$input` and the steps' `as` names.
    pub output: Value,
    #[serde(default)]
    pub response: Option<ResponseConfig>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MacroInputConfig {
    /// The operation whose request the input extends.
    #[serde(default)]
    pub extends: Option<String>,
    /// Extra input fields: name → JSON Schema.
    #[serde(default)]
    pub add: IndexMap<String, Map<String, Value>>,
}

/// One step: exactly one of `call`, `poll` (with `until`) or `paginate`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MacroStepConfig {
    #[serde(default)]
    pub call: Option<String>,
    /// A read-only operation called until `until` holds.
    #[serde(default)]
    pub poll: Option<String>,
    /// A paginated operation whose items are collected into an array.
    #[serde(default)]
    pub paginate: Option<String>,
    /// An expression; default `{}`.
    #[serde(default)]
    pub args: Option<Value>,
    /// Name under which later steps and the output see this step's result.
    #[serde(default, rename = "as")]
    #[schemars(pattern(r"^[A-Za-z_][A-Za-z0-9_]*$"))]
    pub as_name: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_predicate_schema")]
    pub until: Option<Predicate>,
    #[serde(default)]
    #[schemars(range(min = 100))]
    pub interval_ms: Option<u64>,
    /// Milliseconds, or a reference such as `$input.wait_budget_ms`.
    #[serde(default)]
    #[schemars(schema_with = "budget_schema")]
    pub budget_ms: Option<Value>,
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub max_pages: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DisclosureConfig {
    #[serde(default)]
    pub clusters: Vec<ClusterConfig>,
    #[serde(default)]
    pub prune: PruneConfig,
}

/// A semantic group of tools for `search_tools`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClusterConfig {
    #[schemars(pattern(r"^[a-z][a-z0-9_]*$"))]
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    /// Operation ids, macro names, `<namespace>.<resource path>.*` (the
    /// resource and its children) and `<namespace>.*`.
    pub operations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PruneConfig {
    #[serde(default)]
    pub descriptions: DescriptionPruneConfig,
    /// Fields hidden from agent schemas.
    #[serde(default)]
    pub drop_fields: Vec<String>,
    #[serde(default)]
    pub keep_examples: bool,
}

/// How operation descriptions are shortened for tool schemas. Markdown
/// links keep their text and code spans their content.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescriptionPruneConfig {
    /// Sentences kept (default 2).
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub max_sentences: Option<u32>,
    /// Sentences containing any of these are dropped.
    #[serde(default)]
    pub drop_phrases: Vec<String>,
}

fn predicate_schema() -> Schema {
    json_schema!({
        "description": "Field path → `{in: [...]}`, `{equals: x}` or `{contains: {...}}`.",
        "type": "object",
        "additionalProperties": {
            "type": "object",
            "minProperties": 1,
            "maxProperties": 1,
            "properties": {
                "in": { "type": "array" },
                "equals": true,
                "contains": { "type": "object" }
            },
            "additionalProperties": false
        }
    })
}

fn optional_predicate_schema(_: &mut SchemaGenerator) -> Schema {
    let mut schema = predicate_schema();
    schema.insert("type".into(), serde_json::json!(["object", "null"]));
    schema.insert("default".into(), Value::Null);
    schema
}

fn budget_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "description": "Milliseconds, or a `$` reference to an integer.",
        "anyOf": [
            { "type": "integer", "minimum": 100 },
            { "type": "string", "pattern": "^\\$" },
            { "type": "null" }
        ],
        "default": null
    })
}
