// SPDX-License-Identifier: Apache-2.0
//! Parameter serialization (OpenAPI 3 `style`/`explode` for path, query,
//! header and cookie parameters) and request body encoding.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::types::{
    BodyDescriptor, BodyEncoding, BodyShape, OperationDescriptor, ParamDescriptor, ParamStyle,
};
use crate::util::{REDACTED, base64_standard, binary_of, canonical_json, json_text, number_text};

/// Problem found while serializing an argument; becomes `VALIDATION_FAILED`.
#[derive(Debug, Clone, PartialEq)]
pub struct SerializationError {
    pub parameter: String,
    pub expected: String,
    pub value: Value,
}

impl SerializationError {
    fn new(parameter: &str, expected: &str, value: &Value) -> Self {
        SerializationError {
            parameter: parameter.to_owned(),
            expected: expected.to_owned(),
            value: value.clone(),
        }
    }
}

/// `String(value)` for a scalar, JSON text for anything else.
pub fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => number_text(n),
        Value::Bool(b) => b.to_string(),
        other => json_text(other),
    }
}

/// `encodeURIComponent`.
pub fn encode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// One component of an `application/x-www-form-urlencoded` body
/// (`URLSearchParams`): space is `+`, only `*-._` and alphanumerics are kept.
pub fn encode_form_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"*-._".contains(&byte) {
            out.push(char::from(byte));
        } else if byte == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn entries(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    map.iter().filter(|(_, v)| !v.is_null()).collect()
}

fn encoded_items(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter(|v| !v.is_null())
        .map(|v| encode_component(&scalar(v)))
        .collect()
}

fn encoded_pairs(map: &Map<String, Value>) -> Vec<(String, String)> {
    entries(map)
        .into_iter()
        .map(|(k, v)| (encode_component(k), encode_component(&scalar(v))))
        .collect()
}

/// One path parameter value, already percent-encoded (`simple`, `label`,
/// `matrix`).
pub fn serialize_path_param(p: &ParamDescriptor, value: &Value) -> String {
    let name = encode_component(&p.wire);
    let style = match p.style {
        ParamStyle::Label => ParamStyle::Label,
        ParamStyle::Matrix => ParamStyle::Matrix,
        _ => ParamStyle::Simple,
    };
    match value {
        Value::Array(list) => {
            let items = encoded_items(list);
            match style {
                ParamStyle::Label => format!(".{}", items.join(if p.explode { "." } else { "," })),
                ParamStyle::Matrix if p.explode => {
                    items.iter().map(|i| format!(";{name}={i}")).collect()
                }
                ParamStyle::Matrix => format!(";{name}={}", items.join(",")),
                _ => items.join(","),
            }
        }
        Value::Object(map) => {
            let pairs = encoded_pairs(map);
            let exploded: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let flat: Vec<String> = pairs.into_iter().flat_map(|(k, v)| [k, v]).collect();
            match style {
                ParamStyle::Label if p.explode => format!(".{}", exploded.join(".")),
                ParamStyle::Label => format!(".{}", flat.join(",")),
                ParamStyle::Matrix if p.explode => {
                    exploded.iter().map(|e| format!(";{e}")).collect()
                }
                ParamStyle::Matrix => format!(";{name}={}", flat.join(",")),
                _ if p.explode => exploded.join(","),
                _ => flat.join(","),
            }
        }
        other => {
            let v = encode_component(&scalar(other));
            match style {
                ParamStyle::Label => format!(".{v}"),
                ParamStyle::Matrix => format!(";{name}={v}"),
                _ => v,
            }
        }
    }
}

fn deep_object_pairs(prefix: &str, value: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 16 || value.is_null() {
        return;
    }
    match value {
        Value::Array(items) => items
            .iter()
            .for_each(|item| deep_object_pairs(prefix, item, out, depth + 1)),
        Value::Object(map) => {
            for (k, v) in entries(map) {
                deep_object_pairs(
                    &format!("{prefix}[{}]", encode_component(k)),
                    v,
                    out,
                    depth + 1,
                );
            }
        }
        other => out.push(format!("{prefix}={}", encode_component(&scalar(other)))),
    }
}

/// Query string parts (`name=value`, encoded) for one query parameter.
pub fn serialize_query_param(p: &ParamDescriptor, value: &Value) -> Vec<String> {
    if value.is_null() {
        return Vec::new();
    }
    let name = encode_component(&p.wire);
    let delimiter = match p.style {
        ParamStyle::SpaceDelimited => "%20",
        ParamStyle::PipeDelimited => "|",
        _ => ",",
    };
    if p.style == ParamStyle::DeepObject && value.is_object() {
        let mut out = Vec::new();
        deep_object_pairs(&name, value, &mut out, 0);
        return out;
    }
    match value {
        Value::Array(list) => {
            let items = encoded_items(list);
            if items.is_empty() {
                Vec::new()
            } else if p.explode || p.style == ParamStyle::DeepObject {
                items.iter().map(|i| format!("{name}={i}")).collect()
            } else {
                vec![format!("{name}={}", items.join(delimiter))]
            }
        }
        Value::Object(map) => {
            let pairs = encoded_pairs(map);
            if p.explode {
                pairs.iter().map(|(k, v)| format!("{k}={v}")).collect()
            } else {
                let flat: Vec<String> = pairs.into_iter().flat_map(|(k, v)| [k, v]).collect();
                vec![format!("{name}={}", flat.join(delimiter))]
            }
        }
        other => vec![format!("{name}={}", encode_component(&scalar(other)))],
    }
}

/// Header value for one header parameter (`simple` style, not encoded).
pub fn serialize_header_param(p: &ParamDescriptor, value: &Value) -> String {
    match value {
        Value::Array(list) => list
            .iter()
            .filter(|v| !v.is_null())
            .map(scalar)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(map) => {
            let pairs: Vec<(&String, String)> = entries(map)
                .into_iter()
                .map(|(k, v)| (k, scalar(v)))
                .collect();
            if p.explode {
                pairs
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                pairs
                    .iter()
                    .flat_map(|(k, v)| [(*k).clone(), v.clone()])
                    .collect::<Vec<_>>()
                    .join(",")
            }
        }
        other => scalar(other),
    }
}

/// `name=value` for one cookie parameter (`form` style, values encoded).
pub fn serialize_cookie_param(p: &ParamDescriptor, value: &Value) -> String {
    match value {
        Value::Array(list) => format!("{}={}", p.wire, encoded_items(list).join(",")),
        Value::Object(map) => {
            let flat: Vec<String> = encoded_pairs(map)
                .into_iter()
                .flat_map(|(k, v)| [k, v])
                .collect();
            format!("{}={}", p.wire, flat.join(","))
        }
        other => format!("{}={}", p.wire, encode_component(&scalar(other))),
    }
}

// ------------------------------------------------------------------ bodies

/// One part of a multipart body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartContent {
    Text(String),
    Json(String),
    File {
        data: Vec<u8>,
        filename: String,
        content_type: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartPart {
    pub name: String,
    pub content: PartContent,
}

/// What goes on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Empty,
    Bytes(Vec<u8>),
    Multipart(Vec<MultipartPart>),
}

/// An encoded request body.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedBody {
    pub payload: Payload,
    /// Content-Type to set, or `None` (multipart: the transport sets the
    /// boundary).
    pub content_type: Option<String>,
    /// JSON-friendly rendering for previews (binary summarised), redacted.
    pub display: Value,
    /// The same rendering before redaction (never shown).
    pub raw: Value,
    /// Bytes whose SHA-256 is the `content_hash` key.
    pub hash_material: Option<Vec<u8>>,
}

fn no_body() -> EncodedBody {
    EncodedBody {
        payload: Payload::Empty,
        content_type: None,
        display: Value::Null,
        raw: Value::Null,
        hash_material: None,
    }
}

/// The body value before encoding: merged fields picked from the args (sent
/// under their wire names), or the whole `args[arg]`; wrapped in the rpc
/// envelope when the operation has one. `None` means no body.
pub fn body_value(
    op: &OperationDescriptor,
    args: &Map<String, Value>,
    param_names: &[&str],
) -> Option<Value> {
    let mut value: Option<Value> = None;
    if let Some(body) = &op.body {
        match &body.shape {
            BodyShape::Merged { fields } => {
                let mut picked = Map::new();
                for field in fields {
                    if let Some(v) = args.get(&field.arg) {
                        picked.insert(field.wire.clone(), v.clone());
                    }
                }
                if !picked.is_empty() || body.required || op.rpc.is_some() {
                    value = Some(Value::Object(picked));
                }
            }
            BodyShape::Arg { arg } => value = args.get(arg).cloned(),
        }
    }
    let Some(rpc) = &op.rpc else {
        return value;
    };
    let params = if op.body.is_none() {
        Some(Value::Object(
            args.iter()
                .filter(|(k, _)| !param_names.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ))
    } else {
        value
    };
    let mut envelope: Map<String, Value> = rpc
        .constants
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    envelope.insert(rpc.field.clone(), Value::String(rpc.value.clone()));
    envelope.insert(
        rpc.params_field.clone(),
        params.unwrap_or_else(|| Value::Object(Map::new())),
    );
    Some(Value::Object(envelope))
}

fn form_pairs(value: &Value, parameter: &str) -> Result<Vec<(String, String)>, SerializationError> {
    let Value::Object(map) = value else {
        return Err(SerializationError::new(
            parameter,
            "an object of form fields",
            value,
        ));
    };
    let mut out = Vec::new();
    for (k, v) in entries(map) {
        if let Value::Array(items) = v {
            for item in items.iter().filter(|i| !i.is_null()) {
                out.push((k.clone(), scalar(item)));
            }
        } else {
            out.push((k.clone(), scalar(v)));
        }
    }
    Ok(out)
}

fn binary_display(len: usize, media_type: &str) -> String {
    if media_type.is_empty() {
        format!("<{len} bytes>")
    } else {
        format!("<{len} bytes of {media_type}>")
    }
}

/// Encode `value` per the body descriptor. Fails for values the encoding
/// cannot carry.
pub fn encode_body(
    descriptor: Option<&BodyDescriptor>,
    value: Option<&Value>,
    redact_display: &dyn Fn(&Value) -> Value,
) -> Result<EncodedBody, SerializationError> {
    let Some(value) = value else {
        return Ok(no_body());
    };
    let encoding = descriptor.map_or(BodyEncoding::Json, |d| d.encoding);
    let media_type = descriptor.map_or("application/json", |d| d.media_type.as_str());
    let parameter = "body";
    match encoding {
        BodyEncoding::Json => {
            let text = json_text(value);
            let raw: Value = serde_json::from_str(&text).map_err(|_| {
                SerializationError::new(parameter, "a JSON-serializable value", value)
            })?;
            let hash = canonical_json(&raw);
            Ok(EncodedBody {
                payload: Payload::Bytes(text.into_bytes()),
                content_type: Some(media_type.to_owned()),
                display: redact_display(&raw),
                raw,
                hash_material: Some(hash.into_bytes()),
            })
        }
        BodyEncoding::Form => {
            let pairs = form_pairs(value, parameter)?;
            let text = pairs
                .iter()
                .map(|(k, v)| format!("{}={}", encode_form_component(k), encode_form_component(v)))
                .collect::<Vec<_>>()
                .join("&");
            let mut raw = Map::new();
            for (k, v) in &pairs {
                raw.insert(k.clone(), Value::String(v.clone()));
            }
            let raw = Value::Object(raw);
            Ok(EncodedBody {
                payload: Payload::Bytes(text.clone().into_bytes()),
                content_type: Some(media_type.to_owned()),
                display: redact_display(&raw),
                raw,
                hash_material: Some(text.into_bytes()),
            })
        }
        BodyEncoding::Multipart => {
            let Value::Object(map) = value else {
                return Err(SerializationError::new(
                    parameter,
                    "an object of multipart fields",
                    value,
                ));
            };
            let mut parts = Vec::new();
            let mut display = Map::new();
            for (k, v) in entries(map) {
                let list: Vec<&Value> = match v {
                    Value::Array(items) => items.iter().filter(|i| !i.is_null()).collect(),
                    other => vec![other],
                };
                let mut shown = Vec::new();
                for item in list {
                    if let Some(binary) = binary_of(item) {
                        shown.push(Value::String(binary_display(
                            binary.data.len(),
                            binary.content_type.as_deref().unwrap_or(""),
                        )));
                        parts.push(MultipartPart {
                            name: k.clone(),
                            content: PartContent::File {
                                filename: binary.filename.clone().unwrap_or_else(|| k.clone()),
                                content_type: binary
                                    .content_type
                                    .clone()
                                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                                data: binary.data,
                            },
                        });
                    } else if item.is_object() {
                        parts.push(MultipartPart {
                            name: k.clone(),
                            content: PartContent::Json(json_text(item)),
                        });
                        shown.push(item.clone());
                    } else {
                        parts.push(MultipartPart {
                            name: k.clone(),
                            content: PartContent::Text(scalar(item)),
                        });
                        shown.push(Value::String(scalar(item)));
                    }
                }
                let shown_value = if v.is_array() {
                    Value::Array(shown)
                } else {
                    shown.into_iter().next().unwrap_or(Value::Null)
                };
                display.insert(k.clone(), shown_value);
            }
            let display = Value::Object(display);
            Ok(EncodedBody {
                payload: Payload::Multipart(parts),
                content_type: None,
                display: redact_display(&display),
                raw: display,
                hash_material: Some(canonical_json(value).into_bytes()),
            })
        }
        BodyEncoding::Bytes => {
            let data = match value {
                Value::String(text) => text.clone().into_bytes(),
                other => match binary_of(other) {
                    Some(binary) => binary.data,
                    None => {
                        return Err(SerializationError::new(
                            parameter,
                            "binary data (a Binary value)",
                            value,
                        ));
                    }
                },
            };
            let shown = Value::String(binary_display(data.len(), media_type));
            Ok(EncodedBody {
                payload: Payload::Bytes(data.clone()),
                content_type: Some(media_type.to_owned()),
                display: shown.clone(),
                raw: shown,
                hash_material: Some(data),
            })
        }
        BodyEncoding::Text => {
            if !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                return Err(SerializationError::new(parameter, "a string", value));
            }
            let text = scalar(value);
            Ok(EncodedBody {
                payload: Payload::Bytes(text.clone().into_bytes()),
                content_type: Some(media_type.to_owned()),
                display: Value::String(text.clone()),
                raw: Value::String(text.clone()),
                hash_material: Some(text.into_bytes()),
            })
        }
    }
}

/// Whether `name` is a valid HTTP header name (an RFC 9110 token).
pub fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Whether `value` is a valid header value: visible ASCII, tab, space and
/// Latin-1 characters (no line breaks or controls).
pub fn valid_header_value(value: &str) -> bool {
    value.chars().all(|c| {
        c == '\t' || ('\u{20}'..='\u{7e}').contains(&c) || ('\u{80}'..='\u{ff}').contains(&c)
    })
}

/// The bytes of a header value that [`valid_header_value`] accepted.
pub fn header_value_bytes(value: &str) -> Vec<u8> {
    value.chars().map(|c| c as u8).collect()
}

#[derive(Debug, Clone)]
struct HeaderEntry {
    name: String,
    value: String,
    secret: bool,
}

/// Case-insensitive header collection that remembers which values are
/// secrets, so previews and middleware see them redacted.
#[derive(Debug, Clone, Default)]
pub struct HeaderBag {
    entries: Vec<HeaderEntry>,
}

impl HeaderBag {
    pub fn set(&mut self, name: &str, value: impl Into<String>, secret: bool) {
        let entry = HeaderEntry {
            name: name.to_owned(),
            value: value.into(),
            secret,
        };
        match self
            .entries
            .iter_mut()
            .find(|e| e.name.eq_ignore_ascii_case(name))
        {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
            .map(|e| e.value.as_str())
    }

    pub fn is_secret(&self, name: &str) -> bool {
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
            .is_some_and(|e| e.secret)
    }

    /// Plain map; secret values replaced by `<redacted>` when `redact`.
    pub fn to_map(&self, redact: bool) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .map(|e| {
                let value = if redact && e.secret {
                    REDACTED.to_owned()
                } else {
                    e.value.clone()
                };
                (e.name.clone(), value)
            })
            .collect()
    }

    /// Name and value pairs in insertion order.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|e| (e.name.as_str(), e.value.as_str()))
    }

    /// Secret values, for scrubbing diagnostics.
    pub fn secrets(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|e| e.secret)
            .map(|e| e.value.as_str())
    }
}

/// Standard base64 of a Basic-auth pair or any text (shared by auth).
pub fn base64_text(text: &str) -> String {
    base64_standard(text.as_bytes())
}
