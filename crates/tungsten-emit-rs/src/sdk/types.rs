// SPDX-License-Identifier: AGPL-3.0-only
//! Rust types for IR shapes, constraint checks, and the
//! `models/<namespace>.rs` files.
//!
//! Every named record is a struct, every string or integer enum a closed
//! enum, every union an enum, and every other named type a `pub type`
//! alias (a transparent newtype when the alias is on a reference cycle).
//! Presence is one of `T`, `Option<T>` (null on the wire),
//! `Option<T>` that is left out when `None`, and `Patch<T>`. Recursion is
//! broken with `Box` at the edges of by-value cycles only.
//!
//! Constraints (bounds, lengths, patterns, formats, uniqueness, constants)
//! are not part of the types: each named type that has any gets a
//! `check_<name>` function that walks a decoded value and reports issues
//! with exact paths.

use std::cell::RefCell;
use std::collections::BTreeSet;

use serde_json::Value;
use tungsten_emit::Writer;
use tungsten_ir::naming::Role;
use tungsten_ir::{
    Additional, Constraints, EnumValue, Field, NamedType, Presence, Primitive, Shape, StringFormat,
    TypeId, TypeRef, Union, Variant,
};

use super::graph::{checked_format, field_constraints};
use super::ops::fn_sig;
use super::plan::{Emit, Plan, UnionKind, emit_kind, field_names, unique};
use super::rs::{Rx, doc, doc_text, imports_for, paragraphs, string_lit};

/// Renders types from one module's point of view.
#[derive(Debug)]
pub(crate) struct Cx<'p, 'a> {
    pub plan: &'p Plan<'a>,
    /// The model namespace being written; `None` outside `models/`.
    pub home: Option<&'p str>,
    /// Regex sources used by checks, in order of first use.
    pub(crate) patterns: RefCell<Vec<String>>,
}

/// A field or argument slot: its Rust type and serde attributes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Slot {
    pub ty: String,
    /// Arguments of `#[serde(...)]` attributes, one list per attribute.
    pub attrs: Vec<Vec<String>>,
}

impl<'p, 'a> Cx<'p, 'a> {
    pub(crate) fn new(plan: &'p Plan<'a>, home: Option<&'p str>) -> Self {
        Cx {
            plan,
            home,
            patterns: RefCell::new(vec![]),
        }
    }

    /// The path of a named type as written in this module.
    pub(crate) fn type_path(&self, id: &TypeId) -> String {
        match self.plan.types.get(id) {
            Some(info) if self.home == Some(info.ns.as_str()) => info.name.clone(),
            Some(info) => match self.plan.model_ns(&info.ns) {
                Some(m) => format!("crate::models::{}::{}", m.file, info.name),
                None => "Value".into(),
            },
            None => "Value".into(),
        }
    }

    /// The path of a named type's check function as written in this module.
    pub(crate) fn check_path(&self, id: &TypeId) -> Option<String> {
        let info = self.plan.types.get(id)?;
        if !self.plan.graph.needs_check.contains(id) {
            return None;
        }
        if self.home == Some(info.ns.as_str()) {
            return Some(info.check_fn.clone());
        }
        let m = self.plan.model_ns(&info.ns)?;
        Some(format!("crate::models::{}::{}", m.file, info.check_fn))
    }

    pub(crate) fn ty(&self, r: &TypeRef) -> String {
        match r {
            TypeRef::Named(id) => self.type_path(id),
            TypeRef::Inline(s) => self.shape(s),
        }
    }

    /// The type of an inline shape. Shapes the builder always names
    /// (records, enums, unions) fall back to `Value` here.
    pub(crate) fn shape(&self, s: &Shape) -> String {
        match s {
            Shape::Primitive {
                primitive,
                constraints,
            } => primitive_type(primitive, constraints, true),
            Shape::Const { value } => const_type(value).into(),
            Shape::Array { items, .. } => format!("Vec<{}>", self.ty(items)),
            Shape::Map { values } => format!("BTreeMap<String, {}>", self.ty(values)),
            Shape::Nullable { inner } => format!("Option<{}>", self.ty(inner)),
            Shape::Any
            | Shape::Never
            | Shape::Enum { .. }
            | Shape::Record { .. }
            | Shape::Union(_)
            | Shape::Intersection { .. } => "Value".into(),
        }
    }

    /// Whether a type is a base64 string held directly as `Vec<u8>`.
    fn direct_bytes(r: &TypeRef) -> bool {
        matches!(r, TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Primitive {
            primitive: Primitive::String { format: Some(StringFormat::Byte) },
            ..
        }))
    }

    /// The type and serde attributes of a field of the given presence.
    /// `boxed` puts the value behind a `Box`.
    pub(crate) fn slot(&self, r: &TypeRef, presence: Presence, boxed: bool) -> Slot {
        let direct = Self::direct_bytes(r);
        let base = if direct {
            "Vec<u8>".to_string()
        } else if boxed {
            format!("Box<{}>", self.ty(r))
        } else {
            self.ty(r)
        };
        let mut attrs: Vec<Vec<String>> = vec![];
        let ty = match presence {
            Presence::Required => {
                if direct {
                    attrs.push(vec![r#"with = "tungsten_runtime::b64""#.into()]);
                }
                base
            }
            Presence::RequiredNullable => {
                let module = if direct {
                    "s::b64_nullable"
                } else {
                    "s::nullable"
                };
                attrs.push(vec![format!("with = \"{module}\"")]);
                format!("Option<{base}>")
            }
            Presence::Optional => {
                let module = if direct { "s::b64_opt" } else { "s::opt" };
                attrs.push(vec![
                    "default".into(),
                    r#"skip_serializing_if = "Option::is_none""#.into(),
                    format!("with = \"{module}\""),
                ]);
                format!("Option<{base}>")
            }
            Presence::OptionalNullable => {
                let mut a = vec![
                    "default".to_string(),
                    r#"skip_serializing_if = "Patch::is_undefined""#.into(),
                ];
                if direct {
                    a.push(r#"with = "s::b64_patch""#.into());
                }
                attrs.push(a);
                format!("Patch<{base}>")
            }
        };
        Slot { ty, attrs }
    }

    /// The name of the regex static for `source`, registering it.
    fn pattern(&self, source: &str) -> usize {
        let mut p = self.patterns.borrow_mut();
        match p.iter().position(|s| s == source) {
            Some(i) => i + 1,
            None => {
                p.push(source.to_string());
                p.len()
            }
        }
    }

    // ------------------------------------------------------------ checks

    /// Statements checking `x` (an expression of type `&T`) as a `ty`.
    /// `extra` are constraints the field adds to the type.
    pub(crate) fn check_stmts(
        &self,
        ty: &TypeRef,
        extra: Option<&Constraints>,
        x: &str,
        depth: usize,
        out: &mut Vec<String>,
    ) {
        match ty {
            TypeRef::Named(id) => {
                if let Some(f) = self.check_path(id) {
                    out.push(format!("{f}({x}, c);"));
                }
                if let Some(c) = extra
                    && let Some(Shape::Primitive { primitive, .. }) = self.plan.resolve(ty)
                {
                    self.primitive_stmts(primitive, c, x, out);
                }
            }
            TypeRef::Inline(s) => self.shape_stmts(s, extra, x, depth, out),
        }
    }

    fn shape_stmts(
        &self,
        s: &Shape,
        extra: Option<&Constraints>,
        x: &str,
        depth: usize,
        out: &mut Vec<String>,
    ) {
        match s {
            Shape::Primitive {
                primitive,
                constraints,
            } => {
                let c = if constraints.is_empty() {
                    extra.unwrap_or(constraints)
                } else {
                    constraints
                };
                self.primitive_stmts(primitive, c, x, out);
            }
            Shape::Array {
                items,
                min,
                max,
                unique,
            } => {
                if min.is_some() || max.is_some() {
                    out.push(format!(
                        "c.items({x}.len(), {}, {});",
                        opt_u64(*min),
                        opt_u64(*max)
                    ));
                }
                if *unique {
                    out.push(format!("c.unique({x});"));
                }
                let mut inner = vec![];
                let item = format!("x{}", depth + 1);
                self.check_stmts(items, None, &item, depth + 1, &mut inner);
                if !inner.is_empty() {
                    let i = format!("i{}", depth + 1);
                    out.push(format!("for ({i}, {item}) in {x}.iter().enumerate() {{"));
                    out.push(format!("    c.push_index({i});"));
                    out.extend(inner.into_iter().map(|l| format!("    {l}")));
                    out.push("    c.pop();".into());
                    out.push("}".into());
                }
            }
            Shape::Map { values } => {
                let mut inner = vec![];
                let item = format!("x{}", depth + 1);
                self.check_stmts(values, None, &item, depth + 1, &mut inner);
                if !inner.is_empty() {
                    let k = format!("k{}", depth + 1);
                    out.push(format!("for ({k}, {item}) in {x}.iter() {{"));
                    out.push(format!("    c.push_key({k});"));
                    out.extend(inner.into_iter().map(|l| format!("    {l}")));
                    out.push("    c.pop();".into());
                    out.push("}".into());
                }
            }
            Shape::Nullable { inner } => {
                let mut stmts = vec![];
                let item = format!("x{}", depth + 1);
                self.check_stmts(inner, extra, &item, depth + 1, &mut stmts);
                if !stmts.is_empty() {
                    out.push(format!("if let Some({item}) = {x} {{"));
                    out.extend(stmts.into_iter().map(|l| format!("    {l}")));
                    out.push("}".into());
                }
            }
            Shape::Const { value } => out.push(const_check(value, x)),
            Shape::Enum { values, .. } => {
                let list = Value::Array(values.iter().map(|v| v.value.clone()).collect());
                out.push(format!("c.one_of({x}, {});", json_arg(&list)));
            }
            Shape::Never => out.push("c.issue(\"no value is valid here\");".into()),
            Shape::Intersection { members } => {
                for m in members {
                    match m {
                        TypeRef::Named(id) => {
                            let ty = self.type_path(id);
                            match self.check_path(id) {
                                Some(f) => out.push(format!("c.member({x}, {f});")),
                                None => out.push(format!("c.member({x}, s::no_check::<{ty}>);")),
                            }
                        }
                        TypeRef::Inline(_) => {}
                    }
                }
            }
            Shape::Any | Shape::Record { .. } | Shape::Union(_) => {}
        }
    }

    fn primitive_stmts(&self, p: &Primitive, c: &Constraints, x: &str, out: &mut Vec<String>) {
        match p {
            Primitive::String { format } => {
                if c.min_length.is_some() || c.max_length.is_some() {
                    out.push(format!(
                        "c.str_len({x}, {}, {});",
                        opt_u64(c.min_length),
                        opt_u64(c.max_length)
                    ));
                }
                if let Some(f) = format.as_ref().and_then(checked_format) {
                    out.push(format!("c.format({x}, s::Format::{f});"));
                }
                if let Some(pattern) = &c.pattern {
                    let source = translate_pattern(pattern);
                    let n = self.pattern(&source);
                    out.push(format!("c.pattern({x}, &RE_{n}, {});", string_lit(&source)));
                }
            }
            Primitive::Int32 | Primitive::Int64 | Primitive::Integer => {
                let exact = [
                    &c.minimum,
                    &c.maximum,
                    &c.exclusive_minimum,
                    &c.exclusive_maximum,
                    &c.multiple_of,
                ]
                .into_iter()
                .flatten()
                .all(|n| n.is_i64() || n.is_u64());
                let value = if exact {
                    format!("i128::from(*{x})")
                } else {
                    format!("*{x} as f64")
                };
                number_stmts(c, &value, exact, out);
            }
            Primitive::Float | Primitive::Double | Primitive::Number => {
                number_stmts(c, &format!("*{x}"), false, out);
            }
            Primitive::Bool | Primitive::Bytes => {}
        }
    }

    /// The statements checking a record field.
    pub(crate) fn field_stmts(&self, key: &str, f: &Field, access: &str, out: &mut Vec<String>) {
        self.arg_stmts(key, &f.ty, f.presence, field_constraints(f), access, out);
    }

    /// The statements checking a value of presence `presence`: its value
    /// and extra constraints, between `push_key` and `pop`. `access` is the
    /// expression of the value (`v.name`).
    pub(crate) fn arg_stmts(
        &self,
        key: &str,
        ty: &TypeRef,
        presence: Presence,
        extra: Option<&Constraints>,
        access: &str,
        out: &mut Vec<String>,
    ) {
        let mut inner = vec![];
        self.check_stmts(ty, extra, "x0", 0, &mut inner);
        if inner.is_empty() {
            return;
        }
        out.push(format!("c.push_key({});", string_lit(key)));
        match presence {
            Presence::Required => {
                out.push(format!("let x0 = &{access};"));
                out.extend(inner);
            }
            Presence::RequiredNullable | Presence::Optional => {
                out.push(format!("if let Some(x0) = &{access} {{"));
                out.extend(inner.into_iter().map(|l| format!("    {l}")));
                out.push("}".into());
            }
            Presence::OptionalNullable => {
                out.push(format!("if let Some(x0) = {access}.as_value() {{"));
                out.extend(inner.into_iter().map(|l| format!("    {l}")));
                out.push("}".into());
            }
        }
        out.push("c.pop();".into());
    }
}

fn opt_u64(n: Option<u64>) -> String {
    n.map_or_else(|| "None".to_string(), |n| format!("Some({n})"))
}

/// The statements for the bounds of a number; `value` is an expression of
/// type `i128` (`exact`) or `f64`.
fn number_stmts(c: &Constraints, value: &str, exact: bool, out: &mut Vec<String>) {
    let suffix = if exact { "i" } else { "f" };
    for (name, bound) in [
        ("min", &c.minimum),
        ("max", &c.maximum),
        ("gt", &c.exclusive_minimum),
        ("lt", &c.exclusive_maximum),
        ("multiple", &c.multiple_of),
    ] {
        if let Some(n) = bound {
            let lit = if exact {
                n.to_string()
            } else {
                n.as_f64()
                    .map_or_else(|| "0.0".into(), |f| format!("{f:?}"))
            };
            out.push(format!("c.{name}_{suffix}({value}, {lit});"));
        }
    }
}

/// A JSON value as the argument of a check: a string literal holding its
/// text.
fn json_arg(v: &Value) -> String {
    string_lit(&serde_json::to_string(v).unwrap_or_else(|_| "null".into()))
}

/// The Rust type of an inline constant.
fn const_type(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "String",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() => "i64",
        _ => "Value",
    }
}

fn const_check(v: &Value, x: &str) -> String {
    match v {
        Value::String(s) => format!("c.const_str({x}, {});", string_lit(s)),
        Value::Bool(b) => format!("c.const_bool(*{x}, {b});"),
        Value::Number(n) if n.is_i64() => format!("c.const_int(*{x}, {n});"),
        other => format!("c.const_json({x}, {});", json_arg(other)),
    }
}

/// The Rust type of a primitive. `nested` is true outside a direct field
/// (where base64 text is `Vec<u8>` with serde attributes).
pub(crate) fn primitive_type(p: &Primitive, c: &Constraints, nested: bool) -> String {
    match p {
        Primitive::String {
            format: Some(StringFormat::Byte),
        } => {
            if nested {
                "s::Base64".into()
            } else {
                "Vec<u8>".into()
            }
        }
        Primitive::String { .. } => "String".into(),
        Primitive::Bytes => "tungsten_runtime::Binary".into(),
        Primitive::Bool => "bool".into(),
        Primitive::Int32 => "i32".into(),
        Primitive::Int64 | Primitive::Integer => {
            let wide = c
                .minimum
                .as_ref()
                .is_some_and(|m| m.as_i64().is_some_and(|m| m >= 0))
                && c.maximum
                    .as_ref()
                    .is_some_and(|m| m.as_u64().is_some_and(|m| m > i64::MAX as u64));
            if wide { "u64".into() } else { "i64".into() }
        }
        Primitive::Float | Primitive::Double | Primitive::Number => "f64".into(),
    }
}

/// A JSON Schema `pattern` (ECMA-262) as a pattern of the `regex` crate:
/// `\d` and `\w` are ASCII classes in ECMAScript, Unicode ones in `regex`.
pub(crate) fn translate_pattern(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut chars = p.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('d') => out.push_str(if in_class { "0-9" } else { "[0-9]" }),
                Some('w') => out.push_str(if in_class {
                    "A-Za-z0-9_"
                } else {
                    "[A-Za-z0-9_]"
                }),
                Some('D') if !in_class => out.push_str("[^0-9]"),
                Some('W') if !in_class => out.push_str("[^A-Za-z0-9_]"),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            },
            '[' if !in_class => {
                in_class = true;
                out.push('[');
                if chars.peek() == Some(&'^') {
                    out.push('^');
                    chars.next();
                }
                if chars.peek() == Some(&']') {
                    out.push_str("\\]");
                    chars.next();
                }
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            other => out.push(other),
        }
    }
    out
}

// ------------------------------------------------------------------ files

/// `#[serde(...)]` with rustfmt's layout (arguments wider than 70 columns
/// go one per line).
pub(crate) fn serde_attr(w: &mut Writer, args: &[String]) {
    attr(w, "serde", args);
}

pub(crate) fn attr(w: &mut Writer, name: &str, args: &[String]) {
    let joined = args.join(", ");
    if joined.len() <= 70 {
        w.line(format!("#[{name}({joined})]"));
    } else {
        w.line(format!("#[{name}("));
        w.indent();
        for (i, a) in args.iter().enumerate() {
            let comma = if i + 1 < args.len() { "," } else { "" };
            w.line(format!("{a}{comma}"));
        }
        w.dedent();
        w.line(")]");
    }
}

/// The docs of a field: its description plus the facts its type does not
/// show.
pub(crate) fn field_doc(f: &Field, name: &str) -> String {
    let mut notes: Vec<String> = vec![];
    if name != f.wire_name {
        notes.push(format!("Wire name: `{}`.", f.wire_name));
    }
    if f.read_only {
        notes.push("Read-only: set by the server, ignored in requests.".into());
    }
    if f.write_only {
        notes.push("Write-only: never returned by the server.".into());
    }
    if f.sensitive {
        notes.push("Sensitive: never log or persist this value.".into());
    }
    if let TypeRef::Inline(s) = &f.ty {
        notes.extend(shape_notes(s));
    }
    if let Some(d) = &f.default {
        notes.push(format!(
            "Default (applied by the server): `{}`.",
            serde_json::to_string(d).unwrap_or_default()
        ));
    }
    if f.deprecated {
        notes.push("Deprecated.".into());
    }
    paragraphs(std::iter::once(doc_text(f.doc.as_ref())).chain(notes))
}

/// Facts about a shape that its Rust type does not show.
pub(crate) fn shape_notes(shape: &Shape) -> Vec<String> {
    let mut notes = vec![];
    match shape {
        Shape::Primitive {
            primitive: Primitive::String { format: Some(f) },
            ..
        } => match f {
            StringFormat::Byte => notes.push("Base64 on the wire; bytes here.".into()),
            f => notes.push(format!("Format: `{}`.", format_name(f))),
        },
        Shape::Primitive {
            primitive: Primitive::Bytes,
            ..
        } => notes.push("Binary; sent as a file or raw body.".into()),
        Shape::Array { unique: true, .. } => notes.push("Items must be unique.".into()),
        _ => {}
    }
    notes
}

fn format_name(f: &StringFormat) -> String {
    match f {
        StringFormat::Uuid => "uuid".into(),
        StringFormat::DateTime => "date-time".into(),
        StringFormat::Date => "date".into(),
        StringFormat::Time => "time".into(),
        StringFormat::Duration => "duration".into(),
        StringFormat::Email => "email".into(),
        StringFormat::Uri => "uri".into(),
        StringFormat::Hostname => "hostname".into(),
        StringFormat::Ipv4 => "ipv4".into(),
        StringFormat::Ipv6 => "ipv6".into(),
        StringFormat::Byte => "byte".into(),
        StringFormat::Password => "password".into(),
        StringFormat::Other(s) => s.clone(),
    }
}

/// Whether a record has a sensitive field (its `Debug` redacts it).
fn has_sensitive(fields: &[Field]) -> bool {
    fields.iter().any(|f| f.sensitive)
}

/// A match arm in rustfmt's layout: one line when it fits, else the body
/// in a block when it fits there, else the body broken over lines.
pub(crate) fn arm(w: &mut Writer, indent: usize, pattern: &str, body: &Rx) {
    let flat = format!("{pattern} => {},", body.flat());
    if body.flat_valid() && indent + flat.len() <= super::rs::MAX_WIDTH {
        w.line(flat);
    } else if body.flat_valid() && indent + 4 + body.flat().len() <= super::rs::MAX_WIDTH {
        w.line(format!("{pattern} => {{"));
        w.indent();
        w.line(body.flat());
        w.dedent();
        w.line("}");
    } else {
        let col = indent + pattern.len() + 4;
        let text = body.render(indent, col, 1);
        let mut lines = text.lines();
        let mut out = format!("{pattern} => {}", lines.next().unwrap_or(""));
        for l in lines {
            out.push('\n');
            out.push_str(l.get(indent..).unwrap_or(l));
        }
        out.push(',');
        w.line(out);
    }
}

/// Write statements at the writer's current indentation, `base` columns.
/// A call statement too long for a line is laid out the way rustfmt does.
pub(crate) fn emit_stmts(w: &mut Writer, base: usize, stmts: &[String]) {
    for s in stmts {
        let text = s.trim_start();
        let lead = s.len() - text.len();
        if base + s.len() <= super::rs::MAX_WIDTH {
            w.line(s);
            continue;
        }
        let Some((callee, args)) = text
            .strip_suffix(");")
            .and_then(|t| t.split_once('('))
            .filter(|(_, a)| !a.contains('('))
        else {
            w.line(s);
            continue;
        };
        let call = Rx::call(callee, args.split(", ").map(Rx::atom).collect());
        let abs = base + lead;
        let rendered = call.render(abs, abs, 1);
        let mut lines = rendered.lines();
        let mut out = format!("{}{}", " ".repeat(lead), lines.next().unwrap_or(""));
        for l in lines {
            out.push('\n');
            out.push_str(l.get(base..).unwrap_or(l));
        }
        out.push(';');
        w.line(out);
    }
}

/// Whether `word` occurs in `line` as an identifier.
fn mentions(line: &str, word: &str) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(i) = line[from..].find(word) {
        let start = from + i;
        let end = start + word.len();
        let before =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let after =
            end >= bytes.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
        if before && after {
            return true;
        }
        from = end;
    }
    false
}

/// Write `fn check_x(v: &T, c: &mut Checker) { ... }`.
pub(crate) fn write_check_fn(w: &mut Writer, name: &str, ty: &str, stmts: &[String]) {
    w.blank();
    // `v` may be unused (a type no value satisfies).
    let v = if stmts.iter().any(|s| mentions(s, "v")) {
        "v"
    } else {
        "_v"
    };
    fn_sig(
        w,
        0,
        &format!("pub fn {name}"),
        &[format!("{v}: &{ty}"), "c: &mut Checker".to_string()],
        "",
    );
    w.indent();
    if stmts.is_empty() {
        w.line("let _ = (v, c);");
    } else {
        emit_stmts(w, 4, stmts);
    }
    w.dedent();
    w.line("}");
}

/// One enum variant name per entry, unique, rendered for `EnumVariant`.
fn variant_names(entries: Vec<Vec<String>>) -> Vec<String> {
    unique(&["Self"], &entries, Role::EnumVariant)
}

fn write_struct(
    w: &mut Writer,
    cx: &Cx<'_, '_>,
    nt: &NamedType,
    name: &str,
    fields: &[Field],
    additional: &Additional,
) {
    let plan = cx.plan;
    let sensitive = has_sensitive(fields);
    let reserved: &[&str] = if matches!(additional, Additional::Closed) {
        &[]
    } else {
        &["extra"]
    };
    let names = field_names(fields.iter().map(|f| &f.name), reserved);
    let defaultable = plan.graph.defaultable.contains(&nt.id);
    let mut derives = vec![];
    if !sensitive {
        derives.push("Debug");
    }
    derives.push("Clone");
    if defaultable {
        derives.push("Default");
    }
    derives.extend(["PartialEq", "Serialize", "Deserialize"]);
    let notes = paragraphs(
        std::iter::once(doc_text(nt.doc.as_ref())).chain(std::iter::once(String::new())),
    );
    doc(w, &notes);
    w.line(format!("#[derive({})]", derives.join(", ")));
    if matches!(additional, Additional::Closed) {
        w.line("#[serde(deny_unknown_fields)]");
    }
    if fields.is_empty() && matches!(additional, Additional::Closed) {
        w.line(format!("pub struct {name} {{}}"));
    } else {
        w.line(format!("pub struct {name} {{"));
    }
    w.indent();
    for (i, (f, field)) in fields.iter().zip(&names).enumerate() {
        let boxed = plan.graph.is_boxed(&nt.id, i);
        let slot = cx.slot(&f.ty, f.presence, boxed);
        doc(w, &field_doc(f, field));
        if *field != f.wire_name {
            w.line(format!("#[serde(rename = {})]", string_lit(&f.wire_name)));
        }
        for a in &slot.attrs {
            serde_attr(w, a);
        }
        w.line(format!("pub {field}: {},", slot.ty));
    }
    match additional {
        Additional::Closed => {}
        Additional::Open => {
            w.line("/// Members the API description does not name.");
            w.line("#[serde(flatten)]");
            w.line("pub extra: BTreeMap<String, Value>,");
        }
        Additional::Typed { values } => {
            w.line("/// Members the API description does not name.");
            w.line("#[serde(flatten)]");
            w.line(format!("pub extra: BTreeMap<String, {}>,", cx.ty(values)));
        }
    }
    w.dedent();
    if !(fields.is_empty() && matches!(additional, Additional::Closed)) {
        w.line("}");
    }

    if sensitive {
        w.blank();
        w.line(format!("impl std::fmt::Debug for {name} {{"));
        w.indent();
        w.line("fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {");
        w.indent();
        let mut calls = vec![format!("debug_struct({})", string_lit(name))];
        for (f, field) in fields.iter().zip(&names) {
            if f.sensitive {
                calls.push(format!("field({}, &\"<redacted>\")", string_lit(field)));
            } else {
                calls.push(format!("field({}, &self.{field})", string_lit(field)));
            }
        }
        calls.push(if matches!(additional, Additional::Closed) {
            "finish()".into()
        } else {
            "finish_non_exhaustive()".into()
        });
        super::ops::chain_lines(w, "f", &calls);
        w.dedent();
        w.line("}");
        w.dedent();
        w.line("}");
    }

    if plan.graph.needs_check.contains(&nt.id) {
        let mut stmts = vec![];
        for (f, field) in fields.iter().zip(&names) {
            cx.field_stmts(&f.wire_name, f, &format!("v.{field}"), &mut stmts);
        }
        if let Additional::Typed { values } = additional {
            let mut inner = vec![];
            cx.check_stmts(values, None, "x1", 0, &mut inner);
            if !inner.is_empty() {
                stmts.push("for (k1, x1) in v.extra.iter() {".into());
                stmts.push("    c.push_key(k1);".into());
                stmts.extend(inner.into_iter().map(|l| format!("    {l}")));
                stmts.push("    c.pop();".into());
                stmts.push("}".into());
            }
        }
        write_check_fn(w, &plan.types[&nt.id].check_fn, name, &stmts);
    }
}

fn write_str_enum(
    w: &mut Writer,
    cx: &Cx<'_, '_>,
    nt: &NamedType,
    name: &str,
    values: &[EnumValue],
) {
    let _ = cx;
    // Duplicate wire values select one variant.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let unique_values: Vec<&EnumValue> = values
        .iter()
        .filter(|v| v.value.as_str().is_some_and(|s| seen.insert(s)))
        .collect();
    let names = variant_names(unique_values.iter().map(|v| v.name.words.clone()).collect());
    doc(w, &doc_text(nt.doc.as_ref()));
    w.line("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]");
    w.line(format!("pub enum {name} {{"));
    w.indent();
    for (v, variant) in unique_values.iter().zip(&names) {
        let wire = v.value.as_str().unwrap_or_default();
        doc(w, &doc_text(v.doc.as_ref()));
        if variant != wire {
            w.line(format!("#[serde(rename = {})]", string_lit(wire)));
        }
        w.line(format!("{variant},"));
    }
    w.dedent();
    w.line("}");
}

fn write_int_enum(w: &mut Writer, nt: &NamedType, name: &str, values: &[EnumValue]) {
    let mut seen: BTreeSet<i64> = BTreeSet::new();
    let unique_values: Vec<(&EnumValue, i64)> = values
        .iter()
        .filter_map(|v| v.value.as_i64().map(|n| (v, n)))
        .filter(|(_, n)| seen.insert(*n))
        .collect();
    let names = variant_names(
        unique_values
            .iter()
            .map(|(v, _)| v.name.words.clone())
            .collect(),
    );
    doc(w, &doc_text(nt.doc.as_ref()));
    w.line("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]");
    w.line(format!("pub enum {name} {{"));
    w.indent();
    for ((v, n), variant) in unique_values.iter().zip(&names) {
        doc(
            w,
            &paragraphs([doc_text(v.doc.as_ref()), format!("Wire value: `{n}`.")]),
        );
        w.line(format!("{variant},"));
    }
    w.dedent();
    w.line("}");
    w.blank();
    w.line(format!("impl Serialize for {name} {{"));
    w.indent();
    w.line("fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {");
    w.indent();
    w.line("match self {");
    w.indent();
    for ((_, n), variant) in unique_values.iter().zip(&names) {
        let pattern = format!("{name}::{variant}");
        let body = Rx::call("serializer.serialize_i64", vec![Rx::atom(n.to_string())]);
        arm(w, 12, &pattern, &body);
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    w.line(format!("impl<'de> Deserialize<'de> for {name} {{"));
    w.indent();
    w.line("fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {");
    w.indent();
    w.line("match i64::deserialize(deserializer)? {");
    w.indent();
    for ((_, n), variant) in unique_values.iter().zip(&names) {
        let body = Rx::call("Ok", vec![Rx::atom(format!("{name}::{variant}"))]);
        arm(w, 12, &n.to_string(), &body);
    }
    let unexpected = Rx::call(
        "s::unexpected",
        vec![Rx::atom("other"), Rx::atom(string_lit(name))],
    );
    arm(w, 12, "other", &Rx::call("Err", vec![unexpected]));
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
}

/// The Rust type of a union variant's payload.
fn payload(cx: &Cx<'_, '_>, owner: &TypeId, idx: usize, v: &Variant) -> Option<String> {
    if matches!(&v.ty, TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Const { .. })) {
        return None;
    }
    let ty = cx.ty(&v.ty);
    Some(if cx.plan.graph.is_boxed(owner, idx) {
        format!("Box<{ty}>")
    } else {
        ty
    })
}

fn variant_words(u: &Union) -> Vec<Vec<String>> {
    u.variants.iter().map(|v| v.name.words.clone()).collect()
}

fn write_union(
    w: &mut Writer,
    cx: &Cx<'_, '_>,
    nt: &NamedType,
    name: &str,
    u: &Union,
    kind: UnionKind,
) {
    let plan = cx.plan;
    let names = variant_names(variant_words(u));
    let notes = doc_text(nt.doc.as_ref());
    doc(w, &notes);
    match kind {
        UnionKind::Untagged => {
            w.line("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]");
            w.line("#[serde(untagged)]");
        }
        UnionKind::Tagged | UnionKind::Literal => {
            w.line("#[derive(Debug, Clone, PartialEq)]");
        }
    }
    w.line(format!("pub enum {name} {{"));
    w.indent();
    for (i, (v, variant)) in u.variants.iter().zip(&names).enumerate() {
        match payload(cx, &nt.id, i, v) {
            Some(ty) => w.line(format!("{variant}({ty}),")),
            None => {
                if let TypeRef::Inline(s) = &v.ty
                    && let Shape::Const { value } = s.as_ref()
                {
                    doc(
                        w,
                        &format!(
                            "The constant `{}`.",
                            serde_json::to_string(value).unwrap_or_default()
                        ),
                    );
                }
                w.line(format!("{variant},"))
            }
        };
    }
    w.dedent();
    w.line("}");
    match kind {
        UnionKind::Untagged => {}
        UnionKind::Tagged => write_tagged_impls(w, cx, name, u, &names),
        UnionKind::Literal => write_literal_impls(w, name, u, &names),
    }
    if plan.graph.needs_check.contains(&nt.id) {
        let arms: Vec<(String, Option<&TypeRef>)> = u
            .variants
            .iter()
            .zip(&names)
            .map(|(v, variant)| {
                let is_const = matches!(&v.ty, TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Const { .. }));
                (variant.clone(), (!is_const).then_some(&v.ty))
            })
            .collect();
        let stmts = variant_match_stmts(cx, name, &arms);
        write_check_fn(w, &plan.types[&nt.id].check_fn, name, &stmts);
    }
}

/// The statements of the check function of an enum: a `match` over its
/// variants. An arm with a payload type checks it; one without (a constant)
/// is a unit variant.
fn variant_match_stmts(
    cx: &Cx<'_, '_>,
    name: &str,
    arms: &[(String, Option<&TypeRef>)],
) -> Vec<String> {
    let mut stmts = vec!["match v {".to_string()];
    for (variant, ty) in arms {
        let mut inner = vec![];
        if let Some(ty) = ty {
            cx.check_stmts(ty, None, "x", 0, &mut inner);
        }
        let pat = if ty.is_none() {
            format!("{name}::{variant}")
        } else if inner.is_empty() {
            format!("{name}::{variant}(_)")
        } else {
            format!("{name}::{variant}(x)")
        };
        match inner.as_slice() {
            [] => stmts.push(format!("    {pat} => {{}}")),
            [one] if one.starts_with("check_") || one.contains("::check_") => {
                // A call of a check function: `f(x, c);`.
                let call = one.trim_end_matches(';');
                let (callee, args) = call.split_once('(').unwrap_or((call, ")"));
                let args: Vec<Rx> = args
                    .trim_end_matches(')')
                    .split(", ")
                    .map(Rx::atom)
                    .collect();
                let mut tmp = Writer::new("    ");
                arm(&mut tmp, 8, &pat, &Rx::call(callee, args));
                for l in tmp.finish().lines() {
                    stmts.push(format!("    {l}"));
                }
            }
            many => {
                stmts.push(format!("    {pat} => {{"));
                stmts.extend(many.iter().map(|l| format!("        {l}")));
                stmts.push("    }".into());
            }
        }
    }
    stmts.push("}".into());
    stmts
}

/// The check function's statements for the enum `name` whose variants hold
/// `arms`; `None` when no variant has anything to check.
pub(crate) fn variant_check_stmts(
    cx: &Cx<'_, '_>,
    name: &str,
    arms: &[(String, Option<&TypeRef>)],
) -> Option<Vec<String>> {
    let any = arms.iter().any(|(_, ty)| {
        ty.is_some_and(|ty| {
            let mut probe = vec![];
            cx.check_stmts(ty, None, "x", 0, &mut probe);
            !probe.is_empty()
        })
    });
    any.then(|| variant_match_stmts(cx, name, arms))
}

fn write_tagged_impls(w: &mut Writer, cx: &Cx<'_, '_>, name: &str, u: &Union, names: &[String]) {
    let Some(d) = &u.discriminator else { return };
    let property = string_lit(&d.property);
    w.blank();
    w.line(format!("impl Serialize for {name} {{"));
    w.indent();
    w.line("fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {");
    w.indent();
    w.line("match self {");
    w.indent();
    for (v, variant) in u.variants.iter().zip(names) {
        let tag = string_lit(v.tag.as_deref().unwrap_or_default());
        let pattern = format!("{name}::{variant}(inner)");
        let body = Rx::call(
            "s::serialize_tagged",
            vec![
                Rx::atom("serializer"),
                Rx::atom(property.clone()),
                Rx::atom(tag),
                Rx::atom("inner"),
            ],
        );
        arm(w, 12, &pattern, &body);
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    w.line(format!("impl<'de> Deserialize<'de> for {name} {{"));
    w.indent();
    w.line("fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {");
    w.indent();
    w.line(format!(
        "let (tag, value) = s::read_tagged(deserializer, {property})?;"
    ));
    w.line("match tag.as_str() {");
    w.indent();
    for (v, variant) in u.variants.iter().zip(names) {
        let own = v.tag.as_deref().unwrap_or_default();
        let mut tags = vec![own.to_string()];
        // Other tags of the mapping that select this variant.
        for (value, id) in &d.mapping {
            if matches!(&v.ty, TypeRef::Named(t) if t == id)
                && value != own
                && !tags.contains(value)
                && !u.variants.iter().any(|o| o.tag.as_deref() == Some(value))
            {
                tags.push(value.clone());
            }
        }
        let pattern = tags
            .iter()
            .map(|t| string_lit(t))
            .collect::<Vec<_>>()
            .join(" | ");
        let keeps = tag_field(cx, &v.ty, &d.property);
        let body = Rx::call(
            "s::variant_into",
            vec![
                Rx::atom("value"),
                Rx::atom(property.clone()),
                Rx::atom(keeps.to_string()),
                Rx::atom(format!("{name}::{variant}")),
            ],
        );
        arm(w, 12, &pattern, &body);
    }
    let expected = u
        .variants
        .iter()
        .filter_map(|v| v.tag.as_deref())
        .collect::<Vec<_>>()
        .join(", ");
    let unknown = Rx::call(
        "s::unknown_tag",
        vec![
            Rx::atom(property.clone()),
            Rx::atom("&tag"),
            Rx::atom(string_lit(&expected)),
        ],
    );
    arm(w, 12, "_", &Rx::call("Err", vec![unknown]));
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
}

/// Whether the variant's record has a field for the discriminator.
fn tag_field(cx: &Cx<'_, '_>, ty: &TypeRef, property: &str) -> bool {
    matches!(cx.plan.resolve(ty), Some(Shape::Record { fields, .. }) if fields.iter().any(|f| f.wire_name == property))
}

fn write_literal_impls(w: &mut Writer, name: &str, u: &Union, names: &[String]) {
    w.blank();
    w.line(format!("impl Serialize for {name} {{"));
    w.indent();
    w.line("fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {");
    w.indent();
    w.line("match self {");
    w.indent();
    for (v, variant) in u.variants.iter().zip(names) {
        match &v.ty {
            TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Const { .. }) => {
                let Shape::Const { value } = s.as_ref() else {
                    continue;
                };
                let body = Rx::call(
                    "s::serialize_const",
                    vec![Rx::atom("serializer"), Rx::atom(raw_json(value))],
                );
                arm(w, 12, &format!("{name}::{variant}"), &body);
            }
            _ => arm(
                w,
                12,
                &format!("{name}::{variant}(inner)"),
                &Rx::atom("inner.serialize(serializer)"),
            ),
        }
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    w.line(format!("impl<'de> Deserialize<'de> for {name} {{"));
    w.indent();
    w.line("fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {");
    w.indent();
    w.line("let value = Value::deserialize(deserializer)?;");
    for (v, variant) in u.variants.iter().zip(names) {
        match &v.ty {
            TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Const { .. }) => {
                let Shape::Const { value } = s.as_ref() else {
                    continue;
                };
                w.line(format!("if s::is_const(&value, {}) {{", raw_json(value)));
                w.indent();
                w.line(format!("return Ok({name}::{variant});"));
                w.dedent();
                w.line("}");
            }
            _ => {
                w.line("if let Some(inner) = s::try_decode(&value) {");
                w.indent();
                w.line(format!("return Ok({name}::{variant}(inner));"));
                w.dedent();
                w.line("}");
            }
        }
    }
    w.line(format!("Err(s::no_variant({}))", string_lit(name)));
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
}

/// A JSON value as a string literal of its text.
fn raw_json(v: &Value) -> String {
    string_lit(&serde_json::to_string(v).unwrap_or_else(|_| "null".into()))
}

fn write_alias(w: &mut Writer, cx: &Cx<'_, '_>, nt: &NamedType, name: &str) {
    let plan = cx.plan;
    let mut notes = vec![doc_text(nt.doc.as_ref())];
    notes.extend(shape_notes(&nt.shape));
    let text = paragraphs(notes);
    let ty = alias_target(cx, nt);
    let newtype = plan.graph.newtypes.contains(&nt.id);
    doc(w, &text);
    if newtype {
        let mut derives = vec!["Debug", "Clone"];
        if plan.graph.defaultable.contains(&nt.id) {
            derives.push("Default");
        }
        derives.extend(["PartialEq", "Serialize", "Deserialize"]);
        w.line(format!("#[derive({})]", derives.join(", ")));
        w.line("#[serde(transparent)]");
        w.line(format!("pub struct {name}(pub {ty});"));
    } else {
        let flat = format!("pub type {name} = {ty};");
        if flat.len() <= super::rs::MAX_WIDTH {
            w.line(flat);
        } else {
            w.line(format!("pub type {name} ="));
            w.indent();
            w.line(format!("{ty};"));
            w.dedent();
        }
    }
    if plan.graph.needs_check.contains(&nt.id) {
        let mut stmts = vec![];
        let x = if newtype { "&v.0" } else { "v" };
        let ty_ref = TypeRef::Inline(Box::new(nt.shape.clone()));
        cx.check_stmts(&ty_ref, None, x, 0, &mut stmts);
        write_check_fn(w, &plan.types[&nt.id].check_fn, name, &stmts);
    }
}

/// The Rust type an alias stands for, with `Box` where a newtype's
/// by-value edge closes a cycle.
fn alias_target(cx: &Cx<'_, '_>, nt: &NamedType) -> String {
    let boxed = cx.plan.graph.is_boxed(&nt.id, 0);
    match (&nt.shape, boxed) {
        (Shape::Nullable { inner }, true) => format!("Option<Box<{}>>", cx.ty(inner)),
        (Shape::Enum { .. }, _) | (Shape::Union(_), _) => "Value".into(),
        (shape, _) => cx.shape(shape),
    }
}

/// One named type.
fn write_named(w: &mut Writer, cx: &Cx<'_, '_>, nt: &NamedType) {
    let plan = cx.plan;
    let Some(info) = plan.types.get(&nt.id) else {
        return;
    };
    let name = &info.name;
    match (emit_kind(plan.ir, nt), &nt.shape) {
        (Emit::Struct, Shape::Record { fields, additional }) => {
            write_struct(w, cx, nt, name, fields, additional)
        }
        (Emit::StrEnum, Shape::Enum { values, .. }) => write_str_enum(w, cx, nt, name, values),
        (Emit::IntEnum, Shape::Enum { values, .. }) => write_int_enum(w, nt, name, values),
        (Emit::Union(kind), Shape::Union(u)) => write_union(w, cx, nt, name, u, kind),
        _ => write_alias(w, cx, nt, name),
    }
}

/// The regex statics of a file.
pub(crate) fn write_patterns(w: &mut Writer, patterns: &[String]) {
    for (i, source) in patterns.iter().enumerate() {
        let n = i + 1;
        w.blank();
        w.line(format!("fn re_{n}() -> &'static str {{"));
        w.indent();
        w.line(string_lit(source));
        w.dedent();
        w.line("}");
        w.blank();
        w.line(format!(
            "static RE_{n}: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(re_{n}()).ok());"
        ));
    }
}

/// The names a models file imports (and its types therefore cannot take).
const MODEL_TOKENS: &[&str] = &[
    "BTreeMap",
    "Checker",
    "Deserialize",
    "Deserializer",
    "LazyLock",
    "Patch",
    "Regex",
    "Serialize",
    "Serializer",
    "Value",
];

/// The source of `models/<file>.rs` for one namespace.
pub(crate) fn models_file(plan: &Plan<'_>, ns: &str, header: &str) -> String {
    let model = plan.model_ns(ns);
    let ids: &[TypeId] = model.map_or(&[], |m| m.types.as_slice());
    let cx = Cx::new(plan, Some(ns));
    let mut code = Writer::new("    ");
    for id in ids {
        let Some(nt) = plan.ir.types.get(id) else {
            continue;
        };
        code.blank();
        write_named(&mut code, &cx, nt);
    }
    let patterns = cx.patterns.borrow().clone();
    write_patterns(&mut code, &patterns);
    let code = code.finish();
    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line(format!("//! Models of the `{ns}` namespace."));
    let uses = imports_for(&code, &[], Some(MODEL_TOKENS), &[]);
    if !uses.is_empty() {
        w.blank();
        for u in &uses {
            w.line(u);
        }
    }
    let mut out = w.finish();
    let code = code.trim_start_matches('\n');
    if !code.trim().is_empty() {
        out.push('\n');
        out.push_str(code);
    }
    out
}

/// The source of `models/mod.rs`.
pub(crate) fn models_mod(plan: &Plan<'_>, header: &str) -> String {
    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line("//! The API's models, one module per namespace.");
    w.blank();
    for m in &plan.models {
        w.line(format!("pub mod {};", m.file));
    }
    w.finish()
}
