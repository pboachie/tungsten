// SPDX-License-Identifier: AGPL-3.0-only
//! Request bodies and media-type content (`Body`, `BodyContent`). One entry per media type, in spec order.

use serde_json::Value;
use tungsten_core::Diagnostic;
use tungsten_ir::{
    Body, BodyContent, BodyEncoding, PartEncoding, PartHeader, Primitive, Shape, TypeRef,
};
use tungsten_openapi::RefTarget;

use crate::ctx::{Ctx, child, doc, flag, str_of};
use crate::operations::OpScope;

/// The wire encoding of a media type: JSON (`application/json`, `+json`),
/// JSON lines (`application/jsonl`, `application/x-jsonl`,
/// `application/ndjson`, `application/x-ndjson`), form, multipart, text, and
/// bytes for everything else (including `application/octet-stream` and
/// vendor types). Parameters (`; charset`) and case are ignored.
pub(crate) fn encoding_for(media_type: &str) -> BodyEncoding {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if essence == "application/json" || essence.ends_with("+json") {
        BodyEncoding::Json
    } else if matches!(
        essence.as_str(),
        "application/jsonl" | "application/x-jsonl" | "application/ndjson" | "application/x-ndjson"
    ) {
        BodyEncoding::Jsonl
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
/// for a binary string or no schema; and that JSON Lines whose schema is a
/// binary string is a raw byte stream, not lines of JSON.
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
    let schema = schema.and_then(|s| cx.deref_value(s));
    if essence != "*/*" && essence != "application/*" {
        let encoding = encoding_for(media_type);
        let raw = schema.is_some_and(|(_, schema)| is_binary_string(schema));
        return if encoding == BodyEncoding::Jsonl && raw {
            BodyEncoding::Bytes
        } else {
            encoding
        };
    }
    let Some((_, schema)) = schema else {
        return BodyEncoding::Bytes;
    };
    if schema_types(schema) != ["string"] {
        return BodyEncoding::Json;
    }
    if is_binary_string(schema) {
        BodyEncoding::Bytes
    } else {
        BodyEncoding::Text
    }
}

/// The non-null `type`s of a schema.
fn schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect(),
        _ => vec![],
    }
}

/// Whether the schema is a string of raw bytes: `format: binary`, or a
/// `contentMediaType` that is not text or JSON, without a `contentEncoding`.
fn is_binary_string(schema: &Value) -> bool {
    schema_types(schema) == ["string"]
        && (str_of(schema, "format") == Some("binary")
            || (schema.get("contentEncoding").is_none()
                && str_of(schema, "contentMediaType")
                    .is_some_and(|m| encoding_for(m) == BodyEncoding::Bytes)))
}

/// The operation's request body, if it declares one.
pub(crate) fn request_body(cx: &mut Ctx<'_>, scope: &OpScope<'_>, op: &RefTarget) -> Option<Body> {
    let at = child(op, "requestBody");
    cx.get(&at)?;
    let (target, value) = cx.deref_value(&at)?;
    let content = content(cx, scope.ns, &target, &[&scope.hint, "Body"], true);
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

/// The entries of the `content` map of `owner` (a Request Body when
/// `request`, else a Response Object). With more than one media type, the
/// encoding joins the hint so inline schemas get distinct names. JSON lines
/// are a response encoding only: a request body of that media type is sent
/// as bytes.
pub(crate) fn content(
    cx: &mut Ctx<'_>,
    namespace: &str,
    owner: &RefTarget,
    hint: &[&str],
    request: bool,
) -> Vec<BodyContent> {
    let map_at = child(owner, "content");
    let Some(Value::Object(map)) = cx.get(&map_at) else {
        return vec![];
    };
    let several = map.len() > 1;
    let mut out = vec![];
    let reads_3_2 = !request && crate::pathitem::reads_3_2(cx.ws, &map_at);
    for media_type in map.keys() {
        let media_at = media_at(cx, &map_at, media_type);
        // A 3.2 response `itemSchema` the compiler supports stands in for
        // `schema`: the type of one JSON line or of one event's data.
        let item = reads_3_2
            .then(|| cx.get(&media_at).and_then(Value::as_object))
            .flatten()
            .and_then(|m| tungsten_openapi::response_item_schema(media_type, m));
        let entry_at = child(&map_at, media_type);
        if item.is_none()
            && media_at.doc != entry_at.doc
            && crate::pathitem::reads_3_2(cx.ws, &media_at)
            && cx
                .get(&media_at)
                .is_some_and(|m| m.get("itemSchema").is_some())
        {
            // A reusable media type of another document: its own scan leaves
            // `itemSchema` to the use, which only the build sees.
            cx.report(
                Diagnostic::error(
                    "TG0145",
                    "the referenced media type uses OpenAPI 3.2 `itemSchema` in a way that is not supported here",
                )
                .with_help("only response JSON Lines items and server-sent events whose `data` has a JSON `contentSchema` are compiled; otherwise describe the whole payload with `schema`"),
                &child(&entry_at, "$ref"),
            );
        }
        let schema = match item {
            Some(suffix) => RefTarget {
                doc: media_at.doc,
                pointer: format!("{}{suffix}", media_at.pointer),
            },
            None => child(&media_at, "schema"),
        };
        let declared = cx.get(&schema).map(|_| &schema);
        let mut encoding = content_encoding(cx, media_type, declared);
        let lines_as_bytes = request && encoding == BodyEncoding::Jsonl;
        if lines_as_bytes {
            encoding = BodyEncoding::Bytes;
        }
        let ty = match cx.get(&schema) {
            _ if lines_as_bytes => schemaless(encoding),
            Some(_) => {
                let mut words: Vec<&str> = hint.to_vec();
                if several {
                    words.insert(words.len().saturating_sub(1), encoding_word(encoding));
                }
                cx.tb.type_ref(namespace, &schema, &words)
            }
            None => schemaless(encoding),
        };
        let parts = if request && encoding == BodyEncoding::Multipart {
            part_encodings(cx, &media_at)
        } else {
            vec![]
        };
        out.push(BodyContent {
            media_type: media_type.clone(),
            ty,
            encoding,
            parts,
        });
    }
    out
}

/// The `encoding` map of the multipart Media Type Object at `media_at`: the
/// entries whose `contentType` names a concrete media type or that declare a
/// header with a fixed value. Keys that name no property of the schema are
/// kept; runtimes only read the parts a value carries.
fn part_encodings(cx: &Ctx<'_>, media_at: &RefTarget) -> Vec<PartEncoding> {
    let encoding_at = child(media_at, "encoding");
    let Some(Value::Object(map)) = cx.get(&encoding_at) else {
        return vec![];
    };
    map.iter()
        .filter_map(|(name, encoding)| {
            let content_type = str_of(encoding, "contentType").and_then(concrete_media_type);
            let headers = part_headers(cx, &child(&child(&encoding_at, name), "headers"));
            (content_type.is_some() || !headers.is_empty()).then(|| PartEncoding {
                name: name.clone(),
                content_type,
                headers,
            })
        })
        .collect()
}

/// The headers of an Encoding Object's `headers` map at `at` whose schema
/// fixes one scalar value. `Content-Type` (which the spec says to ignore
/// there) and `Content-Disposition` (which names the part) are skipped, and
/// so are names that are not header tokens and values a header cannot carry.
fn part_headers(cx: &Ctx<'_>, at: &RefTarget) -> Vec<PartHeader> {
    let Some(Value::Object(map)) = cx.get(at) else {
        return vec![];
    };
    map.keys()
        .filter(|name| {
            !name.eq_ignore_ascii_case("content-type")
                && !name.eq_ignore_ascii_case("content-disposition")
                && !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        })
        .filter_map(|name| {
            let (header_at, _) = cx.deref_value(&child(at, name))?;
            let (_, schema) = cx.deref_value(&child(&header_at, "schema"))?;
            let fixed = match (schema.get("const"), schema.get("enum")) {
                (Some(value), _) => value,
                (None, Some(Value::Array(values))) if values.len() == 1 => &values[0],
                _ => return None,
            };
            let value = match fixed {
                Value::String(text) => text.clone(),
                Value::Number(_) | Value::Bool(_) => fixed.to_string(),
                _ => return None,
            };
            value
                .bytes()
                .all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
                .then(|| PartHeader {
                    name: name.clone(),
                    value,
                })
        })
        .collect()
}

/// The first entry of a comma-separated `contentType` list that is a valid
/// media type without wildcards, normalized to lower-case type and subtype.
fn concrete_media_type(list: &str) -> Option<String> {
    list.split(',').map(str::trim).find_map(|entry| {
        let (essence, params) = entry.split_once(';').unwrap_or((entry, ""));
        let (kind, subtype) = essence.trim().split_once('/')?;
        let token = |t: &str| {
            !t.is_empty()
                && t != "*"
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                && !t.contains('*')
        };
        if !token(kind)
            || !token(subtype)
            || !params
                .bytes()
                .all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
        {
            return None;
        }
        let essence = format!(
            "{}/{}",
            kind.to_ascii_lowercase(),
            subtype.to_ascii_lowercase()
        );
        let params = params.trim();
        Some(if params.is_empty() {
            essence
        } else {
            format!("{essence}; {params}")
        })
    })
}

/// The media type and schema of the first JSON media type of `owner`'s
/// content that declares a schema ([`content_encoding`]).
pub(crate) fn json_schema(cx: &Ctx<'_>, owner: &RefTarget) -> Option<(String, RefTarget)> {
    let map_at = child(owner, "content");
    let map = cx.get(&map_at)?.as_object()?;
    map.keys()
        .map(|media| (media, child(&media_at(cx, &map_at, media), "schema")))
        .filter(|(_, schema)| cx.get(schema).is_some())
        .find(|(media, schema)| content_encoding(cx, media, Some(schema)) == BodyEncoding::Json)
        .map(|(media, schema)| (media.clone(), schema))
}

/// The Media Type Object of `media` in the `content` map at `map_at`,
/// following an OpenAPI 3.2 `$ref` to a reusable media type.
pub(crate) fn media_at(cx: &Ctx<'_>, map_at: &RefTarget, media: &str) -> RefTarget {
    let at = child(map_at, media);
    cx.deref(&at).unwrap_or(at)
}

fn encoding_word(encoding: BodyEncoding) -> &'static str {
    match encoding {
        BodyEncoding::Json => "Json",
        BodyEncoding::Form => "Form",
        BodyEncoding::Multipart => "Multipart",
        BodyEncoding::Bytes => "Bytes",
        BodyEncoding::Text => "Text",
        BodyEncoding::Jsonl => "Jsonl",
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
        BodyEncoding::Json | BodyEncoding::Form | BodyEncoding::Multipart | BodyEncoding::Jsonl => {
            Shape::Any
        }
    };
    TypeRef::Inline(Box::new(shape))
}
