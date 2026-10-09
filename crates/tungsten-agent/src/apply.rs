// SPDX-License-Identifier: AGPL-3.0-only
//! The agent transform: per operation, method defaults → inferred policy →
//! `x-agent-*` extensions → the manifest's `tools` entry; then the
//! API-wide error policy, gates, macros and clusters.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_core::Severity;
use tungsten_ir::{
    Confirmation, HttpMethod, IdempotencyKind, IdempotencyPolicy, Ir, Operation,
    OperationAgentMeta, OperationId, OperationStatus, ParamRole, PreviewMode as IrPreview,
    Remediation, ResponseKind, Safety, StatusMatch, StringFormat, TypeId, TypeRef, TypeTable,
    VerificationHook,
};

use crate::fields::{self, Lookup, Sensitive};
use crate::index::{Index, Resolved};
use crate::model::*;
use crate::prune::{self, DEFAULT_MAX_SENTENCES, Prune};
use crate::report::{Reporter, child};
use crate::{clusters, errors, expr, extensions, macros};

/// Header used for keyed idempotency when neither the manifest nor the
/// operation names one.
const DEFAULT_IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

/// Where a rule was written: a `tools` entry or an operation's extensions.
#[derive(Debug, Clone)]
pub(crate) enum Origin {
    /// `/tools/<i>`.
    Manifest(String),
    /// The operation object's file and pointer.
    Spec { file: String, pointer: String },
}

impl Origin {
    /// Report on `field` of the rule (`verify`, `remediation`, ...) plus
    /// `rest`, a pointer suffix inside it.
    pub fn warn(&self, r: &mut Reporter<'_>, code: &str, field: &str, rest: &str, message: String) {
        match self {
            Origin::Manifest(base) => {
                r.warning(code, &format!("{}{rest}", child(base, field)), message)
            }
            Origin::Spec { file, pointer } => {
                let at = format!("{}{rest}", child(pointer, &format!("x-agent-{field}")));
                r.spec(Severity::Warning, code, file, &at, message);
            }
        }
    }
}

/// One layer of rules for an operation, highest precedence first.
struct Layer<'c> {
    entry: &'c ToolConfig,
    origin: Origin,
}

/// Everything the per-operation step reads.
pub(crate) struct Cx<'a> {
    pub cfg: &'a AgentConfig,
    pub index: &'a Index,
    pub ops: BTreeMap<&'a str, &'a Operation>,
    pub types: &'a TypeTable,
    pub safety: BTreeMap<String, Safety>,
    /// Namespace → its error codes.
    pub codes: BTreeMap<String, BTreeSet<String>>,
}

impl Cx<'_> {
    /// Resolve a reference to a callable operation, reporting TG0603.
    pub fn callable(
        &self,
        reference: &str,
        r: &mut Reporter<'_>,
        origin: &Origin,
        field: &str,
        rest: &str,
    ) -> Option<String> {
        match self.index.resolve(reference) {
            Resolved::Callable(id) => Some(id),
            Resolved::Planned(id) => {
                origin.warn(
                    r,
                    "TG0603",
                    field,
                    rest,
                    format!("`{id}` is a planned operation and cannot be called; ignored"),
                );
                None
            }
            Resolved::Unknown => {
                origin.warn(
                    r,
                    "TG0603",
                    field,
                    rest,
                    format!("unknown operation `{reference}`; ignored"),
                );
                None
            }
        }
    }
}

/// The computed agent data of one operation.
struct Computed {
    meta: OperationAgentMeta,
    status: Option<OperationStatus>,
    ambiguous: bool,
    /// Fields of named types to mark sensitive.
    sensitive: Vec<(TypeId, String)>,
}

pub(crate) fn run(ir: &mut Ir, cfg: &AgentConfig, r: &mut Reporter<'_>) {
    errors::envelope(ir, cfg, r);
    errors::code_statuses(ir, cfg);
    let index = Index::build(ir);
    let mut computed: BTreeMap<String, Computed> = BTreeMap::new();
    let mut explicit_clusters = BTreeMap::new();
    let mut agent = std::mem::take(&mut ir.agent);
    {
        let ops = all_operations(ir);
        let tools = resolve_tools(cfg, &index, r);
        let mut exts = BTreeMap::new();
        for op in ops.values() {
            exts.insert(op.id.0.clone(), extensions::parse(op, r));
        }
        let mut safety = BTreeMap::new();
        for op in ops.values() {
            let id = op.id.0.as_str();
            let tool = tools.get(id).and_then(|(_, t)| t.safety);
            let ext = exts.get(id).and_then(|e| e.safety);
            let s = tool
                .or(ext)
                .unwrap_or_else(|| method_default(cfg, op.method));
            safety.insert(id.to_string(), s);
        }
        let codes = ir
            .namespaces
            .iter()
            .map(|ns| {
                let codes = ns.errors.codes.iter().map(|c| c.code.clone()).collect();
                (ns.name.wire.clone(), codes)
            })
            .collect();
        let cx = Cx {
            cfg,
            index: &index,
            ops,
            types: &ir.types,
            safety,
            codes,
        };
        let mut sensitive = Sensitive::new(&ir.types);
        for (id, op) in &cx.ops {
            let mut layers = vec![];
            if let Some((i, tool)) = tools.get(*id) {
                layers.push(Layer {
                    entry: tool,
                    origin: Origin::Manifest(format!("/tools/{i}")),
                });
            }
            if let Some(ext) = exts.get(*id) {
                layers.push(Layer {
                    entry: ext,
                    origin: Origin::Spec {
                        file: op.source.file.clone(),
                        pointer: op.source.pointer.clone(),
                    },
                });
            }
            if let Some(cluster) = layers.iter().find_map(|l| l.entry.cluster.clone()) {
                explicit_clusters.insert(id.to_string(), cluster);
            }
            computed.insert(
                id.to_string(),
                operation(&cx, op, &layers, &mut sensitive, r),
            );
        }
        agent.macros = macros::build(&cx, r);
    }
    let marks: Vec<(TypeId, String)> = computed
        .values()
        .flat_map(|c| c.sensitive.iter().cloned())
        .collect();
    errors::model(&mut agent, cfg, &ir.errors, r);
    errors::gates(&mut agent, cfg, ir, r);
    let assigned = clusters::assign(cfg, &index, &explicit_clusters, &mut agent, r);
    for_each_operation(ir, |op| {
        if let Some(mut c) = computed.remove(&op.id.0) {
            c.meta.cluster = assigned.get(&op.id.0).cloned();
            op.agent = c.meta;
            if let Some(status) = c.status {
                op.status = status;
            }
            if c.ambiguous {
                let statuses = &agent.ambiguous_statuses;
                for resp in &mut op.responses {
                    if let StatusMatch::Exact(s) = resp.status
                        && statuses.contains(&s)
                        && resp.kind != ResponseKind::Success
                    {
                        resp.kind = ResponseKind::Ambiguous;
                    }
                }
            }
        }
    });
    for (id, field) in marks {
        mark_sensitive(&mut ir.types, &id, &field);
    }
    ir.agent = agent;
}

/// Every operation (callable and planned) by id.
fn all_operations(ir: &Ir) -> BTreeMap<&str, &Operation> {
    fn walk<'a>(r: &'a tungsten_ir::Resource, out: &mut BTreeMap<&'a str, &'a Operation>) {
        for op in &r.operations {
            out.insert(op.id.0.as_str(), op);
        }
        for c in &r.children {
            walk(c, out);
        }
    }
    let mut out = BTreeMap::new();
    for ns in &ir.namespaces {
        for r in &ns.resources {
            walk(r, &mut out);
        }
        for op in &ns.planned {
            out.insert(op.id.0.as_str(), op);
        }
    }
    out
}

fn for_each_operation(ir: &mut Ir, mut f: impl FnMut(&mut Operation)) {
    fn walk(r: &mut tungsten_ir::Resource, f: &mut impl FnMut(&mut Operation)) {
        r.operations.iter_mut().for_each(&mut *f);
        for c in &mut r.children {
            walk(c, f);
        }
    }
    for ns in &mut ir.namespaces {
        for r in &mut ns.resources {
            walk(r, &mut f);
        }
        ns.planned.iter_mut().for_each(&mut f);
    }
}

/// `tools` entries by the operation they resolve to.
fn resolve_tools<'c>(
    cfg: &'c AgentConfig,
    index: &Index,
    r: &mut Reporter<'_>,
) -> BTreeMap<String, (usize, &'c ToolConfig)> {
    let mut out = BTreeMap::new();
    for (i, tool) in cfg.tools.iter().enumerate() {
        let at = format!("/tools/{i}/operation");
        match index.resolve(&tool.operation) {
            Resolved::Callable(id) => {
                if let Some((first, _)) = out.get(&id) {
                    r.warning(
                        "TG0603",
                        &at,
                        format!(
                            "`{}` names `{id}`, which /tools/{first} already configures; ignored",
                            tool.operation
                        ),
                    );
                } else {
                    out.insert(id, (i, tool));
                }
            }
            Resolved::Planned(id) => r.warning(
                "TG0603",
                &at,
                format!(
                    "`{id}` is a planned operation; agents never call it, so this entry is ignored"
                ),
            ),
            Resolved::Unknown => r.warning(
                "TG0603",
                &at,
                format!(
                    "unknown operation `{}`; this entry is ignored",
                    tool.operation
                ),
            ),
        }
    }
    out
}

fn method_default(cfg: &AgentConfig, method: HttpMethod) -> Safety {
    let s = &cfg.defaults.safety;
    let declared = match method {
        HttpMethod::Get => s.get,
        HttpMethod::Put => s.put,
        HttpMethod::Post => s.post,
        HttpMethod::Delete => s.delete,
        HttpMethod::Options => s.options,
        HttpMethod::Head => s.head,
        HttpMethod::Patch => s.patch,
        HttpMethod::Trace => s.trace,
    };
    declared.unwrap_or_else(|| OperationAgentMeta::default_for(method).safety)
}

fn first<'l, 'c, T>(
    layers: &'l [Layer<'c>],
    get: impl Fn(&'c ToolConfig) -> Option<&'c T>,
) -> Option<(&'c T, &'l Origin)> {
    layers
        .iter()
        .find_map(|l| get(l.entry).map(|v| (v, &l.origin)))
}

fn operation(
    cx: &Cx<'_>,
    op: &Operation,
    layers: &[Layer<'_>],
    sensitive: &mut Sensitive<'_>,
    r: &mut Reporter<'_>,
) -> Computed {
    let id = op.id.0.as_str();
    let safety = cx.safety.get(id).copied().unwrap_or(Safety::Mutating);
    let mut marks = vec![];
    let mut meta = OperationAgentMeta {
        safety,
        idempotency: idempotency(cx.cfg, op, safety, layers),
        preview: preview(cx, op, safety, layers, r),
        ..OperationAgentMeta::default()
    };
    if let Some((c, origin)) = first(layers, |e| e.confirmation.as_ref()) {
        meta.confirmation = Some(confirmation(cx, op, c, origin, r));
    }
    if let Some((v, origin)) = first(layers, |e| e.verify.as_ref()) {
        meta.verify = verify(cx, op, v, origin, r);
    }
    for layer in layers.iter().rev() {
        let namespace = cx
            .index
            .ops
            .get(id)
            .map(|i| i.namespace.as_str())
            .unwrap_or_default();
        for (code, entry) in &layer.entry.remediation {
            if !cx.codes.get(namespace).is_some_and(|c| c.contains(code)) {
                layer.origin.warn(
                    r,
                    "TG0606",
                    "remediation",
                    &child("", code),
                    format!("`{code}` is not an error code of namespace `{namespace}`; the remediation is kept but may never match"),
                );
            }
            meta.remediation.insert(code.clone(), remediation(entry));
        }
    }
    meta.remediation_note = first(layers, |e| e.remediation_note.as_ref()).map(|(n, _)| n.clone());
    let mut response_fields = sensitive.response_fields(op);
    if let Some((resp, origin)) = first(layers, |e| e.response.as_ref()) {
        for (i, path) in resp.sensitive_fields.iter().enumerate() {
            match fields::response(cx.types, op, path) {
                Lookup::Found(owners) => marks.extend(owners),
                Lookup::Opaque => {}
                Lookup::Missing(segment) => origin.warn(
                    r,
                    "TG0607",
                    "response",
                    &format!("/sensitive_fields/{i}"),
                    format!(
                        "`{path}` names no field of the success response of `{id}` (no `{segment}`)"
                    ),
                ),
            }
            response_fields.push(path.clone());
        }
        meta.shown_once = resp.shown_once;
    }
    response_fields.sort();
    response_fields.dedup();
    meta.sensitive_response_fields = response_fields;
    meta.hidden = first(layers, |e| e.hidden.as_ref()).is_some_and(|(h, _)| *h);
    meta.compact_doc = prune::compact(op.doc.as_ref(), &prune_options(cx.cfg));
    let status = first(layers, |e| e.gate.as_ref())
        .and_then(|(g, origin)| errors::gate_status(cx, op, g, origin, r));
    Computed {
        meta,
        status,
        ambiguous: safety != Safety::ReadOnly,
        sensitive: marks,
    }
}

pub(crate) fn prune_options(cfg: &AgentConfig) -> Prune<'_> {
    let d = &cfg.disclosure.prune.descriptions;
    Prune {
        max_sentences: d
            .max_sentences
            .map_or(DEFAULT_MAX_SENTENCES, |m| m as usize),
        drop_phrases: &d.drop_phrases,
        budget_tokens: cfg.defaults.disclosure.description_budget_tokens.map_or(
            tungsten_ir::DisclosurePolicy::default().description_budget_tokens,
            |b| b,
        ) as usize,
    }
}

fn idempotency(
    cfg: &AgentConfig,
    op: &Operation,
    safety: Safety,
    layers: &[Layer<'_>],
) -> IdempotencyPolicy {
    let key_param = op
        .params
        .header
        .iter()
        .find(|p| p.role == ParamRole::IdempotencyKey);
    let policy = match first(layers, |e| e.idempotency.as_ref()) {
        Some((explicit, _)) => explicit.policy(),
        None if safety == Safety::ReadOnly => return IdempotencyPolicy::default(),
        None => match key_param.filter(|p| p.required) {
            Some(p) => IdempotencyPolicyConfig {
                policy: IdempotencyKind::CallerOwned,
                header: Some(p.wire_name.clone()),
                format: is_uuid(&p.ty).then_some(KeyFormat::Uuid),
                persist: Some(Persist::Required),
                note: None,
            },
            None => match &cfg.defaults.idempotency {
                Some(d) => d.policy(),
                None => return IdempotencyPolicy::default(),
            },
        },
    };
    let keyed = matches!(
        policy.policy,
        IdempotencyKind::Auto | IdempotencyKind::CallerOwned | IdempotencyKind::ContentHash
    );
    let header = policy.header.clone().or_else(|| {
        keyed.then(|| {
            key_param.map_or_else(
                || DEFAULT_IDEMPOTENCY_HEADER.to_string(),
                |p| p.wire_name.clone(),
            )
        })
    });
    IdempotencyPolicy {
        policy: policy.policy,
        header,
        format: policy.format.map(|f| f.as_str().to_string()),
        persist_required: policy.persist == Some(Persist::Required),
        note: policy.note,
    }
}

/// Whether a parameter type is an inline `uuid` string.
fn is_uuid(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::Inline(shape) if matches!(
            shape.as_ref(),
            tungsten_ir::Shape::Primitive {
                primitive: tungsten_ir::Primitive::String {
                    format: Some(StringFormat::Uuid)
                },
                ..
            }
        )
    )
}

fn preview(
    cx: &Cx<'_>,
    op: &Operation,
    safety: Safety,
    layers: &[Layer<'_>],
    r: &mut Reporter<'_>,
) -> IrPreview {
    let fallback = || {
        if safety == Safety::ReadOnly {
            return IrPreview::None;
        }
        match cx.cfg.defaults.preview.as_ref().map(PreviewConfig::object) {
            Some(PreviewObject {
                mode: PreviewMode::Header,
                header: Some(header),
                value: Some(value),
                ..
            }) => IrPreview::Header { header, value },
            Some(PreviewObject {
                mode: PreviewMode::None,
                ..
            }) => IrPreview::None,
            _ => IrPreview::Local,
        }
    };
    let Some((explicit, origin)) = first(layers, |e| e.preview.as_ref()) else {
        return fallback();
    };
    let p = explicit.object();
    match p.mode {
        PreviewMode::Local => IrPreview::Local,
        PreviewMode::None => IrPreview::None,
        PreviewMode::Header => IrPreview::Header {
            header: p.header.unwrap_or_default(),
            value: p.value.unwrap_or_default(),
        },
        PreviewMode::Endpoint => {
            let target = p.operation.unwrap_or_default();
            match cx.callable(&target, r, origin, "preview", "/operation") {
                Some(id) if id != op.id.0 => IrPreview::Endpoint {
                    operation: OperationId(id),
                },
                Some(_) => {
                    origin.warn(
                        r,
                        "TG0605",
                        "preview",
                        "/operation",
                        format!(
                            "`{}` cannot preview itself; the default preview is used",
                            op.id.0
                        ),
                    );
                    fallback()
                }
                None => fallback(),
            }
        }
    }
}

fn confirmation(
    cx: &Cx<'_>,
    op: &Operation,
    c: &ConfirmationConfig,
    origin: &Origin,
    r: &mut Reporter<'_>,
) -> Confirmation {
    for (i, path) in c.summary_fields.iter().enumerate() {
        request_field(
            cx,
            op,
            path,
            origin,
            "confirmation",
            &format!("/summary_fields/{i}"),
            r,
        );
    }
    if let Some(message) = &c.message {
        for name in crate::check::placeholders(message).unwrap_or_default() {
            request_field(cx, op, &name, origin, "confirmation", "/message", r);
        }
    }
    Confirmation {
        summary_fields: c.summary_fields.clone(),
        message: c.message.clone(),
    }
}

/// TG0607 when `path` names no request field of `op`.
fn request_field(
    cx: &Cx<'_>,
    op: &Operation,
    path: &str,
    origin: &Origin,
    field: &str,
    rest: &str,
    r: &mut Reporter<'_>,
) {
    if let Lookup::Missing(segment) = fields::request(cx.types, op, path) {
        origin.warn(
            r,
            "TG0607",
            field,
            rest,
            format!(
                "`{path}` names no request field of `{}` (no `{segment}`)",
                op.id.0
            ),
        );
    }
}

fn verify(
    cx: &Cx<'_>,
    op: &Operation,
    v: &VerifyConfig,
    origin: &Origin,
    r: &mut Reporter<'_>,
) -> Option<VerificationHook> {
    let target_id = cx.callable(&v.operation, r, origin, "verify", "/operation")?;
    let target = cx.ops.get(target_id.as_str())?;
    let target_safety = cx
        .safety
        .get(&target_id)
        .copied()
        .unwrap_or(Safety::Mutating);
    if target_safety != Safety::ReadOnly {
        origin.warn(
            r,
            "TG0605",
            "verify",
            "/operation",
            format!("verification hook `{target_id}` is {}, not read_only; the runtime would repeat a side effect, so the hook is dropped", safety_name(target_safety)),
        );
        return None;
    }
    for key in v.args.keys() {
        request_field(cx, target, key, origin, "verify", &child("/args", key), r);
    }
    let args = serde_json::Value::Object(v.args.clone());
    check_call_refs(cx, op, &args, origin, "/args", r);
    for (name, predicate) in [("expect", &v.expect), ("terminal", &v.terminal)] {
        let Some(predicate) = predicate else { continue };
        for path in predicate.keys() {
            if let Lookup::Missing(segment) = fields::response(cx.types, target, path) {
                origin.warn(
                    r,
                    "TG0607",
                    "verify",
                    &child(&format!("/{name}"), path),
                    format!(
                        "`{path}` names no field of the response of `{target_id}` (no `{segment}`)"
                    ),
                );
            }
        }
        check_call_refs(
            cx,
            op,
            &serde_json::Value::Object(predicate.clone()),
            origin,
            &format!("/{name}"),
            r,
        );
    }
    let predicate = |p: &Option<Predicate>| {
        p.clone()
            .map_or(serde_json::Value::Null, serde_json::Value::Object)
    };
    Some(VerificationHook {
        operation: OperationId(target_id),
        args,
        expect: predicate(&v.expect),
        terminal: predicate(&v.terminal),
        poll_interval_ms: v.poll.as_ref().and_then(|p| p.interval_ms),
        poll_budget_ms: v.poll.as_ref().and_then(|p| p.budget_ms),
    })
}

/// `$response.<path>` must name a success-response field and `$args.<path>`
/// a request field of the verified operation.
fn check_call_refs(
    cx: &Cx<'_>,
    op: &Operation,
    value: &serde_json::Value,
    origin: &Origin,
    base: &str,
    r: &mut Reporter<'_>,
) {
    let (refs, _) = expr::references(value);
    for (at, reference) in refs {
        if reference.path.is_empty() {
            continue;
        }
        let path = reference.path.join(".");
        let lookup = match reference.root.as_str() {
            "response" => fields::response(cx.types, op, &path),
            _ => fields::request(cx.types, op, &path),
        };
        if let Lookup::Missing(segment) = lookup {
            let what = if reference.root == "response" {
                "success response"
            } else {
                "request"
            };
            origin.warn(
                r,
                "TG0607",
                "verify",
                &format!("{base}{at}"),
                format!(
                    "`${}.{path}` names no field of the {what} of `{}` (no `{segment}`)",
                    reference.root, op.id.0
                ),
            );
        }
    }
}

pub(crate) fn remediation(entry: &RemediationConfig) -> Remediation {
    Remediation {
        category: entry.category.map(|c| c.as_str().to_string()),
        text: entry.text.clone(),
        retryable: entry.retryable,
        next_action: entry.next_action.clone(),
    }
}

pub(crate) fn safety_name(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "read_only",
        Safety::Mutating => "mutating",
        Safety::Destructive => "destructive",
        Safety::Irreversible => "irreversible",
    }
}

/// Mark `field` of the named type `id` sensitive, when the type is a record
/// declaring it.
fn mark_sensitive(types: &mut TypeTable, id: &TypeId, field: &str) {
    if let Ok(i) = types.types.binary_search_by(|t| t.id.cmp(id))
        && let tungsten_ir::Shape::Record { fields, .. } = &mut types.types[i].shape
    {
        for f in fields.iter_mut().filter(|f| f.wire_name == field) {
            f.sensitive = true;
        }
    }
}
