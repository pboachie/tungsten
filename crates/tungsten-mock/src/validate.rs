// SPDX-License-Identifier: AGPL-3.0-only
//! JSON value validation against IR types.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Map, Value};
use tungsten_ir::{
    Additional, Constraints, Field, Presence, Primitive, Shape, StringFormat, TypeRef, Union,
    UnionStrategy,
};

use crate::generate::tag_text;
use crate::model::Model;

/// Type nesting followed before giving up (recursive unions can nest
/// without consuming the value).
const MAX_DEPTH: usize = 256;
/// Type nodes visited per check before giving up.
const MAX_STEPS: usize = 1_000_000;

/// Which side of the exchange a value belongs to: requests never need
/// read-only fields, responses never need write-only ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Context {
    Request,
    Response,
}

/// A validation failure: where (a JSON Pointer) and what.
#[derive(Debug)]
pub(crate) struct Invalid {
    pub pointer: String,
    pub message: String,
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.pointer.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.pointer, self.message)
        }
    }
}

#[derive(Debug)]
pub(crate) struct Validator<'a> {
    model: &'a Model,
    ctx: Context,
    steps: Cell<usize>,
}

type Checked = Result<(), Invalid>;

impl<'a> Validator<'a> {
    pub fn new(model: &'a Model, ctx: Context) -> Validator<'a> {
        Validator {
            model,
            ctx,
            steps: Cell::new(0),
        }
    }

    pub fn check(&self, ty: &TypeRef, value: &Value) -> Checked {
        self.steps.set(0);
        let mut pointer = String::new();
        self.ty(ty, value, &mut pointer, 0, false)
    }

    fn fail(pointer: &str, message: impl Into<String>) -> Checked {
        Err(Invalid {
            pointer: pointer.to_string(),
            message: message.into(),
        })
    }

    /// `relaxed` skips the closed-record check (intersection members).
    fn ty(
        &self,
        ty: &TypeRef,
        v: &Value,
        ptr: &mut String,
        depth: usize,
        relaxed: bool,
    ) -> Checked {
        let steps = self.steps.get() + 1;
        self.steps.set(steps);
        if depth > MAX_DEPTH || steps > MAX_STEPS {
            return Self::fail(ptr, "value is too complex to validate");
        }
        match self.model.shape(ty) {
            Some(shape) => self.shape(shape, v, ptr, depth, relaxed),
            None => Ok(()),
        }
    }

    fn shape(
        &self,
        shape: &Shape,
        v: &Value,
        ptr: &mut String,
        depth: usize,
        relaxed: bool,
    ) -> Checked {
        match shape {
            Shape::Primitive {
                primitive,
                constraints,
            } => {
                check_primitive(primitive, v).or_else(|m| Self::fail(ptr, m))?;
                self.constraints(constraints, v, ptr)
            }
            Shape::Enum { values, .. } => {
                if values.iter().any(|e| json_eq(&e.value, v)) {
                    Ok(())
                } else {
                    let listed: Vec<String> =
                        values.iter().take(8).map(|e| e.value.to_string()).collect();
                    let more = if values.len() > 8 { ", ..." } else { "" };
                    Self::fail(ptr, format!("must be one of [{}{more}]", listed.join(", ")))
                }
            }
            Shape::Const { value } => {
                if json_eq(value, v) {
                    Ok(())
                } else {
                    Self::fail(ptr, format!("must equal {value}"))
                }
            }
            Shape::Array {
                items,
                min,
                max,
                unique,
            } => {
                let Some(array) = v.as_array() else {
                    return Self::fail(ptr, "expected an array");
                };
                let len = array.len() as u64;
                if min.is_some_and(|m| len < m) {
                    return Self::fail(
                        ptr,
                        format!("must have at least {} items", min.unwrap_or(0)),
                    );
                }
                if max.is_some_and(|m| len > m) {
                    return Self::fail(
                        ptr,
                        format!("must have at most {} items", max.unwrap_or(0)),
                    );
                }
                if *unique {
                    let mut seen = BTreeSet::new();
                    if !array.iter().all(|item| seen.insert(canonical(item))) {
                        return Self::fail(ptr, "items must be unique");
                    }
                }
                for (i, item) in array.iter().enumerate() {
                    let mark = ptr.len();
                    ptr.push_str(&format!("/{i}"));
                    self.ty(items, item, ptr, depth + 1, false)?;
                    ptr.truncate(mark);
                }
                Ok(())
            }
            Shape::Map { values } => {
                let Some(object) = v.as_object() else {
                    return Self::fail(ptr, "expected an object");
                };
                for (key, item) in object {
                    let mark = push_key(ptr, key);
                    self.ty(values, item, ptr, depth + 1, false)?;
                    ptr.truncate(mark);
                }
                Ok(())
            }
            Shape::Record { fields, additional } => {
                self.record(fields, additional, v, ptr, depth, relaxed)
            }
            Shape::Union(union) => self.union(union, v, ptr, depth, relaxed),
            Shape::Intersection { members } => {
                for member in members {
                    self.ty(member, v, ptr, depth + 1, true)?;
                }
                self.intersection_closed(members, v, ptr)
            }
            Shape::Nullable { inner } => {
                if v.is_null() {
                    Ok(())
                } else {
                    self.ty(inner, v, ptr, depth + 1, relaxed)
                }
            }
            Shape::Any => Ok(()),
            Shape::Never => Self::fail(ptr, "no value is allowed here"),
        }
    }

    fn required(&self, field: &Field) -> bool {
        matches!(
            field.presence,
            Presence::Required | Presence::RequiredNullable
        ) && !(self.ctx == Context::Request && field.read_only)
            && !(self.ctx == Context::Response && field.write_only)
    }

    fn record(
        &self,
        fields: &[Field],
        additional: &Additional,
        v: &Value,
        ptr: &mut String,
        depth: usize,
        relaxed: bool,
    ) -> Checked {
        let Some(object) = v.as_object() else {
            return Self::fail(ptr, "expected an object");
        };
        for field in fields {
            let mark = push_key(ptr, &field.wire_name);
            match object.get(&field.wire_name) {
                None => {
                    if self.required(field) {
                        return Self::fail(ptr, "missing required field");
                    }
                }
                Some(Value::Null) => {
                    let nullable = matches!(
                        field.presence,
                        Presence::RequiredNullable | Presence::OptionalNullable
                    );
                    if !nullable
                        && self
                            .ty(&field.ty, &Value::Null, ptr, depth + 1, false)
                            .is_err()
                    {
                        return Self::fail(ptr, "must not be null");
                    }
                }
                Some(value) => {
                    self.ty(&field.ty, value, ptr, depth + 1, false)?;
                    self.constraints(&field.constraints, value, ptr)?;
                }
            }
            ptr.truncate(mark);
        }
        let known = |key: &str| fields.iter().any(|f| f.wire_name == key);
        match additional {
            Additional::Closed if !relaxed => {
                if let Some(extra) = object.keys().find(|k| !known(k)) {
                    let mark = push_key(ptr, extra);
                    let result = Self::fail(ptr, "unknown field");
                    ptr.truncate(mark);
                    return result;
                }
            }
            Additional::Typed { values } => {
                for (key, item) in object.iter().filter(|(k, _)| !known(k)) {
                    let mark = push_key(ptr, key);
                    self.ty(values, item, ptr, depth + 1, false)?;
                    ptr.truncate(mark);
                }
            }
            Additional::Closed | Additional::Open => {}
        }
        Ok(())
    }

    fn union(
        &self,
        union: &Union,
        v: &Value,
        ptr: &mut String,
        depth: usize,
        relaxed: bool,
    ) -> Checked {
        if union.strategy == UnionStrategy::Tagged
            && let Some(discriminator) = &union.discriminator
        {
            let Some(object) = v.as_object() else {
                return Self::fail(ptr, "expected an object");
            };
            let tag = match object.get(&discriminator.property) {
                Some(Value::Null) | None => {
                    return Self::fail(
                        ptr,
                        format!("missing discriminator `{}`", discriminator.property),
                    );
                }
                Some(present) => tag_text(present),
            };
            if let Some(variant) = union
                .variants
                .iter()
                .find(|var| var.tag.as_deref() == Some(tag.as_str()))
            {
                return self.ty(&variant.ty, v, ptr, depth + 1, relaxed);
            }
            if let Some((_, id)) = discriminator.mapping.iter().find(|(k, _)| *k == tag) {
                return self.ty(&TypeRef::Named(id.clone()), v, ptr, depth + 1, relaxed);
            }
            return Self::fail(
                ptr,
                format!(
                    "unknown `{}` value {}",
                    discriminator.property,
                    Value::String(tag)
                ),
            );
        }
        let mut first_error = None;
        for variant in &union.variants {
            match self.ty(&variant.ty, v, ptr, depth + 1, relaxed) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        let names: Vec<&str> = union
            .variants
            .iter()
            .map(|var| var.name.wire.as_str())
            .collect();
        let detail = first_error
            .map(|e| format!(" (first: {e})"))
            .unwrap_or_default();
        Self::fail(
            ptr,
            format!(
                "matches none of the variants [{}]{detail}",
                names.join(", ")
            ),
        )
    }

    /// Members that are all closed records admit only their combined fields.
    fn intersection_closed(&self, members: &[TypeRef], v: &Value, ptr: &mut String) -> Checked {
        let Some(object) = v.as_object() else {
            return Ok(());
        };
        let mut allowed: BTreeSet<&str> = BTreeSet::new();
        for member in members {
            match self.model.shape(member) {
                Some(Shape::Record {
                    fields,
                    additional: Additional::Closed,
                }) => allowed.extend(fields.iter().map(|f| f.wire_name.as_str())),
                _ => return Ok(()),
            }
        }
        if let Some(extra) = object.keys().find(|k| !allowed.contains(k.as_str())) {
            let mark = push_key(ptr, extra);
            let result = Self::fail(ptr, "unknown field");
            ptr.truncate(mark);
            return result;
        }
        Ok(())
    }

    /// Length, pattern and range constraints, applied to strings and numbers.
    fn constraints(&self, c: &Constraints, v: &Value, ptr: &str) -> Checked {
        if c.is_empty() {
            return Ok(());
        }
        if let Some(s) = v.as_str() {
            let len = s.chars().count() as u64;
            if let Some(min) = c.min_length
                && len < min
            {
                return Self::fail(ptr, format!("must be at least {min} characters"));
            }
            if let Some(max) = c.max_length
                && len > max
            {
                return Self::fail(ptr, format!("must be at most {max} characters"));
            }
            if let Some(pattern) = &c.pattern
                && !self.model.pattern_matches(pattern, s)
            {
                return Self::fail(ptr, format!("does not match pattern `{pattern}`"));
            }
        }
        if let Some(x) = v.as_f64() {
            let bound =
                |n: &Option<serde_json::Number>| n.as_ref().and_then(serde_json::Number::as_f64);
            if let Some(min) = bound(&c.minimum)
                && x < min
            {
                return Self::fail(ptr, format!("must be at least {min}"));
            }
            if let Some(max) = bound(&c.maximum)
                && x > max
            {
                return Self::fail(ptr, format!("must be at most {max}"));
            }
            if let Some(min) = bound(&c.exclusive_minimum)
                && x <= min
            {
                return Self::fail(ptr, format!("must be greater than {min}"));
            }
            if let Some(max) = bound(&c.exclusive_maximum)
                && x >= max
            {
                return Self::fail(ptr, format!("must be less than {max}"));
            }
            if let Some(m) = bound(&c.multiple_of)
                && m > 0.0
            {
                let q = x / m;
                if (q - q.round()).abs() > 1e-9 * q.abs().max(1.0) {
                    return Self::fail(ptr, format!("must be a multiple of {m}"));
                }
            }
        }
        Ok(())
    }
}

/// Append `/key` (escaped) to a pointer; returns the length to truncate to.
fn push_key(ptr: &mut String, key: &str) -> usize {
    let mark = ptr.len();
    ptr.push('/');
    ptr.push_str(&key.replace('~', "~0").replace('/', "~1"));
    mark
}

/// Type check of a primitive (constraints are checked separately).
fn check_primitive(primitive: &Primitive, v: &Value) -> Result<(), String> {
    match primitive {
        Primitive::String { format } => {
            let Some(s) = v.as_str() else {
                return Err("expected a string".into());
            };
            match format {
                Some(f) if !format_ok(f, s) => Err(format!("is not a valid {}", format_name(f))),
                _ => Ok(()),
            }
        }
        Primitive::Int32 => integer_in(v, i32::MIN as f64, i32::MAX as f64, "a 32-bit integer"),
        Primitive::Int64 => integer_in(v, i64::MIN as f64, i64::MAX as f64, "a 64-bit integer"),
        Primitive::Integer => integer_in(v, f64::NEG_INFINITY, f64::INFINITY, "an integer"),
        Primitive::Float | Primitive::Double | Primitive::Number => {
            if v.is_number() {
                Ok(())
            } else {
                Err("expected a number".into())
            }
        }
        Primitive::Bool => {
            if v.is_boolean() {
                Ok(())
            } else {
                Err("expected a boolean".into())
            }
        }
        Primitive::Bytes => {
            if v.is_string() {
                Ok(())
            } else {
                Err("expected a string".into())
            }
        }
    }
}

fn integer_in(v: &Value, low: f64, high: f64, what: &str) -> Result<(), String> {
    if v.is_i64() || v.is_u64() {
        let x = v.as_f64().unwrap_or(0.0);
        return if x >= low && x <= high {
            Ok(())
        } else {
            Err(format!("expected {what}"))
        };
    }
    match v.as_f64() {
        Some(x) if x.fract() == 0.0 && x >= low && x <= high => Ok(()),
        _ => Err(format!("expected {what}")),
    }
}

fn format_name(format: &StringFormat) -> &str {
    match format {
        StringFormat::Uuid => "uuid",
        StringFormat::DateTime => "date-time",
        StringFormat::Date => "date",
        StringFormat::Time => "time",
        StringFormat::Duration => "duration",
        StringFormat::Email => "email",
        StringFormat::Uri => "uri",
        StringFormat::Hostname => "hostname",
        StringFormat::Ipv4 => "ipv4",
        StringFormat::Ipv6 => "ipv6",
        StringFormat::Byte => "byte",
        StringFormat::Password => "password",
        StringFormat::Other(name) => name,
    }
}

pub(crate) fn format_ok(format: &StringFormat, s: &str) -> bool {
    match format {
        StringFormat::Uuid => is_uuid(s),
        StringFormat::DateTime => match s.find(['T', 't']) {
            Some(i) => is_date(&s[..i]) && is_time(&s[i + 1..], true),
            None => false,
        },
        StringFormat::Date => is_date(s),
        StringFormat::Time => is_time(s, false),
        StringFormat::Duration => {
            s.len() > 1
                && s.starts_with('P')
                && s[1..]
                    .chars()
                    .all(|c| c.is_ascii_digit() || "YMWDTHS.,".contains(c))
        }
        StringFormat::Email => match s.split_once('@') {
            Some((local, domain)) => {
                !local.is_empty()
                    && !domain.is_empty()
                    && !domain.contains('@')
                    && !s.chars().any(char::is_whitespace)
            }
            None => false,
        },
        StringFormat::Uri => match s.split_once(':') {
            Some((scheme, _)) => {
                scheme
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
                    && !s.chars().any(char::is_whitespace)
            }
            None => false,
        },
        StringFormat::Hostname => {
            !s.is_empty()
                && s.len() <= 253
                && s.trim_end_matches('.').split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                })
        }
        StringFormat::Ipv4 => s.parse::<std::net::Ipv4Addr>().is_ok(),
        StringFormat::Ipv6 => s.parse::<std::net::Ipv6Addr>().is_ok(),
        StringFormat::Byte => {
            s.len().is_multiple_of(4)
                && s.trim_end_matches('=').len() + 2 >= s.len()
                && s.trim_end_matches('=')
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
        }
        StringFormat::Password | StringFormat::Other(_) => true,
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn is_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    match parts.as_slice() {
        [y, m, d] => {
            digits(y, 4).is_some()
                && digits(m, 2).is_some_and(|m| (1..=12).contains(&m))
                && digits(d, 2).is_some_and(|d| (1..=31).contains(&d))
        }
        _ => false,
    }
}

/// `HH:MM:SS[.frac]` then `Z` or `±HH:MM` (required when `offset`).
fn is_time(s: &str, offset: bool) -> bool {
    if !s.is_ascii() || s.len() < 8 {
        return false;
    }
    let (clock, rest) = s.split_at(8);
    let parts: Vec<&str> = clock.split(':').collect();
    let clock_ok = match parts.as_slice() {
        [h, m, sec] => {
            digits(h, 2).is_some_and(|h| h <= 23)
                && digits(m, 2).is_some_and(|m| m <= 59)
                && digits(sec, 2).is_some_and(|s| s <= 60)
        }
        _ => false,
    };
    if !clock_ok {
        return false;
    }
    let rest = match rest.strip_prefix('.') {
        Some(frac) => {
            let n = frac.bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 {
                return false;
            }
            &frac[n..]
        }
        None => rest,
    };
    match rest {
        "" => !offset,
        "Z" | "z" => true,
        _ => {
            let (sign, zone) = rest.split_at(1);
            (sign == "+" || sign == "-")
                && zone.len() == 5
                && zone.as_bytes()[2] == b':'
                && digits(&zone[..2], 2).is_some_and(|h| h <= 23)
                && digits(&zone[3..], 2).is_some_and(|m| m <= 59)
        }
    }
}

/// JSON equality with numbers compared by value (`1` equals `1.0`).
pub(crate) fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(i), Some(j)) => i == j,
            _ => match (x.as_u64(), y.as_u64()) {
                (Some(i), Some(j)) => i == j,
                _ => x.as_f64() == y.as_f64(),
            },
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, p)| y.get(k).is_some_and(|q| json_eq(p, q)))
        }
        _ => a == b,
    }
}

/// A canonical text form for uniqueness checks: sorted keys, numbers by
/// value.
fn canonical(v: &Value) -> String {
    match v {
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) if f.fract() == 0.0 && f.abs() < 9.0e15 => (f as i64).to_string(),
            _ => n.to_string(),
        },
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        Value::Object(map) => {
            let mut keys: Vec<(&String, &Value)> = map.iter().collect();
            keys.sort_by(|a, b| a.0.cmp(b.0));
            let inner: Vec<String> = keys
                .iter()
                .map(|(k, v)| format!("{}:{}", Value::String((*k).clone()), canonical(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// An empty JSON object.
pub(crate) fn empty_object() -> Value {
    Value::Object(Map::new())
}
