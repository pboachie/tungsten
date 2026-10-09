// SPDX-License-Identifier: AGPL-3.0-only
//! Operation shapes (args objects, bodies, success types) and
//! `src/descriptors.ts`: one `OperationDescriptor` per callable operation
//! and the `ApiDescriptor`.

use std::collections::BTreeSet;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::args::{BodyArg, args_layout};
use tungsten_emit::{CommentStyle, Imports, Writer};
use tungsten_ir::{
    ApiKeyIn, AuthScheme, BodyContent, BodyEncoding, CompositePart, HttpMethod, IdempotencyKind,
    Ir, Jitter, OperationAgentMeta, OperationStatus, PaginationStyle, Param, ParamRole, ParamStyle,
    Presence, PreviewMode, Remediation, ResponseKind, RetryPolicy, Retryable, Safety, Shape,
    StatusMatch, TypeRef,
};

use crate::models::{
    Ty, TypeCx, Uses, field_doc, record_of, resolve, shape_notes, union_of, write_imports,
};
use crate::options::Options;
use crate::plan::{OpInfo, Plan};
use crate::ts::{Js, doc_summary, doc_text, paragraphs, prop_key};

/// The error categories of the runtime contract (`Category` in types.ts).
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

/// One key of an operation's args object.
#[derive(Debug, Clone)]
pub(crate) struct ArgField {
    pub key: String,
    pub ts: String,
    /// Schema with presence applied.
    pub zod: String,
    pub optional: bool,
    /// Whether `null` is a valid value.
    pub nullable: bool,
    /// The IR type, when the value is typed by one (not for bytes and text
    /// bodies).
    pub ty: Option<TypeRef>,
    pub doc: String,
}

/// How the body travels in the args object (`BodyDescriptor.shape`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyShape {
    /// Wire-named fields of a JSON record, keys of the args object.
    Merged(Vec<String>),
    /// The whole body is `args[name]`.
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
    /// Every arg is optional, so the args parameter is too.
    pub all_optional: bool,
    /// Type of `Result<T>`'s value.
    pub success: Ty,
    /// The JSON success body type when there is exactly one.
    pub success_ref: Option<TypeRef>,
    /// Schema of the success body when every success body is JSON.
    pub response_zod: Option<String>,
    /// Item type of `pages()`.
    pub page_item: Option<Ty>,
    /// Model namespaces and helpers the args and response need.
    pub uses: Uses,
    /// Model namespaces the success and page item types name.
    pub result_namespaces: BTreeSet<String>,
}

/// Whether a parameter is a key of the args object. Idempotency keys come
/// from `CallOptions.idempotencyKey`; origins and auth parameters from the
/// auth profile.
pub(crate) fn is_arg(p: &Param) -> bool {
    matches!(p.role, ParamRole::Plain | ParamRole::DryRun)
}

/// The shape of an operation's call.
pub(crate) fn op_shape<'a>(plan: &Plan<'a>, info: &OpInfo<'a>) -> OpShape<'a> {
    let op = info.op;
    let cx = TypeCx::outside(plan);
    let mut uses = Uses::default();
    // One layout for the SDK and every manifest (tungsten_emit::args):
    // argument names, the body content and whether it is merged.
    let layout = args_layout(plan.ir, op);
    let params: Vec<ParamPlan<'a>> = layout
        .params
        .iter()
        .chain(&layout.supplied)
        .map(|a| ParamPlan {
            name: a.key.clone(),
            location: a.location.as_str(),
            param: a.param,
        })
        .collect();

    let mut fields: Vec<ArgField> = vec![];
    for p in params.iter().filter(|p| is_arg(p.param)) {
        uses.add_ref(plan, None, &p.param.ty);
        let base = cx.zod_ref(&p.param.ty);
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
            notes.push("@deprecated".into());
        }
        fields.push(ArgField {
            key: p.name.clone(),
            ts: cx.ts_ref(&p.param.ty).text,
            zod: if p.param.required {
                base
            } else {
                format!("{base}.exactOptional()")
            },
            optional: !p.param.required,
            nullable: matches!(resolve(plan, &p.param.ty), Some(Shape::Nullable { .. })),
            ty: Some(p.param.ty.clone()),
            doc: paragraphs(notes),
        });
    }

    let required = layout.body_required;
    let body_doc = op.body.as_ref().and_then(|b| b.doc.as_ref());
    let body = layout.body.map(|arg| {
        let (content, merged, key) = match arg {
            BodyArg::Merged {
                content, fields, ..
            } => (content, Some(fields), None),
            BodyArg::Arg { content, key } => (content, None, Some(key)),
        };
        match merged {
            Some(body_fields) => {
                for f in &body_fields {
                    uses.add_ref(plan, None, &f.ty);
                    fields.push(ArgField {
                        key: f.wire_name.clone(),
                        ts: cx.field_ts(f).text,
                        zod: cx.field_zod(f),
                        optional: matches!(
                            f.presence,
                            Presence::Optional | Presence::OptionalNullable
                        ),
                        nullable: matches!(
                            f.presence,
                            Presence::RequiredNullable | Presence::OptionalNullable
                        ),
                        ty: Some(f.ty.clone()),
                        doc: field_doc(plan, f),
                    });
                }
                BodyPlan {
                    content,
                    required,
                    shape: BodyShape::Merged(
                        body_fields.iter().map(|f| f.wire_name.clone()).collect(),
                    ),
                }
            }
            None => {
                let name = key.unwrap_or_else(|| "body".to_string());
                let (ts, zod, ty) = match content.encoding {
                    BodyEncoding::Bytes | BodyEncoding::Jsonl => (
                        "Uint8Array".to_string(),
                        "z.instanceof(Uint8Array)".to_string(),
                        None,
                    ),
                    BodyEncoding::Text => ("string".to_string(), "z.string()".to_string(), None),
                    BodyEncoding::Json | BodyEncoding::Form | BodyEncoding::Multipart => {
                        uses.add_ref(plan, None, &content.ty);
                        (
                            cx.ts_ref(&content.ty).text,
                            cx.zod_ref(&content.ty),
                            Some(content.ty.clone()),
                        )
                    }
                };
                fields.push(ArgField {
                    key: name.clone(),
                    ts,
                    zod: if required {
                        zod
                    } else {
                        format!("{zod}.exactOptional()")
                    },
                    optional: !required,
                    nullable: false,
                    ty,
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
        }
    });

    // Success value.
    let mut result_uses = Uses::default();
    let mut tys: Vec<Ty> = vec![];
    let mut bare = false;
    let mut json: Vec<(String, TypeRef)> = vec![];
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
            bare = true;
            continue;
        };
        match c.encoding {
            // JSON lines: the value is the array of the lines.
            BodyEncoding::Json | BodyEncoding::Jsonl => {
                let ty = c.value_type();
                uses.add_ref(plan, None, &ty);
                result_uses.add_ref(plan, None, &ty);
                tys.push(cx.ts_ref(&ty));
                let z = cx.zod_ref(&ty);
                if !json.iter().any(|(s, _)| *s == z) {
                    json.push((z, ty));
                }
            }
            BodyEncoding::Text => {
                all_json = false;
                tys.push(Ty::atom("string"));
            }
            BodyEncoding::Bytes => {
                all_json = false;
                tys.push(Ty::atom("Uint8Array"));
            }
            BodyEncoding::Form | BodyEncoding::Multipart => {
                all_json = false;
                result_uses.add_ref(plan, None, &c.ty);
                tys.push(cx.ts_ref(&c.ty));
            }
        }
    }
    let success = match (tys.is_empty(), bare) {
        (true, true) => Ty::atom("void"),
        (true, false) => Ty::atom("unknown"),
        (false, false) => union_of(tys),
        (false, true) => {
            tys.push(Ty::atom("undefined"));
            union_of(tys)
        }
    };
    let response_zod = (all_json && !json.is_empty()).then(|| {
        let schema = if json.len() == 1 {
            json[0].0.clone()
        } else {
            format!(
                "z.union([{}])",
                json.iter()
                    .map(|(z, _)| z.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        if bare {
            format!("{schema}.optional()")
        } else {
            schema
        }
    });
    let success_ref = (json.len() == 1).then(|| json[0].1.clone());
    let page_item = op.pagination.as_ref().map(|p| {
        json.first()
            .and_then(|(_, r)| items_ref(plan, r, &p.items_field))
            .map_or(Ty::atom("unknown"), |items| {
                result_uses.add_ref(plan, None, items);
                cx.ts_ref(items)
            })
    });
    let all_optional = fields.iter().all(|f| f.optional);
    OpShape {
        params,
        body,
        fields,
        all_optional,
        success,
        success_ref,
        response_zod,
        page_item,
        uses,
        result_namespaces: result_uses.namespaces,
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
        ParamRole::Constant => "constant",
    }
}

fn encoding_str(e: BodyEncoding) -> &'static str {
    match e {
        BodyEncoding::Json => "json",
        BodyEncoding::Form => "form",
        BodyEncoding::Multipart => "multipart",
        // JSON Lines is a response encoding: a request body of that media
        // type is bytes (the builder never says otherwise).
        BodyEncoding::Bytes | BodyEncoding::Jsonl => "bytes",
        BodyEncoding::Text => "text",
    }
}

fn status_js(s: StatusMatch) -> Option<Js> {
    match s {
        StatusMatch::Exact(n) => Some(Js::num(n)),
        StatusMatch::Range(d @ 1..=5) => Some(Js::str(&format!("{d}XX"))),
        StatusMatch::Range(_) => None,
        StatusMatch::Default => Some(Js::str("default")),
    }
}

/// A remediation entry; absent fields are omitted (exact optional
/// properties), and a category outside the runtime's closed set is dropped.
fn remediation_js(r: &Remediation) -> Js {
    let mut entries: Vec<(&str, Js)> = vec![];
    if let Some(c) = r.category.as_deref().filter(|c| CATEGORIES.contains(c)) {
        entries.push(("category", Js::str(c)));
    }
    if let Some(t) = &r.text {
        entries.push(("text", Js::str(t)));
    }
    if let Some(rt) = r.retryable {
        entries.push(("retryable", Js::str(retryable_str(rt))));
    }
    if let Some(n) = &r.next_action {
        entries.push(("next_action", Js::str(n)));
    }
    Js::obj(entries)
}

fn json_object(v: &serde_json::Value) -> Js {
    match v {
        serde_json::Value::Object(_) => Js::json(v),
        _ => Js::Object(vec![]),
    }
}

fn opt_num(n: Option<u64>) -> Js {
    n.map_or_else(|| Js::Raw("null".into()), Js::num)
}

/// Dotted args paths of the request body fields marked sensitive
/// (`x-agent-sensitive`): merged fields by their key, an arg-shaped body
/// under its arg name. Each named type is walked once per path, so cycles
/// end; array items are not indexed (`AgentMeta.sensitiveRequestFields`).
pub(crate) fn sensitive_request_fields(plan: &Plan<'_>, shape: &OpShape<'_>) -> Vec<String> {
    let Some(body) = &shape.body else {
        return vec![];
    };
    if matches!(
        body.content.encoding,
        BodyEncoding::Bytes | BodyEncoding::Text | BodyEncoding::Jsonl
    ) {
        return vec![];
    }
    let mut out = vec![];
    let mut active = BTreeSet::new();
    match &body.shape {
        BodyShape::Merged(keys) => {
            if let Some((fields, _)) = record_of(plan, &body.content.ty) {
                for f in fields.iter().filter(|f| keys.contains(&f.wire_name)) {
                    if f.sensitive {
                        out.push(f.wire_name.clone());
                    } else {
                        sensitive_paths(plan, &f.ty, &f.wire_name, &mut active, &mut out);
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

fn agent_js(a: &OperationAgentMeta, sensitive_request: &[String]) -> Js {
    let idem = &a.idempotency;
    let preview = match &a.preview {
        PreviewMode::Local => Js::obj(vec![("mode", Js::str("local"))]),
        PreviewMode::Header { header, value } => Js::obj(vec![
            ("mode", Js::str("header")),
            ("header", Js::str(header)),
            ("value", Js::str(value)),
        ]),
        PreviewMode::Endpoint { operation } => Js::obj(vec![
            ("mode", Js::str("endpoint")),
            ("operation", Js::str(&operation.0)),
        ]),
        PreviewMode::None => Js::obj(vec![("mode", Js::str("none"))]),
    };
    let confirmation = a.confirmation.as_ref().map_or_else(
        || Js::Raw("null".into()),
        |c| {
            Js::obj(vec![
                ("summaryFields", Js::strs(&c.summary_fields)),
                ("message", Js::opt_str(c.message.as_deref())),
            ])
        },
    );
    let verify = a.verify.as_ref().map_or_else(
        || Js::Raw("null".into()),
        |v| {
            Js::obj(vec![
                ("operation", Js::str(&v.operation.0)),
                ("args", json_object(&v.args)),
                ("expect", json_object(&v.expect)),
                ("terminal", json_object(&v.terminal)),
                ("pollIntervalMs", opt_num(v.poll_interval_ms)),
                ("pollBudgetMs", opt_num(v.poll_budget_ms)),
            ])
        },
    );
    let mut entries = vec![
        ("safety", Js::str(safety_str(a.safety))),
        (
            "idempotency",
            Js::obj(vec![
                ("policy", Js::str(idempotency_str(idem.policy))),
                ("header", Js::opt_str(idem.header.as_deref())),
                ("format", Js::opt_str(idem.format.as_deref())),
                ("persistRequired", Js::bool(idem.persist_required)),
                ("note", Js::opt_str(idem.note.as_deref())),
            ]),
        ),
        ("preview", preview),
        ("confirmation", confirmation),
        ("verify", verify),
        (
            "remediation",
            Js::Object(
                a.remediation
                    .iter()
                    .map(|(code, r)| (code.clone(), remediation_js(r)))
                    .collect(),
            ),
        ),
        (
            "remediationNote",
            Js::opt_str(a.remediation_note.as_deref()),
        ),
        (
            "sensitiveResponseFields",
            Js::strs(&a.sensitive_response_fields),
        ),
    ];
    if !sensitive_request.is_empty() {
        entries.push(("sensitiveRequestFields", Js::strs(sensitive_request)));
    }
    entries.push(("shownOnce", Js::bool(a.shown_once)));
    Js::obj(entries)
}

fn pagination_js(info: &OpInfo<'_>, shape: &OpShape<'_>) -> Js {
    let Some(p) = &info.op.pagination else {
        return Js::Raw("null".into());
    };
    // Request parameters are named as args keys, like `ParamDescriptor.name`.
    let arg = |wire: &str| {
        shape
            .params
            .iter()
            .find(|pp| pp.param.wire_name == wire && pp.location == "query")
            .map_or_else(|| wire.to_string(), |pp| pp.name.clone())
    };
    let items = ("itemsField", Js::str(&p.items_field));
    match &p.style {
        PaginationStyle::Cursor {
            request_param,
            response_field,
        } => Js::obj(vec![
            ("style", Js::str("cursor")),
            ("requestParam", Js::str(&arg(request_param))),
            ("responseField", Js::str(response_field)),
            items,
            (
                "pageSizeParam",
                p.page_size_param
                    .as_deref()
                    .map_or_else(|| Js::Raw("null".into()), |s| Js::str(&arg(s))),
            ),
        ]),
        PaginationStyle::Offset {
            offset_param,
            limit_param,
        } => Js::obj(vec![
            ("style", Js::str("offset")),
            ("offsetParam", Js::str(&arg(offset_param))),
            ("limitParam", Js::str(&arg(limit_param))),
            items,
        ]),
        PaginationStyle::Page {
            page_param,
            size_param,
        } => Js::obj(vec![
            ("style", Js::str("page")),
            ("pageParam", Js::str(&arg(page_param))),
            ("sizeParam", Js::str(&arg(size_param))),
            items,
        ]),
        PaginationStyle::LinkHeader => Js::obj(vec![("style", Js::str("link_header")), items]),
    }
}

/// The error code field of the operation's namespace error model.
fn error_code_field<'i>(ir: &'i Ir, ns: &str) -> Option<&'i str> {
    ir.namespaces
        .iter()
        .find(|n| n.name.wire == ns)
        .and_then(|n| n.errors.code_field.as_deref())
}

/// The args object type (`export type XArgs = { ... }`).
pub(crate) fn write_args_type(w: &mut Writer, info: &OpInfo<'_>, shape: &OpShape<'_>) {
    w.doc(
        CommentStyle::JsDoc,
        &format!(
            "Arguments of `{}` ({} {}).",
            info.op.id.0,
            method_str(info.op.method),
            info.op.path.raw
        ),
    );
    if shape.fields.is_empty() {
        w.line(format!(
            "export type {} = {{ [key: string]: never }};",
            info.args_type
        ));
        return;
    }
    w.line(format!("export type {} = {{", info.args_type));
    w.indent();
    for f in &shape.fields {
        w.doc(CommentStyle::JsDoc, &f.doc);
        w.line(format!(
            "{}{}: {};",
            prop_key(&f.key),
            if f.optional { "?" } else { "" },
            f.ts
        ));
    }
    w.dedent();
    w.line("};");
}

/// The descriptor constant of one operation.
fn descriptor_js(plan: &Plan<'_>, info: &OpInfo<'_>, shape: &OpShape<'_>) -> Js {
    let op = info.op;
    let params = Js::Array(
        shape
            .params
            .iter()
            .map(|p| {
                let mut members = vec![
                    ("name", Js::str(&p.name)),
                    ("wire", Js::str(&p.param.wire_name)),
                    ("in", Js::str(p.location)),
                    ("required", Js::bool(p.param.required)),
                    ("style", Js::str(style_str(p.param.style))),
                    ("explode", Js::bool(p.param.explode)),
                    ("role", Js::str(role_str(p.param.role))),
                ];
                if let Some(value) = tungsten_emit::args::constant_text(p.param) {
                    members.push(("constant", Js::str(&value)));
                }
                Js::obj(members)
            })
            .collect(),
    );
    let body = shape.body.as_ref().map_or_else(
        || Js::Raw("null".into()),
        |b| {
            let shape_js = match &b.shape {
                BodyShape::Merged(fields) => Js::obj(vec![
                    ("kind", Js::str("merged")),
                    ("fields", Js::strs(fields)),
                ]),
                BodyShape::Arg(arg) => {
                    Js::obj(vec![("kind", Js::str("arg")), ("arg", Js::str(arg))])
                }
            };
            Js::obj(vec![
                ("mediaType", Js::str(&b.content.media_type)),
                ("encoding", Js::str(encoding_str(b.content.encoding))),
                ("required", Js::bool(b.required)),
                ("shape", shape_js),
            ])
        },
    );
    let responses = Js::Array(
        op.responses
            .iter()
            .filter_map(|r| {
                let kind = match r.kind {
                    ResponseKind::Success => "success",
                    ResponseKind::Error => "error",
                    ResponseKind::Ambiguous => "ambiguous",
                };
                Some(Js::obj(vec![
                    ("status", status_js(r.status)?),
                    ("kind", Js::str(kind)),
                    (
                        "mediaType",
                        Js::opt_str(r.content.first().map(|c| c.media_type.as_str())),
                    ),
                ]))
            })
            .collect(),
    );
    let security = Js::Array(
        op.security
            .iter()
            .map(|req| Js::strs(req.all_of.iter().map(|s| s.scheme.as_str())))
            .collect(),
    );
    let rpc = op.rpc.as_ref().map_or_else(
        || Js::Raw("null".into()),
        |r| {
            let mut entries = vec![
                ("field", Js::str(&r.discriminator_field)),
                ("value", Js::str(&r.discriminator_value)),
                ("paramsField", Js::str(&r.params_field)),
            ];
            // `constants` is optional in the contract: present only when
            // the envelope has constant members (JSON-RPC `jsonrpc`).
            if !r.constants.is_empty() {
                entries.push((
                    "constants",
                    Js::json(&serde_json::Value::Object(
                        r.constants
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                    )),
                ));
            }
            Js::obj(entries)
        },
    );
    let status = match &op.status {
        OperationStatus::Gated { gate } => Js::obj(vec![
            ("kind", Js::str("gated")),
            ("envVar", Js::str(&gate.env_var)),
            ("disabledStatus", Js::num(gate.disabled_status)),
        ]),
        OperationStatus::Implemented | OperationStatus::Planned { .. } => {
            Js::obj(vec![("kind", Js::str("implemented"))])
        }
    };
    let mut request = String::from("toSchemaLike(\n  z.strictObject(");
    if shape.fields.is_empty() {
        request.push_str("{})");
    } else {
        request.push_str("{\n");
        for f in &shape.fields {
            request.push_str(&format!("    {}: {},\n", prop_key(&f.key), f.zod));
        }
        request.push_str("  })");
    }
    request.push_str(&format!(" satisfies z.ZodType<{}>,\n)", info.args_type));
    let mut entries = vec![
        ("id", Js::str(&op.id.0)),
        ("method", Js::str(method_str(op.method))),
        ("path", Js::str(&op.path.raw)),
        ("params", params),
        ("body", body),
        ("responses", responses),
        ("security", security),
        ("pagination", pagination_js(info, shape)),
        ("rpc", rpc),
        (
            "errorCodeField",
            Js::opt_str(error_code_field(plan.ir, &info.ns)),
        ),
        ("status", status),
        (
            "agent",
            agent_js(&op.agent, &sensitive_request_fields(plan, shape)),
        ),
        ("request", Js::Raw(request)),
    ];
    let summary = op_summary(op);
    if let Some(z) = &shape.response_zod {
        entries.push(("response", Js::Raw(format!("toSchemaLike({z})"))));
    }
    entries.push((
        "summary",
        Js::opt_str((!summary.is_empty()).then_some(summary.as_str())),
    ));
    Js::obj(entries)
}

/// `OperationDescriptor.summary`: the pruned agent doc (`compact_doc`),
/// else the spec summary, collapsed to one line; empty when neither exists.
pub(crate) fn op_summary(op: &tungsten_ir::Operation) -> String {
    let text = if op.agent.compact_doc.trim().is_empty() {
        doc_summary(op.doc.as_ref())
    } else {
        op.agent.compact_doc.clone()
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// TG0712 for each OpenID Connect scheme: the SDK sends its credential as
/// a bearer token.
pub(crate) fn auth_notes(ir: &Ir) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for s in &ir.auth {
        if let AuthScheme::OpenIdConnect { name, .. } = s {
            diags.push(Diagnostic::info(
                "TG0712",
                format!(
                    "OpenID Connect scheme `{name}` is sent as a bearer token; the TypeScript SDK does not run discovery or obtain tokens"
                ),
            ));
        }
    }
    diags
}

/// Auth scheme descriptors (`ApiDescriptor.auth`), in IR order.
fn auth_js(ir: &Ir) -> Js {
    let mut out = vec![];
    for s in &ir.auth {
        out.push(match s {
            AuthScheme::ApiKey {
                name,
                location,
                wire_name,
                ..
            } => Js::obj(vec![
                ("kind", Js::str("api_key")),
                ("name", Js::str(name)),
                (
                    "in",
                    Js::str(match location {
                        ApiKeyIn::Header => "header",
                        ApiKeyIn::Query => "query",
                        ApiKeyIn::Cookie => "cookie",
                    }),
                ),
                ("wire", Js::str(wire_name)),
            ]),
            AuthScheme::HttpBearer { name, prefix, .. } => bearer_js(name, prefix.as_deref()),
            AuthScheme::OpenIdConnect { name, .. } => bearer_js(name, None),
            AuthScheme::HttpBasic { name, .. } => Js::obj(vec![
                ("kind", Js::str("http_basic")),
                ("name", Js::str(name)),
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
                Js::obj(vec![
                    ("kind", Js::str("oauth2")),
                    ("name", Js::str(name)),
                    ("tokenUrl", Js::opt_str(token_url)),
                    ("scopes", Js::strs(scopes)),
                ])
            }
            AuthScheme::Composite {
                name,
                satisfies,
                parts,
            } => Js::obj(vec![
                ("kind", Js::str("composite")),
                ("name", Js::str(name)),
                ("satisfies", Js::strs(satisfies)),
                (
                    "parts",
                    Js::Array(
                        parts
                            .iter()
                            .map(|p| match p {
                                CompositePart::Cookie { name } => Js::obj(vec![
                                    ("kind", Js::str("cookie")),
                                    ("name", Js::str(name)),
                                ]),
                                CompositePart::Header {
                                    name,
                                    equals_cookie,
                                    from_config,
                                    mutation_only,
                                } => Js::obj(vec![
                                    ("kind", Js::str("header")),
                                    ("name", Js::str(name)),
                                    ("equalsCookie", Js::opt_str(equals_cookie.as_deref())),
                                    ("fromConfig", Js::opt_str(from_config.as_deref())),
                                    ("mutationOnly", Js::bool(*mutation_only)),
                                ]),
                                CompositePart::Bearer { prefix, .. } => Js::obj(vec![
                                    ("kind", Js::str("bearer")),
                                    ("prefix", Js::opt_str(prefix.as_deref())),
                                ]),
                            })
                            .collect(),
                    ),
                ),
            ]),
        });
    }
    Js::Array(out)
}

/// An `http_bearer` descriptor. `prefix` comes from the auth profile
/// (`HttpBearer.prefix`); the spec's `bearerFormat` only documents the token
/// (`JWT`) and is never a required prefix.
fn bearer_js(name: &str, prefix: Option<&str>) -> Js {
    Js::obj(vec![
        ("kind", Js::str("http_bearer")),
        ("name", Js::str(name)),
        ("prefix", Js::opt_str(prefix)),
    ])
}

/// `ApiDescriptor.nonJson` from `AgentModel.non_json`, in manifest order.
/// An entry whose category is outside the runtime's closed set is left out
/// (the agent compiler already rejects it).
fn non_json_js(ir: &Ir) -> Js {
    Js::Array(
        ir.agent
            .non_json
            .iter()
            .filter(|e| CATEGORIES.contains(&e.category.as_str()))
            .map(|e| {
                Js::obj(vec![
                    ("status", Js::num(e.status)),
                    ("media", Js::str(&e.media)),
                    ("category", Js::str(&e.category)),
                    ("retryable", Js::str(retryable_str(e.retryable))),
                    ("text", Js::opt_str(e.text.as_deref())),
                ])
            })
            .collect(),
    )
}

/// One tier of `ApiDescriptor.retries` (agent.yml `defaults.retries`).
fn retry_js(policy: &RetryPolicy, honor_retry_after: bool) -> Js {
    let jitter = match policy.jitter {
        Jitter::None => "none",
        Jitter::Full => "full",
        Jitter::Equal => "equal",
    };
    Js::obj(vec![
        ("max", Js::num(policy.max)),
        ("baseMs", Js::num(policy.base_ms)),
        ("maxMs", Js::num(policy.max_ms)),
        ("jitter", Js::str(jitter)),
        ("honorRetryAfter", Js::bool(honor_retry_after)),
    ])
}

/// The `ApiDescriptor`. Every IR field the runtime reads is mapped here, in
/// one place: `errorCodes`, `ambiguousStatuses`, `nonJson`, `gates` and
/// `retries` from `Ir.agent`.
fn api_js(plan: &Plan<'_>, opts: &Options) -> Js {
    let ir = plan.ir;
    let retries = &ir.agent.retries;
    Js::obj(vec![
        ("name", Js::str(&ir.api.name.wire)),
        ("version", Js::str(&opts.version)),
        ("tungstenVersion", Js::str(&ir.generator.tungsten_version)),
        (
            "servers",
            Js::strs(ir.api.servers.iter().map(|s| s.url.as_str())),
        ),
        ("auth", auth_js(ir)),
        (
            "errorCodes",
            Js::Object(
                ir.agent
                    .error_codes
                    .iter()
                    .map(|(code, r)| (code.clone(), remediation_js(r)))
                    .collect(),
            ),
        ),
        (
            "ambiguousStatuses",
            Js::Array(
                ir.agent
                    .ambiguous_statuses
                    .iter()
                    .map(|s| Js::num(*s))
                    .collect(),
            ),
        ),
        ("nonJson", non_json_js(ir)),
        (
            "gates",
            Js::Object(
                ir.agent
                    .gates
                    .iter()
                    .map(|(k, v)| (k.clone(), Js::str(v)))
                    .collect(),
            ),
        ),
        (
            "retries",
            Js::obj(vec![
                (
                    "readOnly",
                    retry_js(&retries.read_only, retries.honor_retry_after),
                ),
                (
                    "mutating",
                    retry_js(&retries.mutating, retries.honor_retry_after),
                ),
            ]),
        ),
    ])
}

/// The source of `src/descriptors.ts`.
pub(crate) fn descriptors_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    opts: &Options,
    header: &str,
) -> String {
    let mut body = Writer::new("  ");
    let mut uses = Uses::default();
    let api_text = api_js(plan, opts).render("  ", "export const api: ApiDescriptor = ".len());
    body.doc(
        CommentStyle::JsDoc,
        &format!(
            "The `{}` API as the runtime sees it.",
            tungsten_ir::title_stem(&plan.ir.api.title)
        ),
    );
    body.line(format!("export const api: ApiDescriptor = {api_text};"));
    for (info, shape) in plan.ops.iter().zip(shapes) {
        uses.integer |= shape.uses.integer;
        uses.pattern |= shape.uses.pattern;
        uses.binary |= shape.uses.binary;
        uses.namespaces
            .extend(shape.uses.namespaces.iter().cloned());
        body.blank();
        write_args_type(&mut body, info, shape);
        body.blank();
        let summary = doc_summary(info.op.doc.as_ref());
        body.doc(
            CommentStyle::JsDoc,
            &paragraphs([
                format!("`{} {}`", method_str(info.op.method), info.op.path.raw),
                summary,
            ]),
        );
        let prefix = format!("export const {}: OperationDescriptor = ", info.key);
        body.line(format!(
            "{prefix}{};",
            descriptor_js(plan, info, shape).render("  ", prefix.len())
        ));
    }
    body.blank();
    body.doc(
        CommentStyle::JsDoc,
        "Every callable operation. The client passes them as `ClientOptions.operations`, so the runtime resolves operations by id (verification hooks, endpoint previews, macro steps).",
    );
    let list = crate::ts::Js::Array(plan.ops.iter().map(|o| Js::Raw(o.key.clone())).collect());
    let prefix = "export const operations: OperationDescriptor[] = ";
    body.line(format!("{prefix}{};", list.render("  ", prefix.len())));

    let mut imports = Imports::new();
    imports.add_type("@tungsten/runtime", "ApiDescriptor");
    imports.add_type("@tungsten/runtime", "OperationDescriptor");
    imports.add("zod", "z");
    imports.add("./internal.js", "toSchemaLike");
    if uses.integer {
        imports.add("./internal.js", "integer");
    }
    if uses.pattern {
        imports.add("./internal.js", "withPattern");
    }
    if uses.binary {
        imports.add("./internal.js", "binary");
        imports.add_type("./internal.js", "BinaryInput");
    }
    let mut w = Writer::new("  ");
    w.line(header);
    w.blank();
    w.line("// Operation descriptors: what the runtime needs to call each operation.");
    w.blank();
    write_imports(&mut w, &imports);
    for ns in &uses.namespaces {
        if let Some(m) = plan.model_ns(ns) {
            w.line(format!(
                "import * as {} from \"./models/{}.js\";",
                m.alias, m.file
            ));
        }
    }
    w.blank();
    let mut out = w.finish();
    out.push('\n');
    out.push_str(&body.finish());
    out
}
