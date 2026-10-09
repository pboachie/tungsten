// SPDX-License-Identifier: AGPL-3.0-only
//! Compiled agent metadata: safety tier, idempotency, retries, previews and
//! confirmation, verification hooks, remediation, macros and disclosure
//! policy, in the form emitters and runtimes consume.
//!
//! The builder sets the method defaults ([`OperationAgentMeta::default_for`]);
//! `tungsten-agent` fills the rest from `agent.yml` and the spec's
//! `x-agent-*` extensions. Emitters read only these types, never the
//! manifest.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{OperationId, ir_struct};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    ReadOnly,
    Mutating,
    Destructive,
    Irreversible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Retryable {
    Never,
    AfterDelay,
    SameKeyOnly,
    AfterRemediation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyKind {
    None,
    Auto,
    CallerOwned,
    ContentHash,
    ContentIdentity,
}

ir_struct! {
    pub struct IdempotencyPolicy {
        pub policy: IdempotencyKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub header: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub format: Option<String>,
        /// `required` makes the runtime refuse calls without a key.
        #[serde(default)]
        pub persist_required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub note: Option<String>,
    }
}

impl Default for IdempotencyPolicy {
    fn default() -> Self {
        Self {
            policy: IdempotencyKind::None,
            header: None,
            format: None,
            persist_required: false,
            note: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PreviewMode {
    Local,
    Header { header: String, value: String },
    Endpoint { operation: OperationId },
    None,
}

ir_struct! {
    pub struct Confirmation {
        pub summary_fields: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub message: Option<String>,
    }
}

ir_struct! {
    pub struct VerificationHook {
        pub operation: OperationId,
        #[serde(default)]
        pub args: serde_json::Value,
        #[serde(default)]
        pub expect: serde_json::Value,
        #[serde(default)]
        pub terminal: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub poll_interval_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub poll_budget_ms: Option<u64>,
    }
}

ir_struct! {
    pub struct Remediation {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub category: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub retryable: Option<Retryable>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub next_action: Option<String>,
    }
}

ir_struct! {
    pub struct OperationAgentMeta {
        pub safety: Safety,
        #[serde(default)]
        pub idempotency: IdempotencyPolicy,
        pub preview: PreviewMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub confirmation: Option<Confirmation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub verify: Option<VerificationHook>,
        #[serde(default)]
        pub remediation: BTreeMap<String, Remediation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub remediation_note: Option<String>,
        #[serde(default)]
        pub sensitive_response_fields: Vec<String>,
        #[serde(default)]
        pub shown_once: bool,
        /// Pruned description for agent tool schemas, filled in by `tungsten-agent`.
        #[serde(default)]
        pub compact_doc: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cluster: Option<String>,
        #[serde(default)]
        pub hidden: bool,
    }
}

impl Default for OperationAgentMeta {
    fn default() -> Self {
        Self {
            safety: Safety::Mutating,
            idempotency: IdempotencyPolicy::default(),
            preview: PreviewMode::Local,
            confirmation: None,
            verify: None,
            remediation: BTreeMap::new(),
            remediation_note: None,
            sensitive_response_fields: vec![],
            shown_once: false,
            compact_doc: String::new(),
            cluster: None,
            hidden: false,
        }
    }
}

impl OperationAgentMeta {
    /// Default from the HTTP method alone: safe methods are `read_only`,
    /// `DELETE` is `destructive`, everything else `mutating`; only
    /// `read_only` operations skip the local preview.
    pub fn default_for(method: crate::HttpMethod) -> Self {
        let safety = match method {
            m if m.is_safe() => Safety::ReadOnly,
            crate::HttpMethod::Delete => Safety::Destructive,
            _ => Safety::Mutating,
        };
        let preview = if safety == Safety::ReadOnly {
            PreviewMode::None
        } else {
            PreviewMode::Local
        };
        Self {
            safety,
            preview,
            ..Self::default()
        }
    }
}

ir_struct! {
    /// A multi-step workflow exposed like an operation (an `agent.yml`
    /// macro), in the canonical form shared with the emitters:
    ///
    /// - `steps`: `[{kind: "call"|"poll"|"paginate", operation, args, as,
    ///   until, interval_ms, budget_ms, max_pages}]`, absent keys `null`;
    /// - `output`: an expression;
    /// - `input`: `{extends: <operation id>|null, add: {<name>: <JSON Schema>}}`.
    ///
    /// An expression is JSON in which a string starting with `$` is a
    /// reference (`$input`, `$input.a.b`, `$<as>`, `$<as>.a.b`), an object
    /// `{"expr": "<ref> in [<json>,...]" | "<ref> == <json>" | "<ref> !=
    /// <json>"}` is a boolean, and anything else is a literal. A poll's
    /// `until` is a predicate: field path → `{in: [...]}` | `{equals: x}` |
    /// `{contains: {...}}`.
    pub struct Macro {
        pub name: OperationId,
        pub summary: String,
        /// The strictest step's tier, or a stricter declared one.
        pub safety: Safety,
        pub steps: serde_json::Value,
        pub output: serde_json::Value,
        #[serde(default)]
        pub input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cluster: Option<String>,
        /// Output fields that carry secrets (dotted paths).
        #[serde(default)]
        pub sensitive_response_fields: Vec<String>,
        #[serde(default)]
        pub shown_once: bool,
    }
}

ir_struct! {
    pub struct Cluster {
        pub name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub summary: Option<String>,
        /// Operation ids and macro names, sorted and deduplicated.
        pub operations: Vec<OperationId>,
    }
}

ir_struct! {
    /// How the runtime classifies an error response without a JSON body
    /// (agent.yml `errors.non_json`).
    pub struct NonJsonError {
        pub status: u16,
        /// A media type, or `none` for a bare status.
        pub media: String,
        /// One of the runtime's error categories (for example `RATE_LIMITED`).
        pub category: String,
        pub retryable: Retryable,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub text: Option<String>,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Jitter {
    None,
    Full,
    Equal,
}

ir_struct! {
    pub struct RetryPolicy {
        /// Attempts after the first.
        pub max: u32,
        pub base_ms: u64,
        pub max_ms: u64,
        pub jitter: Jitter,
    }
}

ir_struct! {
    /// Retry defaults by tier (agent.yml `defaults.retries`). Mutations are
    /// retried only with an idempotency key.
    pub struct RetryDefaults {
        pub read_only: RetryPolicy,
        pub mutating: RetryPolicy,
        pub honor_retry_after: bool,
    }
}

impl Default for RetryDefaults {
    fn default() -> Self {
        let backoff = |max| RetryPolicy {
            max,
            base_ms: 200,
            max_ms: 5000,
            jitter: Jitter::Full,
        };
        Self {
            read_only: backoff(3),
            mutating: backoff(0),
            honor_retry_after: true,
        }
    }
}

/// What the runtime reports when a mutation's outcome cannot be known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeRule {
    /// `OUTCOME_UNKNOWN`, retryable only with the same key. Never a blind retry.
    Unknown,
    /// A plain failure of the transport or status category.
    Fail,
}

ir_struct! {
    /// agent.yml `defaults.unknown_outcome`.
    pub struct UnknownOutcomePolicy {
        pub on_timeout: OutcomeRule,
        pub on_connection_reset: OutcomeRule,
        /// For statuses in `AgentModel.ambiguous_statuses`.
        pub on_ambiguous_status: OutcomeRule,
    }
}

impl Default for UnknownOutcomePolicy {
    fn default() -> Self {
        Self {
            on_timeout: OutcomeRule::Unknown,
            on_connection_reset: OutcomeRule::Unknown,
            on_ambiguous_status: OutcomeRule::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureMode {
    /// Progressive when the discrete `tools/list` (with its instructions)
    /// exceeds `list_budget_tokens`, discrete otherwise.
    Auto,
    Discrete,
    Progressive,
}

ir_struct! {
    /// agent.yml `defaults.disclosure` and `disclosure.prune`.
    pub struct DisclosurePolicy {
        pub mode: DisclosureMode,
        /// Tool count, kept for manifests and servers that decide by count
        /// (the MCP runtime's fallback when a manifest has no mode); the
        /// emitter's `auto` decides by `list_budget_tokens`.
        pub threshold: u32,
        /// Tokens of the discrete MCP `tools/list` above which `auto`
        /// selects progressive disclosure (default 10,000).
        #[serde(default = "default_list_budget_tokens")]
        pub list_budget_tokens: u32,
        /// Budget of `OperationAgentMeta.compact_doc` (tokens ≈ chars / 4).
        pub description_budget_tokens: u32,
        pub schema_budget_tokens: u32,
        /// Fields hidden from agent schemas, as written in the manifest.
        #[serde(default)]
        pub drop_fields: Vec<String>,
        #[serde(default)]
        pub keep_examples: bool,
    }
}

/// Default `DisclosurePolicy.list_budget_tokens`.
pub const DEFAULT_LIST_BUDGET_TOKENS: u32 = 10_000;

fn default_list_budget_tokens() -> u32 {
    DEFAULT_LIST_BUDGET_TOKENS
}

impl Default for DisclosurePolicy {
    fn default() -> Self {
        Self {
            mode: DisclosureMode::Auto,
            threshold: 24,
            list_budget_tokens: DEFAULT_LIST_BUDGET_TOKENS,
            description_budget_tokens: 60,
            schema_budget_tokens: 600,
            drop_fields: vec![],
            keep_examples: false,
        }
    }
}

ir_struct! {
    #[derive(Default)]
    pub struct AgentModel {
        #[serde(default)]
        pub macros: Vec<Macro>,
        #[serde(default)]
        pub clusters: Vec<Cluster>,
        /// Gate env var → agent-facing explanation.
        #[serde(default)]
        pub gates: BTreeMap<String, String>,
        /// Error code → global remediation.
        #[serde(default)]
        pub error_codes: BTreeMap<String, Remediation>,
        /// Statuses after which a mutation's outcome is unknown, sorted.
        #[serde(default)]
        pub ambiguous_statuses: Vec<u16>,
        /// Error responses without a JSON body, in manifest order.
        #[serde(default)]
        pub non_json: Vec<NonJsonError>,
        #[serde(default)]
        pub retries: RetryDefaults,
        #[serde(default)]
        pub unknown_outcome: UnknownOutcomePolicy,
        #[serde(default)]
        pub disclosure: DisclosurePolicy,
    }
}
