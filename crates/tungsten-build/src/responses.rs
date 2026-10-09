// SPDX-License-Identifier: AGPL-3.0-only
//! Responses: exact statuses ascending, then
//! `NXX` ranges, then `default`. 1xx to 3xx are successes and everything
//! else is an error; `Ambiguous` is decided later by `agent.yml`
//! (see `tungsten-agent`).

use serde_json::Value;
use tungsten_core::Diagnostic;
use tungsten_ir::{Header, Response, ResponseKind, Shape, StatusMatch, TypeRef};
use tungsten_openapi::RefTarget;

use crate::bodies;
use crate::ctx::{Ctx, child, doc, flag, str_of};
use crate::operations::OpScope;

/// What later steps (pagination, error model) read from a response
/// without going through types.
#[derive(Debug, Clone)]
pub(crate) struct RawResponse {
    pub status: StatusMatch,
    pub description: String,
    /// Schema of the first JSON media type, as written (it may be a `$ref`).
    pub json_schema: Option<RefTarget>,
}

impl RawResponse {
    pub fn is_error(&self) -> bool {
        kind_of(self.status) == ResponseKind::Error
    }
}

/// Parse a Responses Object key. `None` for anything else.
pub(crate) fn parse_status(key: &str) -> Option<StatusMatch> {
    if key == "default" {
        return Some(StatusMatch::Default);
    }
    let bytes = key.as_bytes();
    if bytes.len() != 3 || !(b'1'..=b'5').contains(&bytes[0]) {
        return None;
    }
    let class = bytes[0] - b'0';
    if bytes[1..].iter().all(|b| b.eq_ignore_ascii_case(&b'x')) {
        return Some(StatusMatch::Range(class));
    }
    key.parse::<u16>().ok().map(StatusMatch::Exact)
}

/// Success for 1xx to 3xx, error otherwise (including `default`).
pub(crate) fn kind_of(status: StatusMatch) -> ResponseKind {
    let success = match status {
        StatusMatch::Exact(code) => code < 400,
        StatusMatch::Range(class) => class < 4,
        StatusMatch::Default => false,
    };
    if success {
        ResponseKind::Success
    } else {
        ResponseKind::Error
    }
}

/// Build every response of an operation, sorted.
pub(crate) fn build(
    cx: &mut Ctx<'_>,
    scope: &OpScope<'_>,
    op: &RefTarget,
) -> (Vec<Response>, Vec<RawResponse>) {
    let map_at = child(op, "responses");
    let Some(map) = cx.get(&map_at) else {
        return (vec![], vec![]);
    };
    let Some(map) = map.as_object() else {
        cx.report(
            Diagnostic::warning("TG0508", "`responses` must be an object; ignored"),
            &map_at,
        );
        return (vec![], vec![]);
    };
    let mut entries: Vec<(StatusMatch, RefTarget)> = vec![];
    for key in map.keys() {
        let at = child(&map_at, key);
        match parse_status(key) {
            Some(status) => entries.push((status, at)),
            None => cx.report(
                Diagnostic::warning(
                    "TG0508",
                    format!(
                        "response key `{key}` is not a status code, a range like 4XX, or default; ignored"
                    ),
                ),
                &at,
            ),
        }
    }
    entries.sort_by_key(|(status, _)| *status);

    let mut responses = vec![];
    let mut raws = vec![];
    let mut primary_named = false;
    for (status, at) in entries {
        let Some((target, value)) = cx.deref_value(&at) else {
            continue;
        };
        let kind = kind_of(status);
        let has_content = value
            .get("content")
            .and_then(Value::as_object)
            .is_some_and(|c| !c.is_empty());
        let label = status_word(status);
        let hint: Vec<&str> = if kind == ResponseKind::Success && has_content && !primary_named {
            primary_named = true;
            vec![&scope.hint, "Response"]
        } else {
            vec![&scope.hint, &label, "Response"]
        };
        let content = bodies::content(cx, scope.ns, &target, &hint);
        let headers = headers(cx, scope, &target);
        let description = str_of(value, "description").unwrap_or("").to_string();
        raws.push(RawResponse {
            status,
            description: description.clone(),
            json_schema: bodies::json_schema(cx, &target).map(|(_, schema)| schema),
        });
        responses.push(Response {
            status,
            content,
            headers,
            kind,
            doc: doc(None, Some(&description)),
        });
    }
    (responses, raws)
}

/// `200`, `4XX`, `Default`.
fn status_word(status: StatusMatch) -> String {
    match status {
        StatusMatch::Exact(code) => code.to_string(),
        StatusMatch::Range(class) => format!("{class}XX"),
        StatusMatch::Default => "Default".into(),
    }
}

/// Response headers in spec order. `Content-Type` is ignored, as OpenAPI
/// specifies.
fn headers(cx: &mut Ctx<'_>, scope: &OpScope<'_>, response: &RefTarget) -> Vec<Header> {
    let map_at = child(response, "headers");
    let Some(Value::Object(map)) = cx.get(&map_at) else {
        return vec![];
    };
    let mut out = vec![];
    for name in map.keys() {
        if name.eq_ignore_ascii_case("content-type") {
            continue;
        }
        let Some((target, value)) = cx.deref_value(&child(&map_at, name)) else {
            continue;
        };
        let schema = header_schema(cx, &target, value);
        let ty = match schema {
            Some(s) => cx.tb.type_ref(scope.ns, &s, &[&scope.hint, name, "Header"]),
            None => TypeRef::Inline(Box::new(Shape::Any)),
        };
        out.push(Header {
            wire_name: name.clone(),
            ty,
            required: flag(value, "required"),
            doc: doc(None, str_of(value, "description")),
        });
    }
    out
}

/// A Header Object's `schema`, or the schema of its single `content` entry.
fn header_schema(cx: &Ctx<'_>, header: &RefTarget, value: &Value) -> Option<RefTarget> {
    if value.get("schema").is_some() {
        return Some(child(header, "schema"));
    }
    let (media, _) = value.get("content")?.as_object()?.iter().next()?;
    let target = child(&child(&child(header, "content"), media), "schema");
    cx.get(&target).map(|_| target)
}
