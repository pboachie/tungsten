// SPDX-License-Identifier: AGPL-3.0-only
//! Operation shapes (arguments, request structs, success types) and
//! `descriptors.rs`: one `OperationDescriptor` per callable operation and
//! the `ApiDescriptor`.
//!
//! The arguments follow `tungsten_emit::args::args_layout` (which
//! parameters are arguments, the body content, whether the body is
//! merged), keyed by Rust name: parameters by their name rendered for Rust
//! (`Field`), merged body fields by their model field name (with a numeric
//! suffix when that is a parameter's name), a whole body as `body`.

use std::borrow::Cow;
use std::collections::BTreeSet;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::Writer;
use tungsten_emit::args::{BodyArg, args_layout, resolve as args_resolve};
use tungsten_ir::naming::Role;
use tungsten_ir::{
    ApiKeyIn, AuthScheme, BodyContent, BodyEncoding, CompositePart, Constraints, HttpMethod,
    IdempotencyKind, Ir, Jitter, Operation, OperationAgentMeta, OperationStatus, PaginationStyle,
    Param, ParamRole, ParamStyle, Presence, PreviewMode, Remediation, ResponseKind, RetryPolicy,
    Retryable, Safety, Shape, StatusMatch, TypeRef,
};

use super::graph::ref_defaultable;
use super::plan::{OpInfo, Plan, field_names, unique};
use super::rs::{Rx, doc, doc_summary, doc_text, imports_for, paragraphs, put, string_lit};
use super::types::{Cx, Slot, emit_stmts, field_doc, serde_attr, shape_notes, write_patterns};

/// The error categories of the runtime contract.
const CATEGORIES: &[(&str, &str)] = &[
    ("VALIDATION_FAILED", "ValidationFailed"),
    ("MALFORMED_REQUEST", "MalformedRequest"),
    ("REQUEST_TOO_LARGE", "RequestTooLarge"),
    ("AUTH_FAILED", "AuthFailed"),
    ("NOT_FOUND", "NotFound"),
    ("CONFLICT", "Conflict"),
    ("PRECONDITION_FAILED", "PreconditionFailed"),
    ("RATE_LIMITED", "RateLimited"),
    ("UPSTREAM_UNAVAILABLE", "UpstreamUnavailable"),
    ("OUTCOME_UNKNOWN", "OutcomeUnknown"),
    ("TRANSPORT_FAILED", "TransportFailed"),
    ("CONFIRMATION_REQUIRED", "ConfirmationRequired"),
    ("GATE_DISABLED", "GateDisabled"),
    ("UNEXPECTED_RESPONSE", "UnexpectedResponse"),
];

/// One field of an operation's request struct.
#[derive(Debug, Clone)]
pub(crate) struct ArgField<'a> {
    /// Field name (the key in the arguments object).
    pub key: String,
    pub slot: Slot,
    pub optional: bool,
    pub sensitive: bool,
    pub doc: String,
    /// What the checks run on; `None` for raw bodies.
    pub check: Option<CheckSrc<'a>>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckSrc<'a> {
    pub ty: &'a TypeRef,
    pub presence: Presence,
    pub extra: Option<&'a Constraints>,
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

/// The type a validator decodes and the function that checks it.
#[derive(Debug, Clone)]
pub(crate) struct Validated {
    pub ty: String,
    /// Path of a check function, or `None` for no checks.
    pub check: Option<String>,
}

/// The event stream of an operation.
#[derive(Debug, Clone)]
pub(crate) struct StreamShape {
    /// Type of one event (`Value` when events are untyped).
    pub event: String,
    /// The event validator, when events are typed.
    pub validator: Option<Validated>,
}

/// Everything about one operation's call signature.
#[derive(Debug, Clone)]
pub(crate) struct OpShape<'a> {
    pub params: Vec<ParamPlan<'a>>,
    pub body: Option<BodyPlan<'a>>,
    pub fields: Vec<ArgField<'a>>,
    /// The value type of the typed result.
    pub success: String,
    /// The IR type of the success body, when every success response is JSON
    /// of that one type.
    pub success_ref: Option<Cow<'a, TypeRef>>,
    /// The IR type of a page's items, when they are typed.
    pub item_ref: Option<TypeRef>,
    /// A success may have no body.
    pub bodiless: bool,
    /// Success bodies that differ and cannot be a response enum (a status
    /// range, a body that is not JSON): the result is `Value`.
    pub mixed_success: bool,
    /// Success bodies that differ by exact status: the response enum.
    pub by_status: Option<StatusEnum>,
    /// The response validator's type, when every success body is JSON of
    /// one type.
    pub response: Option<Validated>,
    /// Item type of `<method>_pages`.
    pub page_item: Option<String>,
    /// The page item validator, when the items are typed.
    pub page_validator: Option<Validated>,
    /// The event stream, when the operation has one.
    pub stream: Option<StreamShape>,
    /// Helper check functions for inline types: (name, type, statements).
    pub helpers: Vec<(String, String, Vec<String>)>,
}

/// The response enum of an operation whose success statuses answer with
/// different bodies.
#[derive(Debug, Clone)]
pub(crate) struct StatusEnum {
    /// The enum's name.
    pub name: String,
    /// One variant per declared success status, in IR order.
    pub variants: Vec<StatusVariant>,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusVariant {
    pub status: u16,
    /// `Status<code>`.
    pub name: String,
    /// The body's IR type and Rust type; `None` for a status without body.
    pub body: Option<(TypeRef, String)>,
}

/// The success bodies by status, when the operation can have a response
/// enum: every success response has an exact status and a JSON (or JSON
/// Lines) body or none, and at least two bodies have different Rust types.
pub(crate) fn status_bodies(plan: &Plan<'_>, op: &Operation) -> Option<Vec<StatusVariant>> {
    let cx = Cx::new(plan, None);
    let mut out: Vec<StatusVariant> = vec![];
    for r in op
        .responses
        .iter()
        .filter(|r| r.kind == ResponseKind::Success)
    {
        let StatusMatch::Exact(status) = r.status else {
            return None;
        };
        if out.iter().any(|v| v.status == status) {
            return None;
        }
        let content = r
            .content
            .iter()
            .find(|c| c.encoding == BodyEncoding::Json)
            .or_else(|| r.content.first());
        let body = match content {
            None => None,
            Some(c) => {
                let ty = match c.encoding {
                    BodyEncoding::Json => c.ty.clone(),
                    BodyEncoding::Jsonl => c.value_type(),
                    _ => return None,
                };
                let text = cx.ty(&ty);
                Some((ty, text))
            }
        };
        out.push(StatusVariant {
            status,
            name: format!("Status{status}"),
            body,
        });
    }
    let mut texts: Vec<&str> = out
        .iter()
        .filter_map(|v| v.body.as_ref().map(|(_, t)| t.as_str()))
        .collect();
    texts.sort_unstable();
    texts.dedup();
    (texts.len() > 1).then_some(out)
}

/// Whether a parameter is an argument. Idempotency keys come from
/// `CallOptions::idempotency_key`; origins and auth parameters from the
/// auth profile.
pub(crate) fn is_arg(p: &Param) -> bool {
    matches!(p.role, ParamRole::Plain | ParamRole::DryRun)
}

/// Whether the operation gets a `preview_<method>`.
pub(crate) fn has_preview(op: &Operation) -> bool {
    op.agent.safety != Safety::ReadOnly && op.agent.preview != PreviewMode::None
}

/// `base` with the smallest numeric word (`body_2`) no name in `taken`
/// uses; `base` itself when free.
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
pub(crate) fn op_shape<'a>(plan: &'a Plan<'a>, info: &OpInfo<'a>) -> OpShape<'a> {
    let op = info.op;
    let cx = Cx::new(plan, None);
    let layout = args_layout(plan.ir, op);
    let located: Vec<_> = layout.params.iter().chain(&layout.supplied).collect();
    let keys = unique(
        &[],
        &located
            .iter()
            .map(|a| a.param.name.words.clone())
            .collect::<Vec<_>>(),
        Role::Field,
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
    let mut taken: Vec<String> = params.iter().map(|p| p.name.clone()).collect();

    let mut fields: Vec<ArgField<'a>> = vec![];
    for p in params.iter().filter(|p| is_arg(p.param)) {
        let presence = if p.param.required {
            Presence::Required
        } else {
            Presence::Optional
        };
        let mut notes = vec![doc_text(p.param.doc.as_ref())];
        if p.name != p.param.wire_name {
            notes.push(format!(
                "Sent as the `{}` {} parameter.",
                p.param.wire_name, p.location
            ));
        }
        if let TypeRef::Inline(s) = &p.param.ty {
            notes.extend(shape_notes(s));
        }
        if p.param.deprecated {
            notes.push("Deprecated.".into());
        }
        fields.push(ArgField {
            key: p.name.clone(),
            slot: cx.slot(&p.param.ty, presence, false),
            optional: !p.param.required,
            sensitive: false,
            doc: paragraphs(notes),
            check: Some(CheckSrc {
                ty: &p.param.ty,
                presence,
                extra: None,
            }),
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
            let attrs = field_names(all.iter().map(|f| &f.name), &["extra"]);
            let mut pairs = vec![];
            for f in merged {
                let attr = all
                    .iter()
                    .position(|x| std::ptr::eq(x, f))
                    .and_then(|i| attrs.get(i).cloned())
                    .unwrap_or_else(|| f.wire_name.clone());
                let key = free_name(&taken, &attr);
                taken.push(key.clone());
                fields.push(ArgField {
                    key: key.clone(),
                    slot: cx.slot(&f.ty, f.presence, false),
                    optional: matches!(f.presence, Presence::Optional | Presence::OptionalNullable),
                    sensitive: f.sensitive,
                    doc: field_doc(f, &key),
                    check: Some(CheckSrc {
                        ty: &f.ty,
                        presence: f.presence,
                        extra: super::graph::field_constraints(f),
                    }),
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
            let presence = if required {
                Presence::Required
            } else {
                Presence::Optional
            };
            let (ty, check): (String, Option<CheckSrc<'a>>) = match content.encoding {
                BodyEncoding::Bytes | BodyEncoding::Jsonl => {
                    ("tungsten_runtime::Binary".into(), None)
                }
                BodyEncoding::Text => ("String".into(), None),
                BodyEncoding::Json | BodyEncoding::Form | BodyEncoding::Multipart => (
                    cx.ty(&content.ty),
                    Some(CheckSrc {
                        ty: &content.ty,
                        presence,
                        extra: None,
                    }),
                ),
            };
            let slot = optional_slot(ty, presence);
            fields.push(ArgField {
                key: name.clone(),
                slot,
                optional: !required,
                sensitive: false,
                doc: paragraphs([
                    doc_text(body_doc),
                    format!("The request body, sent as `{}`.", content.media_type),
                ]),
                check,
            });
            BodyPlan {
                content,
                required,
                shape: BodyShape::Arg(name),
            }
        }
    });

    // Success value.
    let mut tys: Vec<(String, Cow<'a, TypeRef>)> = vec![];
    let mut other: Vec<String> = vec![];
    let mut bodiless = false;
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
                let t = cx.ty(&c.ty);
                if !tys.iter().any(|(x, _)| *x == t) {
                    tys.push((t, Cow::Borrowed(&c.ty)));
                }
            }
            // JSON lines: the value is the list of the lines.
            BodyEncoding::Jsonl => {
                let ty = c.value_type();
                let t = cx.ty(&ty);
                if !tys.iter().any(|(x, _)| *x == t) {
                    tys.push((t, Cow::Owned(ty)));
                }
            }
            BodyEncoding::Text => {
                all_json = false;
                other.push("String".into());
            }
            BodyEncoding::Bytes => {
                all_json = false;
                other.push("tungsten_runtime::Binary".into());
            }
            BodyEncoding::Form | BodyEncoding::Multipart => {
                all_json = false;
                other.push(cx.ty(&c.ty));
            }
        }
    }
    let mut distinct: Vec<String> = tys.iter().map(|(t, _)| t.clone()).collect();
    for o in &other {
        if !distinct.contains(o) {
            distinct.push(o.clone());
        }
    }
    let mixed = distinct.len() > 1;
    let by_status = if mixed && !info.response.is_empty() {
        status_bodies(plan, op).map(|variants| StatusEnum {
            name: info.response.clone(),
            variants,
        })
    } else {
        None
    };
    let mixed_success = mixed && by_status.is_none();
    let base = match distinct.as_slice() {
        _ if by_status.is_some() => info.response.clone(),
        [] if bodiless => "()".to_string(),
        [] => "Value".to_string(),
        [one] => one.clone(),
        _ => "Value".to_string(),
    };
    let success = if bodiless && !distinct.is_empty() && !mixed {
        format!("Option<{base}>")
    } else {
        base
    };

    let mut helpers: Vec<(String, String, Vec<String>)> = vec![];
    let mut validated = |ty: &TypeRef, nullable: bool, hint: &str| -> Validated {
        let text = cx.ty(ty);
        let text = if nullable {
            format!("Option<{text}>")
        } else {
            text
        };
        let mut stmts = vec![];
        if nullable {
            let wrapped = TypeRef::Inline(Box::new(Shape::Nullable { inner: ty.clone() }));
            cx.check_stmts(&wrapped, None, "v", 0, &mut stmts);
        } else {
            cx.check_stmts(ty, None, "v", 0, &mut stmts);
        }
        let check = match (ty, nullable) {
            (TypeRef::Named(id), false) => cx.check_path(id),
            _ if stmts.is_empty() => None,
            _ => {
                let name = format!("check_{hint}");
                helpers.push((name.clone(), text.clone(), stmts));
                Some(name)
            }
        };
        Validated { ty: text, check }
    };
    let mut response = (all_json && tys.len() == 1)
        .then(|| validated(&tys[0].1, bodiless, &format!("{}_response", info.builder)));
    if let Some(e) = &by_status {
        response = Some(Validated {
            ty: e.name.clone(),
            check: status_check_stmts(&cx, e).map(|_| response_check_name(info)),
        });
    }
    let items = op.pagination.as_ref().map(|p| {
        tys.first()
            .and_then(|(_, r)| items_ref(plan, r, &p.items_field))
    });
    let page_item = items.map(|items| items.map_or_else(|| "Value".to_string(), |i| cx.ty(i)));
    let page_validator = items
        .flatten()
        .map(|items| validated(items, false, &format!("{}_item", info.builder)));
    let success_ref = (all_json && tys.len() == 1).then(|| tys[0].1.clone());
    let stream = op.stream.as_ref().map(|spec| {
        let untyped = matches!(
            &spec.event,
            TypeRef::Inline(shape) if matches!(**shape, Shape::Any)
        );
        StreamShape {
            event: cx.ty(&spec.event),
            validator: (!untyped)
                .then(|| validated(&spec.event, false, &format!("{}_event", info.builder))),
        }
    });
    OpShape {
        params,
        body,
        fields,
        success,
        success_ref,
        item_ref: items.flatten().cloned(),
        bodiless,
        mixed_success,
        by_status,
        response,
        page_item,
        page_validator,
        stream,
        helpers,
    }
}

/// The slot of a raw argument of Rust type `ty`.
fn optional_slot(ty: String, presence: Presence) -> Slot {
    if presence == Presence::Required {
        Slot { ty, attrs: vec![] }
    } else {
        Slot {
            ty: format!("Option<{ty}>"),
            attrs: vec![vec![
                "default".into(),
                r#"skip_serializing_if = "Option::is_none""#.into(),
                r#"with = "s::opt""#.into(),
            ]],
        }
    }
}

/// The item type of a page: the items of the array at `items_field` of the
/// success body (the body itself when the field is empty).
fn items_ref<'p>(plan: &'p Plan<'_>, body: &'p TypeRef, items_field: &str) -> Option<&'p TypeRef> {
    let list = if items_field.is_empty() {
        body
    } else {
        let Some(Shape::Record { fields, .. }) = plan.resolve(body) else {
            return None;
        };
        &fields.iter().find(|f| f.wire_name == items_field)?.ty
    };
    match plan.resolve(list)? {
        Shape::Array { items, .. } => Some(items),
        _ => None,
    }
}

// ------------------------------------------------------- request structs

/// The Rust type of a request struct field, as the path to a model.
pub(crate) fn request_check_name(info: &OpInfo<'_>) -> String {
    let words = tungsten_ir::naming::split_words(&info.request);
    super::plan::fn_name("check", &words)
}

/// The name of the check function of a response enum.
pub(crate) fn response_check_name(info: &OpInfo<'_>) -> String {
    let words = tungsten_ir::naming::split_words(&info.response);
    super::plan::fn_name("check", &words)
}

/// The body of the check function of a response enum, or `None` when no
/// variant has a constraint to check.
pub(crate) fn status_check_stmts(cx: &Cx<'_, '_>, e: &StatusEnum) -> Option<Vec<String>> {
    let arms: Vec<(String, Option<&TypeRef>)> = e
        .variants
        .iter()
        .map(|v| (v.name.clone(), v.body.as_ref().map(|(t, _)| t)))
        .collect();
    super::types::variant_check_stmts(cx, &e.name, &arms)
}

/// Write the request struct of an operation, its constructor and its check
/// function.
pub(crate) fn write_request(
    w: &mut Writer,
    cx: &Cx<'_, '_>,
    info: &OpInfo<'_>,
    shape: &OpShape<'_>,
) {
    let op = info.op;
    let intro = format!(
        "Arguments of `{}` (`{} {}`).",
        op.id.0,
        method_str(op.method),
        op.path.raw
    );
    let check = request_check_name(info);
    write_args_struct(w, cx, &info.request, &intro, &shape.fields, Some(&check));
}

/// `impl Debug for <name>` that shows `<redacted>` for each sensitive field:
/// (field, sensitive) in declaration order.
pub(crate) fn write_redacted_debug(w: &mut Writer, name: &str, fields: &[(&str, bool)]) {
    w.line(format!("impl std::fmt::Debug for {name} {{"));
    w.indent();
    w.line("fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {");
    w.indent();
    let mut calls = vec![format!("debug_struct({})", string_lit(name))];
    for (key, sensitive) in fields {
        if *sensitive {
            calls.push(format!("field({}, &\"<redacted>\")", string_lit(key)));
        } else {
            calls.push(format!("field({}, &self.{key})", string_lit(key)));
        }
    }
    calls.push("finish()".into());
    chain_lines(w, "f", &calls);
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
}

/// Write a struct of arguments (`name`), its constructor and, when
/// `check` names a function, the function that checks it.
pub(crate) fn write_args_struct(
    w: &mut Writer,
    cx: &Cx<'_, '_>,
    name: &str,
    intro: &str,
    fields: &[ArgField<'_>],
    check: Option<&str>,
) {
    let defaultable = fields.iter().all(|f| {
        f.optional
            || match f.check {
                Some(c) => ref_defaultable(c.ty, &cx.plan.graph.defaultable),
                None => f.slot.ty == "String",
            }
    });
    let sensitive = fields.iter().any(|f| f.sensitive);
    let mut derives = vec![];
    if !sensitive {
        derives.push("Debug");
    }
    derives.push("Clone");
    if defaultable {
        derives.push("Default");
    }
    derives.extend(["PartialEq", "Serialize", "Deserialize"]);
    doc(w, intro);
    w.line(format!("#[derive({})]", derives.join(", ")));
    w.line("#[serde(deny_unknown_fields)]");
    if fields.is_empty() {
        w.line(format!("pub struct {name} {{}}"));
    } else {
        w.line(format!("pub struct {name} {{"));
        w.indent();
        for f in fields {
            doc(w, &f.doc);
            for a in &f.slot.attrs {
                serde_attr(w, a);
            }
            w.line(format!("pub {}: {},", f.key, f.slot.ty));
        }
        w.dedent();
        w.line("}");
    }
    if sensitive {
        w.blank();
        let shown: Vec<(&str, bool)> = fields
            .iter()
            .map(|f| (f.key.as_str(), f.sensitive))
            .collect();
        write_redacted_debug(w, name, &shown);
    }

    // Constructor.
    w.blank();
    w.line(format!("impl {name} {{"));
    w.indent();
    doc(w, "A request with every optional argument left out.");
    let params: Vec<String> = fields
        .iter()
        .filter(|f| !f.optional)
        .map(|f| format!("{}: {}", f.key, f.slot.ty))
        .collect();
    fn_sig(w, 4, "pub fn new", &params, "Self");
    w.indent();
    if fields.is_empty() {
        w.line("Self {}");
    } else {
        let values: Vec<(&str, Rx)> = fields
            .iter()
            .map(|f| {
                let v = if !f.optional {
                    Rx::atom(f.key.clone())
                } else if f.slot.ty.starts_with("Patch<") {
                    Rx::atom("Patch::Undefined")
                } else {
                    Rx::atom("None")
                };
                (f.key.as_str(), v)
            })
            .collect();
        put(w, 8, "", &Rx::record("Self", values), "");
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");

    // Check function.
    let Some(check) = check else { return };
    let mut stmts = vec![];
    for f in fields {
        if let Some(c) = f.check {
            cx.arg_stmts(
                &f.key,
                c.ty,
                c.presence,
                c.extra,
                &format!("v.{}", f.key),
                &mut stmts,
            );
        }
    }
    if !stmts.is_empty() {
        w.blank();
        fn_sig(
            w,
            0,
            &format!("pub fn {check}"),
            &[format!("v: &{name}"), "c: &mut Checker".to_string()],
            "",
        );
        w.indent();
        emit_stmts(w, 4, &stmts);
        w.dedent();
        w.line("}");
    }
}

/// A method chain in rustfmt's layout (see `types::chain`).
pub(crate) fn chain_lines(w: &mut Writer, root: &str, calls: &[String]) {
    let flat = format!("{root}.{}", calls.join("."));
    if flat.len() <= 60 {
        w.line(flat);
        return;
    }
    for (i, c) in calls.iter().enumerate() {
        if i == 0 {
            w.line(format!("{root}.{c}"));
            w.indent();
        } else {
            w.line(format!(".{c}"));
        }
    }
    w.dedent();
}

/// A function signature in rustfmt's layout at `indent` columns: one line
/// when it fits, else one parameter per line. `head` is everything before
/// the parameters (`pub fn new`).
pub(crate) fn fn_sig(w: &mut Writer, indent: usize, head: &str, params: &[String], ret: &str) {
    let ret_part = if ret.is_empty() {
        String::new()
    } else {
        format!(" -> {ret}")
    };
    let flat = format!("{head}({}){ret_part} {{", params.join(", "));
    if indent + flat.len() <= super::rs::MAX_WIDTH {
        w.line(flat);
        return;
    }
    w.line(format!("{head}("));
    w.indent();
    for p in params {
        w.line(format!("{p},"));
    }
    w.dedent();
    let last = format!("){ret_part} {{");
    if indent + last.len() <= super::rs::MAX_WIDTH {
        w.line(last);
        return;
    }
    // The return type does not fit: break its outermost generics.
    match (ret.find('<'), ret.ends_with('>')) {
        (Some(open), true) => {
            w.line(format!(") -> {}", &ret[..=open]));
            w.indent();
            w.line(format!("{},", &ret[open + 1..ret.len() - 1]));
            w.dedent();
            w.line("> {");
        }
        _ => {
            w.line(last);
        }
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

fn method_rs(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "HttpMethod::Get",
        HttpMethod::Put => "HttpMethod::Put",
        HttpMethod::Post => "HttpMethod::Post",
        HttpMethod::Delete => "HttpMethod::Delete",
        HttpMethod::Options => "HttpMethod::Options",
        HttpMethod::Head => "HttpMethod::Head",
        HttpMethod::Patch => "HttpMethod::Patch",
        HttpMethod::Trace => "HttpMethod::Trace",
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

fn safety_rs(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "Safety::ReadOnly",
        Safety::Mutating => "Safety::Mutating",
        Safety::Destructive => "Safety::Destructive",
        Safety::Irreversible => "Safety::Irreversible",
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

fn idempotency_rs(k: IdempotencyKind) -> &'static str {
    match k {
        IdempotencyKind::None => "IdempotencyKind::None",
        IdempotencyKind::Auto => "IdempotencyKind::Auto",
        IdempotencyKind::CallerOwned => "IdempotencyKind::CallerOwned",
        IdempotencyKind::ContentHash => "IdempotencyKind::ContentHash",
        IdempotencyKind::ContentIdentity => "IdempotencyKind::ContentIdentity",
    }
}

fn retryable_rs(r: Retryable) -> &'static str {
    match r {
        Retryable::Never => "Retryable::Never",
        Retryable::AfterDelay => "Retryable::AfterDelay",
        Retryable::SameKeyOnly => "Retryable::SameKeyOnly",
        Retryable::AfterRemediation => "Retryable::AfterRemediation",
    }
}

fn style_rs(s: ParamStyle) -> &'static str {
    match s {
        ParamStyle::Simple => "ParamStyle::Simple",
        ParamStyle::Form => "ParamStyle::Form",
        ParamStyle::Label => "ParamStyle::Label",
        ParamStyle::Matrix => "ParamStyle::Matrix",
        ParamStyle::SpaceDelimited => "ParamStyle::SpaceDelimited",
        ParamStyle::PipeDelimited => "ParamStyle::PipeDelimited",
        ParamStyle::DeepObject => "ParamStyle::DeepObject",
    }
}

fn role_rs(r: ParamRole) -> &'static str {
    match r {
        ParamRole::Plain => "ParamRole::Plain",
        ParamRole::IdempotencyKey => "ParamRole::IdempotencyKey",
        ParamRole::DryRun => "ParamRole::DryRun",
        ParamRole::Origin => "ParamRole::Origin",
        ParamRole::Auth => "ParamRole::Auth",
        ParamRole::Constant => "ParamRole::Constant",
    }
}

fn location_rs(l: &str) -> &'static str {
    match l {
        "path" => "ParamLocation::Path",
        "query" => "ParamLocation::Query",
        "header" => "ParamLocation::Header",
        _ => "ParamLocation::Cookie",
    }
}

fn encoding_rs(e: BodyEncoding) -> &'static str {
    match e {
        BodyEncoding::Json => "BodyEncoding::Json",
        BodyEncoding::Form => "BodyEncoding::Form",
        BodyEncoding::Multipart => "BodyEncoding::Multipart",
        // JSON Lines is a response encoding: a request body of that media
        // type is bytes (the builder never says otherwise).
        BodyEncoding::Bytes | BodyEncoding::Jsonl => "BodyEncoding::Bytes",
        BodyEncoding::Text => "BodyEncoding::Text",
    }
}

fn status_rs(s: StatusMatch) -> Option<Rx> {
    match s {
        StatusMatch::Exact(n) => Some(Rx::call(
            "StatusMatch::Exact",
            vec![Rx::atom(n.to_string())],
        )),
        StatusMatch::Range(d @ 1..=5) => Some(Rx::call(
            "StatusMatch::Class",
            vec![Rx::atom(d.to_string())],
        )),
        StatusMatch::Range(_) => None,
        StatusMatch::Default => Some(Rx::atom("StatusMatch::Default")),
    }
}

fn category_rs(c: &str) -> Option<String> {
    CATEGORIES
        .iter()
        .find(|(wire, _)| *wire == c)
        .map(|(_, name)| format!("Category::{name}"))
}

/// A remediation entry; absent fields are omitted, and a category outside
/// the runtime's closed set is dropped.
fn remediation_rs(r: &Remediation) -> Rx {
    Rx::record(
        "RemediationEntry",
        vec![
            (
                "category",
                r.category
                    .as_deref()
                    .and_then(category_rs)
                    .map_or_else(Rx::none, |c| Rx::some(Rx::atom(c))),
            ),
            ("text", Rx::opt_string(r.text.as_deref())),
            (
                "retryable",
                r.retryable
                    .map_or_else(Rx::none, |x| Rx::some(Rx::atom(retryable_rs(x)))),
            ),
            ("next_action", Rx::opt_string(r.next_action.as_deref())),
        ],
    )
}

fn json_object(v: &serde_json::Value) -> Rx {
    match v {
        serde_json::Value::Object(_) => Rx::json(v),
        _ => Rx::json(&serde_json::Value::Object(serde_json::Map::new())),
    }
}

fn duration_ms(n: Option<u64>) -> Rx {
    n.map_or_else(Rx::none, |n| {
        Rx::some(Rx::call(
            "Duration::from_millis",
            vec![Rx::atom(n.to_string())],
        ))
    })
}

/// Statements that build values before the struct literal that uses
/// them (maps are filled with `insert`s, which rustfmt lays out simply).
#[derive(Debug, Default)]
pub(crate) struct Lets {
    pub stmts: Vec<String>,
}

impl Lets {
    /// A map variable named `var` holding `entries`; `BTreeMap::new()`
    /// when there are none.
    pub(crate) fn map(&mut self, var: &str, entries: Vec<(String, Rx)>) -> Rx {
        if entries.is_empty() {
            return Rx::atom("BTreeMap::new()");
        }
        self.stmts.push(format!("let mut {var} = BTreeMap::new();"));
        for (k, v) in entries {
            let call = Rx::call(format!("{var}.insert"), vec![Rx::string(&k), v]);
            self.stmts.push(stmt_text(4, &call));
        }
        Rx::atom(var)
    }
}

/// `call;` as a statement at `indent` columns, continuation lines relative
/// to the statement block.
fn stmt_text(indent: usize, call: &Rx) -> String {
    let text = call.render(indent, indent, 1);
    let mut lines = text.lines();
    let mut out = lines.next().unwrap_or("").to_string();
    for l in lines {
        out.push('\n');
        out.push_str(l.get(indent..).unwrap_or(l));
    }
    out.push(';');
    out
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
        BodyEncoding::Bytes | BodyEncoding::Text | BodyEncoding::Jsonl
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
    if let TypeRef::Named(id) = ty {
        active.remove(id);
    }
}

fn agent_rs(a: &OperationAgentMeta, sensitive_request: &[String], lets: &mut Lets) -> Rx {
    let idem = &a.idempotency;
    let preview = match &a.preview {
        PreviewMode::Local => Rx::atom("PreviewMode::Local"),
        PreviewMode::Header { header, value } => Rx::record(
            "PreviewMode::Header",
            vec![("header", Rx::string(header)), ("value", Rx::string(value))],
        ),
        PreviewMode::Endpoint { operation } => Rx::record(
            "PreviewMode::Endpoint",
            vec![("operation", Rx::string(&operation.0))],
        ),
        PreviewMode::None => Rx::atom("PreviewMode::None"),
    };
    let confirmation = a.confirmation.as_ref().map_or_else(Rx::none, |c| {
        Rx::some(Rx::record(
            "ConfirmationMeta",
            vec![
                ("summary_fields", Rx::strings(&c.summary_fields)),
                ("message", Rx::opt_string(c.message.as_deref())),
            ],
        ))
    });
    let verify = a.verify.as_ref().map_or_else(Rx::none, |v| {
        Rx::some(Rx::record(
            "VerifyDescriptor",
            vec![
                ("operation", Rx::string(&v.operation.0)),
                ("args", json_object(&v.args)),
                ("expect", json_object(&v.expect)),
                ("terminal", json_object(&v.terminal)),
                ("poll_interval", duration_ms(v.poll_interval_ms)),
                ("poll_budget", duration_ms(v.poll_budget_ms)),
            ],
        ))
    });
    Rx::record(
        "AgentMeta",
        vec![
            ("safety", Rx::atom(safety_rs(a.safety))),
            (
                "idempotency",
                Rx::record(
                    "IdempotencyMeta",
                    vec![
                        ("policy", Rx::atom(idempotency_rs(idem.policy))),
                        ("header", Rx::opt_string(idem.header.as_deref())),
                        ("format", Rx::opt_string(idem.format.as_deref())),
                        ("persist_required", Rx::boolean(idem.persist_required)),
                        ("note", Rx::opt_string(idem.note.as_deref())),
                    ],
                ),
            ),
            ("preview", preview),
            ("confirmation", confirmation),
            ("verify", verify),
            (
                "remediation",
                lets.map(
                    "remediation",
                    a.remediation
                        .iter()
                        .map(|(code, r)| (code.clone(), remediation_rs(r)))
                        .collect(),
                ),
            ),
            (
                "remediation_note",
                Rx::opt_string(a.remediation_note.as_deref()),
            ),
            (
                "sensitive_response_fields",
                Rx::strings(&a.sensitive_response_fields),
            ),
            ("sensitive_request_fields", Rx::strings(sensitive_request)),
            ("shown_once", Rx::boolean(a.shown_once)),
        ],
    )
}

fn pagination_rs(info: &OpInfo<'_>, shape: &OpShape<'_>) -> Rx {
    let Some(p) = &info.op.pagination else {
        return Rx::none();
    };
    // Request parameters are named as arguments, like `ParamDescriptor.name`.
    let arg = |wire: &str| {
        shape
            .params
            .iter()
            .find(|pp| pp.param.wire_name == wire && pp.location == "query")
            .map_or_else(|| wire.to_string(), |pp| pp.name.clone())
    };
    let items = ("items_field", Rx::string(&p.items_field));
    Rx::some(match &p.style {
        PaginationStyle::Cursor {
            request_param,
            response_field,
        } => Rx::record(
            "PaginationDescriptor::Cursor",
            vec![
                ("request_param", Rx::string(&arg(request_param))),
                ("response_field", Rx::string(response_field)),
                items,
                (
                    "page_size_param",
                    p.page_size_param
                        .as_deref()
                        .map_or_else(Rx::none, |s| Rx::some(Rx::string(&arg(s)))),
                ),
                (
                    "has_more_field",
                    p.has_more_field
                        .as_deref()
                        .map_or_else(Rx::none, |f| Rx::some(Rx::string(f))),
                ),
                (
                    "cursor_item_field",
                    p.cursor_item_field
                        .as_deref()
                        .map_or_else(Rx::none, |f| Rx::some(Rx::string(f))),
                ),
            ],
        ),
        PaginationStyle::Offset {
            offset_param,
            limit_param,
        } => Rx::record(
            "PaginationDescriptor::Offset",
            vec![
                ("offset_param", Rx::string(&arg(offset_param))),
                ("limit_param", Rx::string(&arg(limit_param))),
                items,
            ],
        ),
        PaginationStyle::Page {
            page_param,
            size_param,
        } => Rx::record(
            "PaginationDescriptor::Page",
            vec![
                ("page_param", Rx::string(&arg(page_param))),
                ("size_param", Rx::string(&arg(size_param))),
                items,
            ],
        ),
        PaginationStyle::LinkHeader => Rx::record("PaginationDescriptor::LinkHeader", vec![items]),
    })
}

/// The error code field of the operation's namespace error model.
fn error_code_field<'i>(ir: &'i Ir, ns: &str) -> Option<&'i str> {
    ir.namespaces
        .iter()
        .find(|n| n.name.wire == ns)
        .and_then(|n| n.errors.code_field.as_deref())
}

/// The `let` that builds a validator and the expression that uses it.
/// `var` is the variable, `kind` the `Typed` constructor (`request` or
/// `body`).
fn validator_let(var: &str, kind: &str, v: &Validated) -> (String, Rx) {
    let check = v.check.clone().unwrap_or_else(|| "no_check".to_string());
    let ctor = format!("Typed::{kind}({check});");
    let ty = &v.ty;
    // The function body is indented four columns.
    let flat = format!("let {var}: Typed<{ty}> = {ctor}");
    let head = format!("let {var}: Typed<{ty}> =");
    let line = if 4 + flat.len() <= super::rs::MAX_WIDTH {
        flat
    } else if 4 + head.len() <= super::rs::MAX_WIDTH {
        if 8 + ctor.len() <= super::rs::MAX_WIDTH {
            format!("{head}\n    {ctor}")
        } else {
            format!("{head}\n    Typed::{kind}(\n        {check},\n    );")
        }
    } else if 4 + format!("> = {ctor}").len() <= super::rs::MAX_WIDTH {
        format!("let {var}: Typed<\n    {ty},\n> = {ctor}")
    } else {
        format!("let {var}: Typed<\n    {ty},\n> = Typed::{kind}(\n    {check},\n);")
    };
    (line, Rx::some(Rx::call("Arc::new", vec![Rx::atom(var)])))
}

/// The descriptor of one operation: the `let`s of its validators and the
/// struct literal.
pub(crate) fn descriptor_rx(
    plan: &Plan<'_>,
    info: &OpInfo<'_>,
    shape: &OpShape<'_>,
) -> (Vec<String>, Rx) {
    let op = info.op;
    let params = Rx::list(
        shape
            .params
            .iter()
            .map(|p| {
                Rx::record(
                    "ParamDescriptor",
                    vec![
                        ("name", Rx::string(&p.name)),
                        ("wire", Rx::string(&p.param.wire_name)),
                        ("location", Rx::atom(location_rs(p.location))),
                        ("required", Rx::boolean(p.param.required)),
                        ("style", Rx::atom(style_rs(p.param.style))),
                        ("explode", Rx::boolean(p.param.explode)),
                        ("role", Rx::atom(role_rs(p.param.role))),
                        (
                            "constant",
                            Rx::opt_string(tungsten_emit::args::constant_text(p.param).as_deref()),
                        ),
                        ("sensitive", Rx::boolean(false)),
                    ],
                )
            })
            .collect(),
    );
    let body = shape.body.as_ref().map_or_else(Rx::none, |b| {
        let shape_rs = match &b.shape {
            BodyShape::Merged(pairs) => Rx::record(
                "BodyShape::Merged",
                vec![(
                    "fields",
                    Rx::list(
                        pairs
                            .iter()
                            .map(|(arg, wire)| {
                                Rx::record(
                                    "MergedBodyField",
                                    vec![("arg", Rx::string(arg)), ("wire", Rx::string(wire))],
                                )
                            })
                            .collect(),
                    ),
                )],
            ),
            BodyShape::Arg(arg) => Rx::record("BodyShape::Arg", vec![("arg", Rx::string(arg))]),
        };
        Rx::some(Rx::record(
            "BodyDescriptor",
            vec![
                ("media_type", Rx::string(&b.content.media_type)),
                ("encoding", Rx::atom(encoding_rs(b.content.encoding))),
                ("required", Rx::boolean(b.required)),
                ("shape", shape_rs),
            ],
        ))
    });
    let responses = Rx::list(
        op.responses
            .iter()
            .filter_map(|r| {
                let kind = match r.kind {
                    ResponseKind::Success => "ResponseKind::Success",
                    ResponseKind::Error => "ResponseKind::Error",
                    ResponseKind::Ambiguous => "ResponseKind::Ambiguous",
                };
                Some(Rx::record(
                    "ResponseDescriptor",
                    vec![
                        ("status", status_rs(r.status)?),
                        ("kind", Rx::atom(kind)),
                        (
                            "media_type",
                            Rx::opt_string(r.content.first().map(|c| c.media_type.as_str())),
                        ),
                    ],
                ))
            })
            .collect(),
    );
    let security = Rx::list(
        op.security
            .iter()
            .map(|req| Rx::strings(req.all_of.iter().map(|s| s.scheme.as_str())))
            .collect(),
    );
    let mut lets = Lets::default();
    let rpc = op.rpc.as_ref().map_or_else(Rx::none, |r| {
        Rx::some(Rx::record(
            "RpcDescriptor",
            vec![
                ("field", Rx::string(&r.discriminator_field)),
                ("value", Rx::string(&r.discriminator_value)),
                ("params_field", Rx::string(&r.params_field)),
                (
                    "constants",
                    lets.map(
                        "constants",
                        r.constants
                            .iter()
                            .map(|(k, v)| (k.clone(), Rx::json(v)))
                            .collect(),
                    ),
                ),
            ],
        ))
    });
    let status = match &op.status {
        OperationStatus::Gated { gate } => Rx::record(
            "OperationStatus::Gated",
            vec![
                ("env_var", Rx::string(&gate.env_var)),
                (
                    "disabled_status",
                    Rx::atom(gate.disabled_status.to_string()),
                ),
            ],
        ),
        OperationStatus::Implemented | OperationStatus::Planned { .. } => {
            Rx::atom("OperationStatus::Implemented")
        }
    };
    let request = Validated {
        ty: info.request.clone(),
        check: shape_has_checks(plan, shape).then(|| request_check_name(info)),
    };
    let (line, request_value) = validator_let("request", "request", &request);
    lets.stmts.push(line);
    let response_value = match &shape.response {
        Some(v) => {
            let (line, value) = validator_let("response", "body", v);
            lets.stmts.push(line);
            value
        }
        None => Rx::none(),
    };
    let item_value = match &shape.page_validator {
        Some(v) => {
            let (line, value) = validator_let("item", "body", v);
            lets.stmts.push(line);
            value
        }
        None => Rx::none(),
    };
    let summary = op_summary(op);
    let mut fields = vec![
        ("id", Rx::string(&op.id.0)),
        ("method", Rx::atom(method_rs(op.method))),
        ("path", Rx::string(&op.path.raw)),
        ("params", params),
        ("body", body),
        ("responses", responses),
        ("security", security),
        ("pagination", pagination_rs(info, shape)),
        ("rpc", rpc),
        (
            "error_code_field",
            Rx::opt_string(error_code_field(plan.ir, &info.ns)),
        ),
        ("status", status),
        (
            "agent",
            agent_rs(&op.agent, &sensitive_request_fields(plan, shape), &mut lets),
        ),
        ("request", request_value),
        ("response", response_value),
        ("page_item", item_value),
    ];
    fields.push((
        "summary",
        Rx::opt_string((!summary.is_empty()).then_some(summary.as_str())),
    ));
    (lets.stmts, Rx::record("OperationDescriptor", fields))
}

/// The stream descriptor of one operation: the `let` of its event validator
/// and the struct literal.
pub(crate) fn stream_descriptor_rx(
    info: &OpInfo<'_>,
    shape: &StreamShape,
) -> Option<(Vec<String>, Rx)> {
    let spec = info.op.stream.as_ref()?;
    let mut stmts = vec![];
    let event = match &shape.validator {
        Some(v) => {
            let (line, value) = validator_let("event", "body", v);
            stmts.push(line);
            value
        }
        None => Rx::none(),
    };
    let done = match &spec.done {
        Some(done) => Rx::some(Rx::string(done)),
        None => Rx::none(),
    };
    let flag = match &spec.request_flag {
        Some(flag) => Rx::some(Rx::string(flag)),
        None => Rx::none(),
    };
    Some((
        stmts,
        Rx::record(
            "StreamDescriptor",
            vec![("event", event), ("done", done), ("flag", flag)],
        ),
    ))
}

/// Whether the request struct has a check function.
pub(crate) fn shape_has_checks(plan: &Plan<'_>, shape: &OpShape<'_>) -> bool {
    let cx = Cx::new(plan, None);
    shape.fields.iter().any(|f| {
        f.check.is_some_and(|c| {
            let mut stmts = vec![];
            cx.arg_stmts(&f.key, c.ty, c.presence, c.extra, "v", &mut stmts);
            !stmts.is_empty()
        })
    })
}

/// `OperationDescriptor::summary`: the pruned agent doc (`compact_doc`),
/// else the spec summary, collapsed to one line; empty when neither exists.
pub(crate) fn op_summary(op: &Operation) -> String {
    let text = if op.agent.compact_doc.trim().is_empty() {
        doc_summary(op.doc.as_ref())
    } else {
        op.agent.compact_doc.clone()
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// TG0743 for each OpenID Connect scheme: the SDK sends its credential as
/// a bearer token.
pub(crate) fn auth_notes(ir: &Ir) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for s in &ir.auth {
        if let AuthScheme::OpenIdConnect { name, .. } = s {
            diags.push(Diagnostic::info(
                "TG0743",
                format!(
                    "OpenID Connect scheme `{name}` is sent as a bearer token; the Rust SDK does not run discovery or obtain tokens"
                ),
            ));
        }
    }
    diags
}

fn auth_rs(ir: &Ir) -> Rx {
    let mut out = vec![];
    for s in &ir.auth {
        out.push(match s {
            AuthScheme::ApiKey {
                name,
                location,
                wire_name,
                ..
            } => Rx::record(
                "AuthSchemeDescriptor::ApiKey",
                vec![
                    ("name", Rx::string(name)),
                    (
                        "location",
                        Rx::atom(match location {
                            ApiKeyIn::Header => "ApiKeyLocation::Header",
                            ApiKeyIn::Query => "ApiKeyLocation::Query",
                            ApiKeyIn::Cookie => "ApiKeyLocation::Cookie",
                        }),
                    ),
                    ("wire", Rx::string(wire_name)),
                ],
            ),
            AuthScheme::HttpBearer { name, prefix, .. } => bearer_rs(name, prefix.as_deref()),
            AuthScheme::OpenIdConnect { name, .. } => bearer_rs(name, None),
            AuthScheme::HttpBasic { name, .. } => Rx::record(
                "AuthSchemeDescriptor::HttpBasic",
                vec![("name", Rx::string(name))],
            ),
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
                Rx::record(
                    "AuthSchemeDescriptor::Oauth2",
                    vec![
                        ("name", Rx::string(name)),
                        ("token_url", Rx::opt_string(token_url)),
                        ("scopes", Rx::strings(scopes)),
                    ],
                )
            }
            AuthScheme::Composite {
                name,
                satisfies,
                parts,
            } => Rx::record(
                "AuthSchemeDescriptor::Composite",
                vec![
                    ("name", Rx::string(name)),
                    ("satisfies", Rx::strings(satisfies)),
                    (
                        "parts",
                        Rx::list(
                            parts
                                .iter()
                                .map(|p| match p {
                                    CompositePart::Cookie { name } => Rx::record(
                                        "CompositePart::Cookie",
                                        vec![("name", Rx::string(name))],
                                    ),
                                    CompositePart::Header {
                                        name,
                                        equals_cookie,
                                        from_config,
                                        mutation_only,
                                    } => Rx::record(
                                        "CompositePart::Header",
                                        vec![
                                            ("name", Rx::string(name)),
                                            (
                                                "equals_cookie",
                                                Rx::opt_string(equals_cookie.as_deref()),
                                            ),
                                            ("from_config", Rx::opt_string(from_config.as_deref())),
                                            ("mutation_only", Rx::boolean(*mutation_only)),
                                        ],
                                    ),
                                    CompositePart::Bearer { prefix, .. } => Rx::record(
                                        "CompositePart::Bearer",
                                        vec![("prefix", Rx::opt_string(prefix.as_deref()))],
                                    ),
                                })
                                .collect(),
                        ),
                    ),
                ],
            ),
        });
    }
    Rx::list(out)
}

/// An `http_bearer` descriptor. `prefix` comes from the auth profile; the
/// spec's `bearerFormat` only documents the token and is never a prefix.
fn bearer_rs(name: &str, prefix: Option<&str>) -> Rx {
    Rx::record(
        "AuthSchemeDescriptor::HttpBearer",
        vec![
            ("name", Rx::string(name)),
            ("prefix", Rx::opt_string(prefix)),
        ],
    )
}

/// `ApiDescriptor::non_json` from `AgentModel.non_json`, in manifest
/// order; an entry whose category is outside the runtime's set is left out.
fn non_json_rs(ir: &Ir) -> Rx {
    Rx::list(
        ir.agent
            .non_json
            .iter()
            .filter_map(|e| {
                let category = category_rs(&e.category)?;
                Some(Rx::record(
                    "NonJsonError",
                    vec![
                        ("status", Rx::atom(e.status.to_string())),
                        ("media", Rx::string(&e.media)),
                        ("category", Rx::atom(category)),
                        ("retryable", Rx::atom(retryable_rs(e.retryable))),
                        ("text", Rx::opt_string(e.text.as_deref())),
                    ],
                ))
            })
            .collect(),
    )
}

/// One tier of `ApiDescriptor::retries` (agent.yml `defaults.retries`).
fn retry_rs(policy: &RetryPolicy, honor_retry_after: bool) -> Rx {
    let jitter = match policy.jitter {
        Jitter::None => "Jitter::None",
        Jitter::Full => "Jitter::Full",
        Jitter::Equal => "Jitter::Equal",
    };
    Rx::record(
        "PartialRetryOptions",
        vec![
            ("max", Rx::some(Rx::atom(policy.max.to_string()))),
            ("base", duration_ms(Some(policy.base_ms))),
            ("max_delay", duration_ms(Some(policy.max_ms))),
            ("jitter", Rx::some(Rx::atom(jitter))),
            (
                "honor_retry_after",
                Rx::some(Rx::boolean(honor_retry_after)),
            ),
        ],
    )
}

/// The `ApiDescriptor`: every IR field the runtime reads, in one place.
fn api_rx(plan: &Plan<'_>, version: &str, lets: &mut Lets) -> Rx {
    let ir = plan.ir;
    let retries = &ir.agent.retries;
    Rx::record(
        "ApiDescriptor",
        vec![
            ("name", Rx::string(&ir.api.name.wire)),
            ("version", Rx::string(version)),
            (
                "tungsten_version",
                Rx::string(&ir.generator.tungsten_version),
            ),
            (
                "servers",
                Rx::strings(ir.api.servers.iter().map(|s| s.url.as_str())),
            ),
            ("auth", auth_rs(ir)),
            (
                "error_codes",
                lets.map(
                    "error_codes",
                    ir.agent
                        .error_codes
                        .iter()
                        .map(|(code, r)| (code.clone(), remediation_rs(r)))
                        .collect(),
                ),
            ),
            (
                "ambiguous_statuses",
                Rx::list(
                    ir.agent
                        .ambiguous_statuses
                        .iter()
                        .map(|s| Rx::atom(s.to_string()))
                        .collect(),
                ),
            ),
            ("non_json", non_json_rs(ir)),
            (
                "gates",
                lets.map(
                    "gates",
                    ir.agent
                        .gates
                        .iter()
                        .map(|(k, v)| (k.clone(), Rx::string(v)))
                        .collect(),
                ),
            ),
            (
                "retries",
                Rx::some(Rx::record(
                    "TierRetries",
                    vec![
                        (
                            "read_only",
                            retry_rs(&retries.read_only, retries.honor_retry_after),
                        ),
                        (
                            "mutating",
                            retry_rs(&retries.mutating, retries.honor_retry_after),
                        ),
                    ],
                )),
            ),
        ],
    )
}

/// The source of `descriptors.rs`.
pub(crate) fn descriptors_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    version: &str,
    has_macros: bool,
    header: &str,
) -> String {
    let cx = Cx::new(plan, None);
    let streams = shapes.iter().any(|s| s.stream.is_some());
    let mut code = Writer::new("    ");
    for (i, info) in plan.ops.iter().enumerate() {
        code.line(format!("pub const {}: usize = {i};", info.konst));
    }
    code.blank();
    doc(&mut code, "Every descriptor of the API, built once.");
    code.line("#[derive(Debug)]");
    code.line("pub struct Descriptors {");
    code.indent();
    code.line("/// The API: servers, auth schemes, error mappings, retry defaults.");
    code.line("pub api: ApiDescriptor,");
    code.line("/// Every callable operation, in IR order (indexed by the `OP_*` constants).");
    code.line("pub operations: Vec<Arc<OperationDescriptor>>,");
    code.line("/// The same descriptors by value.");
    code.line("pub list: Vec<OperationDescriptor>,");
    code.line("/// Every macro, in IR order.");
    code.line("pub macros: Vec<MacroDescriptor>,");
    if streams {
        code.line("/// The event stream descriptors, by the index of their operation.");
        code.line("pub streams: Vec<(usize, Arc<StreamDescriptor>)>,");
    }
    code.dedent();
    code.line("}");
    code.blank();
    code.line("static DESCRIPTORS: OnceLock<Arc<Descriptors>> = OnceLock::new();");
    code.blank();
    doc(
        &mut code,
        "The descriptors of the API (built on first use).",
    );
    code.line("pub fn get() -> Arc<Descriptors> {");
    code.indent();
    code.line("DESCRIPTORS.get_or_init(|| Arc::new(build())).clone()");
    code.dedent();
    code.line("}");
    code.blank();
    code.line("fn build() -> Descriptors {");
    code.indent();
    let list = Rx::list(
        plan.ops
            .iter()
            .map(|o| Rx::call(o.builder.clone(), vec![]))
            .collect(),
    );
    put(&mut code, 4, "let list = ", &list, ";");
    code.line("let operations = list.iter().cloned().map(Arc::new).collect();");
    let macros = if has_macros {
        Rx::call("crate::macros::descriptors", vec![])
    } else {
        Rx::atom("Vec::new()")
    };
    put(&mut code, 4, "let macros = ", &macros, ";");
    let mut build_fields = vec![
        ("api", Rx::call("api", vec![])),
        ("operations", Rx::atom("operations")),
        ("list", Rx::atom("list")),
        ("macros", Rx::atom("macros")),
    ];
    if streams {
        let entries = Rx::list(
            plan.ops
                .iter()
                .zip(shapes)
                .enumerate()
                .filter(|(_, (_, shape))| shape.stream.is_some())
                .map(|(i, (info, _))| {
                    Rx::atom(format!("({i}, Arc::new({}_stream()))", info.builder))
                })
                .collect(),
        );
        put(&mut code, 4, "let streams = ", &entries, ";");
        build_fields.push(("streams", Rx::atom("streams")));
    }
    let build = Rx::record("Descriptors", build_fields);
    put(&mut code, 4, "", &build, "");
    code.dedent();
    code.line("}");
    code.blank();
    code.line("fn api() -> ApiDescriptor {");
    code.indent();
    let mut api_lets = Lets::default();
    let api_value = api_rx(plan, version, &mut api_lets);
    for l in &api_lets.stmts {
        code.line(l);
    }
    put(&mut code, 4, "", &api_value, "");
    code.dedent();
    code.line("}");
    for (info, shape) in plan.ops.iter().zip(shapes) {
        code.blank();
        code.line(format!("fn {}() -> OperationDescriptor {{", info.builder));
        code.indent();
        let (lets, value) = descriptor_rx(plan, info, shape);
        for l in &lets {
            code.line(l);
        }
        put(&mut code, 4, "", &value, "");
        code.dedent();
        code.line("}");
        if let Some((lets, value)) = shape
            .stream
            .as_ref()
            .and_then(|stream| stream_descriptor_rx(info, stream))
        {
            code.blank();
            code.line(format!(
                "fn {}_stream() -> StreamDescriptor {{",
                info.builder
            ));
            code.indent();
            for l in &lets {
                code.line(l);
            }
            put(&mut code, 4, "", &value, "");
            code.dedent();
            code.line("}");
        }
        for (name, ty, stmts) in &shape.helpers {
            code.blank();
            code.line(format!("fn {name}(v: &{ty}, c: &mut Checker) {{"));
            code.indent();
            for s in stmts {
                code.line(s);
            }
            code.dedent();
            code.line("}");
        }
    }
    let patterns = cx.patterns.borrow().clone();
    write_patterns(&mut code, &patterns);
    let code = code.finish();

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line("//! Operation descriptors: what the runtime needs to call each operation.");
    let mut extra: Vec<(String, String)> = vec![];
    for (info, shape) in plan.ops.iter().zip(shapes) {
        let module = format!("crate::resources::{}", plan.resources[info.res].module);
        extra.push((module.clone(), info.request.clone()));
        if shape_has_checks(plan, shape) {
            extra.push((module.clone(), request_check_name(info)));
        }
        if let Some(e) = &shape.by_status {
            extra.push((module.clone(), e.name.clone()));
            if status_check_stmts(&cx, e).is_some() {
                extra.push((module, response_check_name(info)));
            }
        }
    }
    let uses = imports_for(&code, &[], None, &extra);
    if !uses.is_empty() {
        w.blank();
        for u in &uses {
            w.line(u);
        }
    }
    let mut out = w.finish();
    out.push('\n');
    out.push_str(&code);
    out
}
