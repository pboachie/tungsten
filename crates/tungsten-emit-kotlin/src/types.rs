// SPDX-License-Identifier: AGPL-3.0-only
//! Kotlin types of IR types, and the model files: data classes (four-state
//! presence with `Patch`, additional members kept), enum classes with a
//! strict decoder, sealed interfaces with data-class variants for unions.

use std::collections::BTreeSet;

use tungsten_emit::Writer;
use tungsten_emit::sdk::{AllOfPlan, EnumPlan, TypeKind, UnionPlan};
use tungsten_ir::{
    Additional, Field, NamedType, Presence, Primitive, Shape, StringFormat, TypeId, TypeRef,
};

use crate::Ctx;
use crate::kotlin::{self, kdoc, kdoc_line, lit};

/// How a Kotlin type is written, decoded and encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kt {
    /// The type without `?`.
    pub base: String,
    /// A `(Any?) -> base` expression (never given null).
    pub dec: String,
    /// A `(base) -> Any?` expression.
    pub enc: String,
    /// Whether null is a value of the type (`base?` where it is held).
    pub nullable: bool,
}

impl Kt {
    fn new(base: &str, dec: &str, enc: &str) -> Self {
        Self {
            base: base.into(),
            dec: dec.into(),
            enc: enc.into(),
            nullable: false,
        }
    }

    /// The runtime's JSON value (`Any?`).
    pub fn json() -> Self {
        Self {
            nullable: true,
            ..Self::new("Any", "Codec::json", "Codec::same")
        }
    }

    fn plain(base: &str, dec: &str) -> Self {
        Self::new(base, dec, "Codec::same")
    }

    /// The type where a value of it is held: `base?` when null is a value.
    pub fn full(&self) -> String {
        if self.nullable {
            format!("{}?", self.base)
        } else {
            self.base.clone()
        }
    }

    /// A `(Any?) -> full` expression.
    pub fn full_dec(&self) -> String {
        if self.nullable {
            wrap("Codec.nullable", &self.dec)
        } else {
            self.dec.clone()
        }
    }
}

/// `{ v<n> -> call(v<n>, inner) }`: the parameter is named by the number of
/// lambdas in `inner`, so it never shadows one of theirs.
pub fn wrap(call: &str, inner: &str) -> String {
    let n = inner.matches("->").count();
    format!("{{ v{n} -> {call}(v{n}, {inner}) }}")
}

/// `{ v<n> -> Class.fromJson(v<n>) }`.
fn from_json(class: &str) -> String {
    format!("{{ v0 -> {class}.fromJson(v0) }}")
}

/// Where a type is referenced from: which model namespace's package is
/// imported, and the simple names that are taken there.
#[derive(Debug, Clone, Copy)]
pub enum From<'a> {
    /// A model file of this namespace (same package).
    Model(&'a str),
    /// A resources file of this namespace (its models are imported).
    Resources(&'a str),
    /// Everywhere else: fully qualified.
    Elsewhere,
}

impl Ctx<'_> {
    /// The package of a model namespace's types.
    pub fn model_package(&self, ns: &str) -> String {
        if self.single_model_package {
            format!("{}.models", self.opts.package)
        } else {
            let module = self
                .names
                .modules
                .get(ns)
                .cloned()
                .unwrap_or_else(|| "models".into());
            format!("{}.models.{module}", self.opts.package)
        }
    }

    /// Whether a named type has a model file.
    pub fn has_class(&self, id: &TypeId) -> bool {
        let Some(t) = self.plan.types.get(id) else {
            return false;
        };
        match &t.kind {
            TypeKind::Record { .. } => true,
            TypeKind::Enum(e) => matches!(e, EnumPlan::Strings | EnumPlan::Integers),
            TypeKind::Union(u) => !matches!(u, UnionPlan::Empty),
            TypeKind::AllOf(a) => matches!(a, AllOfPlan::Merged { .. }),
            TypeKind::Alias => false,
        }
    }

    /// The reference to a model class from a context.
    pub fn class_ref(&self, id: &TypeId, from: From<'_>) -> String {
        let simple = self
            .names
            .types
            .get(id)
            .cloned()
            .unwrap_or_else(|| "Model".into());
        let ns = self
            .plan
            .types
            .get(id)
            .map(|t| t.namespace.as_str())
            .unwrap_or("");
        let local = match from {
            From::Model(n) => n == ns,
            From::Resources(n) => n == ns && !self.resources_taken.contains(&simple),
            From::Elsewhere => false,
        };
        if local {
            simple
        } else {
            format!("{}.{simple}", self.model_package(ns))
        }
    }

    /// The Kotlin type of a type reference.
    pub fn kt(&self, ty: &TypeRef, from: From<'_>) -> Kt {
        self.kt_depth(ty, from, 0)
    }

    fn kt_depth(&self, ty: &TypeRef, from: From<'_>, depth: usize) -> Kt {
        if depth > 32 {
            return Kt::json();
        }
        match ty {
            TypeRef::Named(id) => {
                if self.has_class(id) {
                    let c = self.class_ref(id, from);
                    return Kt::new(&c, &from_json(&c), &format!("{c}::toJson"));
                }
                match self.ir.types.get(id) {
                    Some(t) if !self.plan.types.get(id).is_some_and(|p| p.cyclic) => {
                        self.shape_kt(&t.shape, from, depth + 1)
                    }
                    _ => Kt::json(),
                }
            }
            TypeRef::Inline(shape) => self.shape_kt(shape, from, depth + 1),
        }
    }

    fn shape_kt(&self, shape: &Shape, from: From<'_>, depth: usize) -> Kt {
        match shape {
            Shape::Primitive { primitive, .. } => primitive_kt(primitive),
            Shape::Enum { values, .. } => {
                if values.iter().all(|v| v.value.is_string()) && !values.is_empty() {
                    Kt::plain("String", "Codec::string")
                } else if values.iter().all(|v| v.value.is_i64()) && !values.is_empty() {
                    Kt::plain("Long", "Codec::long")
                } else {
                    Kt::json()
                }
            }
            Shape::Const { value } => match value {
                serde_json::Value::String(_) => Kt::plain("String", "Codec::string"),
                serde_json::Value::Bool(_) => Kt::plain("Boolean", "Codec::bool"),
                serde_json::Value::Number(n) if n.is_i64() => Kt::plain("Long", "Codec::long"),
                serde_json::Value::Number(_) => Kt::plain("Double", "Codec::double"),
                _ => Kt::json(),
            },
            Shape::Array { items, .. } => {
                let item = self.kt_depth(items, from, depth);
                Kt {
                    base: format!("List<{}>", item.full()),
                    dec: wrap("Codec.list", &item.full_dec()),
                    enc: wrap("Codec.encodeList", &item.enc),
                    nullable: false,
                }
            }
            Shape::Map { values } => self.map_kt(values, from, depth),
            Shape::Nullable { inner } => Kt {
                nullable: true,
                ..self.kt_depth(inner, from, depth)
            },
            Shape::Record { fields, additional } => {
                if fields.is_empty()
                    && let Additional::Typed { values } = additional
                {
                    return self.map_kt(values, from, depth);
                }
                Kt::new("JsonObject", "Codec::jsonObject", "Codec::same")
            }
            Shape::Union(_) | Shape::Intersection { .. } | Shape::Any | Shape::Never => Kt::json(),
        }
    }

    fn map_kt(&self, values: &TypeRef, from: From<'_>, depth: usize) -> Kt {
        let item = self.kt_depth(values, from, depth);
        Kt {
            base: format!("Map<String, {}>", item.full()),
            dec: wrap("Codec.map", &item.full_dec()),
            enc: wrap("Codec.encodeMap", &item.enc),
            nullable: false,
        }
    }
}

/// The Kotlin type of a primitive.
pub fn primitive_kt(p: &Primitive) -> Kt {
    match p {
        Primitive::String {
            format: Some(StringFormat::Byte),
        } => Kt::new("ByteArray", "Codec::bytes", "Codec::encodeBytes"),
        Primitive::String { .. } => Kt::plain("String", "Codec::string"),
        Primitive::Int32 => Kt::plain("Int", "Codec::int"),
        Primitive::Int64 | Primitive::Integer => Kt::plain("Long", "Codec::long"),
        Primitive::Float | Primitive::Double | Primitive::Number => {
            Kt::plain("Double", "Codec::double")
        }
        Primitive::Bool => Kt::plain("Boolean", "Codec::bool"),
        Primitive::Bytes => Kt::new("Binary", "Codec::binary", "Codec::encodeBinary"),
    }
}

/// One property of a generated data class (a model field or an argument).
#[derive(Debug, Clone)]
pub struct Component {
    /// The Kotlin name.
    pub name: String,
    /// The JSON member name (wire name, or arguments key).
    pub key: String,
    pub presence: Presence,
    pub kt: Kt,
    pub doc: String,
}

impl Component {
    /// Whether null is a value of the member (a `Patch`).
    pub fn is_patch(&self) -> bool {
        matches!(
            self.presence,
            Presence::RequiredNullable | Presence::OptionalNullable
        )
    }

    /// The declared type: `Patch<T>` when null is a value, else `T?`.
    pub fn kotlin_type(&self) -> String {
        if self.is_patch() {
            format!("Patch<{}>", self.kt.base)
        } else {
            format!("{}?", self.kt.base)
        }
    }

    /// The default value: every member starts unset.
    fn default(&self) -> &'static str {
        if self.is_patch() { "Patch.Unset" } else { "null" }
    }

    fn decode(&self, obj: &str) -> String {
        if self.is_patch() {
            format!("Codec.patch({obj}, {}, {})", lit(&self.key), self.kt.dec)
        } else {
            format!("Codec.member({obj}, {}, {})", lit(&self.key), self.kt.dec)
        }
    }

    fn encode(&self, obj: &str) -> String {
        let call = if self.is_patch() {
            "Codec.putPatch"
        } else {
            "Codec.put"
        };
        format!(
            "{call}({obj}, {}, {}, {})",
            lit(&self.key),
            self.name,
            self.kt.enc
        )
    }
}

/// What a data class keeps besides its components.
#[derive(Debug, Clone)]
pub enum Extra {
    /// Unknown members are refused when decoding (request classes).
    Refused,
    /// Unknown members are kept as `additionalProperties`, decoded as this
    /// type, and written back after the components.
    Kept(Kt),
}

/// The local the generated `fromJson` and `toJson` use for the object:
/// no namer name starts with two underscores.
const OBJ: &str = "__o";

/// Writes a data class with `toJson` and a companion `fromJson`; a class
/// without members (a request without arguments) is a plain class with
/// value equality.
pub fn write_class(w: &mut Writer, name: &str, doc: &str, comps: &[Component], extra: &Extra) {
    kdoc(w, doc);
    let mut params: Vec<(String, String)> = vec![];
    for c in comps {
        let mut p = String::new();
        if !c.doc.trim().is_empty() {
            p.push_str(&format!("/** {} */\n", kotlin::doc_text(&one_line(&c.doc))));
        }
        p.push_str(&format!("val {}: {} = {},", c.name, c.kotlin_type(), c.default()));
        params.push((c.name.clone(), p));
    }
    if let Extra::Kept(t) = extra {
        params.push((
            "additionalProperties".into(),
            format!(
                "/** Members the type does not declare, kept in order and written back after the declared ones. */\nval additionalProperties: Map<String, {}> = emptyMap(),",
                t.full()
            ),
        ));
    }
    if params.is_empty() {
        w.block(
            format!("class {name} : Json.Encodable {{"),
            "}",
            |w| {
                w.line("override fun toJson(): JsonObject = JsonObject()");
                w.blank();
                w.line(format!("override fun equals(other: Any?): Boolean = other is {name}"));
                w.blank();
                w.line("override fun hashCode(): Int = 0");
                w.blank();
                w.line(format!("override fun toString(): String = {}", lit(&format!("{name}()"))));
                w.blank();
                w.block("companion object {", "}", |w| {
                    kdoc_line(w, "Decodes the JSON value; any member is refused.");
                    w.block(format!("fun fromJson(json: Any?): {name} {{"), "}", |w| {
                        w.line("Codec.closed(Codec.obj(json), emptySet())");
                        w.line(format!("return {name}()"));
                    });
                });
            },
        );
        return;
    }
    w.line(format!("data class {name}("));
    w.indent();
    for (_, p) in &params {
        for l in p.lines() {
            w.line(l);
        }
    }
    w.dedent();
    w.block(") : Json.Encodable {", "}", |w| {
        w.block("override fun toJson(): JsonObject {", "}", |w| {
            w.line(format!("val {OBJ} = JsonObject()"));
            for c in comps {
                w.line(c.encode(OBJ));
            }
            if let Extra::Kept(t) = extra {
                w.line(format!(
                    "Codec.putExtra({OBJ}, additionalProperties, {})",
                    t.enc
                ));
            }
            w.line(format!("return {OBJ}"));
        });
        w.blank();
        w.block("companion object {", "}", |w| {
            let keys: Vec<String> = comps.iter().map(|c| lit(&c.key)).collect();
            w.line(format!(
                "private val KNOWN: Set<String> = setOf({})",
                keys.join(", ")
            ));
            w.blank();
            kdoc_line(w, "Decodes the JSON value.");
            w.block(format!("fun fromJson(json: Any?): {name} {{"), "}", |w| {
                w.line(format!("val {OBJ} = Codec.obj(json)"));
                let mut args: Vec<String> = comps
                    .iter()
                    .map(|c| format!("{} = {}", c.name, c.decode(OBJ)))
                    .collect();
                match extra {
                    Extra::Refused => w.line(format!("Codec.closed({OBJ}, KNOWN)")),
                    Extra::Kept(t) => args.push(format!(
                        "additionalProperties = Codec.extra({OBJ}, KNOWN, {})",
                        t.full_dec()
                    )),
                }
                w.line(format!("return {name}("));
                w.indent();
                for a in &args {
                    w.line(format!("{a},"));
                }
                w.dedent();
                w.line(")");
            });
        });
    });
}

/// The text on one line (a KDoc on a constructor parameter).
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The doc text of a named type.
fn type_doc(t: &NamedType) -> String {
    let mut text = t
        .doc
        .as_ref()
        .and_then(|d| d.description.clone().or_else(|| d.summary.clone()))
        .unwrap_or_default();
    if text.trim().is_empty() {
        text = format!("The {} type.", t.id.0);
    }
    text
}

fn field_doc(f: &Field) -> String {
    f.doc
        .as_ref()
        .and_then(|d| d.description.clone().or_else(|| d.summary.clone()))
        .unwrap_or_default()
}

/// The imports of a model file.
fn model_imports() -> Vec<String> {
    vec![
        format!("{}.Binary", kotlin::RUNTIME),
        format!("{}.Decode", kotlin::RUNTIME),
        format!("{}.Json", kotlin::RUNTIME),
        format!("{}.JsonObject", kotlin::RUNTIME),
        format!("{}.Codec", kotlin::KOTLIN_RUNTIME),
        format!("{}.Patch", kotlin::KOTLIN_RUNTIME),
    ]
}

/// Drops the imports a file does not use.
pub fn used_imports(imports: Vec<String>, body: &str) -> Vec<String> {
    imports
        .into_iter()
        .filter(|i| {
            let simple = i.rsplit('.').next().unwrap_or(i);
            contains_word(body, simple)
        })
        .collect()
}

/// Whether `word` occurs in `text` as a whole identifier.
fn contains_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        // `a.Word` is a qualified name, `Word.member` a use of `Word`.
        let before = start == 0 || !(is_ident(bytes[start - 1]) || bytes[start - 1] == b'.');
        let after = end >= bytes.len() || !is_ident(bytes[end]);
        if before && after {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Ctx<'_> {
    /// The properties of a record shape's fields, named by the plan's field
    /// names of `id`.
    fn components(&self, id: &TypeId, fields: &[&Field], ns: &str) -> Vec<Component> {
        let names = self.names.fields.get(id).cloned().unwrap_or_default();
        fields
            .iter()
            .enumerate()
            .map(|(i, f)| Component {
                name: names.get(i).cloned().unwrap_or_else(|| format!("field{i}")),
                key: f.wire_name.clone(),
                presence: f.presence,
                kt: self.kt(&f.ty, From::Model(ns)),
                doc: field_doc(f),
            })
            .collect()
    }

    /// Every model file: (path, contents).
    pub fn models(&self) -> Vec<(String, String)> {
        let mut out = vec![];
        let ir = self.ir;
        for t in &ir.types.types {
            if !self.has_class(&t.id) {
                continue;
            }
            let Some(plan) = self.plan.types.get(&t.id) else {
                continue;
            };
            let name = self
                .names
                .types
                .get(&t.id)
                .cloned()
                .unwrap_or_else(|| "Model".into());
            let ns = t.namespace.clone();
            let package = self.model_package(&ns);
            let mut body = Writer::new("    ");
            match (&plan.kind, &t.shape) {
                (TypeKind::Record { .. }, Shape::Record { fields, additional }) => {
                    let refs: Vec<&Field> = fields.iter().collect();
                    let comps = self.components(&t.id, &refs, &ns);
                    let extra = match additional {
                        Additional::Typed { values } => {
                            Extra::Kept(self.kt(values, From::Model(&ns)))
                        }
                        _ => Extra::Kept(Kt::json()),
                    };
                    write_class(&mut body, &name, &type_doc(t), &comps, &extra);
                }
                (
                    TypeKind::AllOf(AllOfPlan::Merged { fields, .. }),
                    Shape::Intersection { members },
                ) => {
                    let mut refs: Vec<&Field> = vec![];
                    for &(m, f) in fields {
                        if let Some(Shape::Record { fields: own, .. }) = members
                            .get(m)
                            .and_then(|r| tungsten_emit::args::resolve(ir, r))
                            && let Some(field) = own.get(f)
                        {
                            refs.push(field);
                        }
                    }
                    let comps = self.components(&t.id, &refs, &ns);
                    write_class(
                        &mut body,
                        &name,
                        &type_doc(t),
                        &comps,
                        &Extra::Kept(Kt::json()),
                    );
                }
                (TypeKind::Enum(e), Shape::Enum { values, .. }) => {
                    self.write_enum(&mut body, t, &name, e, values);
                }
                (TypeKind::Union(u), Shape::Union(union)) => {
                    self.write_union(&mut body, t, &name, u, union);
                }
                _ => continue,
            }
            let text = body.finish();
            let imports = used_imports(model_imports(), &text);
            let mut w = kotlin::file(ir, &package, &imports);
            w.line(text.trim_end());
            out.push((
                format!("src/main/kotlin/{}/{name}.kt", package.replace('.', "/")),
                w.finish(),
            ));
        }
        out
    }

    fn write_enum(
        &self,
        w: &mut Writer,
        t: &NamedType,
        name: &str,
        plan: &EnumPlan,
        values: &[tungsten_ir::EnumValue],
    ) {
        let names = self
            .names
            .enum_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        fn long_literal(v: &serde_json::Value) -> String {
            let n = v.as_i64().unwrap_or(0);
            if n == i64::MIN {
                // -9223372036854775808L does not parse: the literal is
                // the negation of an out-of-range positive number.
                "Long.MIN_VALUE".to_string()
            } else {
                format!("{n}L")
            }
        }
        fn string_literal(v: &serde_json::Value) -> String {
            lit(v.as_str().unwrap_or(""))
        }
        let (base, literal): (&str, fn(&serde_json::Value) -> String) = match plan {
            EnumPlan::Integers => ("Long", long_literal),
            _ => ("String", string_literal),
        };
        kdoc(w, &type_doc(t));
        w.line(format!(
            "enum class {name}(/** The value on the wire. */ val value: {base}) : Json.Encodable {{"
        ));
        w.indent();
        let n = values.len();
        for (i, v) in values.iter().enumerate() {
            let doc = v
                .doc
                .as_ref()
                .and_then(|d| d.description.clone().or_else(|| d.summary.clone()))
                .unwrap_or_default();
            kdoc(w, &doc);
            let cname = names
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("VALUE_{i}"));
            w.line(format!(
                "{cname}({}){}",
                literal(&v.value),
                if i + 1 < n { "," } else { ";" }
            ));
        }
        let shown: Vec<String> = values.iter().map(|v| v.value.to_string()).collect();
        w.blank();
        w.line("override fun toJson(): Any = value");
        w.blank();
        w.block("companion object {", "}", |w| {
            kdoc_line(w, "Decodes the JSON value; a value outside the enum is refused.");
            w.block(format!("fun fromJson(json: Any?): {name} {{"), "}", |w| {
                w.block("for (v in entries) {", "}", |w| {
                    w.block("if (Json.deepEqual(v.value, json)) {", "}", |w| {
                        w.line("return v");
                    });
                });
                w.line(format!(
                    "throw Decode.Failure({})",
                    lit(&format!("expected one of [{}]", shown.join(", ")))
                ));
            });
        });
        w.dedent();
        w.line("}");
    }

    fn write_union(
        &self,
        w: &mut Writer,
        t: &NamedType,
        name: &str,
        plan: &UnionPlan,
        union: &tungsten_ir::Union,
    ) {
        let raw_names = self
            .names
            .union_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        let mut variant_names: Vec<String> = vec![];
        for (i, _) in union.variants.iter().enumerate() {
            let mut n = raw_names
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("Variant{i}"));
            if n == name || n == "Companion" {
                n.push_str("Variant");
            }
            while variant_names.contains(&n) {
                n.push('2');
            }
            variant_names.push(n);
        }
        let payloads: Vec<Kt> = union
            .variants
            .iter()
            .map(|v| self.kt(&v.ty, From::Elsewhere))
            .collect();
        kdoc(w, &type_doc(t));
        w.line(format!("sealed interface {name} : Json.Encodable {{"));
        w.indent();
        for (i, v) in union.variants.iter().enumerate() {
            let vn = &variant_names[i];
            let p = &payloads[i];
            let doc = match &v.tag {
                Some(tag) => format!("The variant tagged {tag}."),
                None => format!("The variant {}.", v.name.wire),
            };
            kdoc(w, &doc);
            w.block(
                format!(
                    "data class {vn}(/** The value. */ val value: {}) : {name} {{",
                    p.full()
                ),
                "}",
                |w| {
                    w.line(format!(
                        "override fun toJson(): Any? = Codec.encode(value, {})",
                        p.enc
                    ));
                },
            );
            w.blank();
        }
        w.block("companion object {", "}", |w| {
            kdoc_line(w, "Decodes the JSON value.");
            w.block(
                format!("fun fromJson(json: Any?): {name} {{"),
                "}",
                |w| match plan {
                    UnionPlan::Tagged { tag, variants } => {
                        w.line(format!("val tag = Codec.obj(json).get({})", lit(tag)));
                        let mut all: Vec<String> = vec![];
                        for (index, tags) in variants {
                            let vn = &variant_names[*index];
                            let dec = payloads[*index].full_dec();
                            let conds: Vec<String> =
                                tags.iter().map(|t| format!("{} == tag", lit(t))).collect();
                            all.extend(tags.iter().map(|t| format!("\"{t}\"")));
                            if conds.is_empty() {
                                continue;
                            }
                            w.block(format!("if ({}) {{", conds.join(" || ")), "}", |w| {
                                w.line(format!("return {vn}(Codec.apply(json, {dec}))"));
                            });
                        }
                        w.line(format!(
                            "throw Decode.Failure({}).below({})",
                            lit(&format!("must be one of [{}]", all.join(", "))),
                            lit(tag)
                        ));
                    }
                    UnionPlan::LiteralTagged { variants } => {
                        for (index, _) in variants {
                            try_variant(w, &variant_names[*index], &payloads[*index].full_dec());
                        }
                        w.line("throw Decode.Failure(\"does not match any variant\")");
                    }
                    UnionPlan::Untagged { ordered } => {
                        for index in ordered {
                            try_variant(w, &variant_names[*index], &payloads[*index].full_dec());
                        }
                        w.line("throw Decode.Failure(\"does not match any variant\")");
                    }
                    UnionPlan::Empty => {
                        w.line("throw Decode.Failure(\"does not match any variant\")");
                    }
                },
            );
        });
        w.dedent();
        w.line("}");
    }

    /// The simple names of every model class of a namespace.
    pub fn model_names(&self, ns: &str) -> BTreeSet<String> {
        self.plan
            .types
            .values()
            .filter(|t| t.namespace == ns && self.has_class(&t.id))
            .filter_map(|t| self.names.types.get(&t.id).cloned())
            .collect()
    }
}

fn try_variant(w: &mut Writer, variant: &str, dec: &str) {
    w.line(format!(
        "Codec.attempt(json, {dec})?.let {{ return {variant}(it.value) }}"
    ));
}
