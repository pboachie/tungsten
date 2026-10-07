// SPDX-License-Identifier: AGPL-3.0-only
//! The request as the validators see it, and parameter checks: presence,
//! parsing by IR type and style, then value validation.

use serde_json::{Number, Value};
use tungsten_ir::{Operation, Param, ParamStyle, Primitive, Shape, TypeRef};

use crate::model::Model;
use crate::validate::{Context, Validator};

/// Nesting allowed when parsing a parameter through unions and nullables.
const MAX_PARAM_DEPTH: usize = 16;

/// Decode `%XX` escapes (and `+` as a space in query strings). Invalid
/// escapes are kept as written; invalid UTF-8 is replaced.
pub(crate) fn percent_decode(text: &str, plus_as_space: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => match (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                (Some(high), Some(low)) => {
                    out.push(high << 4 | low);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' if plus_as_space => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: Option<&u8>) -> Option<u8> {
    let b = *b?;
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Parse a query string into decoded pairs, in order.
pub(crate) fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k, true), percent_decode(v, true)),
            None => (percent_decode(pair, true), String::new()),
        })
        .collect()
}

/// The request facts the auth and parameter checks need.
#[derive(Debug)]
pub(crate) struct RequestView<'a> {
    pub method: &'a str,
    /// Lowercased names, in arrival order.
    pub headers: &'a [(String, String)],
    pub query: Vec<(String, String)>,
    pub cookies: Vec<(String, String)>,
    pub path_params: Vec<(String, String)>,
}

impl<'a> RequestView<'a> {
    pub fn new(
        method: &'a str,
        headers: &'a [(String, String)],
        query: &str,
        path_params: Vec<(String, String)>,
    ) -> RequestView<'a> {
        let cookies = headers
            .iter()
            .filter(|(n, _)| n == "cookie")
            .flat_map(|(_, v)| v.split(';'))
            .filter_map(|c| {
                let (name, value) = c.trim().split_once('=')?;
                Some((name.trim().to_string(), value.trim().to_string()))
            })
            .collect();
        RequestView {
            method,
            headers,
            query: parse_query(query),
            cookies,
            path_params,
        }
    }

    /// A header's values joined with `, `, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<String> {
        let values: Vec<&str> = self
            .headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect();
        (!values.is_empty()).then(|| values.join(", "))
    }

    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn query_values(&self, name: &str) -> Vec<&str> {
        self.query
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn is_safe_method(&self) -> bool {
        matches!(self.method, "GET" | "HEAD" | "OPTIONS" | "TRACE")
    }
}

/// Check every declared parameter of `op`.
pub(crate) fn check(model: &Model, op: &Operation, req: &RequestView<'_>) -> Result<(), String> {
    let lists: [(&str, &Vec<Param>); 4] = [
        ("path", &op.params.path),
        ("query", &op.params.query),
        ("header", &op.params.header),
        ("cookie", &op.params.cookie),
    ];
    for (location, params) in lists {
        for param in params {
            let values: Vec<String> = match location {
                "path" => req
                    .path_params
                    .iter()
                    .filter(|(n, _)| *n == param.wire_name)
                    .map(|(_, v)| v.clone())
                    .take(1)
                    .collect(),
                "query" => req
                    .query_values(&param.wire_name)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                "header" => req.header(&param.wire_name).into_iter().collect(),
                _ => req
                    .cookie(&param.wire_name)
                    .map(str::to_string)
                    .into_iter()
                    .collect(),
            };
            if values.is_empty() {
                if param.required {
                    return Err(format!(
                        "missing required {location} parameter `{}`",
                        param.wire_name
                    ));
                }
                continue;
            }
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            let value = match parse(model, param, &refs) {
                Ok(Some(value)) => value,
                Ok(None) => continue,
                Err(e) => return Err(format!("{location} parameter `{}`: {e}", param.wire_name)),
            };
            Validator::new(model, Context::Request)
                .check(&param.ty, &value)
                .map_err(|e| format!("{location} parameter `{}`: {e}", param.wire_name))?;
        }
    }
    Ok(())
}

/// Parse raw parameter values into JSON by the parameter's type. `None`
/// when the type (an object) cannot be checked from its serialized form.
fn parse(model: &Model, param: &Param, values: &[&str]) -> Result<Option<Value>, String> {
    if let Some(media_type) = &param.media_type {
        if is_json(media_type) {
            return serde_json::from_str(values[0])
                .map(Some)
                .map_err(|e| format!("not valid JSON: {e}"));
        }
        return Ok(None);
    }
    Ok(parse_typed(model, &param.ty, param, values, 0))
}

fn parse_typed(
    model: &Model,
    ty: &TypeRef,
    param: &Param,
    values: &[&str],
    depth: usize,
) -> Option<Value> {
    if depth > MAX_PARAM_DEPTH {
        return None;
    }
    match model.shape(ty)? {
        Shape::Array { items, .. } => {
            let raw: Vec<&str> =
                if values.len() > 1 || (param.style == ParamStyle::Form && param.explode) {
                    values.to_vec()
                } else {
                    let separator = match param.style {
                        ParamStyle::SpaceDelimited => ' ',
                        ParamStyle::PipeDelimited => '|',
                        _ => ',',
                    };
                    values[0].split(separator).collect()
                };
            raw.iter()
                .map(|v| scalar(model, items, v, depth + 1))
                .collect::<Option<Vec<Value>>>()
                .map(Value::Array)
        }
        Shape::Nullable { inner } => parse_typed(model, inner, param, values, depth + 1),
        _ => scalar(model, ty, values[0], depth),
    }
}

/// One serialized value. Values that do not parse stay strings, so the
/// validator reports the type mismatch.
fn scalar(model: &Model, ty: &TypeRef, raw: &str, depth: usize) -> Option<Value> {
    if depth > MAX_PARAM_DEPTH {
        return None;
    }
    let text = Value::String(raw.to_string());
    match model.shape(ty)? {
        Shape::Primitive { primitive, .. } => Some(primitive_value(primitive, raw)),
        Shape::Enum { base, values } => {
            if values.iter().any(|v| v.value == text) {
                Some(text)
            } else {
                Some(primitive_value(base, raw))
            }
        }
        Shape::Const { value } => {
            if value.is_string() {
                Some(text)
            } else {
                Some(serde_json::from_str(raw).unwrap_or(text))
            }
        }
        Shape::Union(union) => {
            let validator = Validator::new(model, Context::Request);
            for variant in &union.variants {
                if let Some(v) = scalar(model, &variant.ty, raw, depth + 1)
                    && validator.check(&variant.ty, &v).is_ok()
                {
                    return Some(v);
                }
            }
            Some(text)
        }
        Shape::Nullable { inner } => scalar(model, inner, raw, depth + 1),
        Shape::Record { .. } | Shape::Map { .. } | Shape::Intersection { .. } => None,
        Shape::Array { .. } | Shape::Any | Shape::Never => Some(text),
    }
}

fn primitive_value(primitive: &Primitive, raw: &str) -> Value {
    let text = || Value::String(raw.to_string());
    match primitive {
        Primitive::Int32 | Primitive::Int64 | Primitive::Integer => raw
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| raw.parse::<u64>().map(Value::from))
            .unwrap_or_else(|_| text()),
        Primitive::Float | Primitive::Double | Primitive::Number => {
            if let Ok(i) = raw.parse::<i64>() {
                return Value::from(i);
            }
            raw.parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .map(Value::Number)
                .unwrap_or_else(text)
        }
        Primitive::Bool => match raw {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => text(),
        },
        Primitive::String { .. } | Primitive::Bytes => text(),
    }
}

/// `application/json` and `+json` media types.
pub(crate) fn is_json(media_type: &str) -> bool {
    let essence = media_essence(media_type);
    essence == "application/json" || essence.ends_with("+json")
}

/// A media type lowercased without parameters.
pub(crate) fn media_essence(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}
