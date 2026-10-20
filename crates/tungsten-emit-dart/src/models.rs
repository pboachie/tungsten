// SPDX-License-Identifier: AGPL-3.0-only
//! Dart types of IR types and the model libraries
//! (`lib/src/models/<namespace>.dart`), plus `lib/src/wire.dart`, the
//! decoders and encoders they share.
//!
//! - A record (and an `allOf` merged into one) is a `final class` with a
//!   `const` constructor of named parameters, `fromJson(json, [path])` and
//!   `toJson()`. Presence has four states: required `T`, required nullable
//!   `T?` (null is sent), optional `T?` (null is not sent) and optional
//!   nullable `Patch<T>` (absent, null or a value). Members the schema does
//!   not declare are kept in `additionalProperties` unless the record is
//!   closed. Equality and `hashCode` compare the JSON forms.
//! - An enum is an enhanced enum with its wire value (`wire`; `Object?` for
//!   an enum of mixed JSON types, whose values are compared as JSON);
//!   decoding refuses a value outside the enum.
//! - An `allOf` that cannot be merged into one record is a `final class`
//!   with one typed part per member: each part decodes the whole value, and
//!   `toJson()` merges the parts' JSON.
//! - A union is a sealed class with one final subclass per variant
//!   (`<Union><Variant>`, holding `value`): decoded by the tag of a
//!   discriminated union, by the value check of a literal-tagged one, else
//!   by the first variant (in plan order) that decodes.
//! - Any other named type (an alias, an `allOf` without members, an enum
//!   without values or with an integer beyond 64 bits, a union without
//!   variants) has no class: references use the Dart type of its shape, or
//!   the JSON value (`Object?`).
//!
//! Decoding failures are `FormatException`s whose message starts with the
//! JSON path of the value (`$.tags[2].name: expected a string`).

use std::collections::BTreeSet;

use tungsten_emit::sdk::{AllOfPlan, EnumPlan, LiteralKey, TypeKind, UnionPlan};
use tungsten_ir::naming::{self, Role, Target};
use tungsten_ir::{
    Additional, EnumValue, Field, Ident, NamedType, Presence, Primitive, Shape, StringFormat,
    TypeId, TypeRef,
};

use crate::dart::{Code, Expr, const_lit, contains_word, directive, json_lit, str_lit};
use crate::options::RUNTIME_PACKAGE;
use crate::{Gen, Library};

/// The Dart representation of a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dt {
    Str,
    Int,
    Double,
    Bool,
    /// `Uint8List`; `base64`: the JSON form is base64 text (`format:
    /// byte`), else the bytes themselves (`format: binary`).
    Bytes {
        base64: bool,
    },
    /// The JSON value, `Object?`.
    Json,
    /// A JSON object, `Map<String, Object?>`.
    Object,
    /// A model class.
    Class(TypeId),
    List(Box<Dt>),
    Map(Box<Dt>),
    Nullable(Box<Dt>),
}

impl Dt {
    /// Whether the Dart type admits null.
    pub fn nullable(&self) -> bool {
        matches!(self, Dt::Json | Dt::Nullable(_))
    }
}

impl Gen<'_, '_> {
    /// Whether a named type has a class of its own.
    pub fn has_class(&self, id: &TypeId) -> bool {
        let Some(t) = self.plan.types.get(id) else {
            return false;
        };
        match &t.kind {
            TypeKind::Record { .. } => true,
            TypeKind::Enum(EnumPlan::Strings | EnumPlan::Integers) => true,
            TypeKind::Enum(EnumPlan::Mixed { .. }) => matches!(
                self.plan.ir.types.get(id).map(|t| &t.shape),
                Some(Shape::Enum { values, .. }) if representable(values)
            ),
            TypeKind::Enum(_) => false,
            TypeKind::Union(u) => !matches!(u, UnionPlan::Empty),
            TypeKind::AllOf(AllOfPlan::Merged { .. }) => true,
            TypeKind::AllOf(AllOfPlan::Intersection) => matches!(
                self.plan.ir.types.get(id).map(|t| &t.shape),
                Some(Shape::Intersection { members }) if !members.is_empty()
            ),
            TypeKind::Alias => false,
        }
    }

    /// The class name of a named type in a library: bare in its own model
    /// library (`from` is its namespace), else with the namespace's import
    /// prefix (`$public.Pet`).
    pub fn class_ref(&self, id: &TypeId, from: Option<&str>) -> String {
        let name = self
            .names
            .types
            .get(id)
            .cloned()
            .unwrap_or_else(|| "Object".into());
        let ns = self
            .plan
            .types
            .get(id)
            .map(|t| t.namespace.as_str())
            .unwrap_or("");
        if from == Some(ns) {
            name
        } else {
            format!("{}.{name}", self.ns_prefix(ns))
        }
    }

    /// The Dart representation of a type reference.
    pub fn dt(&self, ty: &TypeRef) -> Dt {
        self.dt_depth(ty, 0)
    }

    fn dt_depth(&self, ty: &TypeRef, depth: usize) -> Dt {
        if depth > 32 {
            return Dt::Json;
        }
        match ty {
            TypeRef::Named(id) => {
                if self.has_class(id) {
                    return Dt::Class(id.clone());
                }
                match self.plan.ir.types.get(id) {
                    Some(t) if !self.plan.types.get(id).is_some_and(|p| p.cyclic) => {
                        self.shape_dt(&t.shape, depth + 1)
                    }
                    _ => Dt::Json,
                }
            }
            TypeRef::Inline(shape) => self.shape_dt(shape, depth + 1),
        }
    }

    fn shape_dt(&self, shape: &Shape, depth: usize) -> Dt {
        match shape {
            Shape::Primitive { primitive, .. } => primitive_dt(primitive),
            Shape::Enum { values, .. } => {
                if !values.is_empty() && values.iter().all(|v| v.value.is_string()) {
                    Dt::Str
                } else if !values.is_empty() && values.iter().all(|v| v.value.is_i64()) {
                    Dt::Int
                } else {
                    Dt::Json
                }
            }
            Shape::Const { value } => match value {
                serde_json::Value::String(_) => Dt::Str,
                serde_json::Value::Bool(_) => Dt::Bool,
                serde_json::Value::Number(n) if n.is_i64() => Dt::Int,
                serde_json::Value::Number(_) => Dt::Double,
                _ => Dt::Json,
            },
            Shape::Array { items, .. } => Dt::List(Box::new(self.dt_depth(items, depth))),
            Shape::Map { values } => Dt::Map(Box::new(self.dt_depth(values, depth))),
            Shape::Nullable { inner } => match self.dt_depth(inner, depth) {
                Dt::Json => Dt::Json,
                d @ Dt::Nullable(_) => d,
                d => Dt::Nullable(Box::new(d)),
            },
            Shape::Record { fields, additional } => match additional {
                Additional::Typed { values } if fields.is_empty() => {
                    Dt::Map(Box::new(self.dt_depth(values, depth)))
                }
                _ => Dt::Object,
            },
            Shape::Union(_) | Shape::Intersection { .. } | Shape::Any | Shape::Never => Dt::Json,
        }
    }

    /// The Dart type of a representation, written in the library of
    /// namespace `from` (`None`: outside the model libraries).
    pub fn dart_type(&self, dt: &Dt, from: Option<&str>) -> String {
        match dt {
            Dt::Str => "String".into(),
            Dt::Int => "int".into(),
            Dt::Double => "double".into(),
            Dt::Bool => "bool".into(),
            Dt::Bytes { .. } => "Uint8List".into(),
            Dt::Json => "Object?".into(),
            Dt::Object => "Map<String, Object?>".into(),
            Dt::Class(id) => self.class_ref(id, from),
            Dt::List(i) => format!("List<{}>", self.dart_type(i, from)),
            Dt::Map(i) => format!("Map<String, {}>", self.dart_type(i, from)),
            Dt::Nullable(i) => {
                let inner = self.dart_type(i, from);
                if i.nullable() {
                    inner
                } else {
                    format!("{inner}?")
                }
            }
        }
    }

    /// A `Decoder<T>` (a `T Function(Object? json, String path)`) for a
    /// representation.
    pub fn decoder(&self, dt: &Dt, from: Option<&str>) -> Expr {
        match dt {
            Dt::Str => Expr::raw("Wire.string"),
            Dt::Int => Expr::raw("Wire.integer"),
            Dt::Double => Expr::raw("Wire.number"),
            Dt::Bool => Expr::raw("Wire.boolean"),
            Dt::Bytes { .. } => Expr::raw("Wire.bytes"),
            Dt::Json => Expr::raw("Wire.any"),
            Dt::Object => Expr::raw("Wire.object"),
            Dt::Class(id) => Expr::raw(format!("{}.fromJson", self.class_ref(id, from))),
            Dt::List(i) => Expr::call("Wire.listOf", vec![self.decoder(i, from)]),
            Dt::Map(i) => Expr::call("Wire.mapOf", vec![self.decoder(i, from)]),
            Dt::Nullable(i) => Expr::call("Wire.nullableOf", vec![self.decoder(i, from)]),
        }
    }

    /// An `Encoder<T>` (a `Object? Function(T value)`) for a
    /// representation; `None` when the Dart value is its JSON value.
    pub fn encoder(&self, dt: &Dt) -> Option<Expr> {
        match dt {
            Dt::Bytes { base64: true } => Some(Expr::raw("Wire.base64Text")),
            Dt::Class(_) => Some(Expr::raw("Wire.encodable")),
            Dt::List(i) => self.encoder(i).map(|e| Expr::call("Wire.listOut", vec![e])),
            Dt::Map(i) => self.encoder(i).map(|e| Expr::call("Wire.mapOut", vec![e])),
            Dt::Nullable(i) => self
                .encoder(i)
                .map(|e| Expr::call("Wire.nullableOut", vec![e])),
            _ => None,
        }
    }

    /// The expression that decodes `json` at `path` as `dt`.
    pub fn decode_value(&self, dt: &Dt, from: Option<&str>, json: &str, path: &str) -> Expr {
        match self.decoder(dt, from) {
            Expr::Raw(f) => Expr::call(f, vec![Expr::raw(json), Expr::raw(path)]),
            other => Expr::call("Wire.apply", vec![Expr::raw(json), Expr::raw(path), other]),
        }
    }

    /// The expression that encodes `value` (a non-null Dart value of `dt`).
    pub fn encode_value(&self, dt: &Dt, value: &str) -> Expr {
        match self.encoder(dt) {
            None => Expr::raw(value),
            Some(Expr::Raw(f)) if f == "Wire.encodable" => Expr::raw(format!("{value}.toJson()")),
            Some(e) => Expr::call("Wire.encode", vec![Expr::raw(value), e]),
        }
    }
}

/// Whether every value of an enum is a Dart constant that keeps its JSON
/// value: a Dart `int` is 64 bits, so an integer beyond `i64` cannot be one
/// (as a `double` it would be sent as another number).
pub(crate) fn representable(values: &[EnumValue]) -> bool {
    fn ok(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Number(n) => n.is_i64() || (!n.is_u64() && n.as_f64().is_some()),
            serde_json::Value::Array(items) => items.iter().all(ok),
            serde_json::Value::Object(members) => members.values().all(ok),
            _ => true,
        }
    }
    values.iter().all(|v| ok(&v.value))
}

/// The Dart representation of a primitive.
fn primitive_dt(p: &Primitive) -> Dt {
    match p {
        Primitive::String {
            format: Some(StringFormat::Byte),
        } => Dt::Bytes { base64: true },
        Primitive::String { .. } => Dt::Str,
        Primitive::Int32 | Primitive::Int64 | Primitive::Integer => Dt::Int,
        Primitive::Float | Primitive::Double | Primitive::Number => Dt::Double,
        Primitive::Bool => Dt::Bool,
        Primitive::Bytes => Dt::Bytes { base64: false },
    }
}

/// One member of a generated record.
struct Comp {
    name: String,
    wire: String,
    presence: Presence,
    dt: Dt,
    doc: String,
}

/// Every model library and the wire library.
pub(crate) fn libraries(g: &Gen<'_, '_>) -> Vec<Library> {
    let mut out = vec![Library {
        path: "src/wire.dart".into(),
        text: wire(g),
        exports: None,
    }];
    for ns in &g.plan.namespaces {
        let ids: Vec<&TypeId> = ns.types.iter().filter(|id| g.has_class(id)).collect();
        if ids.is_empty() {
            continue;
        }
        let taken: BTreeSet<String> = g
            .plan
            .types
            .values()
            .filter(|t| t.namespace == ns.name)
            .filter_map(|t| g.names.types.get(&t.id).cloned())
            .chain(crate::TAKEN_TYPES.iter().map(|s| s.to_string()))
            .collect();
        let mut file = ModelFile {
            g,
            ns: &ns.name,
            taken,
            declared: vec![],
        };
        let mut body = Code::new();
        for id in ids {
            let Some(t) = g.plan.ir.types.get(id) else {
                continue;
            };
            body.blank();
            file.named_type(&mut body, t);
        }
        let body = body.finish();
        let text = file.finish(&body);
        out.push(Library {
            path: g.ns_path(&ns.name),
            text,
            exports: Some(file.declared),
        });
    }
    out
}

struct ModelFile<'f, 'p, 'a> {
    g: &'f Gen<'p, 'a>,
    ns: &'f str,
    /// Names of the library's scope: its types and the imported names.
    taken: BTreeSet<String>,
    /// The classes and enums it declares, in order.
    declared: Vec<String>,
}

fn type_doc(t: &NamedType) -> String {
    let mut text = String::new();
    if let Some(doc) = &t.doc {
        if let Some(s) = doc
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            text.push_str(s);
        }
        if let Some(d) = doc
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            && Some(d) != doc.summary.as_deref().map(str::trim)
        {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(d);
        }
    }
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(&format!("Wire type `{}`.", t.id.0));
    text
}

fn field_doc(f: &Field) -> String {
    let mut text = f
        .doc
        .as_ref()
        .and_then(|d| d.description.clone().or_else(|| d.summary.clone()))
        .unwrap_or_default()
        .trim()
        .to_string();
    let presence = match f.presence {
        Presence::Required => "required",
        Presence::RequiredNullable => "required, may be null",
        Presence::Optional => "optional (null: not sent)",
        Presence::OptionalNullable => "optional, may be null",
    };
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(&format!("Wire member `{}`, {presence}.", f.wire_name));
    if f.deprecated {
        text.push_str(" Deprecated.");
    }
    if f.read_only {
        text.push_str(" Read-only.");
    }
    if f.write_only {
        text.push_str(" Write-only.");
    }
    text
}

impl ModelFile<'_, '_, '_> {
    fn from(&self) -> Option<&str> {
        Some(self.ns)
    }

    /// The library's text: header, imports, body.
    fn finish(&self, body: &str) -> String {
        let g = self.g;
        // Imports follow the code, not the comments.
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let body_code = code.as_str();
        let mut out = String::new();
        out.push_str(&g.header);
        out.push_str("\n\n");
        if contains_word(body_code, "Uint8List") {
            out.push_str(&directive(
                "import",
                "dart:typed_data",
                "show",
                &["Uint8List".to_string()],
            ));
            out.push_str("\n\n");
        }
        let runtime: Vec<String> = ["JsonEncodable", "Patch"]
            .into_iter()
            .filter(|n| contains_word(body_code, n))
            .map(String::from)
            .collect();
        if !runtime.is_empty() {
            out.push_str(&directive(
                "import",
                &format!("package:{RUNTIME_PACKAGE}/{RUNTIME_PACKAGE}.dart"),
                "show",
                &runtime,
            ));
            out.push_str("\n\n");
        }
        out.push_str(&directive("import", "../wire.dart", "", &[]));
        out.push('\n');
        for other in &g.plan.namespaces {
            if other.name == self.ns {
                continue;
            }
            let prefix = g.ns_prefix(&other.name);
            if body_code.contains(&format!("{prefix}.")) {
                out.push_str(&format!(
                    "{} as {prefix};\n",
                    directive(
                        "import",
                        &format!("{}.dart", g.ns_module(&other.name)),
                        "",
                        &[]
                    )
                    .trim_end_matches(';')
                ));
            }
        }
        out.push('\n');
        out.push_str(body.trim_start_matches('\n'));
        out
    }

    fn named_type(&mut self, w: &mut Code, t: &NamedType) {
        let g = self.g;
        let Some(plan) = g.plan.types.get(&t.id) else {
            return;
        };
        let name = g.names.types.get(&t.id).cloned().unwrap_or_default();
        let doc = type_doc(t);
        match (&plan.kind, &t.shape) {
            (TypeKind::Record { .. }, Shape::Record { fields, additional }) => {
                let own: Vec<&Field> = fields.iter().collect();
                let extra = match additional {
                    Additional::Closed => None,
                    Additional::Open => Some(Dt::Json),
                    Additional::Typed { values } => Some(g.dt(values)),
                };
                let comps = self.comps(&t.id, &own);
                self.record(w, &name, &doc, &comps, extra.as_ref());
            }
            (
                TypeKind::AllOf(AllOfPlan::Merged { fields, closed }),
                Shape::Intersection { members },
            ) => {
                let own: Vec<&Field> = fields
                    .iter()
                    .filter_map(|&(m, f)| match g.plan.resolve(members.get(m)?) {
                        Some(Shape::Record { fields, .. }) => fields.get(f),
                        _ => None,
                    })
                    .collect();
                let comps = self.comps(&t.id, &own);
                let extra = if *closed { None } else { Some(Dt::Json) };
                self.record(w, &name, &doc, &comps, extra.as_ref());
            }
            (TypeKind::AllOf(AllOfPlan::Intersection), Shape::Intersection { members }) => {
                self.intersection(w, &name, &doc, members);
            }
            (TypeKind::Enum(e), Shape::Enum { values, .. }) => {
                self.enumeration(w, t, &name, &doc, e, values);
            }
            (TypeKind::Union(u), Shape::Union(union)) => {
                self.union(w, t, &name, &doc, u, union);
            }
            _ => {}
        }
    }

    fn comps(&self, id: &TypeId, fields: &[&Field]) -> Vec<Comp> {
        let names = self.g.names.fields.get(id).cloned().unwrap_or_default();
        fields
            .iter()
            .enumerate()
            .map(|(i, f)| Comp {
                name: names.get(i).cloned().unwrap_or_else(|| format!("field{i}")),
                wire: f.wire_name.clone(),
                presence: f.presence,
                dt: self.g.dt(&f.ty),
                doc: field_doc(f),
            })
            .collect()
    }

    fn field_type(&self, c: &Comp) -> String {
        let t = self.g.dart_type(&c.dt, self.from());
        match c.presence {
            Presence::Required => t,
            Presence::RequiredNullable | Presence::Optional => {
                if c.dt.nullable() {
                    t
                } else {
                    format!("{t}?")
                }
            }
            Presence::OptionalNullable => format!("Patch<{t}>"),
        }
    }

    fn record(&mut self, w: &mut Code, name: &str, doc: &str, comps: &[Comp], extra: Option<&Dt>) {
        let g = self.g;
        let from: Option<&str> = Some(self.ns);
        self.declared.push(name.to_string());
        w.doc(doc);
        w.class_header(&format!("final class {name}"), "implements JsonEncodable");
        w.indent();
        // Constructor.
        w.doc("Builds the value from its members.");
        let mut params: Vec<String> = comps
            .iter()
            .map(|c| match c.presence {
                Presence::Required | Presence::RequiredNullable => {
                    format!("required this.{}", c.name)
                }
                Presence::Optional => format!("this.{}", c.name),
                Presence::OptionalNullable => format!("this.{} = const Patch.absent()", c.name),
            })
            .collect();
        if extra.is_some() {
            params.push("this.additionalProperties = const {}".into());
        }
        if params.is_empty() {
            w.line(format!("const {name}();"));
        } else {
            w.line(format!("const {name}({{"));
            w.indent();
            for p in &params {
                w.line(format!("{p},"));
            }
            w.dedent();
            w.line("});");
        }
        // Decoder.
        w.blank();
        w.doc(&format!(
            "Decodes a `{name}` from its JSON object; unknown members are {}. Throws a `FormatException` naming the JSON path of the first value that does not fit.",
            if extra.is_some() {
                "kept in `additionalProperties`"
            } else {
                "ignored"
            }
        ));
        factory_header(w, name);
        w.indent();
        let uses_object = !comps.is_empty() || extra.is_some();
        if uses_object {
            w.line("final o = Wire.object(json, path);");
        } else {
            w.line("Wire.object(json, path);");
        }
        let mut args: Vec<Expr> = comps
            .iter()
            .map(|c| {
                let mut helper = match c.presence {
                    Presence::Required => "Wire.required",
                    Presence::RequiredNullable => "Wire.requiredNullable",
                    Presence::Optional => "Wire.optional",
                    Presence::OptionalNullable => "Wire.patch",
                }
                .to_string();
                // `T?` of a nullable `T` is `T`: inference would drop the
                // `?` from the decoder's type, so the type is explicit.
                if c.dt.nullable()
                    && matches!(c.presence, Presence::RequiredNullable | Presence::Optional)
                {
                    helper.push_str(&format!("<{}>", g.dart_type(&c.dt, from)));
                }
                Expr::named(
                    c.name.clone(),
                    Expr::call(
                        helper,
                        vec![
                            Expr::raw("o"),
                            Expr::raw(str_lit(&c.wire)),
                            Expr::raw("path"),
                            g.decoder(&c.dt, from),
                        ],
                    ),
                )
            })
            .collect();
        if let Some(e) = extra {
            args.push(Expr::named(
                "additionalProperties",
                Expr::call(
                    "Wire.extra",
                    vec![
                        Expr::raw("o"),
                        Expr::raw("_known"),
                        Expr::raw("path"),
                        g.decoder(e, from),
                    ],
                ),
            ));
        }
        if args.is_empty() {
            w.line(format!("return const {name}();"));
        } else {
            // A trailing comma keeps one member per line.
            w.line(format!("return {name}("));
            w.indent();
            for a in &args {
                w.expr("", a, ",");
            }
            w.dedent();
            w.line(");");
        }
        w.dedent();
        w.line("}");
        if extra.is_some() {
            w.blank();
            let keys: Vec<String> = comps.iter().map(|c| str_lit(&c.wire)).collect();
            let flat = format!("static const Set<String> _known = {{{}}};", keys.join(", "));
            if keys.is_empty() {
                w.line("static const Set<String> _known = {};");
            } else if w.fits(&flat) {
                w.line(flat);
            } else {
                w.line("static const Set<String> _known = {");
                w.indent();
                for k in &keys {
                    w.line(format!("{k},"));
                }
                w.dedent();
                w.line("};");
            }
        }
        // Members.
        for c in comps {
            w.blank();
            w.doc(&c.doc);
            field_line(w, &self.field_type(c), &c.name);
        }
        if let Some(e) = extra {
            w.blank();
            w.doc("The members the schema does not declare, by wire name.");
            field_line(
                w,
                &format!("Map<String, {}>", g.dart_type(e, from)),
                "additionalProperties",
            );
        }
        // Encoder.
        w.blank();
        w.doc("The JSON object: members by wire name; an optional member that is null or absent is left out.");
        w.line("@override");
        w.line("Map<String, Object?> toJson() {");
        w.indent();
        if comps.is_empty() && extra.is_none() {
            w.line("return {};");
        } else {
            w.line("return {");
            w.indent();
            if let Some(e) = extra {
                match g.encoder(e) {
                    None => w.line("...additionalProperties,"),
                    Some(enc) => w.expr(
                        "...",
                        &Expr::call(
                            "Wire.encodeMap",
                            vec![Expr::raw("additionalProperties"), enc],
                        ),
                        ",",
                    ),
                }
            }
            for c in comps {
                let key = format!("{}: ", str_lit(&c.wire));
                match c.presence {
                    Presence::Required => {
                        w.expr(&key, &g.encode_value(&c.dt, &c.name), ",");
                    }
                    Presence::RequiredNullable => {
                        let value = match g.encoder(&c.dt) {
                            None => Expr::raw(c.name.clone()),
                            Some(enc) if c.dt.nullable() => {
                                Expr::call("Wire.encode", vec![Expr::raw(c.name.clone()), enc])
                            }
                            Some(enc) => Expr::call(
                                "Wire.encode",
                                vec![
                                    Expr::raw(c.name.clone()),
                                    Expr::call("Wire.nullableOut", vec![enc]),
                                ],
                            ),
                        };
                        w.expr(&key, &value, ",");
                    }
                    Presence::Optional => {
                        if c.dt.nullable() && g.encoder(&c.dt).is_none() {
                            w.if_element(
                                &format!("{} != null", c.name),
                                &key,
                                &Expr::raw(c.name.clone()),
                                ",",
                            );
                        } else {
                            w.if_element(
                                &format!("{} case final v$?", c.name),
                                &key,
                                &g.encode_value(&c.dt, "v$"),
                                ",",
                            );
                        }
                    }
                    Presence::OptionalNullable => {
                        let value = match g.encoder(&c.dt) {
                            None => Expr::raw(format!("{}.valueOrNull", c.name)),
                            Some(enc) => {
                                Expr::call("Wire.encodePatch", vec![Expr::raw(c.name.clone()), enc])
                            }
                        };
                        w.if_element(&format!("{}.isPresent", c.name), &key, &value, ",");
                    }
                }
            }
            w.dedent();
            w.line("};");
        }
        w.dedent();
        w.line("}");
        equality(w, name);
        w.dedent();
        w.line("}");
    }

    fn enumeration(
        &mut self,
        w: &mut Code,
        t: &NamedType,
        name: &str,
        doc: &str,
        plan: &EnumPlan,
        values: &[EnumValue],
    ) {
        let g = self.g;
        self.declared.push(name.to_string());
        let names = g
            .names
            .enum_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        let base = match plan {
            EnumPlan::Integers => "int",
            EnumPlan::Mixed { .. } => "Object?",
            _ => "String",
        };
        w.doc(doc);
        w.class_header(&format!("enum {name}"), "implements JsonEncodable");
        w.indent();
        let n = values.len();
        for (i, v) in values.iter().enumerate() {
            let vdoc = v
                .doc
                .as_ref()
                .and_then(|d| d.description.clone().or_else(|| d.summary.clone()))
                .unwrap_or_default();
            if i > 0 && !vdoc.trim().is_empty() {
                w.blank();
            }
            w.doc(&vdoc);
            let vname = names.get(i).cloned().unwrap_or_else(|| format!("value{i}"));
            let lit = match plan {
                EnumPlan::Integers => v.value.as_i64().unwrap_or(0).to_string(),
                EnumPlan::Mixed { .. } => const_lit(&v.value),
                _ => str_lit(v.value.as_str().unwrap_or("")),
            };
            w.expr(
                "",
                &Expr::call(vname, vec![Expr::raw(lit)]),
                if i + 1 == n { ";" } else { "," },
            );
        }
        w.blank();
        one_param_ctor(w, name, "this.wire");
        w.blank();
        w.doc("The value on the wire.");
        w.line(format!("final {base} wire;"));
        w.blank();
        w.doc(&format!(
            "The `{name}` whose wire value is the JSON value; throws a `FormatException` for any other value."
        ));
        factory_header(w, name);
        w.indent();
        if matches!(plan, EnumPlan::Mixed { .. }) {
            // The values are JSON values of different types: compared as
            // JSON (`1` is `1.0`, arrays and objects by content).
            w.line("for (final v in values) {");
            w.indent();
            w.line("if (Wire.same(v.wire, json)) {");
            w.indent();
            w.line("return v;");
            w.dedent();
            w.line("}");
            w.dedent();
            w.line("}");
            let allowed: Vec<String> = values
                .iter()
                .map(|v| serde_json::to_string(&v.value).unwrap_or_default())
                .collect();
            failure(w, &format!("expected one of {}", allowed.join(", ")));
        } else {
            w.line("return Wire.enumValue(values, json, path);");
        }
        w.dedent();
        w.line("}");
        w.blank();
        w.line("@override");
        w.line("Object? toJson() => wire;");
        w.dedent();
        w.line("}");
    }

    /// An `allOf` that cannot be merged into one record: a class with one
    /// typed part per member. Every part decodes the whole value (a member
    /// may be a union, a record with typed extra members or any other
    /// type), and `toJson()` merges the parts' JSON values (objects member
    /// by member).
    fn intersection(&mut self, w: &mut Code, name: &str, doc: &str, members: &[TypeRef]) {
        let g = self.g;
        let from: Option<&str> = Some(self.ns);
        self.declared.push(name.to_string());
        let mut taken: Vec<String> = ["toJson", "hashCode", "runtimeType", "fromJson"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parts: Vec<(String, Dt, String)> = members
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let (base, label) = match m {
                    TypeRef::Named(id) if g.has_class(id) => (
                        g.plan.types.get(id).map_or_else(String::new, |p| {
                            naming::render(
                                &Ident {
                                    wire: p.words.join(" "),
                                    words: p.words.clone(),
                                },
                                Target::Dart,
                                Role::Field,
                            )
                        }),
                        format!("The value as the member `{}`.", id.0),
                    ),
                    _ => (
                        format!("part{}", i + 1),
                        format!("The value as the member number {}.", i + 1),
                    ),
                };
                let mut field = if base.is_empty() {
                    format!("part{}", i + 1)
                } else {
                    base
                };
                while taken.contains(&field) {
                    field.push('2');
                }
                taken.push(field.clone());
                (field, g.dt(m), label)
            })
            .collect();
        w.doc(doc);
        w.class_header(&format!("final class {name}"), "implements JsonEncodable");
        w.indent();
        w.doc("Builds the value from its parts.");
        w.line(format!("const {name}({{"));
        w.indent();
        for (field, _, _) in &parts {
            w.line(format!("required this.{field},"));
        }
        w.dedent();
        w.line("});");
        w.blank();
        w.doc(&format!(
            "Decodes a `{name}`: each part decodes the whole JSON value. Throws a `FormatException` naming the JSON path of the first value that does not fit."
        ));
        factory_header(w, name);
        w.indent();
        w.line(format!("return {name}("));
        w.indent();
        for (field, dt, _) in &parts {
            w.expr(
                &format!("{field}: "),
                &g.decode_value(dt, from, "json", "path"),
                ",",
            );
        }
        w.dedent();
        w.line(");");
        w.dedent();
        w.line("}");
        for (field, dt, label) in &parts {
            w.blank();
            w.doc(label);
            field_line(w, &g.dart_type(dt, from), field);
        }
        w.blank();
        w.doc("The JSON value: the parts' values, merged (objects member by member).");
        w.line("@override");
        w.line("Object? toJson() {");
        w.indent();
        w.line("return Wire.merge([");
        w.indent();
        for (field, dt, _) in &parts {
            let value = if dt.nullable() {
                match g.encoder(dt) {
                    None => Expr::raw(field.clone()),
                    Some(e) => Expr::call("Wire.encode", vec![Expr::raw(field.clone()), e]),
                }
            } else {
                g.encode_value(dt, field)
            };
            w.expr("", &value, ",");
        }
        w.dedent();
        w.line("]);");
        w.dedent();
        w.line("}");
        equality(w, name);
        w.dedent();
        w.line("}");
    }

    /// A class name for a variant of `union` that no other name of the
    /// library takes.
    fn variant_class(&mut self, union: &str, variant: &str) -> String {
        let clean: String = variant.chars().filter(|c| *c != '$' && *c != '_').collect();
        let mut name = format!("{union}{clean}");
        if name == union {
            name.push_str("Variant");
        }
        let base = name.clone();
        let mut n = 2;
        while self.taken.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        self.taken.insert(name.clone());
        name
    }

    fn union(
        &mut self,
        w: &mut Code,
        t: &NamedType,
        name: &str,
        doc: &str,
        plan: &UnionPlan,
        union: &tungsten_ir::Union,
    ) {
        let g = self.g;
        let from: Option<&str> = Some(self.ns);
        self.declared.push(name.to_string());
        let raw = g
            .names
            .union_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        let classes: Vec<String> = union
            .variants
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let v = raw.get(i).cloned().unwrap_or_else(|| format!("Variant{i}"));
                self.variant_class(name, &v)
            })
            .collect();
        let payloads: Vec<Dt> = union.variants.iter().map(|v| g.dt(&v.ty)).collect();
        w.doc(doc);
        w.class_header(&format!("sealed class {name}"), "implements JsonEncodable");
        w.indent();
        w.doc("For the variant classes.");
        w.line(format!("const {name}();"));
        w.blank();
        let how = match plan {
            UnionPlan::Tagged { tag, .. } => format!("by its `{tag}` member"),
            UnionPlan::LiteralTagged { .. } => "by its value".to_string(),
            _ => "as the first variant, in order, that accepts it".to_string(),
        };
        w.doc(&format!(
            "Decodes the variant the JSON value is, {how}; throws a `FormatException` when it is none."
        ));
        factory_header(w, name);
        w.indent();
        let construct = |i: usize| -> Expr {
            Expr::call(
                classes[i].clone(),
                vec![g.decode_value(&payloads[i], from, "json", "path")],
            )
        };
        match plan {
            UnionPlan::Tagged { tag, variants } => {
                w.line("final o = Wire.object(json, path);");
                w.line(format!("switch (o[{}]) {{", str_lit(tag)));
                w.indent();
                let mut all: Vec<String> = vec![];
                for (index, tags) in variants {
                    if tags.is_empty() {
                        continue;
                    }
                    for t in tags {
                        w.line(format!("case {}:", str_lit(t)));
                        all.push(format!("\"{t}\""));
                    }
                    w.indent();
                    w.expr("return ", &construct(*index), ";");
                    w.dedent();
                }
                w.dedent();
                w.line("}");
                failure(
                    w,
                    &format!("member \"{tag}\" must be one of {}", all.join(", ")),
                );
            }
            UnionPlan::LiteralTagged { variants } => {
                let mut open = true;
                for (index, key) in variants {
                    let cond = match key {
                        LiteralKey::Value(v) => Some(format!("Wire.same(json, {})", json_lit(v))),
                        LiteralKey::JsonType("any") => None,
                        LiteralKey::JsonType(t) => Some(format!("Wire.isType(json, '{t}')")),
                    };
                    match cond {
                        Some(cond) => {
                            w.line(format!("if ({cond}) {{"));
                            w.indent();
                            w.expr("return ", &construct(*index), ";");
                            w.dedent();
                            w.line("}");
                        }
                        None => {
                            w.expr("return ", &construct(*index), ";");
                            open = false;
                            break;
                        }
                    }
                }
                if open {
                    failure(w, "matches no variant");
                }
            }
            UnionPlan::Untagged { ordered } => {
                for index in ordered {
                    w.line("try {");
                    w.indent();
                    w.expr("return ", &construct(*index), ";");
                    w.dedent();
                    w.line("} on FormatException {");
                    w.indent();
                    w.line("// Not this variant: try the next one.");
                    w.dedent();
                    w.line("}");
                }
                failure(w, "matches no variant");
            }
            UnionPlan::Empty => failure(w, "matches no variant"),
        }
        w.dedent();
        w.line("}");
        w.dedent();
        w.line("}");
        for (i, v) in union.variants.iter().enumerate() {
            let class = &classes[i];
            self.declared.push(class.clone());
            let ty = g.dart_type(&payloads[i], from);
            w.blank();
            let vdoc = match &v.tag {
                Some(tag) => format!("The `{name}` variant tagged `{tag}`."),
                None => format!("The `{name}` variant `{}`.", v.name.wire),
            };
            w.doc(&vdoc);
            w.class_header(&format!("final class {class}"), &format!("extends {name}"));
            w.indent();
            w.doc(&format!("The variant holding a `{ty}`."));
            one_param_ctor(w, class, "this.value");
            w.blank();
            w.doc("The variant's value.");
            field_line(w, &ty, "value");
            w.blank();
            w.line("@override");
            w.line("Object? toJson() {");
            w.indent();
            let value = if payloads[i].nullable() {
                match g.encoder(&payloads[i]) {
                    None => Expr::raw("value"),
                    Some(e) => Expr::call("Wire.encode", vec![Expr::raw("value"), e]),
                }
            } else {
                g.encode_value(&payloads[i], "value")
            };
            w.expr("return ", &value, ";");
            w.dedent();
            w.line("}");
            equality(w, class);
            w.dedent();
            w.line("}");
        }
    }
}

/// `factory <name>.fromJson(Object? json, [String path = r'$']) {`, split
/// like `dart format` when it does not fit.
pub(crate) fn factory_header(w: &mut Code, name: &str) {
    let flat = format!("factory {name}.fromJson(Object? json, [String path = r'$']) {{");
    if w.fits(&flat) {
        w.line(flat);
    } else {
        w.line(format!("factory {name}.fromJson("));
        w.line("  Object? json, [");
        w.line("  String path = r'$',");
        w.line("]) {");
    }
}

/// `const <name>(<param>);`, with the parameter on its own line and a
/// trailing comma when it does not fit.
pub(crate) fn one_param_ctor(w: &mut Code, name: &str, param: &str) {
    let flat = format!("const {name}({param});");
    if w.fits(&flat) {
        w.line(flat);
    } else {
        w.line(format!("const {name}("));
        w.line(format!("  {param},"));
        w.line(");");
    }
}

/// `final <type> <name>;`, split before the name when it does not fit.
pub(crate) fn field_line(w: &mut Code, ty: &str, name: &str) {
    let flat = format!("final {ty} {name};");
    if w.fits(&flat) {
        w.line(flat);
    } else {
        w.line(format!("final {ty}"));
        w.line(format!("    {name};"));
    }
}

/// `throw Wire.failure(path, '<message>');`.
pub(crate) fn failure(w: &mut Code, message: &str) {
    w.expr(
        "throw ",
        &Expr::call(
            "Wire.failure",
            vec![Expr::raw("path"), Expr::raw(str_lit(message))],
        ),
        ";",
    );
}

/// `==` and `hashCode` over the JSON form.
pub(crate) fn equality(w: &mut Code, name: &str) {
    w.blank();
    w.line("@override");
    w.line("bool operator ==(Object other) {");
    w.indent();
    w.line(format!("if (other is! {name}) {{"));
    w.indent();
    w.line("return false;");
    w.dedent();
    w.line("}");
    w.line("return Wire.same(toJson(), other.toJson());");
    w.dedent();
    w.line("}");
    w.blank();
    w.line("@override");
    w.line("int get hashCode => Wire.hash(toJson());");
}

/// `lib/src/wire.dart`.
fn wire(g: &Gen<'_, '_>) -> String {
    let mut out = String::new();
    out.push_str(&g.header);
    out.push_str("\n\n");
    let merges =
        g.plan.types.values().any(|t| {
            matches!(t.kind, TypeKind::AllOf(AllOfPlan::Intersection)) && g.has_class(&t.id)
        });
    match WIRE.strip_suffix("}\n") {
        Some(head) if merges => {
            out.push_str(head);
            out.push_str(WIRE_MERGE);
            out.push_str("}\n");
        }
        _ => out.push_str(WIRE),
    }
    out
}

/// The merge of the parts of an intersection (in `lib/src/wire.dart` only
/// when the package has one).
const WIRE_MERGE: &str = r#"
  /// The JSON value of an intersection: the JSON values of its parts,
  /// merged. Objects are merged member by member (recursively); of any
  /// other values the first one that is not null is kept.
  static Object? merge(List<Object?> parts) {
    Object? merged;
    for (final part in parts) {
      merged = _merge(merged, part);
    }
    return merged;
  }

  static Object? _merge(Object? left, Object? right) {
    if (left is Map<Object?, Object?> && right is Map<Object?, Object?>) {
      final merged = <String, Object?>{
        for (final e in left.entries) '${e.key}': e.value,
      };
      for (final e in right.entries) {
        final key = '${e.key}';
        merged[key] =
            merged.containsKey(key) ? _merge(merged[key], e.value) : e.value;
      }
      return merged;
    }
    return left ?? right;
  }
"#;

/// The decoders and encoders of the model classes (the same in every
/// package).
const WIRE: &str = r#"/// JSON decoders and encoders of the model classes.
library;

import 'dart:convert';
import 'dart:typed_data';

import 'package:tungsten_runtime/tungsten_runtime.dart'
    show Binary, JsonEncodable, Patch;

/// Reads the JSON value at `path` as a `T`; throws a `FormatException`.
typedef Decoder<T> = T Function(Object? json, String path);

/// The JSON value of a `T`.
typedef Encoder<T> = Object? Function(T value);

/// Decoders, encoders and value equality of JSON values.
abstract final class Wire {
  /// A decoding failure at `path`.
  static FormatException failure(String path, String message) {
    return FormatException('$path: $message');
  }

  static String _kind(Object? json) {
    if (json == null) return 'null';
    if (json is bool) return 'a boolean';
    if (json is num) return 'a number';
    if (json is String) return 'a string';
    if (json is List<Object?>) return 'an array';
    if (json is Map<Object?, Object?>) return 'an object';
    return 'a ${json.runtimeType}';
  }

  static String _member(String path, String key) {
    final plain = RegExp(r'^[A-Za-z_$][A-Za-z0-9_$]*$').hasMatch(key);
    return plain ? '$path.$key' : '$path[${jsonEncode(key)}]';
  }

  /// A JSON object.
  static Map<String, Object?> object(Object? json, String path) {
    if (json is Map<String, Object?>) return json;
    if (json is Map<Object?, Object?>) {
      return {for (final e in json.entries) '${e.key}': e.value};
    }
    throw failure(path, 'expected an object, got ${_kind(json)}');
  }

  /// A string.
  static String string(Object? json, String path) {
    if (json is String) return json;
    throw failure(path, 'expected a string, got ${_kind(json)}');
  }

  /// An integer (a number without a fractional part).
  static int integer(Object? json, String path) {
    if (json is int) return json;
    if (json is double && json.isFinite && json == json.truncateToDouble()) {
      return json.toInt();
    }
    throw failure(path, 'expected an integer, got ${_kind(json)}');
  }

  /// A number.
  static double number(Object? json, String path) {
    if (json is num) return json.toDouble();
    throw failure(path, 'expected a number, got ${_kind(json)}');
  }

  /// A boolean.
  static bool boolean(Object? json, String path) {
    if (json is bool) return json;
    throw failure(path, 'expected a boolean, got ${_kind(json)}');
  }

  /// Bytes: base64 text, or bytes the runtime already decoded.
  static Uint8List bytes(Object? json, String path) {
    if (json is Uint8List) return json;
    if (json is String) {
      try {
        return base64Decode(json);
      } on FormatException {
        throw failure(path, 'expected base64 text');
      }
    }
    final binary = Binary.tryFrom(json);
    if (binary != null) return binary.data;
    throw failure(path, 'expected bytes, got ${_kind(json)}');
  }

  /// Any JSON value, as it is.
  static Object? any(Object? json, String path) => json;

  /// A JSON value parsed from its text (constants of the descriptions).
  static Object? literal(String text) => jsonDecode(text);

  /// Whether a JSON value has a JSON type (`string`, `integer`, `number`,
  /// `boolean`, `null`, `array`, `object`).
  static bool isType(Object? json, String type) {
    switch (type) {
      case 'string':
        return json is String;
      case 'integer':
        return json is int ||
            (json is double &&
                json.isFinite &&
                json == json.truncateToDouble());
      case 'number':
        return json is num;
      case 'boolean':
        return json is bool;
      case 'null':
        return json == null;
      case 'array':
        return json is List<Object?>;
      case 'object':
        return json is Map<Object?, Object?>;
    }
    return true;
  }

  /// A decoder of arrays of `T`.
  static Decoder<List<T>> listOf<T>(Decoder<T> item) {
    return (json, path) {
      if (json is! List<Object?>) {
        throw failure(path, 'expected an array, got ${_kind(json)}');
      }
      return [for (var i = 0; i < json.length; i++) item(json[i], '$path[$i]')];
    };
  }

  /// A decoder of objects whose members are `T`s.
  static Decoder<Map<String, T>> mapOf<T>(Decoder<T> item) {
    return (json, path) {
      final o = object(json, path);
      return {
        for (final e in o.entries) e.key: item(e.value, _member(path, e.key)),
      };
    };
  }

  /// A decoder of `T` or null.
  static Decoder<T?> nullableOf<T>(Decoder<T> inner) {
    return (json, path) => json == null ? null : inner(json, path);
  }

  /// Decodes `json` with `decoder`.
  static T apply<T>(Object? json, String path, Decoder<T> decoder) {
    return decoder(json, path);
  }

  /// A member that must be present.
  static T required<T>(
    Map<String, Object?> o,
    String key,
    String path,
    Decoder<T> decoder,
  ) {
    if (!o.containsKey(key)) {
      throw failure(path, 'missing member ${jsonEncode(key)}');
    }
    return decoder(o[key], _member(path, key));
  }

  /// A member that must be present and may be null.
  static T? requiredNullable<T>(
    Map<String, Object?> o,
    String key,
    String path,
    Decoder<T> decoder,
  ) {
    if (!o.containsKey(key)) {
      throw failure(path, 'missing member ${jsonEncode(key)}');
    }
    final value = o[key];
    return value == null ? null : decoder(value, _member(path, key));
  }

  /// An optional member: null when absent (or null).
  static T? optional<T>(
    Map<String, Object?> o,
    String key,
    String path,
    Decoder<T> decoder,
  ) {
    final value = o[key];
    return value == null ? null : decoder(value, _member(path, key));
  }

  /// An optional member that may be null: absent, null or a value.
  static Patch<T> patch<T>(
    Map<String, Object?> o,
    String key,
    String path,
    Decoder<T> decoder,
  ) {
    if (!o.containsKey(key)) return Patch<T>.absent();
    final value = o[key];
    if (value == null) return Patch<T>.nullValue();
    return Patch<T>.of(decoder(value, _member(path, key)));
  }

  /// The members of `o` that are not in `known`, decoded.
  static Map<String, T> extra<T>(
    Map<String, Object?> o,
    Set<String> known,
    String path,
    Decoder<T> decoder,
  ) {
    return {
      for (final e in o.entries)
        if (!known.contains(e.key))
          e.key: decoder(e.value, _member(path, e.key)),
    };
  }

  /// The enum value whose JSON value is `json`.
  static T enumValue<T extends JsonEncodable>(
    List<T> values,
    Object? json,
    String path,
  ) {
    for (final v in values) {
      if (v.toJson() == json) return v;
    }
    final allowed = values.map((v) => jsonEncode(v.toJson())).join(', ');
    throw failure(path, 'expected one of $allowed, got ${_kind(json)}');
  }

  /// Encodes `value` with `encoder`.
  static Object? encode<T>(T value, Encoder<T> encoder) => encoder(value);

  /// The JSON value of a model.
  static Object? encodable(JsonEncodable value) => value.toJson();

  /// Bytes as base64 text.
  static Object? base64Text(Uint8List value) => base64Encode(value);

  /// An encoder of lists of `T`.
  static Encoder<List<T>> listOut<T>(Encoder<T> item) {
    return (value) => [for (final v in value) item(v)];
  }

  /// An encoder of maps of `T`.
  static Encoder<Map<String, T>> mapOut<T>(Encoder<T> item) {
    return (value) => encodeMap(value, item);
  }

  /// An encoder of `T` or null.
  static Encoder<T?> nullableOut<T>(Encoder<T> inner) {
    return (value) => value == null ? null : inner(value);
  }

  /// A map of `T` as a JSON object.
  static Map<String, Object?> encodeMap<T>(
    Map<String, T> value,
    Encoder<T> item,
  ) {
    return {for (final e in value.entries) e.key: item(e.value)};
  }

  /// The JSON value of a present `Patch`: null or the encoded value.
  static Object? encodePatch<T>(Patch<T> value, Encoder<T> encoder) {
    return value.hasValue ? encoder(value.value) : null;
  }

  /// Whether two JSON values are equal (objects regardless of member
  /// order).
  static bool same(Object? a, Object? b) {
    if (a is List<Object?> && b is List<Object?>) {
      if (a.length != b.length) return false;
      for (var i = 0; i < a.length; i++) {
        if (!same(a[i], b[i])) return false;
      }
      return true;
    }
    if (a is Map<Object?, Object?> && b is Map<Object?, Object?>) {
      if (a.length != b.length) return false;
      for (final e in a.entries) {
        if (!b.containsKey(e.key) || !same(e.value, b[e.key])) return false;
      }
      return true;
    }
    return a == b;
  }

  /// A hash code consistent with [same].
  static int hash(Object? value) {
    if (value is List<Object?>) return Object.hashAll(value.map(hash));
    if (value is Map<Object?, Object?>) {
      return Object.hashAllUnordered(
        value.entries.map((e) => Object.hash(e.key, hash(e.value))),
      );
    }
    return value.hashCode;
  }
}
"#;
