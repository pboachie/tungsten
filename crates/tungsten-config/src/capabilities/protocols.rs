// SPDX-License-Identifier: AGPL-3.0-only
//! The protocol design gate. Every frontend binds its operations to a
//! registered protocol; an IR namespace without a binding is HTTP. A target
//! generates only protocols it declares here, so a new frontend lands with
//! a registration and per-target declarations instead of reshaping REST
//! fields, and every other target refuses its IR before writing (TG0706).

use schemars::JsonSchema;
use serde::Serialize;

use super::{SourceSupport, TargetCapability};

/// The implicit protocol of IR namespaces without a binding.
pub const HTTP: &str = "http";

/// Whether a frontend in this version produces the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolStatus {
    /// A built-in frontend produces IR bound to it.
    Implemented,
    /// Registered for design and external producers; no built-in frontend.
    Reserved,
}

/// Which built-in targets generate a protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "targets")]
pub enum ProtocolTargets {
    /// Every built-in emitter target.
    AllBuiltIn,
    /// Only the listed target ids.
    Only(&'static [&'static str]),
}

/// One registered protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ProtocolDefinition {
    pub id: &'static str,
    pub status: ProtocolStatus,
    pub description: &'static str,
    /// What the protocol needs beyond the common operation contract; it
    /// belongs in the binding's `extension`, not in REST fields.
    pub extension: &'static str,
    pub targets: ProtocolTargets,
}

impl ProtocolDefinition {
    pub fn supports(&self, target: &str) -> bool {
        match self.targets {
            ProtocolTargets::AllBuiltIn => true,
            ProtocolTargets::Only(ids) => ids.contains(&target),
        }
    }
}

pub const PROTOCOLS: &[ProtocolDefinition] = &[
    ProtocolDefinition {
        id: HTTP,
        status: ProtocolStatus::Implemented,
        description: "HTTP request/response operations, including SSE streams",
        extension: "none; method, path, parameters and bodies are the common contract",
        targets: ProtocolTargets::AllBuiltIn,
    },
    ProtocolDefinition {
        id: "graphql",
        status: ProtocolStatus::Reserved,
        description: "GraphQL queries, mutations and subscriptions",
        extension: "selection sets, fragments and subscription transport",
        targets: ProtocolTargets::Only(&[]),
    },
    ProtocolDefinition {
        id: "grpc",
        status: ProtocolStatus::Reserved,
        description: "gRPC and Connect RPC services",
        extension: "service and method names, streaming mode and status codes",
        targets: ProtocolTargets::Only(&[]),
    },
    ProtocolDefinition {
        id: "async_messaging",
        status: ProtocolStatus::Reserved,
        description: "broker channels described by AsyncAPI",
        extension: "channel bindings, delivery guarantees and acknowledgment",
        targets: ProtocolTargets::Only(&[]),
    },
];

/// Look up a registered protocol.
pub fn protocol(id: &str) -> Option<&'static ProtocolDefinition> {
    PROTOCOLS.iter().find(|definition| definition.id == id)
}

/// Whether built-in target `target` generates IR bound to `protocol`. An
/// unregistered protocol is supported by no target.
pub fn target_supports(target: &str, protocol_id: &str) -> bool {
    protocol(protocol_id).is_some_and(|definition| definition.supports(target))
}

/// The registered protocols `target` does not generate, among `protocols`.
pub fn unsupported<'a>(target: &str, protocols: &[&'a str]) -> Vec<&'a str> {
    protocols
        .iter()
        .copied()
        .filter(|id| !target_supports(target, id))
        .collect()
}

/// The built-in targets generating `protocol_id`, for a refusal's help.
pub fn supporting_targets(protocol_id: &str, targets: &[TargetCapability]) -> Vec<&'static str> {
    targets
        .iter()
        .filter(|target| target.source_support == SourceSupport::BuiltInEmitter)
        .filter(|target| target_supports(target.id, protocol_id))
        .map(|target| target.id)
        .collect()
}

/// Conflicts: duplicate ids, unknown or reserved target names, and an HTTP
/// registration that is not universal.
pub fn validate(protocols: &[ProtocolDefinition], targets: &[TargetCapability]) -> Vec<String> {
    let mut errors = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for definition in protocols {
        if !seen.insert(definition.id) {
            errors.push(format!("protocol `{}` is registered twice", definition.id));
        }
        if let ProtocolTargets::Only(ids) = definition.targets {
            for id in ids {
                let built_in = targets.iter().any(|target| {
                    target.id == *id && target.source_support == SourceSupport::BuiltInEmitter
                });
                if !built_in {
                    errors.push(format!(
                        "protocol `{}` lists `{id}`, which is not a built-in emitter target",
                        definition.id
                    ));
                }
            }
        }
    }
    if protocol(HTTP).map(|http| http.targets) != Some(ProtocolTargets::AllBuiltIn) {
        errors.push("protocol `http` must be registered for every built-in target".into());
    }
    errors
}
