// SPDX-License-Identifier: AGPL-3.0-only
//! Pagination: an explicit `tungsten.yml` entry
//! wins (`none: true` switches inference off); otherwise callable GET
//! operations are matched against the heuristics below, which read the
//! normalized schemas directly. Every inference is a TG0501 warning.
//!
//! - Cursor: the 200 JSON response is a closed object with exactly one
//!   array property and exactly one `next_*` property, which is a nullable
//!   string; the request cursor is the query parameter named like the
//!   property without `next_`, or else the only one of `before`, `cursor`,
//!   `after`, `page_token` the operation has.
//!   An open (not closed) response object qualifies too when the parameter
//!   is a string named exactly like the property without `next_` (an opaque
//!   `page` cursor next to `next_page`).
//! - After id: query parameter `after_id`, and a response object with
//!   exactly one array property and a boolean `has_more`; the next cursor is
//!   the response's `last_id`, or the `id` of the last item when `last_id` is
//!   absent or null. The
//!   runtime stops as soon as `has_more` is false.
//! - Link header: the 200 response declares a `Link` header and its JSON
//!   body is an array or an object with exactly one array property; the
//!   runtime follows `rel="next"` until there is none (GitHub style). Tried
//!   after cursor and before offset and page, since the server's links are
//!   authoritative when it sends them.
//! - Offset: query parameters `offset` and `limit`, and a response that is
//!   an array or an object with exactly one array property.
//! - Page: query parameter `page` (not a string: a string `page` is an
//!   opaque cursor) plus one of `page_size`, `per_page`, `size`, `limit`,
//!   with the same response shape.

use serde_json::{Map, Value};
use tungsten_config::PaginationConfig;
use tungsten_core::Diagnostic;
use tungsten_ir::{
    Exhausted, HttpMethod, Operation, Pagination, PaginationStyle, Primitive, Shape, StatusMatch,
    TypeRef,
};
use tungsten_openapi::RefTarget;

use crate::ctx::{Ctx, child};
use crate::operations::BuiltOp;

const CURSOR_PARAMS: [&str; 4] = ["before", "cursor", "after", "page_token"];
const PAGE_SIZE_PARAMS: [&str; 3] = ["limit", "page_size", "per_page"];
const PAGE_PARAM_SIZES: [&str; 4] = ["page_size", "per_page", "size", "limit"];

/// Set the pagination of callable and planned operations. Only callable
/// operations are inferred.
pub(crate) fn apply(cx: &mut Ctx<'_>, callable: &mut [BuiltOp], planned: &mut [BuiltOp]) {
    let cfg = cx.cfg;
    for (built, infer) in callable
        .iter_mut()
        .map(|b| (b, true))
        .chain(planned.iter_mut().map(|b| (b, false)))
    {
        if let Some(entry) = cfg.pagination.get(&built.op.id.0) {
            built.op.pagination = configured(entry);
            continue;
        }
        if !infer {
            continue;
        }
        if let Some(p) = infer_for(cx, built) {
            let what = match &p.style {
                PaginationStyle::Cursor {
                    request_param,
                    response_field,
                } => {
                    let next = match (response_field.as_str(), &p.cursor_item_field) {
                        ("", Some(item)) => format!("last item {item}"),
                        _ => response_field.clone(),
                    };
                    match &p.has_more_field {
                        Some(more) => format!(
                            "cursor pagination ({request_param} / {next}, stops when {more} is false)"
                        ),
                        None => format!("cursor pagination ({request_param} / {next})"),
                    }
                }
                PaginationStyle::Offset { .. } => "offset pagination".to_string(),
                PaginationStyle::Page { .. } => "page pagination".to_string(),
                PaginationStyle::LinkHeader => "link-header pagination".to_string(),
            };
            cx.report(
                Diagnostic::warning(
                    "TG0501",
                    format!(
                        "`{}` inferred as {what} over `{}`",
                        built.op.id.0,
                        if p.items_field.is_empty() {
                            "the response"
                        } else {
                            &p.items_field
                        }
                    ),
                )
                .with_help("confirm it, or declare the operation under tungsten.yml `pagination`"),
                &built.at,
            );
            built.op.pagination = Some(p);
        }
    }
}

/// The pagination a manifest entry declares (`None` for `none: true`).
fn configured(entry: &PaginationConfig) -> Option<Pagination> {
    if let Some(c) = &entry.cursor {
        return Some(Pagination {
            style: PaginationStyle::Cursor {
                request_param: c.param.clone(),
                response_field: c.field.clone(),
            },
            items_field: c.items.clone(),
            page_size_param: c.page_size_param.clone(),
            exhausted_when: if c.has_more.is_some() {
                Exhausted::HasMoreFalse
            } else {
                Exhausted::CursorNull
            },
            has_more_field: c.has_more.clone(),
            cursor_item_field: None,
            inferred: false,
        });
    }
    if let Some(o) = &entry.offset {
        return Some(Pagination {
            style: PaginationStyle::Offset {
                offset_param: o.offset_param.clone(),
                limit_param: o.limit_param.clone(),
            },
            items_field: o.items.clone(),
            page_size_param: Some(o.limit_param.clone()),
            exhausted_when: Exhausted::EmptyItems,
            has_more_field: None,
            cursor_item_field: None,
            inferred: false,
        });
    }
    if let Some(p) = &entry.page {
        return Some(Pagination {
            style: PaginationStyle::Page {
                page_param: p.page_param.clone(),
                size_param: p.size_param.clone(),
            },
            items_field: p.items.clone(),
            page_size_param: Some(p.size_param.clone()),
            exhausted_when: Exhausted::EmptyItems,
            has_more_field: None,
            cursor_item_field: None,
            inferred: false,
        });
    }
    entry.link_header.as_ref().map(|l| Pagination {
        style: PaginationStyle::LinkHeader,
        items_field: l.items.clone(),
        page_size_param: None,
        exhausted_when: Exhausted::NoLink,
        has_more_field: None,
        cursor_item_field: None,
        inferred: false,
    })
}

fn infer_for(cx: &Ctx<'_>, built: &BuiltOp) -> Option<Pagination> {
    if built.op.method != HttpMethod::Get {
        return None;
    }
    let schema = built
        .responses
        .iter()
        .find(|r| r.status == StatusMatch::Exact(200))?
        .json_schema
        .as_ref()?;
    let (target, schema) = cx.deref_value(schema)?;
    let query: Vec<&str> = built
        .op
        .params
        .query
        .iter()
        .map(|p| p.wire_name.as_str())
        .collect();
    let op = &built.op;
    cursor(cx, op, &target, schema, &query)
        .or_else(|| after_id(cx, &target, schema, &query))
        .or_else(|| link_header(cx, op, &target, schema, &query))
        .or_else(|| offset_or_page(cx, op, &target, schema, &query))
}

fn link_header(
    cx: &Ctx<'_>,
    op: &Operation,
    target: &RefTarget,
    schema: &Value,
    query: &[&str],
) -> Option<Pagination> {
    let declares_link = op
        .responses
        .iter()
        .find(|r| r.status == StatusMatch::Exact(200))?
        .headers
        .iter()
        .any(|h| h.wire_name.eq_ignore_ascii_case("link"));
    if !declares_link {
        return None;
    }
    let items = if is_array(schema) {
        String::new()
    } else {
        single_array_property(cx, target, schema.get("properties")?.as_object()?)?
    };
    Some(Pagination {
        style: PaginationStyle::LinkHeader,
        items_field: items,
        page_size_param: first_present(&PAGE_SIZE_PARAMS, query),
        exhausted_when: Exhausted::NoLink,
        has_more_field: None,
        cursor_item_field: None,
        inferred: true,
    })
}

fn cursor(
    cx: &Ctx<'_>,
    op: &Operation,
    target: &RefTarget,
    schema: &Value,
    query: &[&str],
) -> Option<Pagination> {
    let closed = is_closed(schema);
    let props = schema.get("properties")?.as_object()?;
    let items = single_array_property(cx, target, props)?;
    let mut nexts = props.keys().filter(|k| k.starts_with("next_"));
    let (Some(next), None) = (nexts.next(), nexts.next()) else {
        return None;
    };
    let (next_target, next_schema) = cx.deref_value(&property(target, next))?;
    if !is_nullable_string(cx, &next_target, next_schema) {
        return None;
    }
    let suffix = &next["next_".len()..];
    let request = if query.contains(&suffix) {
        // An open object is only trusted with an exact, string-typed match.
        if !closed && !is_string_param(op, suffix) {
            return None;
        }
        suffix.to_string()
    } else if closed {
        let mut known = CURSOR_PARAMS.iter().filter(|c| query.contains(c));
        match (known.next(), known.next()) {
            (Some(only), None) => only.to_string(),
            _ => return None,
        }
    } else {
        return None;
    };
    Some(Pagination {
        style: PaginationStyle::Cursor {
            request_param: request,
            response_field: next.clone(),
        },
        items_field: items,
        page_size_param: first_present(&PAGE_SIZE_PARAMS, query),
        exhausted_when: Exhausted::CursorNull,
        has_more_field: None,
        cursor_item_field: None,
        inferred: true,
    })
}

/// `after_id` request, `has_more` stop; the next cursor is `last_id` or the
/// last item's `id`.
fn after_id(
    cx: &Ctx<'_>,
    target: &RefTarget,
    schema: &Value,
    query: &[&str],
) -> Option<Pagination> {
    if !query.contains(&"after_id") {
        return None;
    }
    let props = schema.get("properties")?.as_object()?;
    let items = single_array_property(cx, target, props)?;
    let (_, has_more) = cx.deref_value(&property(target, "has_more"))?;
    if types(has_more) != ["boolean"] {
        return None;
    }
    let has_last_id = cx
        .deref_value(&property(target, "last_id"))
        .is_some_and(|(t, s)| types(s) == ["string"] || is_nullable_string(cx, &t, s));
    let item_id = items_have_string_id(cx, target, &items).then(|| "id".to_string());
    if !has_last_id && item_id.is_none() {
        return None;
    }
    let (response_field, cursor_item_field) = (if has_last_id { "last_id" } else { "" }, item_id);
    Some(Pagination {
        style: PaginationStyle::Cursor {
            request_param: "after_id".into(),
            response_field: response_field.into(),
        },
        items_field: items,
        page_size_param: first_present(&PAGE_SIZE_PARAMS, query),
        exhausted_when: Exhausted::HasMoreFalse,
        has_more_field: Some("has_more".into()),
        cursor_item_field,
        inferred: true,
    })
}

/// The array property's items are objects with a string `id` property.
fn items_have_string_id(cx: &Ctx<'_>, target: &RefTarget, array: &str) -> bool {
    let Some((array_target, _)) = cx.deref_value(&property(target, array)) else {
        return false;
    };
    let Some((item_target, _)) = cx.deref_value(&child(&array_target, "items")) else {
        return false;
    };
    cx.deref_value(&property(&item_target, "id"))
        .is_some_and(|(_, id)| types(id) == ["string"])
}

/// The query parameter `name` is a string (possibly nullable).
fn is_string_param(op: &Operation, name: &str) -> bool {
    op.params
        .query
        .iter()
        .find(|p| p.wire_name == name)
        .is_some_and(|p| is_string_type(&p.ty))
}

fn is_string_type(ty: &TypeRef) -> bool {
    let TypeRef::Inline(shape) = ty else {
        return false;
    };
    match shape.as_ref() {
        Shape::Primitive {
            primitive: Primitive::String { .. },
            ..
        }
        | Shape::Enum {
            base: Primitive::String { .. },
            ..
        } => true,
        Shape::Nullable { inner } => is_string_type(inner),
        _ => false,
    }
}

fn offset_or_page(
    cx: &Ctx<'_>,
    op: &Operation,
    target: &RefTarget,
    schema: &Value,
    query: &[&str],
) -> Option<Pagination> {
    let (style, size) = if query.contains(&"offset") && query.contains(&"limit") {
        let style = PaginationStyle::Offset {
            offset_param: "offset".into(),
            limit_param: "limit".into(),
        };
        (style, "limit".to_string())
    } else if query.contains(&"page") && !is_string_param(op, "page") {
        let size = first_present(&PAGE_PARAM_SIZES, query)?;
        let style = PaginationStyle::Page {
            page_param: "page".into(),
            size_param: size.clone(),
        };
        (style, size)
    } else {
        return None;
    };
    let items = if is_array(schema) {
        String::new()
    } else {
        single_array_property(cx, target, schema.get("properties")?.as_object()?)?
    };
    Some(Pagination {
        style,
        items_field: items,
        page_size_param: Some(size),
        exhausted_when: Exhausted::EmptyItems,
        has_more_field: None,
        cursor_item_field: None,
        inferred: true,
    })
}

fn first_present(names: &[&str], query: &[&str]) -> Option<String> {
    names
        .iter()
        .find(|n| query.contains(n))
        .map(|n| n.to_string())
}

fn property(object: &RefTarget, name: &str) -> RefTarget {
    child(&child(object, "properties"), name)
}

/// The name of the only array-typed property, if exactly one exists.
fn single_array_property(
    cx: &Ctx<'_>,
    target: &RefTarget,
    props: &Map<String, Value>,
) -> Option<String> {
    let mut arrays = props.keys().filter(|name| {
        cx.deref_value(&property(target, name))
            .is_some_and(|(_, s)| is_array(s))
    });
    match (arrays.next(), arrays.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

/// `additionalProperties: false` or `unevaluatedProperties: false`.
fn is_closed(schema: &Value) -> bool {
    ["additionalProperties", "unevaluatedProperties"]
        .iter()
        .any(|k| schema.get(*k) == Some(&Value::Bool(false)))
}

/// The `type` keyword as a list of names.
fn types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

fn is_array(schema: &Value) -> bool {
    types(schema).contains(&"array")
}

/// `type: [string, null]`, or an `anyOf`/`oneOf` of a string schema and a
/// null schema.
fn is_nullable_string(cx: &Ctx<'_>, target: &RefTarget, schema: &Value) -> bool {
    let mut t = types(schema);
    t.sort_unstable();
    if t == ["null", "string"] {
        return true;
    }
    for key in ["anyOf", "oneOf"] {
        let Some(Value::Array(members)) = schema.get(key) else {
            continue;
        };
        if members.len() != 2 {
            continue;
        }
        let list = child(target, key);
        let resolved: Vec<&Value> = (0..members.len())
            .filter_map(|i| cx.deref_value(&child(&list, &i.to_string())))
            .map(|(_, m)| m)
            .collect();
        let null = resolved
            .iter()
            .any(|m| types(m) == ["null"] || m.get("const") == Some(&Value::Null));
        let string = resolved.iter().any(|m| types(m) == ["string"]);
        if null && string {
            return true;
        }
    }
    false
}
