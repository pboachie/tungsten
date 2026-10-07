// SPDX-License-Identifier: AGPL-3.0-only
//! Compiled agent metadata (planning/04 "Compiled form in the IR").
//!
//! Phase 1 only populates defaults from HTTP methods; the agent.yml
//! transform that fills the rest arrives in Phase 2. The types are fixed now
//! so the IR shape is stable.

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
        /// Pruned description for agent tool schemas (Phase 2).
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
    /// Phase-1 default from the HTTP method alone (planning/04 defaults).
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
    pub struct Macro {
        pub name: OperationId,
        pub summary: String,
        pub safety: Safety,
        /// Steps and output kept as manifest JSON until Phase 2 types them.
        pub steps: serde_json::Value,
        pub output: serde_json::Value,
        #[serde(default)]
        pub input: serde_json::Value,
    }
}

ir_struct! {
    pub struct Cluster {
        pub name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub summary: Option<String>,
        pub operations: Vec<OperationId>,
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
        #[serde(default)]
        pub ambiguous_statuses: Vec<u16>,
    }
}
