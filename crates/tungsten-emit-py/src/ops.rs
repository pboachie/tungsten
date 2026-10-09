// SPDX-License-Identifier: AGPL-3.0-only
//! Operation shapes (keyword arguments, bodies, success types) and
//! `<module>/_descriptors.py`: one `OperationDescriptor` per callable
//! operation and the `ApiDescriptor`.
//!
//! The arguments follow `tungsten_emit::args::args_layout` (which
//! parameters are arguments, the body content, whether the body is
//! merged), keyed by Python name: parameters by their name rendered for
//! Python, merged body fields by their model attribute name (with a
//! numeric suffix when that is a parameter's name), a whole body as `body`.

use std::collections::BTreeSet;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::Writer;
use tungsten_emit::args::{BodyArg, args_layout, resolve as args_resolve};
use tungsten_ir::naming::Role;
use tungsten_ir::{
    ApiKeyIn, AuthScheme, BodyContent, BodyEncoding, CompositePart, HttpMethod, IdempotencyKind,
    Ir, Jitter, Operation, OperationAgentMeta, OperationStatus, PaginationStyle, Param, ParamRole,
    ParamStyle, Presence, PreviewMode, Remediation, ResponseKind, RetryPolicy, Retryable, Safety,
    Shape, StatusMatch, TypeRef,
};

use crate::options::Options;
use crate::plan::{OpInfo, Plan, field_names, no_leading_digit, unique};
use crate::py::{
    Py, PyImports, after_imports, doc_summary, doc_text, docstring, paragraphs, two_blank,
};
use crate::types::{Cx, Flavor, PyTy, Uses, field_doc, record_of, resolve, shape_notes};

/// The error categories of the runtime contract (`Category` in types.py).
pub(crate) const CATEGORIES: &[&str] = &[
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

/// Keyword arguments every operation method takes besides its arguments,
/// and the ambiguous names `I`, `O` and `l` (ruff E741).
pub(crate) const ARG_RESERVED: &[&str] = &["I", "O", "l", "opts", "self"];

/// One keyword argument of an operation.
#[derive(Debug, Clone)]
pub(crate) struct ArgField {
    pub key: String,
    /// Annotation in signatures (hint flavor, `| Unset` when optional).
    pub hint: String,
    /// The type the request validator checks (schema flavor, no `Unset`).
    pub schema: String,
    pub optional: bool,
    /// Whether the validated value is dumped JSON-ready (`False` for
    /// bytes, text, form and multipart bodies).
    pub json: bool,
    pub doc: String,
}

/// How the body travels in the arguments (`BodyDescriptor.shape`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyShape {
    /// (argument, wire name) of each merged field of a JSON record.
    Merged(Vec<(String, String)>),
    /// The whole body is the argument.
    Arg(String),
}

#[derive(Debug, Clone)]
pub(crate) struct BodyPlan<'a> {
    pub content: &'a BodyContent,
    pub required: bool,
    pub shape: BodyShape,
}

/// One parameter as the descriptor lists it.
#[derive(Debug, Clone)]
pub(crate) struct ParamPlan<'a> {
    pub name: String,
    pub location: &'static str,
    pub param: &'a Param,
}

/// Everything about one operation's call signature.
#[derive(Debug, Clone)]
pub(crate) struct OpShape<'a> {
    pub params: Vec<ParamPlan<'a>>,
    pub body: Option<BodyPlan<'a>>,
    pub fields: Vec<ArgField>,
    /// The value type of `Result[T]` (hint flavor).
    pub success: PyTy,
    /// A success may have no body (`None` in the result).
    pub bodiless: bool,
    /// The response validator's type when every success body is JSON.
    pub response_schema: Option<String>,
    /// Item type of `<method>_pages` (hint flavor).
    pub page_item: Option<PyTy>,
    /// The page item validator's type (schema flavor), when the items are
    /// typed (`OperationDescriptor["page_item"]`: the runtime validates each
    /// item of a page into it).
    pub page_item_schema: Option<String>,
    /// Imports of the arguments' hint-flavor types (signatures).
    pub hint_uses: Uses,
    /// Imports of the result types (`Result[T]`, page items).
    pub result_uses: Uses,
    /// Imports of the schema-flavor types (validators).
    pub schema_uses: Uses,
}

/// Whether a parameter is a keyword argument. Idempotency keys come from
/// `CallOptions["idempotency_key"]`; origins and auth parameters from the
/// auth profile.
pub(crate) fn is_arg(p: &Param) -> bool {
    matches!(p.role, ParamRole::Plain | ParamRole::DryRun)
}

/// Whether the operation gets a `preview_<method>`.
pub(crate) fn has_preview(op: &Operation) -> bool {
    op.agent.safety != Safety::ReadOnly && op.agent.preview != PreviewMode::None
}

/// `base` with the smallest numeric word (`body_2`, as
/// `naming::disambiguate` spells it in snake case) no name in `taken` uses;
/// `base` itself when free.
fn free_name(taken: &[String], base: &str) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    (2u64..)
        .map(|n| format!("{base}_{n}"))
        .find(|c| !taken.contains(c))
        .unwrap_or_else(|| base.to_string())
}

/// The shape of an operation's call.
pub(crate) fn op_shape<'a>(plan: &Plan<'a>, info: &OpInfo<'a>) -> OpShape<'a> {
    let op = info.op;
    let hint = Cx::new(plan, None);
    let result = Cx::new(plan, None);
    let schema = Cx::new(plan, None);
    let layout = args_layout(plan.ir, op);
    let located: Vec<_> = layout.params.iter().chain(&layout.supplied).collect();
    let keys = unique(
        ARG_RESERVED,
        &located
            .iter()
            .map(|a| no_leading_digit(&a.param.name.words, "param"))
            .collect::<Vec<_>>(),
        Role::Param,
    );
    let params: Vec<ParamPlan<'a>> = located
        .iter()
        .zip(keys)
        .map(|(a, name)| ParamPlan {
            name,
            location: a.location.as_str(),
            param: a.param,
        })
        .collect();
    let mut taken: Vec<String> = ARG_RESERVED.iter().map(|s| s.to_string()).collect();
    taken.extend(params.iter().map(|p| p.name.clone()));

    let mut fields: Vec<ArgField> = vec![];
    for p in params.iter().filter(|p| is_arg(p.param)) {
        let value = hint.ty(&p.param.ty, Flavor::Hint);
        let mut notes = vec![doc_text(p.param.doc.as_ref())];
        if p.name != p.param.wire_name {
            notes.push(format!(
                "Sent as the `{}` {} parameter.",
                p.param.wire_name, p.location
            ));
        }
        if let Some(s) = resolve(plan, &p.param.ty) {
            notes.extend(shape_notes(s));
        }
        if p.param.deprecated {
            notes.push("Deprecated.".into());
        }
        let optional = !p.param.required;
        fields.push(ArgField {
            key: p.name.clone(),
            hint: if optional {
                hint.omittable(&value).text()
            } else {
                value.text()
            },
            schema: schema.value(&schema.ty(&p.param.ty, Flavor::Schema)),
            optional,
            json: true,
            doc: paragraphs(notes),
        });
    }

    let required = layout.body_required;
    let body_doc = op.body.as_ref().and_then(|b| b.doc.as_ref());
    let body = layout.body.map(|arg| match arg {
        BodyArg::Merged {
            content,
            fields: merged,
            ..
        } => {
            let all: &[tungsten_ir::Field] = match args_resolve(plan.ir, &content.ty) {
                Some(Shape::Record { fields, .. }) => fields,
                _ => &[],
            };
            let attrs = field_names(all.iter().map(|f| &f.name));
            let mut pairs = vec![];
            for f in merged {
                let attr = all
                    .iter()
                    .position(|x| std::ptr::eq(x, f))
                    .and_then(|i| attrs.get(i).cloned())
                    .unwrap_or_else(|| f.wire_name.clone());
                let key = free_name(&taken, &attr);
                taken.push(key.clone());
                let optional =
                    matches!(f.presence, Presence::Optional | Presence::OptionalNullable);
                let value = hint.field_value(f, Flavor::Hint);
                fields.push(ArgField {
                    key: key.clone(),
                    hint: if optional {
                        hint.omittable(&value).text()
                    } else {
                        value.text()
                    },
                    schema: schema.value(&schema.field_value(f, Flavor::Schema)),
                    optional,
                    json: true,
                    doc: field_doc(plan, f, &key),
                });
                pairs.push((key, f.wire_name.clone()));
            }
            BodyPlan {
                content,
                required,
                shape: BodyShape::Merged(pairs),
            }
        }
        BodyArg::Arg { content, .. } => {
            let name = free_name(&taken, "body");
            taken.push(name.clone());
            let (value, schema_text, json) = match content.encoding {
                BodyEncoding::Bytes => (PyTy::one("bytes"), schema.internal("Binary"), false),
                BodyEncoding::Text => (PyTy::one("str"), "str".to_string(), false),
                BodyEncoding::Json | BodyEncoding::Form | BodyEncoding::Multipart => (
                    hint.ty(&content.ty, Flavor::Hint),
                    schema.value(&schema.ty(&content.ty, Flavor::Schema)),
                    content.encoding == BodyEncoding::Json,
                ),
            };
            fields.push(ArgField {
                key: name.clone(),
                hint: if required {
                    value.text()
                } else {
                    hint.omittable(&value).text()
                },
                schema: schema_text,
                optional: !required,
                json,
                doc: paragraphs([
                    doc_text(body_doc),
                    format!("The request body, sent as `{}`.", content.media_type),
                ]),
            });
            BodyPlan {
                content,
                required,
                shape: BodyShape::Arg(name),
            }
        }
    });

    // Success value.
    let mut tys: Vec<PyTy> = vec![];
    let mut bodiless = false;
    let mut json: Vec<(PyTy, TypeRef)> = vec![];
    let mut all_json = true;
    for r in op
        .responses
        .iter()
        .filter(|r| r.kind == ResponseKind::Success)
    {
        let Some(c) = r
            .content
            .iter()
            .find(|c| c.encoding == BodyEncoding::Json)
            .or_else(|| r.content.first())
        else {
            bodiless = true;
            continue;
        };
        match c.encoding {
            BodyEncoding::Json => {
                tys.push(result.ty(&c.ty, Flavor::Hint));
                let s = schema.ty(&c.ty, Flavor::Schema);
                if !json.iter().any(|(t, _)| *t == s) {
                    json.push((s, c.ty.clone()));
                }
            }
            BodyEncoding::Text => {
                all_json = false;
                tys.push(PyTy::one("str"));
            }
            BodyEncoding::Bytes => {
                all_json = false;
                tys.push(PyTy::one("bytes"));
            }
            BodyEncoding::Form | BodyEncoding::Multipart => {
                all_json = false;
                tys.push(result.ty(&c.ty, Flavor::Hint));
            }
        }
    }
    let success = match (tys.is_empty(), bodiless) {
        (true, true) => PyTy::one("None"),
        (true, false) => {
            result.uses.borrow_mut().typing.insert("Any");
            PyTy::any()
        }
        (false, false) => PyTy::union(tys),
        (false, true) => PyTy::union(tys).nullable(),
    };
    let response_schema = (all_json && !json.is_empty()).then(|| {
        let joined = if json.len() == 1 {
            json[0].0.clone()
        } else {
            schema.uses.borrow_mut().typing.insert("Annotated");
            schema.uses.borrow_mut().pydantic.insert("Field");
            PyTy::one(format!(
                "Annotated[{}, Field(union_mode=\"left_to_right\")]",
                PyTy::union(json.iter().map(|(s, _)| s.clone())).text()
            ))
        };
        schema.value(&if bodiless { joined.nullable() } else { joined })
    });
    let items = op.pagination.as_ref().map(|p| {
        json.first()
            .and_then(|(_, r)| items_ref(plan, r, &p.items_field))
    });
    let page_item = items.map(|items| {
        items.map_or_else(
            || {
                result.uses.borrow_mut().typing.insert("Any");
                PyTy::any()
            },
            |items| result.ty(items, Flavor::Hint),
        )
    });
    let page_item_schema = items
        .flatten()
        .map(|items| schema.ty(items, Flavor::Schema))
        .filter(|t| !t.is_any())
        .map(|t| schema.value(&t));
    if success.is_any() || page_item.as_ref().is_some_and(PyTy::is_any) {
        result.uses.borrow_mut().typing.insert("Any");
    }
    OpShape {
        params,
        body,
        fields,
        success,
        bodiless,
        response_schema,
        page_item,
        page_item_schema,
        hint_uses: hint.uses.into_inner(),
        result_uses: result.uses.into_inner(),
        schema_uses: schema.uses.into_inner(),
    }
}

/// The item type of a page: the items of the array at `items_field` of the
/// success body (the body itself when the field is empty).
fn items_ref<'p>(plan: &'p Plan<'_>, body: &'p TypeRef, items_field: &str) -> Option<&'p TypeRef> {
    let list = if items_field.is_empty() {
        body
    } else {
        let (fields, _) = record_of(plan, body)?;
        &fields.iter().find(|f| f.wire_name == items_field)?.ty
    };
    match resolve(plan, list)? {
        Shape::Array { items, .. } => Some(items),
        _ => None,
    }
}

pub(crate) fn method_str(m: HttpMethod) -> &'static str {
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

pub(crate) fn safety_str(s: Safety) -> &'static str {
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

pub(crate) fn idempotency_str(k: IdempotencyKind) -> &'static str {
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
    }
}

fn encoding_str(e: BodyEncoding) -> &'static str {
    match e {
        BodyEncoding::Json => "json",
        BodyEncoding::Form => "form",
        BodyEncoding::Multipart => "multipart",
        BodyEncoding::Bytes => "bytes",
        BodyEncoding::Text => "text",
    }
}

fn status_py(s: StatusMatch) -> Option<Py> {
    match s {
        StatusMatch::Exact(n) => Some(Py::num(n)),
        StatusMatch::Range(d @ 1..=5) => Some(Py::str(&format!("{d}XX"))),
        StatusMatch::Range(_) => None,
        StatusMatch::Default => Some(Py::str("default")),
    }
}

/// A remediation entry; absent fields are omitted, and a category outside
/// the runtime's closed set is dropped.
fn remediation_py(r: &Remediation) -> Py {
    let mut entries: Vec<(&str, Py)> = vec![];
    if let Some(c) = r.category.as_deref().filter(|c| CATEGORIES.contains(c)) {
        entries.push(("category", Py::str(c)));
    }
    if let Some(t) = &r.text {
        entries.push(("text", Py::str(t)));
    }
    if let Some(rt) = r.retryable {
        entries.push(("retryable", Py::str(retryable_str(rt))));
    }
    if let Some(n) = &r.next_action {
        entries.push(("next_action", Py::str(n)));
    }
    Py::dict(entries)
}

fn json_object(v: &serde_json::Value) -> Py {
    match v {
        serde_json::Value::Object(_) => Py::json(v),
        _ => Py::Dict(vec![]),
    }
}

fn opt_num(n: Option<u64>) -> Py {
    n.map_or_else(Py::none, Py::num)
}

/// Dotted args paths of the request body fields marked sensitive
/// (`x-agent-sensitive`): merged fields by their argument name, an
/// arg-shaped body under its argument, nested fields by wire name. Each
/// named type is walked once per path, so cycles end; array items are not
/// indexed (`AgentMeta.sensitive_request_fields`).
pub(crate) fn sensitive_request_fields(plan: &Plan<'_>, shape: &OpShape<'_>) -> Vec<String> {
    let Some(body) = &shape.body else {
        return vec![];
    };
    if matches!(
        body.content.encoding,
        BodyEncoding::Bytes | BodyEncoding::Text
    ) {
        return vec![];
    }
    let mut out = vec![];
    let mut active = BTreeSet::new();
    match &body.shape {
        BodyShape::Merged(pairs) => {
            if let Some(Shape::Record { fields, .. }) = args_resolve(plan.ir, &body.content.ty) {
                for (arg, wire) in pairs {
                    let Some(f) = fields.iter().find(|f| &f.wire_name == wire) else {
                        continue;
                    };
                    if f.sensitive {
                        out.push(arg.clone());
                    } else {
                        sensitive_paths(plan, &f.ty, arg, &mut active, &mut out);
                    }
                }
            }
        }
        BodyShape::Arg(arg) => sensitive_paths(plan, &body.content.ty, arg, &mut active, &mut out),
    }
    out.sort();
    out.dedup();
    out
}

fn sensitive_paths<'p>(
    plan: &'p Plan<'_>,
    ty: &'p TypeRef,
    prefix: &str,
    active: &mut BTreeSet<&'p tungsten_ir::TypeId>,
    out: &mut Vec<String>,
) {
    // Nesting is bounded so wide type graphs that share types stay cheap.
    if prefix.matches('.').count() >= 16 || out.len() >= 256 {
        return;
    }
    if let TypeRef::Named(id) = ty
        && !active.insert(id)
    {
        return;
    }
    match resolve(plan, ty) {
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
    if let TypeRef::Named(id) = ty {
        active.remove(id);
    }
}

fn agent_py(a: &OperationAgentMeta, sensitive_request: &[String]) -> Py {
    let idem = &a.idempotency;
    let preview = match &a.preview {
        PreviewMode::Local => Py::dict(vec![("mode", Py::str("local"))]),
        PreviewMode::Header { header, value } => Py::dict(vec![
            ("mode", Py::str("header")),
            ("header", Py::str(header)),
            ("value", Py::str(value)),
        ]),
        PreviewMode::Endpoint { operation } => Py::dict(vec![
            ("mode", Py::str("endpoint")),
            ("operation", Py::str(&operation.0)),
        ]),
        PreviewMode::None => Py::dict(vec![("mode", Py::str("none"))]),
    };
    let confirmation = a.confirmation.as_ref().map_or_else(Py::none, |c| {
        Py::dict(vec![
            ("summary_fields", Py::strs(&c.summary_fields)),
            ("message", Py::opt_str(c.message.as_deref())),
        ])
    });
    let verify = a.verify.as_ref().map_or_else(Py::none, |v| {
        Py::dict(vec![
            ("operation", Py::str(&v.operation.0)),
            ("args", json_object(&v.args)),
            ("expect", json_object(&v.expect)),
            ("terminal", json_object(&v.terminal)),
            ("poll_interval_ms", opt_num(v.poll_interval_ms)),
            ("poll_budget_ms", opt_num(v.poll_budget_ms)),
        ])
    });
    let mut entries = vec![
        ("safety", Py::str(safety_str(a.safety))),
        (
            "idempotency",
            Py::dict(vec![
                ("policy", Py::str(idempotency_str(idem.policy))),
                ("header", Py::opt_str(idem.header.as_deref())),
                ("format", Py::opt_str(idem.format.as_deref())),
                ("persist_required", Py::bool(idem.persist_required)),
                ("note", Py::opt_str(idem.note.as_deref())),
            ]),
        ),
        ("preview", preview),
        ("confirmation", confirmation),
        ("verify", verify),
        (
            "remediation",
            Py::Dict(
                a.remediation
                    .iter()
                    .map(|(code, r)| (code.clone(), remediation_py(r)))
                    .collect(),
            ),
        ),
        (
            "remediation_note",
            Py::opt_str(a.remediation_note.as_deref()),
        ),
        (
            "sensitive_response_fields",
            Py::strs(&a.sensitive_response_fields),
        ),
    ];
    if !sensitive_request.is_empty() {
        entries.push(("sensitive_request_fields", Py::strs(sensitive_request)));
    }
    entries.push(("shown_once", Py::bool(a.shown_once)));
    Py::dict(entries)
}

fn pagination_py(info: &OpInfo<'_>, shape: &OpShape<'_>) -> Py {
    let Some(p) = &info.op.pagination else {
        return Py::none();
    };
    // Request parameters are named as arguments, like `ParamDescriptor.name`.
    let arg = |wire: &str| {
        shape
            .params
            .iter()
            .find(|pp| pp.param.wire_name == wire && pp.location == "query")
            .map_or_else(|| wire.to_string(), |pp| pp.name.clone())
    };
    let items = ("items_field", Py::str(&p.items_field));
    match &p.style {
        PaginationStyle::Cursor {
            request_param,
            response_field,
        } => Py::dict(vec![
            ("style", Py::str("cursor")),
            ("request_param", Py::str(&arg(request_param))),
            ("response_field", Py::str(response_field)),
            items,
            (
                "page_size_param",
                p.page_size_param
                    .as_deref()
                    .map_or_else(Py::none, |s| Py::str(&arg(s))),
            ),
        ]),
        PaginationStyle::Offset {
            offset_param,
            limit_param,
        } => Py::dict(vec![
            ("style", Py::str("offset")),
            ("offset_param", Py::str(&arg(offset_param))),
            ("limit_param", Py::str(&arg(limit_param))),
            items,
        ]),
        PaginationStyle::Page {
            page_param,
            size_param,
        } => Py::dict(vec![
            ("style", Py::str("page")),
            ("page_param", Py::str(&arg(page_param))),
            ("size_param", Py::str(&arg(size_param))),
            items,
        ]),
        PaginationStyle::LinkHeader => Py::dict(vec![("style", Py::str("link_header")), items]),
    }
}

/// The error code field of the operation's namespace error model.
fn error_code_field<'i>(ir: &'i Ir, ns: &str) -> Option<&'i str> {
    ir.namespaces
        .iter()
        .find(|n| n.name.wire == ns)
        .and_then(|n| n.errors.code_field.as_deref())
}

/// The request validator expression of an operation.
fn request_py(shape: &OpShape<'_>) -> String {
    if shape.fields.is_empty() {
        return "_internal.Request(lambda: {})".to_string();
    }
    let mut out = String::from("_internal.Request(\n    lambda: {\n");
    for f in &shape.fields {
        let mut args = vec![
            f.schema.clone(),
            format!("required={}", py_bool(!f.optional)),
        ];
        if !f.json {
            args.push("json=False".into());
        }
        out.push_str(&format!(
            "        {}: _internal.Arg({}),\n",
            crate::py::string_lit(&f.key),
            args.join(", ")
        ));
    }
    out.push_str("    }\n)");
    out
}

fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// The descriptor literal of one operation.
pub(crate) fn descriptor_py(plan: &Plan<'_>, info: &OpInfo<'_>, shape: &OpShape<'_>) -> Py {
    let op = info.op;
    let params = Py::List(
        shape
            .params
            .iter()
            .map(|p| {
                Py::dict(vec![
                    ("name", Py::str(&p.name)),
                    ("wire", Py::str(&p.param.wire_name)),
                    ("location", Py::str(p.location)),
                    ("required", Py::bool(p.param.required)),
                    ("style", Py::str(style_str(p.param.style))),
                    ("explode", Py::bool(p.param.explode)),
                    ("role", Py::str(role_str(p.param.role))),
                ])
            })
            .collect(),
    );
    let body = shape.body.as_ref().map_or_else(Py::none, |b| {
        let shape_py = match &b.shape {
            BodyShape::Merged(pairs) => Py::dict(vec![
                ("kind", Py::str("merged")),
                (
                    "fields",
                    Py::List(
                        pairs
                            .iter()
                            .map(|(arg, wire)| {
                                Py::dict(vec![("arg", Py::str(arg)), ("wire", Py::str(wire))])
                            })
                            .collect(),
                    ),
                ),
            ]),
            BodyShape::Arg(arg) => Py::dict(vec![("kind", Py::str("arg")), ("arg", Py::str(arg))]),
        };
        Py::dict(vec![
            ("media_type", Py::str(&b.content.media_type)),
            ("encoding", Py::str(encoding_str(b.content.encoding))),
            ("required", Py::bool(b.required)),
            ("shape", shape_py),
        ])
    });
    let responses = Py::List(
        op.responses
            .iter()
            .filter_map(|r| {
                let kind = match r.kind {
                    ResponseKind::Success => "success",
                    ResponseKind::Error => "error",
                    ResponseKind::Ambiguous => "ambiguous",
                };
                Some(Py::dict(vec![
                    ("status", status_py(r.status)?),
                    ("kind", Py::str(kind)),
                    (
                        "media_type",
                        Py::opt_str(r.content.first().map(|c| c.media_type.as_str())),
                    ),
                ]))
            })
            .collect(),
    );
    let security = Py::List(
        op.security
            .iter()
            .map(|req| Py::strs(req.all_of.iter().map(|s| s.scheme.as_str())))
            .collect(),
    );
    let rpc = op.rpc.as_ref().map_or_else(Py::none, |r| {
        let mut entries = vec![
            ("field", Py::str(&r.discriminator_field)),
            ("value", Py::str(&r.discriminator_value)),
            ("params_field", Py::str(&r.params_field)),
        ];
        // `constants` is optional in the contract: present only when the
        // envelope has constant members (JSON-RPC `jsonrpc`).
        if !r.constants.is_empty() {
            entries.push((
                "constants",
                Py::json(&serde_json::Value::Object(
                    r.constants
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )),
            ));
        }
        Py::dict(entries)
    });
    let status = match &op.status {
        OperationStatus::Gated { gate } => Py::dict(vec![
            ("kind", Py::str("gated")),
            ("env_var", Py::str(&gate.env_var)),
            ("disabled_status", Py::num(gate.disabled_status)),
        ]),
        OperationStatus::Implemented | OperationStatus::Planned { .. } => {
            Py::dict(vec![("kind", Py::str("implemented"))])
        }
    };
    let mut entries = vec![
        ("id", Py::str(&op.id.0)),
        ("method", Py::str(method_str(op.method))),
        ("path", Py::str(&op.path.raw)),
        ("params", params),
        ("body", body),
        ("responses", responses),
        ("security", security),
        ("pagination", pagination_py(info, shape)),
        ("rpc", rpc),
        (
            "error_code_field",
            Py::opt_str(error_code_field(plan.ir, &info.ns)),
        ),
        ("status", status),
        (
            "agent",
            agent_py(&op.agent, &sensitive_request_fields(plan, shape)),
        ),
        ("request", Py::Raw(request_py(shape))),
    ];
    if let Some(s) = &shape.response_schema {
        entries.push((
            "response",
            Py::Raw(format!("_internal.Response(lambda: {s})")),
        ));
    }
    if let Some(s) = &shape.page_item_schema {
        entries.push((
            "page_item",
            Py::Raw(format!("_internal.Response(lambda: {s})")),
        ));
    }
    let summary = op_summary(op);
    entries.push((
        "summary",
        Py::opt_str((!summary.is_empty()).then_some(summary.as_str())),
    ));
    Py::dict(entries)
}

/// `OperationDescriptor["summary"]`: the pruned agent doc (`compact_doc`),
/// else the spec summary, collapsed to one line; empty when neither exists.
pub(crate) fn op_summary(op: &Operation) -> String {
    let text = if op.agent.compact_doc.trim().is_empty() {
        doc_summary(op.doc.as_ref())
    } else {
        op.agent.compact_doc.clone()
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// TG0732 for each OpenID Connect scheme: the SDK sends its credential as
/// a bearer token.
pub(crate) fn auth_notes(ir: &Ir) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for s in &ir.auth {
        if let AuthScheme::OpenIdConnect { name, .. } = s {
            diags.push(Diagnostic::info(
                "TG0732",
                format!(
                    "OpenID Connect scheme `{name}` is sent as a bearer token; the Python SDK does not run discovery or obtain tokens"
                ),
            ));
        }
    }
    diags
}

/// Auth scheme descriptors (`ApiDescriptor["auth"]`), in IR order.
fn auth_py(ir: &Ir) -> Py {
    let mut out = vec![];
    for s in &ir.auth {
        out.push(match s {
            AuthScheme::ApiKey {
                name,
                location,
                wire_name,
                ..
            } => Py::dict(vec![
                ("kind", Py::str("api_key")),
                ("name", Py::str(name)),
                (
                    "location",
                    Py::str(match location {
                        ApiKeyIn::Header => "header",
                        ApiKeyIn::Query => "query",
                        ApiKeyIn::Cookie => "cookie",
                    }),
                ),
                ("wire", Py::str(wire_name)),
            ]),
            AuthScheme::HttpBearer { name, prefix, .. } => bearer_py(name, prefix.as_deref()),
            AuthScheme::OpenIdConnect { name, .. } => bearer_py(name, None),
            AuthScheme::HttpBasic { name, .. } => Py::dict(vec![
                ("kind", Py::str("http_basic")),
                ("name", Py::str(name)),
            ]),
            AuthScheme::OAuth2 { name, flows, .. } => {
                let token_url = flows
                    .iter()
                    .find(|f| f.kind == "clientCredentials" && f.token_url.is_some())
                    .or_else(|| flows.iter().find(|f| f.token_url.is_some()))
                    .and_then(|f| f.token_url.as_deref());
                let scopes: BTreeSet<&str> = flows
                    .iter()
                    .flat_map(|f| f.scopes.keys().map(String::as_str))
                    .collect();
                Py::dict(vec![
                    ("kind", Py::str("oauth2")),
                    ("name", Py::str(name)),
                    ("token_url", Py::opt_str(token_url)),
                    ("scopes", Py::strs(scopes)),
                ])
            }
            AuthScheme::Composite {
                name,
                satisfies,
                parts,
            } => Py::dict(vec![
                ("kind", Py::str("composite")),
                ("name", Py::str(name)),
                ("satisfies", Py::strs(satisfies)),
                (
                    "parts",
                    Py::List(
                        parts
                            .iter()
                            .map(|p| match p {
                                CompositePart::Cookie { name } => Py::dict(vec![
                                    ("kind", Py::str("cookie")),
                                    ("name", Py::str(name)),
                                ]),
                                CompositePart::Header {
                                    name,
                                    equals_cookie,
                                    from_config,
                                    mutation_only,
                                } => Py::dict(vec![
                                    ("kind", Py::str("header")),
                                    ("name", Py::str(name)),
                                    ("equals_cookie", Py::opt_str(equals_cookie.as_deref())),
                                    ("from_config", Py::opt_str(from_config.as_deref())),
                                    ("mutation_only", Py::bool(*mutation_only)),
                                ]),
                                CompositePart::Bearer { prefix, .. } => Py::dict(vec![
                                    ("kind", Py::str("bearer")),
                                    ("prefix", Py::opt_str(prefix.as_deref())),
                                ]),
                            })
                            .collect(),
                    ),
                ),
            ]),
        });
    }
    Py::List(out)
}

/// An `http_bearer` descriptor. `prefix` comes from the auth profile; the
/// spec's `bearerFormat` only documents the token and is never a prefix.
fn bearer_py(name: &str, prefix: Option<&str>) -> Py {
    Py::dict(vec![
        ("kind", Py::str("http_bearer")),
        ("name", Py::str(name)),
        ("prefix", Py::opt_str(prefix)),
    ])
}

/// `ApiDescriptor["non_json"]` from `AgentModel.non_json`, in manifest
/// order; an entry whose category is outside the runtime's set is left out.
fn non_json_py(ir: &Ir) -> Py {
    Py::List(
        ir.agent
            .non_json
            .iter()
            .filter(|e| CATEGORIES.contains(&e.category.as_str()))
            .map(|e| {
                Py::dict(vec![
                    ("status", Py::num(e.status)),
                    ("media", Py::str(&e.media)),
                    ("category", Py::str(&e.category)),
                    ("retryable", Py::str(retryable_str(e.retryable))),
                    ("text", Py::opt_str(e.text.as_deref())),
                ])
            })
            .collect(),
    )
}

/// One tier of `ApiDescriptor["retries"]` (agent.yml `defaults.retries`).
fn retry_py(policy: &RetryPolicy, honor_retry_after: bool) -> Py {
    let jitter = match policy.jitter {
        Jitter::None => "none",
        Jitter::Full => "full",
        Jitter::Equal => "equal",
    };
    Py::dict(vec![
        ("max", Py::num(policy.max)),
        ("base_ms", Py::num(policy.base_ms)),
        ("max_ms", Py::num(policy.max_ms)),
        ("jitter", Py::str(jitter)),
        ("honor_retry_after", Py::bool(honor_retry_after)),
    ])
}

/// The `ApiDescriptor`: every IR field the runtime reads, in one place.
pub(crate) fn api_py(plan: &Plan<'_>, opts: &Options) -> Py {
    let ir = plan.ir;
    let retries = &ir.agent.retries;
    Py::dict(vec![
        ("name", Py::str(&ir.api.name.wire)),
        ("version", Py::str(&opts.version)),
        ("tungsten_version", Py::str(&ir.generator.tungsten_version)),
        (
            "servers",
            Py::strs(ir.api.servers.iter().map(|s| s.url.as_str())),
        ),
        ("auth", auth_py(ir)),
        (
            "error_codes",
            Py::Dict(
                ir.agent
                    .error_codes
                    .iter()
                    .map(|(code, r)| (code.clone(), remediation_py(r)))
                    .collect(),
            ),
        ),
        (
            "ambiguous_statuses",
            Py::List(
                ir.agent
                    .ambiguous_statuses
                    .iter()
                    .map(|s| Py::num(*s))
                    .collect(),
            ),
        ),
        ("non_json", non_json_py(ir)),
        (
            "gates",
            Py::Dict(
                ir.agent
                    .gates
                    .iter()
                    .map(|(k, v)| (k.clone(), Py::str(v)))
                    .collect(),
            ),
        ),
        (
            "retries",
            Py::dict(vec![
                (
                    "read_only",
                    retry_py(&retries.read_only, retries.honor_retry_after),
                ),
                (
                    "mutating",
                    retry_py(&retries.mutating, retries.honor_retry_after),
                ),
            ]),
        ),
    ])
}

/// The source of `<module>/_descriptors.py`.
pub(crate) fn descriptors_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    opts: &Options,
    header: &str,
) -> String {
    let mut body = Writer::new("    ");
    let mut uses = Uses::default();
    let prefix = "API: ApiDescriptor = ";
    body.line(format!(
        "{prefix}{}",
        api_py(plan, opts).render(prefix.len())
    ));
    docstring(
        &mut body,
        &format!("The `{}` API as the runtime sees it.", plan.ir.api.title),
    );
    for (info, shape) in plan.ops.iter().zip(shapes) {
        uses.merge(&shape.schema_uses);
        two_blank(&mut body);
        let prefix = format!("{}: OperationDescriptor = ", info.key);
        body.line(format!(
            "{prefix}{}",
            descriptor_py(plan, info, shape).render(prefix.len())
        ));
        docstring(
            &mut body,
            &paragraphs([
                format!("`{} {}`", method_str(info.op.method), info.op.path.raw),
                doc_summary(info.op.doc.as_ref()),
            ]),
        );
    }
    two_blank(&mut body);
    let list = Py::List(plan.ops.iter().map(|o| Py::Raw(o.key.clone())).collect());
    let prefix = "OPERATIONS: list[OperationDescriptor] = ";
    body.line(format!("{prefix}{}", list.render(prefix.len())));
    docstring(
        &mut body,
        "Every callable operation. The client registers them as `ClientOptions.operations`, so the runtime resolves operations by id (verification hooks, endpoint previews, macro steps).",
    );

    let mut imports = PyImports::default();
    imports.add("__future__", "annotations");
    imports.add("tungsten_runtime", "ApiDescriptor");
    imports.add("tungsten_runtime", "OperationDescriptor");
    // Every descriptor names `_internal.Request`.
    uses.internal |= !plan.ops.is_empty();
    uses.add_to(plan, &mut imports, ".", ".models");
    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    docstring(
        &mut w,
        "Operation descriptors: what the runtime needs to call each operation.",
    );
    w.blank();
    w.line(imports.render());
    let mut out = w.finish();
    let text = body.finish();
    let text = text.trim_start_matches('\n');
    out.push_str(after_imports(text));
    out.push_str(text);
    out
}

/// The text `field` (one of an operation's keyword arguments) contributes
/// to a docstring's `Args:` section.
pub(crate) fn arg_doc_line(f: &ArgField) -> String {
    let first = f
        .doc
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    if first.is_empty() {
        format!(
            "    {}: {}",
            f.key,
            if f.optional { "Optional." } else { "Required." }
        )
    } else {
        format!("    {}: {first}", f.key)
    }
}
