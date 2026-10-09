// SPDX-License-Identifier: AGPL-3.0-only
//! Pure helpers over schema objects: keyword classes, declared and inferred
//! JSON types, docs, constraints and primitive formats.

use serde_json::{Map, Value};
use tungsten_ir::{Constraints, Doc, Primitive, StringFormat};

pub(super) type Object = Map<String, Value>;

/// Keywords that describe a schema without constraining its values.
/// `discriminator` only matters next to `oneOf`/`anyOf`, where the union
/// code reads it; `$defs`/`definitions` only hold schemas for `$ref`.
const ANNOTATIONS: &[&str] = &[
    "$anchor",
    "$comment",
    "$defs",
    "$dynamicAnchor",
    "$id",
    "$schema",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
    "default",
    "definitions",
    "deprecated",
    "description",
    "discriminator",
    "example",
    "examples",
    "externalDocs",
    "readOnly",
    "title",
    "writeOnly",
    "xml",
];

/// Keywords tungsten does not model. Each occurrence is reported once as
/// TG0303 and the keyword is ignored; the rest of the schema still builds.
const UNSUPPORTED: &[&str] = &[
    "$dynamicRef",
    "additionalItems",
    "contains",
    "dependentRequired",
    "dependentSchemas",
    "else",
    "if",
    "maxContains",
    "minContains",
    "not",
    "patternProperties",
    "prefixItems",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
];

/// Keywords that imply `type: object` when `type` is absent.
const OBJECT_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "dependentRequired",
    "dependentSchemas",
    "maxProperties",
    "minProperties",
    "patternProperties",
    "properties",
    "propertyNames",
    "required",
    "unevaluatedProperties",
];
/// Keywords that imply `type: array` when `type` is absent.
const ARRAY_KEYWORDS: &[&str] = &[
    "additionalItems",
    "contains",
    "items",
    "maxContains",
    "maxItems",
    "minContains",
    "minItems",
    "prefixItems",
    "unevaluatedItems",
    "uniqueItems",
];
/// Keywords that imply `type: string` when `type` is absent.
const STRING_KEYWORDS: &[&str] = &[
    "contentEncoding",
    "contentMediaType",
    "maxLength",
    "minLength",
    "pattern",
];
/// Keywords that imply `type: number` when `type` is absent.
const NUMBER_KEYWORDS: &[&str] = &[
    "exclusiveMaximum",
    "exclusiveMinimum",
    "maximum",
    "minimum",
    "multipleOf",
];

/// Whether `key` annotates a schema (including `x-*` extensions).
pub(super) fn is_annotation(key: &str) -> bool {
    key.starts_with("x-") || ANNOTATIONS.contains(&key)
}

/// Whether `key` is a keyword tungsten ignores with TG0303.
pub(super) fn is_unsupported(key: &str) -> bool {
    UNSUPPORTED.contains(&key)
}

/// Whether `key` constrains values in a way the converter models.
pub(super) fn is_structural(key: &str) -> bool {
    !is_annotation(key) && !is_unsupported(key)
}

/// True when the schema has no keyword the converter models: only
/// annotations and unsupported keywords (`{}`, `{description}`, `{if, then}`).
pub(super) fn is_inert(map: &Object) -> bool {
    !map.keys().any(|k| is_structural(k))
}

/// The unsupported keywords of a schema, in document order. `then` and
/// `else` are folded into a sibling `if`.
pub(super) fn unsupported_keywords(map: &Object) -> impl Iterator<Item = &str> {
    let has_if = map.contains_key("if");
    map.keys()
        .map(String::as_str)
        .filter(move |k| is_unsupported(k) && !(has_if && (*k == "then" || *k == "else")))
}

/// JSON value types, as named by the `type` keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum JsonType {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Array,
    Object,
}

impl JsonType {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "null" => Self::Null,
            "boolean" => Self::Boolean,
            "integer" => Self::Integer,
            "number" => Self::Number,
            "string" => Self::String,
            "array" => Self::Array,
            "object" => Self::Object,
            _ => return None,
        })
    }

    /// The JSON type of a value; whole numbers are `Integer`.
    pub(super) fn of(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Boolean,
            Value::Number(n) if n.is_i64() || n.is_u64() => Self::Integer,
            Value::Number(_) => Self::Number,
            Value::String(_) => Self::String,
            Value::Array(_) => Self::Array,
            Value::Object(_) => Self::Object,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::String => "string",
            Self::Array => "array",
            Self::Object => "object",
        }
    }
}

/// The declared `type`: `Ok(None)` when absent, the listed types in
/// declaration order (duplicates removed) otherwise, `Err` with the
/// offending text when a member is not a JSON type name.
pub(super) fn declared_types(map: &Object) -> Result<Option<Vec<JsonType>>, String> {
    let parse = |v: &Value| match v {
        Value::String(s) => JsonType::parse(s).ok_or_else(|| s.clone()),
        other => Err(other.to_string()),
    };
    match map.get("type") {
        None => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = vec![];
            for item in items {
                let t = parse(item)?;
                if !out.contains(&t) {
                    out.push(t);
                }
            }
            Ok(Some(out))
        }
        Some(one) => parse(one).map(|t| Some(vec![t])),
    }
}

/// The non-null declared types, when `type` is present and valid.
pub(super) fn non_null_types(map: &Object) -> Option<Vec<JsonType>> {
    let types = declared_types(map).ok()??;
    Some(types.into_iter().filter(|t| *t != JsonType::Null).collect())
}

/// What the keywords of a schema without `type` imply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Inferred {
    One(JsonType),
    /// Keywords of more than one type are present.
    Ambiguous,
    /// No type-specific keyword.
    Unknown,
}

/// Infer the type of a schema without `type` from its keywords. A `format`
/// implies `integer`/`number` for the numeric formats and `string`
/// otherwise.
pub(super) fn infer_type(map: &Object) -> Inferred {
    let has = |keys: &[&str]| keys.iter().any(|k| map.contains_key(*k));
    let format = map.get("format").and_then(Value::as_str);
    let mut found = vec![];
    if has(OBJECT_KEYWORDS) {
        found.push(JsonType::Object);
    }
    if has(ARRAY_KEYWORDS) {
        found.push(JsonType::Array);
    }
    match format {
        Some("int32" | "int64") => found.push(JsonType::Integer),
        Some("float" | "double") => found.push(JsonType::Number),
        Some(_) => found.push(JsonType::String),
        None => {}
    }
    if has(STRING_KEYWORDS) && !found.contains(&JsonType::String) {
        found.push(JsonType::String);
    }
    if has(NUMBER_KEYWORDS) && !found.contains(&JsonType::Integer) {
        found.push(JsonType::Number);
    }
    match found.as_slice() {
        [] => Inferred::Unknown,
        [one] => Inferred::One(*one),
        _ => Inferred::Ambiguous,
    }
}

/// `title` and `description` as a [`Doc`]: the summary is the title, or
/// else the first sentence of the description; the description is kept as
/// written.
pub(super) fn doc_of(map: &Object) -> Option<Doc> {
    let text = |key: &str| {
        map.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    let title = text("title").map(|t| t.trim().to_string());
    let description = text("description");
    if title.is_none() && description.is_none() {
        return None;
    }
    Some(Doc {
        summary: title.or_else(|| description.map(first_sentence)),
        description: description.map(str::to_string),
    })
}

/// The first sentence of a text: up to and including the first period
/// followed by whitespace, or up to the first blank line, trimmed.
fn first_sentence(text: &str) -> String {
    let text = text.trim();
    let paragraph = text.split("\n\n").next().unwrap_or(text);
    let bytes = paragraph.as_bytes();
    let end = bytes
        .iter()
        .enumerate()
        .find(|(i, b)| **b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_whitespace))
        .map_or(paragraph.len(), |(i, _)| i + 1);
    paragraph[..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The validation keywords of a schema that tungsten keeps.
pub(super) fn constraints_of(map: &Object) -> Constraints {
    let count = |key: &str| map.get(key).and_then(Value::as_u64);
    let number = |key: &str| match map.get(key) {
        Some(Value::Number(n)) => Some(n.clone()),
        _ => None,
    };
    Constraints {
        pattern: map
            .get("pattern")
            .and_then(Value::as_str)
            .map(str::to_string),
        min_length: count("minLength"),
        max_length: count("maxLength"),
        minimum: number("minimum"),
        maximum: number("maximum"),
        exclusive_minimum: number("exclusiveMinimum"),
        exclusive_maximum: number("exclusiveMaximum"),
        multiple_of: number("multipleOf"),
    }
}

/// The primitive for `type: string` with an optional `format`, and whether
/// the format is unknown (TG0304; kept as `StringFormat::Other`).
pub(super) fn string_primitive(format: Option<&str>) -> (Primitive, bool) {
    let known = match format {
        None => return (Primitive::String { format: None }, false),
        Some("binary") => return (Primitive::Bytes, false),
        Some("uuid") => StringFormat::Uuid,
        Some("date-time") => StringFormat::DateTime,
        Some("date") => StringFormat::Date,
        Some("time") => StringFormat::Time,
        Some("duration") => StringFormat::Duration,
        Some("email") => StringFormat::Email,
        Some("uri" | "uri-reference") => StringFormat::Uri,
        Some("hostname") => StringFormat::Hostname,
        Some("ipv4") => StringFormat::Ipv4,
        Some("ipv6") => StringFormat::Ipv6,
        Some("byte") => StringFormat::Byte,
        Some("password") => StringFormat::Password,
        Some(other) => {
            return (
                Primitive::String {
                    format: Some(StringFormat::Other(other.to_string())),
                },
                true,
            );
        }
    };
    (
        Primitive::String {
            format: Some(known),
        },
        false,
    )
}

/// The primitive a 3.1 string declares through its content keywords,
/// which replace 3.0's `format: byte`/`binary`: `contentEncoding: base64`
/// (or `base64url`) is a base64 string (`format: byte`), and a
/// `contentMediaType` that is not text (an image, `application/octet-stream`)
/// without an encoding is raw bytes (`format: binary`). `None` when neither
/// applies.
pub(super) fn content_primitive(map: &Object) -> Option<Primitive> {
    let text = |key: &str| map.get(key).and_then(Value::as_str);
    if let Some(encoding) = text("contentEncoding") {
        return matches!(
            encoding.to_ascii_lowercase().as_str(),
            "base64" | "base64url"
        )
        .then_some(Primitive::String {
            format: Some(StringFormat::Byte),
        });
    }
    let media = text("contentMediaType")?.to_ascii_lowercase();
    let essence = media.split(';').next().unwrap_or("").trim();
    let textual = essence.starts_with("text/")
        || essence.ends_with("/json")
        || essence.ends_with("+json")
        || essence.ends_with("/xml")
        || essence.ends_with("+xml")
        || essence == "application/x-www-form-urlencoded";
    (!textual).then_some(Primitive::Bytes)
}

/// The primitive for `type: integer`; formats other than `int32`/`int64`
/// give the widest integer.
pub(super) fn integer_primitive(format: Option<&str>) -> Primitive {
    match format {
        Some("int32") => Primitive::Int32,
        Some("int64") => Primitive::Int64,
        _ => Primitive::Integer,
    }
}

/// The primitive for `type: number`; formats other than `float`/`double`
/// give the widest number.
pub(super) fn number_primitive(format: Option<&str>) -> Primitive {
    match format {
        Some("float") => Primitive::Float,
        Some("double") => Primitive::Double,
        _ => Primitive::Number,
    }
}

/// A JSON value as name text: strings as written, anything else as JSON.
pub(super) fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
