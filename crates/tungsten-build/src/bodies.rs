// SPDX-License-Identifier: AGPL-3.0-only
//! Request bodies and media-type content (planning/03 `Body`,
//! `BodyContent`). One entry per media type, in spec order.

use serde_json::Value;
use tungsten_ir::{Body, BodyContent, BodyEncoding, Primitive, Shape, TypeRef};
use tungsten_openapi::RefTarget;

use crate::ctx::{Ctx, child, doc, flag, str_of};
use crate::operations::OpScope;

/// The wire encoding of a media type: JSON (`application/json`, `+json`),
/// form, multipart, text, and bytes for everything else (including
/// `application/octet-stream` and vendor types). Parameters (`; charset`)
/// and case are ignored.
pub(crate) fn encoding_for(media_type: &str) -> BodyEncoding {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if essence == "application/json" || essence.ends_with("+json") {
        BodyEncoding::Json
    } else if essence == "application/x-www-form-urlencoded" {
        BodyEncoding::Form
    } else if essence.starts_with("multipart/") {
        BodyEncoding::Multipart
    } else if essence.starts_with("text/") {
        BodyEncoding::Text
    } else {
        BodyEncoding::Bytes
    }
}

/// The encoding of one `content` entry: [`encoding_for`] its media type,
/// except that a wildcard media type (`*/*`, `application/*`, as springdoc
/// writes for every response) is read from its schema: JSON for an object,
/// array, number, boolean or composed schema, text for a string, and bytes
/// for a binary string or no schema.
pub(crate) fn content_encoding(
    cx: &Ctx<'_>,
    media_type: &str,
    schema: Option<&RefTarget>,
) -> BodyEncoding {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if essence != "*/*" && essence != "application/*" {
        return encoding_for(media_type);
    }
    let Some((_, schema)) = schema.and_then(|s| cx.deref_value(s)) else {
        return BodyEncoding::Bytes;
    };
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect(),
        _ => vec![],
    };
    if types != ["string"] {
        return BodyEncoding::Json;
    }
    let binary = str_of(schema, "format") == Some("binary")
        || (schema.get("contentEncoding").is_none()
            && str_of(schema, "contentMediaType")
                .is_some_and(|m| encoding_for(m) == BodyEncoding::Bytes));
    if binary {
        BodyEncoding::Bytes
    } else {
        BodyEncoding::Text
    }
}

/// The operation's request body, if it declares one.
pub(crate) fn request_body(cx: &mut Ctx<'_>, scope: &OpScope<'_>, op: &RefTarget) -> Option<Body> {
    let at = child(op, "requestBody");
    cx.get(&at)?;
    let (target, value) = cx.deref_value(&at)?;
    let content = content(cx, scope.ns, &target, &[&scope.hint, "Body"]);
    Some(Body {
        content,
        required: flag(value, "required"),
        doc: doc(None, str_of(value, "description")),
    })
}

/// Whether the operation's request body is declared `required: true`.
pub(crate) fn is_required(cx: &Ctx<'_>, op: &RefTarget) -> bool {
    cx.deref_value(&child(op, "requestBody"))
        .is_some_and(|(_, value)| flag(value, "required"))
}

/// The entries of the `content` map of `owner` (a Request Body or Response
/// Object). With more than one media type, the encoding joins the hint so
/// inline schemas get distinct names.
pub(crate) fn content(
    cx: &mut Ctx<'_>,
    namespace: &str,
    owner: &RefTarget,
    hint: &[&str],
) -> Vec<BodyContent> {
    let map_at = child(owner, "content");
    let Some(Value::Object(map)) = cx.get(&map_at) else {
        return vec![];
    };
    let several = map.len() > 1;
    let mut out = vec![];
    for media_type in map.keys() {
        let schema = child(&child(&map_at, media_type), "schema");
        let declared = cx.get(&schema).map(|_| &schema);
        let encoding = content_encoding(cx, media_type, declared);
        let ty = match cx.get(&schema) {
            Some(_) => {
                let mut words: Vec<&str> = hint.to_vec();
                if several {
                    words.insert(words.len().saturating_sub(1), encoding_word(encoding));
                }
                cx.tb.type_ref(namespace, &schema, &words)
            }
            None => schemaless(encoding),
        };
        out.push(BodyContent {
            media_type: media_type.clone(),
            ty,
            encoding,
        });
    }
    out
}

/// The media type and schema of the first JSON media type of `owner`'s
/// content that declares a schema ([`content_encoding`]).
pub(crate) fn json_schema(cx: &Ctx<'_>, owner: &RefTarget) -> Option<(String, RefTarget)> {
    let map_at = child(owner, "content");
    let map = cx.get(&map_at)?.as_object()?;
    map.iter()
        .filter(|(_, entry)| entry.get("schema").is_some())
        .map(|(media, _)| (media, child(&child(&map_at, media), "schema")))
        .find(|(media, schema)| content_encoding(cx, media, Some(schema)) == BodyEncoding::Json)
        .map(|(media, schema)| (media.clone(), schema))
}

fn encoding_word(encoding: BodyEncoding) -> &'static str {
    match encoding {
        BodyEncoding::Json => "Json",
        BodyEncoding::Form => "Form",
        BodyEncoding::Multipart => "Multipart",
        BodyEncoding::Bytes => "Bytes",
        BodyEncoding::Text => "Text",
    }
}

/// The type of a media type declared without a schema.
fn schemaless(encoding: BodyEncoding) -> TypeRef {
    let shape = match encoding {
        BodyEncoding::Bytes => Shape::Primitive {
            primitive: Primitive::Bytes,
            constraints: Default::default(),
        },
        BodyEncoding::Text => Shape::Primitive {
            primitive: Primitive::String { format: None },
            constraints: Default::default(),
        },
        BodyEncoding::Json | BodyEncoding::Form | BodyEncoding::Multipart => Shape::Any,
    };
    TypeRef::Inline(Box::new(shape))
}
