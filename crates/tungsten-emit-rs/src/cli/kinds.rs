// SPDX-License-Identifier: AGPL-3.0-only
//! How an argument's type becomes the kind of a command-line flag.
//!
//! A flag takes a scalar (string, integer, number, boolean, one of a set of
//! strings), a repeatable list of scalars, or the bytes of a file; any other
//! type (records, maps, unions, lists of those, anything) is JSON text, given
//! inline, as `@file.json` or as `-` for standard input.

use serde_json::Value;
use tungsten_ir::{Ir, Primitive, Shape, StringFormat, TypeRef};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    String,
    Integer,
    Number,
    Boolean,
    Enum(Vec<String>),
    Array(Box<Kind>),
    Json,
    File,
}

/// The shape a reference denotes, through named types and nullability.
fn shape_of<'a>(ir: &'a Ir, ty: &'a TypeRef) -> Option<&'a Shape> {
    let mut cur = ty;
    for _ in 0..=ir.types.types.len() + 8 {
        let shape: &Shape = match cur {
            TypeRef::Inline(s) => s,
            TypeRef::Named(id) => &ir.types.get(id)?.shape,
        };
        match shape {
            Shape::Nullable { inner } => cur = inner,
            other => return Some(other),
        }
    }
    None
}

fn primitive_kind(p: &Primitive) -> Kind {
    match p {
        Primitive::String { .. } => Kind::String,
        Primitive::Int32 | Primitive::Int64 | Primitive::Integer => Kind::Integer,
        Primitive::Float | Primitive::Double | Primitive::Number => Kind::Number,
        Primitive::Bool => Kind::Boolean,
        Primitive::Bytes => Kind::File,
    }
}

fn scalar(shape: &Shape) -> Option<Kind> {
    match shape {
        Shape::Primitive { primitive, .. } => Some(primitive_kind(primitive)),
        Shape::Enum { base, values } => {
            let names: Vec<String> = values
                .iter()
                .filter_map(|v| v.value.as_str().map(str::to_string))
                .collect();
            if !names.is_empty() && names.len() == values.len() {
                Some(Kind::Enum(names))
            } else {
                Some(primitive_kind(base))
            }
        }
        Shape::Const { value } => match value {
            Value::String(_) => Some(Kind::String),
            Value::Bool(_) => Some(Kind::Boolean),
            Value::Number(n) if n.is_i64() || n.is_u64() => Some(Kind::Integer),
            Value::Number(_) => Some(Kind::Number),
            _ => None,
        },
        _ => None,
    }
}

/// The flag kind of a type, or `None` when it can only be JSON text.
pub(crate) fn flag_kind(ir: &Ir, ty: &TypeRef) -> Option<Kind> {
    match shape_of(ir, ty)? {
        Shape::Array { items, .. } => {
            let inner = scalar(shape_of(ir, items)?)?;
            (inner != Kind::File).then(|| Kind::Array(Box::new(inner)))
        }
        other => scalar(other),
    }
}

/// Whether the type is a password string (`format: password`).
pub(crate) fn is_password(ir: &Ir, ty: &TypeRef) -> bool {
    matches!(
        shape_of(ir, ty),
        Some(Shape::Primitive {
            primitive: Primitive::String {
                format: Some(StringFormat::Password)
            },
            ..
        })
    )
}

/// A short description of a type for diagnostics.
pub(crate) fn describe(ir: &Ir, ty: &TypeRef) -> &'static str {
    match shape_of(ir, ty) {
        Some(Shape::Record { .. }) => "an object",
        Some(Shape::Map { .. }) => "a map",
        Some(Shape::Union(_)) => "a union",
        Some(Shape::Intersection { .. }) => "an intersection",
        Some(Shape::Array { .. }) => "a list of non-scalars",
        Some(Shape::Any) => "any JSON value",
        _ => "a type without a flag form",
    }
}

fn json_scalar(schema: &Value) -> Option<Kind> {
    if let Some(Value::Array(values)) = schema.get("enum") {
        let names: Vec<String> = values
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if !names.is_empty() && names.len() == values.len() {
            return Some(Kind::Enum(names));
        }
    }
    match schema.get("type").and_then(Value::as_str)? {
        "string" => Some(Kind::String),
        "integer" => Some(Kind::Integer),
        "number" => Some(Kind::Number),
        "boolean" => Some(Kind::Boolean),
        _ => None,
    }
}

/// The flag kind of an input a macro adds, from its JSON Schema.
pub(crate) fn schema_kind(schema: &Value) -> Kind {
    if schema.get("type").and_then(Value::as_str) == Some("array") {
        return schema
            .get("items")
            .and_then(json_scalar)
            .map_or(Kind::Json, |k| Kind::Array(Box::new(k)));
    }
    json_scalar(schema).unwrap_or(Kind::Json)
}
