// SPDX-License-Identifier: AGPL-3.0-only
//! C# types of IR types, and the model files: sealed records (four-state
//! presence with `Patch<T>`, additional members kept), enums as readonly
//! record structs with known values and a strict decoder, unions as
//! abstract records with variant records (by tag, by value, else the first
//! variant that decodes, in plan order). An enum of mixed JSON types is a
//! readonly record struct of known JSON values; an `allOf` that cannot be
//! merged into one record is a record with one typed part per member, each
//! decoded from the whole value and encoded back into one merged value.
//! Every model decodes and encodes itself over the runtime's JSON tree,
//! without reflection.

use std::collections::BTreeSet;

use tungsten_emit::Writer;
use tungsten_emit::sdk::{AllOfPlan, EnumPlan, LiteralKey, TypeKind, UnionPlan};
use tungsten_ir::{Additional, Doc, Field, NamedType, Presence, Primitive, Shape, TypeId, TypeRef};

use crate::Ctx;
use crate::cs::{self, lit};

/// The kind of a C# type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ck {
    Str,
    Bool,
    I32,
    I64,
    F64,
    /// Base64 text in JSON (`byte[]`).
    Bytes,
    /// A binary body or multipart field (`Binary`).
    Binary,
    /// The runtime's JSON value (`JsonNode?`).
    Any,
    /// A generated model: its (qualified) name and whether it is a struct.
    Named {
        name: String,
        value: bool,
    },
    List(Box<Ct>),
    Map(Box<Ct>),
}

/// A C# type: its kind, and whether JSON null is one of its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ct {
    pub kind: Ck,
    pub nullable: bool,
}

impl Ct {
    pub fn of(kind: Ck) -> Self {
        let nullable = kind == Ck::Any;
        Self { kind, nullable }
    }

    pub fn any() -> Self {
        Self::of(Ck::Any)
    }

    /// The type without its null.
    pub fn base(&self) -> String {
        match &self.kind {
            Ck::Str => "string".into(),
            Ck::Bool => "bool".into(),
            Ck::I32 => "int".into(),
            Ck::I64 => "long".into(),
            Ck::F64 => "double".into(),
            Ck::Bytes => "byte[]".into(),
            Ck::Binary => "Binary".into(),
            Ck::Any => "JsonNode".into(),
            Ck::Named { name, .. } => name.clone(),
            Ck::List(i) => format!("IReadOnlyList<{}>", i.ty()),
            Ck::Map(i) => format!("IReadOnlyDictionary<string, {}>", i.ty()),
        }
    }

    /// The type as written: `T?` when null is a value.
    pub fn ty(&self) -> String {
        if self.nullable {
            format!("{}?", self.base())
        } else {
            self.base()
        }
    }

    /// A value type (a struct): its nullable form is `Nullable<T>`.
    pub fn value(&self) -> bool {
        match &self.kind {
            Ck::Bool | Ck::I32 | Ck::I64 | Ck::F64 => true,
            Ck::Named { value, .. } => *value,
            _ => false,
        }
    }

    /// The same type without its null.
    pub fn non_null(&self) -> Ct {
        Ct {
            kind: self.kind.clone(),
            nullable: self.kind == Ck::Any,
        }
    }

    /// The same type with null as a value.
    pub fn with_null(&self) -> Ct {
        Ct {
            kind: self.kind.clone(),
            nullable: true,
        }
    }

    /// A `Func<JsonNode?, Decoder, ty()>` expression.
    pub fn dec(&self, depth: usize) -> String {
        let (n, d) = (format!("n{depth}"), format!("d{depth}"));
        if self.nullable && self.kind != Ck::Any {
            let inner = self.non_null();
            let helper = if inner.value() {
                "NullableValue"
            } else {
                "Nullable"
            };
            return format!(
                "({n}, {d}) => Decoder.{helper}<{}>({n}, {d}, {})",
                inner.base(),
                inner.dec(depth + 1)
            );
        }
        match &self.kind {
            Ck::Str => "Decoder.String".into(),
            Ck::Bool => "Decoder.Boolean".into(),
            Ck::I32 => "Decoder.Int32".into(),
            Ck::I64 => "Decoder.Int64".into(),
            Ck::F64 => "Decoder.Double".into(),
            Ck::Bytes => "Decoder.Bytes".into(),
            Ck::Binary => "Decoder.Binary".into(),
            Ck::Any => "Decoder.Any".into(),
            Ck::Named { name, .. } => format!("{name}.FromJson"),
            Ck::List(i) => format!(
                "({n}, {d}) => Decoder.List<{}>({n}, {d}, {})",
                i.ty(),
                i.dec(depth + 1)
            ),
            Ck::Map(i) => format!(
                "({n}, {d}) => Decoder.Map<{}>({n}, {d}, {})",
                i.ty(),
                i.dec(depth + 1)
            ),
        }
    }

    /// The decoding of the JSON value `n` with the decoder `d`, as an
    /// expression of type `ty()`.
    pub fn dec_call(&self, n: &str, d: &str, depth: usize) -> String {
        if self.nullable && self.kind != Ck::Any {
            let inner = self.non_null();
            let helper = if inner.value() {
                "NullableValue"
            } else {
                "Nullable"
            };
            return format!(
                "Decoder.{helper}<{}>({n}, {d}, {})",
                inner.base(),
                inner.dec(depth + 1)
            );
        }
        match &self.kind {
            Ck::List(i) => format!("Decoder.List<{}>({n}, {d}, {})", i.ty(), i.dec(depth + 1)),
            Ck::Map(i) => format!("Decoder.Map<{}>({n}, {d}, {})", i.ty(), i.dec(depth + 1)),
            _ => format!("{}({n}, {d})", self.dec(depth)),
        }
    }

    /// A `JsonNode?` expression of the value `e` of this type.
    pub fn enc(&self, e: &str, depth: usize) -> String {
        if self.nullable && self.kind != Ck::Any {
            let inner = self.non_null();
            let x = format!("x{depth}");
            return if inner.value() {
                format!(
                    "({e} is {} {x} ? {} : null)",
                    inner.base(),
                    inner.enc(&x, depth + 1)
                )
            } else {
                format!("({e} is null ? null : {})", inner.enc(e, depth + 1))
            };
        }
        let x = format!("x{depth}");
        match &self.kind {
            Ck::Str => format!("Encode.String({e})"),
            Ck::Bool => format!("Encode.Boolean({e})"),
            Ck::I32 => format!("Encode.Int32({e})"),
            Ck::I64 => format!("Encode.Int64({e})"),
            Ck::F64 => format!("Encode.Double({e})"),
            Ck::Bytes => format!("Encode.Bytes({e})"),
            Ck::Binary => format!("Encode.Binary({e})"),
            Ck::Any => format!("Encode.Any({e})"),
            Ck::Named { .. } => format!("{e}.ToJson()"),
            Ck::List(i) => format!("Encode.List({e}, {x} => {})", i.enc(&x, depth + 1)),
            Ck::Map(i) => format!("Encode.Map({e}, {x} => {})", i.enc(&x, depth + 1)),
        }
    }
}

/// How a member with a presence is declared, decoded and encoded.
pub struct MemberCode {
    /// The property type.
    pub ty: String,
    /// `required ` or nothing.
    pub required: bool,
    /// The decode expression, given the object variable `o` and decoder `d`.
    pub dec: String,
    /// The encode statement, given the object variable `o`.
    pub enc: String,
}

/// The declaration, decoding and encoding of a member with this presence.
pub fn member_code(ct: &Ct, presence: Presence, prop: &str, wire: &str) -> MemberCode {
    let w = lit(wire);
    let null_allowed = ct.nullable;
    match presence {
        Presence::Required if !null_allowed => MemberCode {
            ty: ct.ty(),
            required: true,
            dec: format!("d.Required<{}>(o, {w}, {})", ct.ty(), ct.dec(0)),
            enc: format!("Encode.Put(o, {w}, {});", ct.enc(prop, 0)),
        },
        Presence::Required | Presence::RequiredNullable => {
            if ct.kind == Ck::Any {
                return MemberCode {
                    ty: "JsonNode?".into(),
                    required: true,
                    dec: format!("d.Required<JsonNode?>(o, {w}, Decoder.Any)"),
                    enc: format!("Encode.Put(o, {w}, Encode.Any({prop}));"),
                };
            }
            let inner = ct.non_null();
            let (dh, eh) = if inner.value() {
                ("RequiredNullableValue", "NullableValue")
            } else {
                ("RequiredNullable", "Nullable")
            };
            MemberCode {
                ty: format!("{}?", inner.base()),
                required: true,
                dec: format!("d.{dh}<{}>(o, {w}, {})", inner.base(), inner.dec(0)),
                enc: format!("Encode.{eh}(o, {w}, {prop}, x0 => {});", inner.enc("x0", 1)),
            }
        }
        Presence::Optional if !null_allowed => {
            let (dh, eh) = if ct.value() {
                ("OptionalValue", "OptionalValue")
            } else {
                ("Optional", "Optional")
            };
            MemberCode {
                ty: format!("{}?", ct.base()),
                required: false,
                dec: format!("d.{dh}<{}>(o, {w}, {})", ct.base(), ct.dec(0)),
                enc: format!("Encode.{eh}(o, {w}, {prop}, x0 => {});", ct.enc("x0", 1)),
            }
        }
        Presence::Optional | Presence::OptionalNullable => {
            if ct.kind == Ck::Any {
                return MemberCode {
                    ty: "Patch<JsonNode?>".into(),
                    required: false,
                    dec: format!("d.Patch<JsonNode?>(o, {w}, Decoder.Any)"),
                    enc: format!("Encode.Patch(o, {w}, {prop}, Encode.Any);"),
                };
            }
            let inner = ct.non_null();
            MemberCode {
                ty: format!("Patch<{}>", inner.base()),
                required: false,
                dec: format!("d.Patch<{}>(o, {w}, {})", inner.base(), inner.dec(0)),
                enc: format!(
                    "Encode.Patch(o, {w}, {prop}, x0 => {});",
                    inner.enc("x0", 1)
                ),
            }
        }
    }
}

/// The documentation text of an IR doc.
pub fn doc_text(doc: Option<&Doc>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    match (&doc.summary, &doc.description) {
        (Some(s), Some(d)) if d.trim().starts_with(s.trim()) => d.trim().to_string(),
        (Some(s), Some(d)) => format!("{}\n\n{}", s.trim(), d.trim()),
        (Some(s), None) => s.trim().to_string(),
        (None, Some(d)) => d.trim().to_string(),
        (None, None) => String::new(),
    }
}

impl Ctx<'_> {
    /// The C# namespace of a model namespace's types.
    pub fn model_ns(&self, ns: &str) -> String {
        if self.single_model_namespace {
            format!("{}.Models", self.opts.namespace)
        } else {
            let module = self
                .names
                .modules
                .get(ns)
                .cloned()
                .unwrap_or_else(|| "Shared".into());
            format!("{}.Models.{module}", self.opts.namespace)
        }
    }

    /// Whether a named type has a model file.
    pub fn has_class(&self, id: &TypeId) -> bool {
        let Some(t) = self.plan.types.get(id) else {
            return false;
        };
        match &t.kind {
            TypeKind::Record { .. } => true,
            TypeKind::Enum(e) => matches!(
                e,
                EnumPlan::Strings | EnumPlan::Integers | EnumPlan::Mixed { .. }
            ),
            TypeKind::Union(u) => !matches!(u, UnionPlan::Empty),
            TypeKind::AllOf(AllOfPlan::Merged { .. }) => true,
            TypeKind::AllOf(AllOfPlan::Intersection) => self.intersection_members(id) > 0,
            TypeKind::Alias => false,
        }
    }

    /// How many members the `allOf` named `id` has.
    fn intersection_members(&self, id: &TypeId) -> usize {
        match self.ir.types.get(id).map(|t| &t.shape) {
            Some(Shape::Intersection { members }) => members.len(),
            _ => 0,
        }
    }

    /// The fully qualified name of a model class (`from`, the namespace it is
    /// written from, does not shorten it).
    pub fn class_ref(&self, id: &TypeId, from: Option<&str>) -> String {
        let name = self.names.types.get(id).cloned().unwrap_or_default();
        let ns = self
            .plan
            .types
            .get(id)
            .map(|t| t.namespace.as_str())
            .unwrap_or_default();
        // Always qualified: a member may share its type's name (a `Status? Status`
        // property hides the type in static code), and so may a union variant.
        let _ = from;
        format!("global::{}.{name}", self.model_ns(ns))
    }

    /// The C# type of a type reference.
    pub fn ct(&self, ty: &TypeRef, from: Option<&str>) -> Ct {
        let mut active = BTreeSet::new();
        self.ct_in(ty, from, &mut active)
    }

    fn ct_in(&self, ty: &TypeRef, from: Option<&str>, active: &mut BTreeSet<TypeId>) -> Ct {
        match ty {
            TypeRef::Inline(shape) => self.shape_ct(shape, from, active),
            TypeRef::Named(id) => {
                let Some(t) = self.plan.types.get(id) else {
                    return Ct::any();
                };
                match &t.kind {
                    TypeKind::Alias => {
                        if !active.insert(id.clone()) {
                            return Ct::any();
                        }
                        let out = match self.ir.types.get(id) {
                            Some(named) => self.shape_ct(&named.shape, from, active),
                            None => Ct::any(),
                        };
                        active.remove(id);
                        out
                    }
                    TypeKind::Enum(
                        EnumPlan::Strings | EnumPlan::Integers | EnumPlan::Mixed { .. },
                    ) => Ct::of(Ck::Named {
                        name: self.class_ref(id, from),
                        value: true,
                    }),
                    _ if self.has_class(id) => Ct::of(Ck::Named {
                        name: self.class_ref(id, from),
                        value: false,
                    }),
                    _ => Ct::any(),
                }
            }
        }
    }

    fn shape_ct(&self, shape: &Shape, from: Option<&str>, active: &mut BTreeSet<TypeId>) -> Ct {
        match shape {
            Shape::Primitive { primitive, .. } => Ct::of(primitive_ck(primitive)),
            Shape::Enum { base, .. } => Ct::of(primitive_ck(base)),
            Shape::Const { value } => Ct::of(match value {
                serde_json::Value::String(_) => Ck::Str,
                serde_json::Value::Bool(_) => Ck::Bool,
                serde_json::Value::Number(n) if n.is_i64() => Ck::I64,
                serde_json::Value::Number(_) => Ck::F64,
                _ => Ck::Any,
            }),
            Shape::Array { items, .. } => {
                Ct::of(Ck::List(Box::new(self.ct_in(items, from, active))))
            }
            Shape::Map { values } => Ct::of(Ck::Map(Box::new(self.ct_in(values, from, active)))),
            Shape::Nullable { inner } => self.ct_in(inner, from, active).with_null(),
            Shape::Record { .. }
            | Shape::Union(_)
            | Shape::Intersection { .. }
            | Shape::Any
            | Shape::Never => Ct::any(),
        }
    }

    // -------------------------------------------------------------- models

    /// Every model file: (path, text).
    pub fn models(&self) -> Vec<(String, String)> {
        let mut out = vec![];
        for t in &self.ir.types.types {
            if !self.has_class(&t.id) {
                continue;
            }
            let Some(plan) = self.plan.types.get(&t.id) else {
                continue;
            };
            let name = self.names.types.get(&t.id).cloned().unwrap_or_default();
            let mut w = self.file_writer();
            w.line("using System.Collections.Generic;");
            w.line("using System.Text.Json.Nodes;");
            w.line("using Tungsten.Runtime;");
            w.blank();
            w.line(format!("namespace {};", self.model_ns(&t.namespace)));
            w.blank();
            match &plan.kind {
                TypeKind::Record { .. } => self.record_file(&mut w, t, &name),
                TypeKind::AllOf(AllOfPlan::Merged { fields, closed }) => {
                    self.merged_file(&mut w, t, &name, fields, *closed)
                }
                TypeKind::AllOf(AllOfPlan::Intersection) => {
                    self.intersection_file(&mut w, t, &name)
                }
                TypeKind::Enum(EnumPlan::Mixed { .. }) => self.mixed_enum_file(&mut w, t, &name),
                TypeKind::Enum(e) => self.enum_file(&mut w, t, &name, e),
                TypeKind::Union(u) => self.union_file(&mut w, t, &name, u),
                _ => {}
            }
            let dir = if self.single_model_namespace {
                "Models".to_string()
            } else {
                format!(
                    "Models/{}",
                    self.names
                        .modules
                        .get(&t.namespace)
                        .cloned()
                        .unwrap_or_else(|| "Shared".into())
                )
            };
            out.push((format!("{dir}/{name}.cs"), w.finish()));
        }
        out
    }

    /// The property names of a record's fields: the namer's, never the
    /// class's own name (C# forbids it).
    fn field_props(&self, id: &TypeId, class: &str, count: usize) -> Vec<String> {
        let names = self.names.fields.get(id).cloned().unwrap_or_default();
        let mut taken: Vec<String> = cs::RECORD_MEMBERS.iter().map(|s| s.to_string()).collect();
        taken.push(class.to_string());
        let mut out = vec![];
        for i in 0..count {
            let base = names.get(i).cloned().unwrap_or_else(|| format!("Field{i}"));
            let base = if base == class {
                format!("{base}Value")
            } else {
                base
            };
            out.push(cs::unique(base, &mut taken));
        }
        out
    }

    fn record_file(&self, w: &mut Writer, t: &NamedType, name: &str) {
        let Shape::Record { fields, additional } = &t.shape else {
            return;
        };
        let refs: Vec<&Field> = fields.iter().collect();
        let props = self.field_props(&t.id, name, refs.len());
        self.record_body(w, t, name, &refs, &props, additional);
    }

    fn merged_file(
        &self,
        w: &mut Writer,
        t: &NamedType,
        name: &str,
        merged: &[(usize, usize)],
        closed: bool,
    ) {
        let Shape::Intersection { members } = &t.shape else {
            return;
        };
        let mut refs: Vec<&Field> = vec![];
        for (m, f) in merged {
            if let Some(member) = members.get(*m)
                && let Some(Shape::Record { fields, .. }) = self.plan.resolve(member)
                && let Some(field) = fields.get(*f)
            {
                refs.push(field);
            }
        }
        let props = self.field_props(&t.id, name, refs.len());
        let additional = if closed {
            Additional::Closed
        } else {
            Additional::Open
        };
        self.record_body(w, t, name, &refs, &props, &additional);
    }

    fn record_body(
        &self,
        w: &mut Writer,
        t: &NamedType,
        name: &str,
        fields: &[&Field],
        props: &[String],
        additional: &Additional,
    ) {
        let from = Some(t.namespace.as_str());
        cs::summary(w, &doc_text(t.doc.as_ref()));
        let codes: Vec<MemberCode> = fields
            .iter()
            .zip(props)
            .map(|(f, p)| member_code(&self.ct(&f.ty, from), f.presence, p, &f.wire_name))
            .collect();
        let extra = match additional {
            Additional::Typed { values } => Some(self.ct(values, from)),
            _ => None,
        };
        let wires: Vec<String> = fields.iter().map(|f| lit(&f.wire_name)).collect();
        w.line(format!("public sealed record {name}"));
        w.line("{");
        w.indent();
        for ((f, p), code) in fields.iter().zip(props).zip(&codes) {
            let doc = doc_text(f.doc.as_ref());
            if doc.is_empty() {
                w.line(format!(
                    "/// <summary>The <c>{}</c> member.</summary>",
                    cs::xml(&f.wire_name)
                ));
            } else {
                cs::summary(w, &doc);
            }
            let required = if code.required { "required " } else { "" };
            w.line(format!("public {required}{} {p} {{ get; init; }}", code.ty));
            w.blank();
        }
        match additional {
            Additional::Open => {
                cs::summary_line(
                    w,
                    "Members the API description does not list, kept as they are.",
                );
                w.line("public JsonObject? AdditionalProperties { get; init; }");
                w.blank();
            }
            Additional::Typed { .. } => {
                if let Some(ct) = &extra {
                    cs::summary_line(w, "Members the API description does not list by name.");
                    w.line(format!(
                        "public IReadOnlyDictionary<string, {}>? AdditionalProperties {{ get; init; }}",
                        ct.ty()
                    ));
                    w.blank();
                }
            }
            Additional::Closed => {}
        }
        cs::summary_line(
            w,
            "Decodes a JSON value; strict decoding refuses members a closed type does not list.",
        );
        w.line(format!(
            "public static {name} FromJson(JsonNode? node, Decoder d)"
        ));
        w.line("{");
        w.indent();
        w.line("var o = d.Object(node);");
        let known = wires.join(", ");
        if matches!(additional, Additional::Closed) {
            if wires.is_empty() {
                w.line("d.Known(o);");
            } else {
                w.line(format!("d.Known(o, {known});"));
            }
        }
        w.line(format!("return new {name}"));
        w.line("{");
        w.indent();
        for (p, code) in props.iter().zip(&codes) {
            w.line(format!("{p} = {},", code.dec));
        }
        match additional {
            Additional::Open => {
                w.line(format!(
                    "AdditionalProperties = d.Extra(o{}),",
                    known_args(&known)
                ));
            }
            Additional::Typed { .. } => {
                if let Some(ct) = &extra {
                    w.line(format!(
                        "AdditionalProperties = d.ExtraTyped<{}>(o, {}{}),",
                        ct.ty(),
                        ct.dec(0),
                        known_args(&known)
                    ));
                }
            }
            Additional::Closed => {}
        }
        w.dedent();
        w.line("};");
        w.dedent();
        w.line("}");
        w.blank();
        cs::summary_line(
            w,
            "Decodes a JSON value, ignoring members the type does not list.",
        );
        w.line(format!(
            "public static {name} FromJson(JsonNode? node) => FromJson(node, Decoder.Lenient());"
        ));
        w.blank();
        cs::summary_line(w, "The value as JSON.");
        w.line("public JsonObject ToJson()");
        w.line("{");
        w.indent();
        w.line("var o = new JsonObject();");
        for code in &codes {
            w.line(&code.enc);
        }
        match additional {
            Additional::Open => w.line("Encode.Extra(o, AdditionalProperties);"),
            Additional::Typed { .. } => match &extra {
                Some(ct) => w.line(format!(
                    "Encode.ExtraTyped(o, AdditionalProperties, x0 => {});",
                    ct.enc("x0", 1)
                )),
                None => w,
            },
            Additional::Closed => w,
        };
        w.line("return o;");
        w.dedent();
        w.line("}");
        w.dedent();
        w.line("}");
    }

    fn enum_file(&self, w: &mut Writer, t: &NamedType, name: &str, plan: &EnumPlan) {
        let Shape::Enum { values, .. } = &t.shape else {
            return;
        };
        let strings = matches!(plan, EnumPlan::Strings);
        let (vty, dec, enc) = if strings {
            ("string", "Decoder.String", "Encode.String(Value)")
        } else {
            ("long", "Decoder.Int64", "Encode.Int64(Value)")
        };
        let variant_names = self
            .names
            .enum_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        let mut taken: Vec<String> = ["Value", "Values", "FromJson", "ToJson", "ToString", name]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let consts: Vec<(String, String)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let base = variant_names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("Value{i}"));
                let literal = match &v.value {
                    serde_json::Value::String(s) => lit(s),
                    other => format!("{other}L"),
                };
                (cs::unique(base, &mut taken), literal)
            })
            .collect();
        cs::summary(w, &doc_text(t.doc.as_ref()));
        w.line(format!("public readonly record struct {name}"));
        w.line("{");
        w.indent();
        w.line(format!("private {name}({vty} value)"));
        w.line("{");
        w.line("    Value = value;");
        w.line("}");
        w.blank();
        cs::summary_line(w, "The value on the wire.");
        w.line(format!("public {vty} Value {{ get; }}"));
        w.blank();
        for (c, literal) in &consts {
            w.line(format!(
                "public static readonly {name} {c} = new({literal});"
            ));
        }
        w.blank();
        cs::summary_line(w, "Every known value, in the API description's order.");
        let list = consts
            .iter()
            .map(|(c, _)| c.clone())
            .collect::<Vec<_>>()
            .join(", ");
        w.line(format!(
            "public static IReadOnlyList<{name}> Values {{ get; }} = new[] {{ {list} }};"
        ));
        w.blank();
        cs::summary_line(
            w,
            "Decodes a JSON value: one of the known values, else a failure.",
        );
        w.line(format!(
            "public static {name} FromJson(JsonNode? node, Decoder d)"
        ));
        w.line("{");
        w.indent();
        w.line(format!("var value = {dec}(node, d);"));
        w.line("return value switch");
        w.line("{");
        w.indent();
        for (c, literal) in &consts {
            w.line(format!("{literal} => {c},"));
        }
        let all = consts
            .iter()
            .map(|(c, _)| format!("{c}.ToJson()"))
            .collect::<Vec<_>>()
            .join(", ");
        w.line(format!(
            "_ => throw d.NotOneOf(node, new JsonNode?[] {{ {all} }}),"
        ));
        w.dedent();
        w.line("};");
        w.dedent();
        w.line("}");
        w.blank();
        w.line(format!(
            "public static {name} FromJson(JsonNode? node) => FromJson(node, Decoder.Lenient());"
        ));
        w.blank();
        cs::summary_line(w, "The value as JSON.");
        w.line(format!("public JsonNode ToJson() => {enc};"));
        w.blank();
        if strings {
            w.line("public override string ToString() => Value;");
        } else {
            w.line(
                "public override string ToString() => Value.ToString(System.Globalization.CultureInfo.InvariantCulture);",
            );
        }
        w.dedent();
        w.line("}");
    }

    /// An enum of values of different JSON types (or numbers that are not
    /// integers): a readonly record struct of the known values, each held as
    /// its JSON text and compared as JSON.
    fn mixed_enum_file(&self, w: &mut Writer, t: &NamedType, name: &str) {
        let Shape::Enum { values, .. } = &t.shape else {
            return;
        };
        let variant_names = self
            .names
            .enum_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        let mut taken: Vec<String> = [
            "Value", "Values", "FromJson", "ToJson", "ToString", "Json", "Typed", "JsonNode",
            "Decoder", "text", name,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let consts: Vec<(String, String)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let base = variant_names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("Value{i}"));
                let text = serde_json::to_string(&v.value).unwrap_or_else(|_| "null".into());
                (cs::unique(base, &mut taken), lit(&text))
            })
            .collect();
        cs::summary(w, &doc_text(t.doc.as_ref()));
        w.line(format!("public readonly record struct {name}"));
        w.line("{");
        w.indent();
        w.line("private readonly string? text;");
        w.blank();
        w.line(format!("private {name}(string text)"));
        w.line("{");
        w.line("    this.text = text;");
        w.line("}");
        w.blank();
        cs::summary_line(
            w,
            "The value on the wire, as a JSON value (a copy; JSON null is null).",
        );
        w.line("public JsonNode? Value => Typed.Parse(text ?? \"null\");");
        w.blank();
        for ((c, literal), v) in consts.iter().zip(values) {
            let doc = v
                .doc
                .as_ref()
                .map(|d| doc_text(Some(d)))
                .unwrap_or_default();
            if doc.is_empty() {
                cs::summary_line(w, &format!("The value <c>{}</c>.", cs::xml(literal)));
            } else {
                cs::summary(w, &doc);
            }
            w.line(format!(
                "public static readonly {name} {c} = new({literal});"
            ));
        }
        w.blank();
        cs::summary_line(w, "Every known value, in the API description's order.");
        let list = consts
            .iter()
            .map(|(c, _)| c.clone())
            .collect::<Vec<_>>()
            .join(", ");
        w.line(format!(
            "public static IReadOnlyList<{name}> Values {{ get; }} = new[] {{ {list} }};"
        ));
        w.blank();
        cs::summary_line(
            w,
            "Decodes a JSON value: one of the known values (compared as JSON), else a failure.",
        );
        w.line(format!(
            "public static {name} FromJson(JsonNode? node, Decoder d)"
        ));
        w.line("{");
        w.indent();
        for (c, _) in &consts {
            w.line(format!("if (Json.Equal(node, {c}.Value))"));
            w.line("{");
            w.line(format!("    return {c};"));
            w.line("}");
            w.blank();
        }
        let all = consts
            .iter()
            .map(|(c, _)| format!("{c}.Value"))
            .collect::<Vec<_>>()
            .join(", ");
        w.line(format!(
            "throw d.NotOneOf(node, new JsonNode?[] {{ {all} }});"
        ));
        w.dedent();
        w.line("}");
        w.blank();
        w.line(format!(
            "public static {name} FromJson(JsonNode? node) => FromJson(node, Decoder.Lenient());"
        ));
        w.blank();
        cs::summary_line(w, "The value as JSON.");
        w.line("public JsonNode? ToJson() => Value;");
        w.blank();
        w.line("public override string ToString() => text ?? \"null\";");
        w.dedent();
        w.line("}");
    }

    /// An `allOf` that cannot be merged into one record: a record with one
    /// typed part per member. Every part is decoded from the whole value
    /// (so a member that is a union, a record with typed extra members or
    /// any other type works), and the parts are encoded back into one value
    /// (objects are merged member by member).
    fn intersection_file(&self, w: &mut Writer, t: &NamedType, name: &str) {
        let Shape::Intersection { members } = &t.shape else {
            return;
        };
        let from = Some(t.namespace.as_str());
        let mut taken: Vec<String> = cs::RECORD_MEMBERS.iter().map(|s| s.to_string()).collect();
        taken.push(name.to_string());
        taken.extend(
            ["Merge", "Decoder", "JsonNode", "JsonObject"]
                .iter()
                .map(|s| s.to_string()),
        );
        let parts: Vec<(String, Ct, String)> = members
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let (base, label) = match m {
                    TypeRef::Named(id) => {
                        let class = self.names.types.get(id).cloned().unwrap_or_default();
                        (
                            class.clone(),
                            format!("the member <c>{}</c>", cs::xml(&id.0)),
                        )
                    }
                    TypeRef::Inline(_) => (
                        format!("Part{}", i + 1),
                        format!("the member number {}", i + 1),
                    ),
                };
                let base = if base.is_empty() {
                    format!("Part{}", i + 1)
                } else {
                    base
                };
                (cs::unique(base, &mut taken), self.ct(m, from), label)
            })
            .collect();
        cs::summary(w, &doc_text(t.doc.as_ref()));
        w.line(format!("public sealed record {name}"));
        w.line("{");
        w.indent();
        for (p, ct, label) in &parts {
            cs::summary_line(w, &format!("The value as {label}."));
            w.line(format!("public required {} {p} {{ get; init; }}", ct.ty()));
            w.blank();
        }
        cs::summary_line(
            w,
            "Decodes a JSON value: each part decodes the whole value; strict decoding refuses members a closed part does not list.",
        );
        w.line(format!(
            "public static {name} FromJson(JsonNode? node, Decoder d)"
        ));
        w.line("{");
        w.indent();
        w.line(format!("return new {name}"));
        w.line("{");
        w.indent();
        for (p, ct, _) in &parts {
            w.line(format!("{p} = {},", ct.dec_call("node", "d", 0)));
        }
        w.dedent();
        w.line("};");
        w.dedent();
        w.line("}");
        w.blank();
        cs::summary_line(
            w,
            "Decodes a JSON value, ignoring members the parts do not list.",
        );
        w.line(format!(
            "public static {name} FromJson(JsonNode? node) => FromJson(node, Decoder.Lenient());"
        ));
        w.blank();
        cs::summary_line(w, "The value as JSON: the parts, merged.");
        w.line("public JsonNode? ToJson()");
        w.line("{");
        w.indent();
        w.line("JsonNode? merged = null;");
        for (p, ct, _) in &parts {
            w.line(format!("merged = Merge(merged, {});", ct.enc(p, 0)));
        }
        w.line("return merged;");
        w.dedent();
        w.line("}");
        w.blank();
        w.line("private static JsonNode? Merge(JsonNode? left, JsonNode? right)");
        w.line("{");
        w.indent();
        w.line("if (left is JsonObject l && right is JsonObject r)");
        w.line("{");
        w.indent();
        w.line("var merged = new JsonObject();");
        w.line("foreach (var member in l)");
        w.line("{");
        w.line("    merged[member.Key] = member.Value?.DeepClone();");
        w.line("}");
        w.blank();
        w.line("foreach (var member in r)");
        w.line("{");
        w.indent();
        w.line("merged[member.Key] = merged.TryGetPropertyValue(member.Key, out var known)");
        w.line("    ? Merge(known, member.Value)");
        w.line("    : member.Value?.DeepClone();");
        w.dedent();
        w.line("}");
        w.blank();
        w.line("return merged;");
        w.dedent();
        w.line("}");
        w.blank();
        w.line("return left?.DeepClone() ?? right?.DeepClone();");
        w.dedent();
        w.line("}");
        w.dedent();
        w.line("}");
    }

    fn union_file(&self, w: &mut Writer, t: &NamedType, name: &str, plan: &UnionPlan) {
        let Shape::Union(u) = &t.shape else {
            return;
        };
        // Fully qualified: a variant record may share its model's name.
        let from = None;
        let variant_names = self
            .names
            .union_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        let mut taken: Vec<String> = ["FromJson", "ToJson", "ToString", "Equals", name]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let variants: Vec<(String, Ct)> = u
            .variants
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let base = variant_names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("Variant{i}"));
                (cs::unique(base, &mut taken), self.ct(&v.ty, from))
            })
            .collect();
        cs::summary(w, &doc_text(t.doc.as_ref()));
        w.line(format!("public abstract record {name}"));
        w.line("{");
        w.indent();
        w.line(format!("private protected {name}()"));
        w.line("{");
        w.line("}");
        w.blank();
        for (vname, ct) in variants.iter() {
            cs::summary_line(w, &format!("The variant {vname}."));
            w.line(format!(
                "public sealed record {vname}({} Value) : {name}",
                ct.ty()
            ));
            w.line("{");
            w.line(format!(
                "    public override JsonNode? ToJson() => {};",
                ct.enc("Value", 0)
            ));
            w.line("}");
            w.blank();
        }
        cs::summary_line(w, "The value as JSON.");
        w.line("public abstract JsonNode? ToJson();");
        w.blank();
        cs::summary_line(w, "Decodes a JSON value into its variant.");
        w.line(format!(
            "public static {name} FromJson(JsonNode? node, Decoder d)"
        ));
        w.line("{");
        w.indent();
        match plan {
            UnionPlan::Tagged {
                tag,
                variants: tagged,
            } => {
                w.line("var o = d.Object(node);");
                w.line(format!("var tag = d.Tag(o, {});", lit(tag)));
                w.line("return tag switch");
                w.line("{");
                w.indent();
                let mut all = vec![];
                for (index, tags) in tagged {
                    let Some((vname, ct)) = variants.get(*index) else {
                        continue;
                    };
                    for value in tags {
                        all.push(lit(value));
                        w.line(format!(
                            "{} => new {vname}({}),",
                            lit(value),
                            ct.dec_call("node", "d", 0)
                        ));
                    }
                }
                let mut args = vec![lit(tag), "tag".into()];
                args.extend(all);
                w.line(format!("_ => throw d.UnknownTag({}),", args.join(", ")));
                w.dedent();
                w.line("};");
            }
            UnionPlan::LiteralTagged { variants: keyed } => {
                for (index, key) in keyed {
                    let Some((vname, ct)) = variants.get(*index) else {
                        continue;
                    };
                    let test = match key {
                        LiteralKey::Value(v) => format!(
                            "Json.Equal(node, Typed.Parse({}))",
                            lit(&serde_json::to_string(v).unwrap_or_else(|_| "null".into()))
                        ),
                        LiteralKey::JsonType("any") => "true".to_string(),
                        LiteralKey::JsonType(kind) => {
                            format!("Json.KindOf(node) == {}", json_kind(kind))
                        }
                    };
                    w.line(format!("if ({test})"));
                    w.line("{");
                    w.line(format!(
                        "    return new {vname}({});",
                        ct.dec_call("node", "d", 0)
                    ));
                    w.line("}");
                }
                w.line(format!(
                    "return d.FirstOf<{name}>(node, {}, {});",
                    lit(name),
                    variant_trials(&variants, keyed.iter().map(|(i, _)| *i))
                ));
            }
            UnionPlan::Untagged { ordered } => {
                w.line(format!(
                    "return d.FirstOf<{name}>(node, {}, {});",
                    lit(name),
                    variant_trials(&variants, ordered.iter().copied())
                ));
            }
            UnionPlan::Empty => {
                w.line(format!("return d.FirstOf<{name}>(node, {});", lit(name)));
            }
        }
        w.dedent();
        w.line("}");
        w.blank();
        w.line(format!(
            "public static {name} FromJson(JsonNode? node) => FromJson(node, Decoder.Lenient());"
        ));
        w.dedent();
        w.line("}");
    }
}

/// `, "a", "b"` for a list of known members (or nothing).
fn known_args(known: &str) -> String {
    if known.is_empty() {
        String::new()
    } else {
        format!(", {known}")
    }
}

/// The trial decoders of the variants at these indexes, for `FirstOf`.
fn variant_trials(variants: &[(String, Ct)], order: impl Iterator<Item = usize>) -> String {
    order
        .filter_map(|i| variants.get(i))
        .map(|(vname, ct)| format!("(n0, d0) => new {vname}({})", ct.dec_call("n0", "d0", 1)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn json_kind(kind: &str) -> &'static str {
    match kind {
        "string" => "JsonKind.String",
        "integer" => "JsonKind.Integer",
        "number" => "JsonKind.Number",
        "boolean" => "JsonKind.Boolean",
        "null" => "JsonKind.Null",
        "array" => "JsonKind.Array",
        _ => "JsonKind.Object",
    }
}

fn primitive_ck(p: &Primitive) -> Ck {
    match p {
        Primitive::String { .. } => Ck::Str,
        Primitive::Bytes => Ck::Bytes,
        Primitive::Int32 => Ck::I32,
        Primitive::Int64 | Primitive::Integer => Ck::I64,
        Primitive::Float | Primitive::Double | Primitive::Number => Ck::F64,
        Primitive::Bool => Ck::Bool,
    }
}
