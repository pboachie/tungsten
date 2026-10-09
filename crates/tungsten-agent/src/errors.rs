// SPDX-License-Identifier: AGPL-3.0-only
//! The manifest's `errors` and `gates`.
//!
//! - `errors.envelope` replaces the inferred error envelope of every
//!   namespace that has the named type (a type id names one namespace, a
//!   bare component name every namespace with that component). The codes
//!   become the values of the code field; a code the builder had already
//!   found keeps its statuses. `Ir.errors` is merged again afterwards.
//! - `errors.ambiguous_statuses` become `AgentModel.ambiguous_statuses`;
//!   the matching exact-status error responses of operations that are not
//!   `read_only` are marked `ResponseKind::Ambiguous`. No response is
//!   invented for a status an operation does not declare: the runtime
//!   applies the list to any response of a mutation.
//! - `errors.non_json` and `errors.codes` become `AgentModel.non_json` and
//!   `AgentModel.error_codes`.
//! - `gates` become `AgentModel.gates`; a tool's `gate` gates an operation
//!   the spec does not gate already, with the spec's `x-runtime-gate` of
//!   that variable when another operation has one, else the `gates`
//!   entry's `disabled_status` (default 404) and `default_on` (default
//!   false).

use std::collections::BTreeMap;

use tungsten_ir::{
    AgentModel, DisclosurePolicy, ErrorCode, ErrorModel, Ir, NonJsonError, Operation,
    OperationStatus, RetryDefaults, RetryPolicy, RuntimeGate, Shape, TypeId, TypeRef, TypeTable,
    UnknownOutcomePolicy,
};

use crate::apply::{Cx, Origin, remediation};
use crate::model::*;
use crate::report::{Reporter, child};

/// Default status of a manifest-declared gate that is off.
const DEFAULT_GATE_STATUS: u16 = 404;

pub(crate) fn envelope(ir: &mut Ir, cfg: &AgentConfig, r: &mut Reporter<'_>) {
    let Some(env) = &cfg.errors.envelope else {
        return;
    };
    let mut matched = false;
    for ns in &mut ir.namespaces {
        let name = ns.name.wire.as_str();
        let id = match env.schema.strip_prefix(&format!("{name}.")) {
            Some(_) => TypeId(env.schema.clone()),
            None if env.schema.contains('.') => continue,
            None => TypeId(format!("{name}.{}", env.schema)),
        };
        if ir.types.get(&id).is_none() {
            continue;
        }
        matched = true;
        let same = ns.errors.envelope.as_ref() == Some(&id);
        let Some(code_field) = env
            .code_field
            .clone()
            .or_else(|| same.then(|| ns.errors.code_field.clone()).flatten())
        else {
            r.warning(
                "TG0607",
                "/errors/envelope",
                format!(
                    "`{}` replaces the inferred error envelope of `{name}`; name its code_field",
                    id.0
                ),
            );
            continue;
        };
        let Some(values) = string_values(&ir.types, &id, &code_field) else {
            r.warning(
                "TG0607",
                "/errors/envelope/code_field",
                format!("`{code_field}` is not a string field of `{}`; the envelope of `{name}` is unchanged", id.0),
            );
            continue;
        };
        if let Some(message) = &env.message_field
            && string_values(&ir.types, &id, message).is_none()
        {
            r.warning(
                "TG0607",
                "/errors/envelope/message_field",
                format!("`{message}` is not a string field of `{}`", id.0),
            );
        }
        let statuses: BTreeMap<&str, &Vec<u16>> = ns
            .errors
            .codes
            .iter()
            .map(|c| (c.code.as_str(), &c.statuses))
            .collect();
        let mut codes: Vec<ErrorCode> = values
            .iter()
            .map(|code| ErrorCode {
                code: code.clone(),
                statuses: statuses
                    .get(code.as_str())
                    .map(|s| s.to_vec())
                    .unwrap_or_default(),
            })
            .collect();
        codes.sort_by(|a, b| a.code.cmp(&b.code));
        codes.dedup_by(|a, b| a.code == b.code);
        ns.errors = ErrorModel {
            envelope: Some(id),
            code_field: Some(code_field),
            message_field: env
                .message_field
                .clone()
                .or_else(|| same.then(|| ns.errors.message_field.clone()).flatten()),
            codes,
        };
    }
    if !matched {
        r.warning(
            "TG0604",
            "/errors/envelope/schema",
            format!(
                "no namespace has a type `{}`; the inferred error envelopes are kept",
                env.schema
            ),
        );
        return;
    }
    ir.errors = merge(ir.namespaces.iter().map(|n| &n.errors));
}

/// The values a string field admits: its enum or const values, empty for
/// any string, `None` when the path is not a string field.
fn string_values(types: &TypeTable, id: &TypeId, path: &str) -> Option<Vec<String>> {
    let mut ty = TypeRef::Named(id.clone());
    for segment in path.split('.') {
        let fields = match shape(types, &ty)? {
            Shape::Record { fields, .. } => fields,
            _ => return None,
        };
        ty = fields.iter().find(|f| f.wire_name == segment)?.ty.clone();
    }
    match shape(types, &ty)? {
        Shape::Enum { values, .. } => Some(
            values
                .iter()
                .filter_map(|v| v.value.as_str().map(str::to_string))
                .collect(),
        ),
        Shape::Const { value } => value.as_str().map(|v| vec![v.to_string()]),
        Shape::Primitive {
            primitive: tungsten_ir::Primitive::String { .. },
            ..
        } => Some(vec![]),
        _ => None,
    }
}

/// The shape behind a reference, through named types and nullability.
fn shape<'t>(types: &'t TypeTable, ty: &'t TypeRef) -> Option<&'t Shape> {
    let mut current = ty;
    for _ in 0..32 {
        let s = match current {
            TypeRef::Named(id) => &types.get(id)?.shape,
            TypeRef::Inline(s) => s.as_ref(),
        };
        match s {
            Shape::Nullable { inner } => current = inner,
            other => return Some(other),
        }
    }
    None
}

/// The API-wide error model: every code with its statuses merged, and the
/// envelope and fields only when every namespace that has one agrees.
fn merge<'m>(models: impl Iterator<Item = &'m ErrorModel>) -> ErrorModel {
    let models: Vec<&ErrorModel> = models.collect();
    let mut codes: BTreeMap<&str, Vec<u16>> = BTreeMap::new();
    for m in &models {
        for c in &m.codes {
            codes
                .entry(c.code.as_str())
                .or_default()
                .extend(&c.statuses);
        }
    }
    fn agreed<'v, T: PartialEq + Clone + 'v>(mut values: impl Iterator<Item = &'v T>) -> Option<T> {
        let first = values.next()?;
        values.all(|v| v == first).then(|| first.clone())
    }
    ErrorModel {
        envelope: agreed(models.iter().filter_map(|m| m.envelope.as_ref())),
        code_field: agreed(models.iter().filter_map(|m| m.code_field.as_ref())),
        message_field: agreed(models.iter().filter_map(|m| m.message_field.as_ref())),
        codes: codes
            .into_iter()
            .map(|(code, mut statuses)| {
                statuses.sort_unstable();
                statuses.dedup();
                ErrorCode {
                    code: code.to_string(),
                    statuses,
                }
            })
            .collect(),
    }
}

/// API-wide policies: ambiguous statuses, non-JSON errors, global codes and
/// the defaults emitters read.
pub(crate) fn model(
    agent: &mut AgentModel,
    cfg: &AgentConfig,
    api_errors: &ErrorModel,
    r: &mut Reporter<'_>,
) {
    let e = &cfg.errors;
    let mut ambiguous: Vec<u16> = e.ambiguous_statuses.iter().map(|s| s.0).collect();
    ambiguous.sort_unstable();
    ambiguous.dedup();
    agent.ambiguous_statuses = ambiguous;
    agent.non_json = e
        .non_json
        .iter()
        .map(|n| NonJsonError {
            status: n.status.0,
            media: n.media.trim().to_ascii_lowercase(),
            category: n.category.as_str().to_string(),
            retryable: n
                .retryable
                .unwrap_or_else(|| n.category.default_retryable()),
            text: n.text.clone(),
        })
        .collect();
    agent.error_codes = BTreeMap::new();
    for (code, entry) in &e.codes {
        if !api_errors.codes.iter().any(|c| &c.code == code) {
            r.warning(
                "TG0606",
                &child("/errors/codes", code),
                format!("`{code}` is not an error code of any namespace; the remediation is kept but may never match"),
            );
        }
        agent.error_codes.insert(code.clone(), remediation(entry));
    }
    let d = &cfg.defaults;
    let base = RetryDefaults::default();
    let policy = |given: &Option<RetryPolicyConfig>, base: RetryPolicy| match given {
        None => base,
        Some(p) => {
            let b = p.backoff.clone().unwrap_or_default();
            RetryPolicy {
                max: p.max,
                base_ms: b.base_ms.unwrap_or(base.base_ms),
                max_ms: b.max_ms.unwrap_or(base.max_ms),
                jitter: b.jitter.unwrap_or(base.jitter),
            }
        }
    };
    agent.retries = RetryDefaults {
        read_only: policy(&d.retries.read_only, base.read_only),
        mutating: policy(&d.retries.mutating, base.mutating),
        honor_retry_after: d
            .retries
            .honor_retry_after
            .unwrap_or(base.honor_retry_after),
    };
    let u = UnknownOutcomePolicy::default();
    agent.unknown_outcome = UnknownOutcomePolicy {
        on_timeout: d.unknown_outcome.on_timeout.unwrap_or(u.on_timeout),
        on_connection_reset: d
            .unknown_outcome
            .on_connection_reset
            .unwrap_or(u.on_connection_reset),
        on_ambiguous_status: d
            .unknown_outcome
            .on_ambiguous_status
            .unwrap_or(u.on_ambiguous_status),
    };
    let base = DisclosurePolicy::default();
    let dd = &d.disclosure;
    agent.disclosure = DisclosurePolicy {
        mode: dd.mode.unwrap_or(base.mode),
        threshold: dd.threshold.unwrap_or(base.threshold),
        description_budget_tokens: dd
            .description_budget_tokens
            .unwrap_or(base.description_budget_tokens),
        schema_budget_tokens: dd.schema_budget_tokens.unwrap_or(base.schema_budget_tokens),
        drop_fields: cfg.disclosure.prune.drop_fields.clone(),
        keep_examples: cfg.disclosure.prune.keep_examples,
    };
    // Accepted by the schema, but no emitter of this version applies them
    // (agent schemas follow the SDK's arguments exactly); say so instead of
    // ignoring them silently.
    if !cfg.disclosure.prune.drop_fields.is_empty() {
        r.warning(
            "TG0613",
            "/disclosure/prune/drop_fields",
            "disclosure.prune.drop_fields is not applied by this version of tungsten: tool schemas still list every argument the SDK takes".into(),
        );
    }
    if cfg.disclosure.prune.keep_examples {
        r.warning(
            "TG0613",
            "/disclosure/prune/keep_examples",
            "disclosure.prune.keep_examples is not applied by this version of tungsten: tool schemas carry no examples".into(),
        );
    }
}

/// `gates` texts, checked against the spec's `x-runtime-gate`s.
pub(crate) fn gates(agent: &mut AgentModel, cfg: &AgentConfig, ir: &Ir, r: &mut Reporter<'_>) {
    let spec = spec_gates(ir.operations().into_iter());
    agent.gates = BTreeMap::new();
    for (name, gate) in &cfg.gates {
        if let (Some(status), Some(found)) = (gate.disabled_status, spec.get(name.as_str()))
            && status.0 != found.disabled_status
        {
            r.warning(
                "TG0608",
                &child(&child("/gates", name), "disabled_status"),
                format!(
                    "the spec's x-runtime-gate `{name}` answers {} when off, not {}; the spec wins",
                    found.disabled_status, status.0
                ),
            );
        }
        agent.gates.insert(name.clone(), gate.text.clone());
    }
}

/// `gates` entries no operation is gated by, after every tool's `gate` was
/// applied: a text nobody can be shown, and 404s reported as NOT_FOUND.
pub(crate) fn unused_gates(cfg: &AgentConfig, ir: &Ir, r: &mut Reporter<'_>) {
    let used = spec_gates(ir.operations().into_iter());
    for name in cfg.gates.keys() {
        if !used.contains_key(name.as_str()) {
            r.warning(
                "TG0614",
                &child("/gates", name),
                format!(
                    "gate `{name}` is used by no operation (no x-runtime-gate in the spec and no tools entry with `gate: {name}`); its text is never shown"
                ),
            );
        }
    }
}

/// The spec's gates by environment variable (first operation wins).
fn spec_gates<'o>(ops: impl Iterator<Item = &'o Operation>) -> BTreeMap<&'o str, &'o RuntimeGate> {
    let mut out = BTreeMap::new();
    for op in ops {
        if let OperationStatus::Gated { gate } = &op.status {
            out.entry(gate.env_var.as_str()).or_insert(gate);
        }
    }
    out
}

/// The status a tool's `gate` gives `op`, when it changes anything.
pub(crate) fn gate_status(
    cx: &Cx<'_>,
    op: &Operation,
    gate: &str,
    origin: &Origin,
    r: &mut Reporter<'_>,
) -> Option<OperationStatus> {
    match &op.status {
        OperationStatus::Gated { gate: g } if g.env_var == gate => None,
        OperationStatus::Gated { gate: g } => {
            origin.warn(
                r,
                "TG0608",
                "gate",
                "",
                format!(
                    "`{}` is already gated by the spec's x-runtime-gate `{}`; `{gate}` is ignored",
                    op.id.0, g.env_var
                ),
            );
            None
        }
        OperationStatus::Planned { .. } => None,
        OperationStatus::Implemented => {
            if let Some(spec) = spec_gates(cx.ops.values().copied()).get(gate) {
                return Some(OperationStatus::Gated {
                    gate: (*spec).clone(),
                });
            }
            if let Some(declared) = cx.cfg.gates.get(gate) {
                return Some(OperationStatus::Gated {
                    gate: RuntimeGate {
                        env_var: gate.to_string(),
                        default_on: declared.default_on.unwrap_or(false),
                        disabled_status: declared
                            .disabled_status
                            .map_or(DEFAULT_GATE_STATUS, |s| s.0),
                    },
                });
            }
            origin.warn(
                r,
                "TG0608",
                "gate",
                "",
                format!("`{gate}` is neither an x-runtime-gate variable of the spec nor a gates entry; ignored"),
            );
            None
        }
    }
}
