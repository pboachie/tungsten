// SPDX-License-Identifier: AGPL-3.0-only
//! Java types of IR types, and the model files: records (four-state
//! presence with `Patch`, additional members kept), enums with a strict
//! decoder, sealed interfaces for unions.

use std::collections::BTreeSet;

use tungsten_emit::sdk::{AllOfPlan, EnumPlan, TypeKind, UnionPlan};
use tungsten_ir::{
    Additional, Field, NamedType, Presence, Primitive, Shape, StringFormat, TypeId, TypeRef,
};

use crate::Ctx;
use crate::java::{self, javadoc, lit};

/// How a Java type is written, decoded and encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jt {
    /// The type (boxed: usable as a type argument).
    pub java: String,
    /// A `Function<Object, T>` expression.
    pub dec: String,
    /// A `Function<T, Object>` expression.
    pub enc: String,
}

impl Jt {
    fn new(java: &str, dec: &str, enc: &str) -> Self {
        Self {
            java: java.into(),
            dec: dec.into(),
            enc: enc.into(),
        }
    }

    /// The runtime's JSON value.
    pub fn json() -> Self {
        Self::new("Object", "Decode::any", "v -> v")
    }

    fn plain(java: &str, dec: &str) -> Self {
        Self::new(java, dec, "v -> v")
    }
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

    /// The Java type of a type reference.
    pub fn jt(&self, ty: &TypeRef, from: From<'_>) -> Jt {
        self.jt_depth(ty, from, 0)
    }

    fn jt_depth(&self, ty: &TypeRef, from: From<'_>, depth: usize) -> Jt {
        if depth > 32 {
            return Jt::json();
        }
        match ty {
            TypeRef::Named(id) => {
                if self.has_class(id) {
                    let c = self.class_ref(id, from);
                    return Jt::new(&c, &format!("{c}::fromJson"), &format!("{c}::toJson"));
                }
                match self.ir.types.get(id) {
                    Some(t) if !self.plan.types.get(id).is_some_and(|p| p.cyclic) => {
                        self.shape_jt(&t.shape, from, depth + 1)
                    }
                    _ => Jt::json(),
                }
            }
            TypeRef::Inline(shape) => self.shape_jt(shape, from, depth + 1),
        }
    }

    fn shape_jt(&self, shape: &Shape, from: From<'_>, depth: usize) -> Jt {
        match shape {
            Shape::Primitive { primitive, .. } => primitive_jt(primitive),
            Shape::Enum { values, .. } => {
                if values.iter().all(|v| v.value.is_string()) && !values.is_empty() {
                    Jt::plain("String", "Decode::string")
                } else if values.iter().all(|v| v.value.is_i64()) && !values.is_empty() {
                    Jt::plain("Long", "Decode::int64")
                } else {
                    Jt::json()
                }
            }
            Shape::Const { value } => match value {
                serde_json::Value::String(_) => Jt::plain("String", "Decode::string"),
                serde_json::Value::Bool(_) => Jt::plain("Boolean", "Decode::bool"),
                serde_json::Value::Number(n) if n.is_i64() => Jt::plain("Long", "Decode::int64"),
                serde_json::Value::Number(_) => Jt::plain("Double", "Decode::number"),
                _ => Jt::json(),
            },
            Shape::Array { items, .. } => {
                let item = self.jt_depth(items, from, depth);
                Jt {
                    java: format!("List<{}>", item.java),
                    dec: wrap("Decode.list", &item.dec),
                    enc: wrap("Decode.encodeList", &item.enc),
                }
            }
            Shape::Map { values } => {
                let item = self.jt_depth(values, from, depth);
                Jt {
                    java: format!("Map<String, {}>", item.java),
                    dec: wrap("Decode.map", &item.dec),
                    enc: wrap("Decode.encodeMap", &item.enc),
                }
            }
            Shape::Nullable { inner } => {
                let i = self.jt_depth(inner, from, depth);
                Jt {
                    dec: wrap("Decode.nullable", &i.dec),
                    ..i
                }
            }
            Shape::Record { fields, additional } => {
                if fields.is_empty()
                    && let Additional::Typed { values } = additional
                {
                    let item = self.jt_depth(values, from, depth);
                    return Jt {
                        java: format!("Map<String, {}>", item.java),
                        dec: wrap("Decode.map", &item.dec),
                        enc: wrap("Decode.encodeMap", &item.enc),
                    };
                }
                Jt::new(
                    "JsonObject",
                    "v -> (JsonObject) Decode.any(Decode.object(v))",
                    "v -> v",
                )
            }
            Shape::Union(_) | Shape::Intersection { .. } | Shape::Any | Shape::Never => Jt::json(),
        }
    }
}

/// The Java type of a primitive.
pub fn primitive_jt(p: &Primitive) -> Jt {
    match p {
        Primitive::String {
            format: Some(StringFormat::Byte),
        } => Jt::new("byte[]", "Decode::bytes", "Decode::encodeBytes"),
        Primitive::String { .. } => Jt::plain("String", "Decode::string"),
        Primitive::Int32 => Jt::plain("Integer", "Decode::int32"),
        Primitive::Int64 | Primitive::Integer => Jt::plain("Long", "Decode::int64"),
        Primitive::Float | Primitive::Double | Primitive::Number => {
            Jt::plain("Double", "Decode::number")
        }
        Primitive::Bool => Jt::plain("Boolean", "Decode::bool"),
        Primitive::Bytes => Jt::new("Binary", "Decode::binary", "Binary::toJson"),
    }
}

/// One component of a generated record (a model field or an argument).
#[derive(Debug, Clone)]
pub struct Component {
    /// The Java name.
    pub name: String,
    /// The JSON member name (wire name, or arguments key).
    pub key: String,
    pub presence: Presence,
    pub jt: Jt,
    pub doc: String,
}

impl Component {
    /// The component's declared type: `Patch<T>` when null is a value.
    pub fn java_type(&self) -> String {
        match self.presence {
            Presence::RequiredNullable | Presence::OptionalNullable => {
                format!("Patch<{}>", self.jt.java)
            }
            Presence::Required | Presence::Optional => self.jt.java.clone(),
        }
    }

    fn decode(&self, obj: &str) -> String {
        match self.presence {
            Presence::RequiredNullable | Presence::OptionalNullable => {
                format!("Decode.patch({obj}, {}, {})", lit(&self.key), self.jt.dec)
            }
            Presence::Required | Presence::Optional => {
                format!("Decode.member({obj}, {}, {})", lit(&self.key), self.jt.dec)
            }
        }
    }

    fn encode(&self, obj: &str) -> String {
        match self.presence {
            Presence::RequiredNullable | Presence::OptionalNullable => {
                format!(
                    "Decode.putPatch({obj}, {}, {}, {});",
                    lit(&self.key),
                    self.name,
                    self.jt.enc
                )
            }
            Presence::Required | Presence::Optional => {
                format!(
                    "Decode.put({obj}, {}, {}, {});",
                    lit(&self.key),
                    self.name,
                    self.jt.enc
                )
            }
        }
    }
}

/// What a record keeps besides its components.
#[derive(Debug, Clone)]
pub enum Extra {
    /// Unknown members are refused when decoding (request records).
    Refused,
    /// Unknown members are kept as `additionalProperties`, decoded as this
    /// type, and written back after the components.
    Kept(Jt),
}

/// Writes a record with `fromJson`, `toJson` and a builder.
pub fn write_record(
    w: &mut tungsten_emit::Writer,
    name: &str,
    doc: &str,
    comps: &[Component],
    extra: &Extra,
) {
    javadoc(w, doc);
    let mut params: Vec<String> = comps
        .iter()
        .map(|c| format!("{} {}", c.java_type(), c.name))
        .collect();
    if let Extra::Kept(t) = extra {
        params.push(format!("Map<String, {}> additionalProperties", t.java));
    }
    w.line(format!(
        "public record {name}({}) implements Json.Encodable {{",
        params.join(", ")
    ));
    w.indent();
    let keys: Vec<String> = comps.iter().map(|c| lit(&c.key)).collect();
    w.line(format!(
        "private static final java.util.Set<String> KNOWN = java.util.Set.of({});",
        keys.join(", ")
    ));
    if let Extra::Kept(_) = extra {
        w.blank();
        w.line("/** A record; null additional members are none. */");
        w.block(format!("public {name} {{"), "}", |w| {
            w.line("additionalProperties = Decode.frozen(additionalProperties);");
        });
    }
    w.blank();
    w.line("/** Decodes the JSON value. */");
    w.block(
        format!("public static {name} fromJson(Object json) {{"),
        "}",
        |w| {
            w.line("JsonObject o = Decode.object(json);");
            let mut args: Vec<String> = comps.iter().map(|c| c.decode("o")).collect();
            match extra {
                Extra::Refused => {
                    w.line("Decode.closed(o, KNOWN);");
                }
                Extra::Kept(t) => args.push(format!("Decode.extra(o, KNOWN, {})", t.dec)),
            }
            if args.is_empty() {
                w.line(format!("return new {name}();"));
            } else {
                w.line(format!("return new {name}("));
                w.indent();
                w.indent();
                let n = args.len();
                for (i, a) in args.iter().enumerate() {
                    w.line(format!("{a}{}", if i + 1 < n { "," } else { ");" }));
                }
                w.dedent();
                w.dedent();
            }
        },
    );
    w.blank();
    w.line("@Override");
    w.block("public JsonObject toJson() {", "}", |w| {
        // `$` keeps the local apart from every component name.
        w.line("JsonObject o$ = new JsonObject();");
        for c in comps {
            w.line(c.encode("o$"));
        }
        if let Extra::Kept(t) = extra {
            w.line(format!(
                "Decode.putExtra(o$, additionalProperties, {});",
                t.enc
            ));
        }
        w.line("return o$;");
    });
    w.blank();
    w.line("/** A builder with every member unset. */");
    w.block("public static Builder builder() {", "}", |w| {
        w.line("return new Builder();");
    });
    w.blank();
    w.line(format!("/** Builds a {{@link {name}}}. */"));
    w.block("public static final class Builder {", "}", |w| {
        for c in comps {
            w.line(format!("private {} {};", c.java_type(), c.name));
        }
        if let Extra::Kept(t) = extra {
            w.line(format!(
                "private final Map<String, {}> additionalProperties = new java.util.LinkedHashMap<>();",
                t.java
            ));
        }
        w.blank();
        w.line("private Builder() {}");
        for c in comps {
            w.blank();
            javadoc(w, &c.doc);
            w.block(format!("public Builder {}({} value) {{", c.name, c.java_type()), "}", |w| {
                w.line(format!("{} = value;", c.name));
                w.line("return this;");
            });
            if matches!(c.presence, Presence::RequiredNullable | Presence::OptionalNullable) {
                w.blank();
                w.line(format!("/** Sets {} to a value. */", c.name));
                w.block(format!("public Builder {}({} value) {{", c.name, c.jt.java), "}", |w| {
                    w.line(format!("{} = value == null ? Patch.ofNull() : Patch.of(value);", c.name));
                    w.line("return this;");
                });
            }
        }
        if let Extra::Kept(t) = extra {
            w.blank();
            w.line("/** An additional member. */");
            w.block(format!("public Builder additionalProperty(String name, {} value) {{", t.java), "}", |w| {
                w.line("additionalProperties.put(name, value);");
                w.line("return this;");
            });
        }
        w.blank();
        w.line(format!("/** The {{@link {name}}}. */"));
        w.block(format!("public {name} build() {{"), "}", |w| {
            let mut fields: Vec<String> = comps.iter().map(|c| c.name.clone()).collect();
            if let Extra::Kept(_) = extra {
                fields.push("additionalProperties".into());
            }
            w.line(format!("return new {name}({});", fields.join(", ")));
        });
    });
    w.dedent();
    w.line("}");
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
        format!("{}.Binary", java::RUNTIME),
        format!("{}.Decode", java::RUNTIME),
        format!("{}.Json", java::RUNTIME),
        format!("{}.JsonObject", java::RUNTIME),
        format!("{}.Patch", java::RUNTIME),
        "java.util.List".into(),
        "java.util.Map".into(),
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

/// `v<n> -> call(v<n>, inner)`: the lambda's parameter is named by the
/// number of lambdas in `inner`, so it never shadows one of theirs.
fn wrap(call: &str, inner: &str) -> String {
    let n = inner.matches("->").count();
    format!("v{n} -> {call}(v{n}, {inner})")
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
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

impl Ctx<'_> {
    /// The record components of a record shape's fields, named by the plan's
    /// field names of `id`.
    fn components(&self, id: &TypeId, fields: &[&Field], ns: &str) -> Vec<Component> {
        let names = self.names.fields.get(id).cloned().unwrap_or_default();
        fields
            .iter()
            .enumerate()
            .map(|(i, f)| Component {
                name: names.get(i).cloned().unwrap_or_else(|| format!("field{i}")),
                key: f.wire_name.clone(),
                presence: f.presence,
                jt: self.jt(&f.ty, From::Model(ns)),
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
            let mut body = tungsten_emit::Writer::new("    ");
            match (&plan.kind, &t.shape) {
                (TypeKind::Record { .. }, Shape::Record { fields, additional }) => {
                    let refs: Vec<&Field> = fields.iter().collect();
                    let comps = self.components(&t.id, &refs, &ns);
                    let extra = match additional {
                        Additional::Typed { values } => {
                            Extra::Kept(self.jt(values, From::Model(&ns)))
                        }
                        _ => Extra::Kept(Jt::json()),
                    };
                    write_record(&mut body, &name, &type_doc(t), &comps, &extra);
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
                    write_record(
                        &mut body,
                        &name,
                        &type_doc(t),
                        &comps,
                        &Extra::Kept(Jt::json()),
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
            let mut w = java::file(ir, &package, &imports);
            w.line(text.trim_end());
            out.push((
                format!("src/main/java/{}/{name}.java", package.replace('.', "/")),
                w.finish(),
            ));
        }
        out
    }

    fn write_enum(
        &self,
        w: &mut tungsten_emit::Writer,
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
            format!("{}L", v.as_i64().unwrap_or(0))
        }
        fn string_literal(v: &serde_json::Value) -> String {
            lit(v.as_str().unwrap_or(""))
        }
        let (base, literal): (&str, fn(&serde_json::Value) -> String) = match plan {
            EnumPlan::Integers => ("long", long_literal),
            _ => ("String", string_literal),
        };
        javadoc(w, &type_doc(t));
        w.line(format!("public enum {name} implements Json.Encodable {{"));
        w.indent();
        let n = values.len();
        for (i, v) in values.iter().enumerate() {
            let doc = v
                .doc
                .as_ref()
                .and_then(|d| d.description.clone().or_else(|| d.summary.clone()))
                .unwrap_or_default();
            javadoc(w, &doc);
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
        w.line(format!("private final {base} value;"));
        w.blank();
        w.block(format!("{name}({base} value) {{"), "}", |w| {
            w.line("this.value = value;");
        });
        w.blank();
        w.line("/** The value on the wire. */");
        w.block(format!("public {base} value() {{"), "}", |w| {
            w.line("return value;");
        });
        w.blank();
        w.line("/** Decodes the JSON value; a value outside the enum is refused. */");
        w.block(
            format!("public static {name} fromJson(Object json) {{"),
            "}",
            |w| {
                w.block(format!("for ({name} v : values()) {{"), "}", |w| {
                    w.block("if (Json.deepEqual(v.value, json)) {", "}", |w| {
                        w.line("return v;");
                    });
                });
                w.line(format!(
                    "throw new Decode.Failure({});",
                    lit(&format!("expected one of [{}]", shown.join(", ")))
                ));
            },
        );
        w.blank();
        w.line("@Override");
        w.block("public Object toJson() {", "}", |w| {
            w.line("return value;");
        });
        w.dedent();
        w.line("}");
    }

    fn write_union(
        &self,
        w: &mut tungsten_emit::Writer,
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
            if n == name || n == "Builder" {
                n.push_str("Variant");
            }
            while variant_names.contains(&n) {
                n.push('2');
            }
            variant_names.push(n);
        }
        let payloads: Vec<Jt> = union
            .variants
            .iter()
            .map(|v| self.jt(&v.ty, From::Elsewhere))
            .collect();
        javadoc(w, &type_doc(t));
        w.line(format!(
            "public sealed interface {name} extends Json.Encodable permits {} {{",
            variant_names
                .iter()
                .map(|v| format!("{name}.{v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        w.indent();
        for (i, v) in union.variants.iter().enumerate() {
            let vn = &variant_names[i];
            let p = &payloads[i];
            let doc = match &v.tag {
                Some(tag) => format!("The variant tagged {tag}."),
                None => format!("The variant {}.", v.name.wire),
            };
            javadoc(w, &doc);
            w.block(
                format!("record {vn}({} value) implements {name} {{", p.java),
                "}",
                |w| {
                    w.line("@Override");
                    w.block("public Object toJson() {", "}", |w| {
                        w.line(format!(
                            "return value == null ? null : Decode.encode(value, {});",
                            p.enc
                        ));
                    });
                },
            );
            w.blank();
        }
        w.line("/** Decodes the JSON value. */");
        w.block(
            format!("static {name} fromJson(Object json) {{"),
            "}",
            |w| match plan {
                UnionPlan::Tagged {
                    tag,
                    variants,
                    default,
                } => {
                    w.line("JsonObject o = Decode.object(json);");
                    w.line(format!("Object tag = o.get({});", lit(tag)));
                    let mut all: Vec<String> = vec![];
                    for (index, tags) in variants {
                        let vn = &variant_names[*index];
                        let dec = &payloads[*index].dec;
                        let conds: Vec<String> = tags
                            .iter()
                            .map(|t| format!("{}.equals(tag)", lit(t)))
                            .collect();
                        all.extend(tags.iter().map(|t| format!("\"{t}\"")));
                        if conds.is_empty() || *default == Some(*index) {
                            continue;
                        }
                        w.block(format!("if ({}) {{", conds.join(" || ")), "}", |w| {
                            w.line(format!("return new {vn}(Decode.apply(json, {dec}));"));
                        });
                    }
                    if let Some(index) = default {
                        // OpenAPI 3.2 `defaultMapping`: any other object.
                        w.line(format!(
                            "return new {}(Decode.apply(json, {}));",
                            variant_names[*index], payloads[*index].dec
                        ));
                    } else {
                        w.line(format!(
                            "throw new Decode.Failure({}).below({});",
                            lit(&format!("must be one of [{}]", all.join(", "))),
                            lit(tag)
                        ));
                    }
                }
                UnionPlan::LiteralTagged { variants } => {
                    for (index, _) in variants {
                        try_variant(w, &variant_names[*index], &payloads[*index].dec);
                    }
                    w.line("throw new Decode.Failure(\"does not match any variant\");");
                }
                UnionPlan::Untagged { ordered } => {
                    for index in ordered {
                        try_variant(w, &variant_names[*index], &payloads[*index].dec);
                    }
                    w.line("throw new Decode.Failure(\"does not match any variant\");");
                }
                UnionPlan::Empty => {
                    w.line("throw new Decode.Failure(\"does not match any variant\");");
                }
            },
        );
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

fn try_variant(w: &mut tungsten_emit::Writer, variant: &str, dec: &str) {
    w.block("try {", "} catch (Decode.Failure next) {", |w| {
        w.line(format!("return new {variant}(Decode.apply(json, {dec}));"));
    });
    w.indent();
    w.line("// the next variant");
    w.dedent();
    w.line("}");
}
