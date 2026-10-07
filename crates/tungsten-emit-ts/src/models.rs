// SPDX-License-Identifier: AGPL-3.0-only
//! TypeScript types and Zod schemas for IR shapes, and the
//! `src/models/<namespace>.ts` files.
//!
//! Every named type is written twice under one name: an explicit type
//! alias (the documented source of truth, wire field names, presence per
//! planning/03) and a Zod schema constant checked against it with
//! `satisfies z.ZodType<T>`. Schemas are declared dependencies first; a
//! reference inside a cycle or to another namespace's module is wrapped in
//! `z.lazy`, and the members of a cycle are annotated `z.ZodType<T>` so
//! TypeScript never has to infer through the cycle.

use serde_json::Value;
use tungsten_emit::{CommentStyle, Imports, Writer};
use tungsten_ir::{
    Additional, Constraints, Field, NamedType, Presence, Primitive, Shape, StringFormat, TypeId,
    TypeRef, Union, UnionStrategy, Variant,
};

use crate::plan::Plan;
use crate::ts::{doc_text, json_lit, json_type, paragraphs, prop_key, string_lit};

/// Operator precedence of a rendered TypeScript type, for parenthesizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Prec {
    Union,
    Intersection,
    Atom,
}

/// A rendered TypeScript type.
#[derive(Debug, Clone)]
pub(crate) struct Ty {
    pub text: String,
    pub prec: Prec,
}

impl Ty {
    pub(crate) fn atom(text: impl Into<String>) -> Ty {
        Ty {
            text: text.into(),
            prec: Prec::Atom,
        }
    }
    /// The text, parenthesized when it binds looser than `min`.
    pub(crate) fn at(&self, min: Prec) -> String {
        if self.prec < min {
            format!("({})", self.text)
        } else {
            self.text.clone()
        }
    }
    /// `T | null`.
    pub(crate) fn nullable(&self) -> Ty {
        Ty {
            text: format!("{} | null", self.text),
            prec: Prec::Union,
        }
    }
}

/// `A | B | ...` of distinct members (in order); `never` when empty.
pub(crate) fn union_of(members: Vec<Ty>) -> Ty {
    let mut seen: Vec<Ty> = vec![];
    for m in members {
        if !seen.iter().any(|s| s.text == m.text) {
            seen.push(m);
        }
    }
    match seen.len() {
        0 => Ty::atom("never"),
        1 => seen.remove(0),
        _ => Ty {
            // Intersections are parenthesized for readability only.
            text: seen
                .iter()
                .map(|m| {
                    if m.prec == Prec::Intersection {
                        m.at(Prec::Atom)
                    } else {
                        m.at(Prec::Union)
                    }
                })
                .collect::<Vec<_>>()
                .join(" | "),
            prec: Prec::Union,
        },
    }
}

/// Renders shapes from one module's point of view.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypeCx<'p, 'a> {
    pub plan: &'p Plan<'a>,
    /// The model namespace being written; `None` outside `src/models/`.
    pub home: Option<&'p str>,
    /// Strongly connected component of the named type being written.
    pub scc: Option<usize>,
}

impl<'p, 'a> TypeCx<'p, 'a> {
    pub(crate) fn outside(plan: &'p Plan<'a>) -> Self {
        TypeCx {
            plan,
            home: None,
            scc: None,
        }
    }

    /// The name of a named type as written in this module.
    pub(crate) fn type_name(&self, id: &TypeId) -> Option<String> {
        let info = self.plan.types.get(id)?;
        Some(if self.home == Some(info.ns.as_str()) {
            info.name.clone()
        } else {
            format!("{}.{}", self.plan.alias(&info.ns), info.name)
        })
    }

    /// Whether a reference to `id` must be lazy: it is in another module
    /// (whose evaluation may not have finished, with cyclic imports) or in
    /// the cycle being declared.
    fn lazy(&self, id: &TypeId) -> bool {
        let Some(home) = self.home else {
            return false;
        };
        self.plan
            .types
            .get(id)
            .is_some_and(|t| t.ns != home || Some(t.scc) == self.scc)
    }

    pub(crate) fn ts_ref(&self, r: &TypeRef) -> Ty {
        match r {
            TypeRef::Named(id) => Ty::atom(self.type_name(id).unwrap_or_else(|| "unknown".into())),
            TypeRef::Inline(s) => self.ts_shape(s),
        }
    }

    pub(crate) fn ts_shape(&self, s: &Shape) -> Ty {
        match s {
            Shape::Primitive { primitive, .. } => Ty::atom(ts_primitive(primitive)),
            Shape::Enum { values, .. } => union_of(
                values
                    .iter()
                    .map(|v| Ty::atom(json_type(&v.value)))
                    .collect(),
            ),
            Shape::Const { value } => Ty::atom(json_type(value)),
            Shape::Array { items, .. } => {
                let inner = self.ts_ref(items);
                if inner.prec == Prec::Atom {
                    Ty::atom(format!("{}[]", inner.text))
                } else {
                    Ty::atom(format!("Array<{}>", inner.text))
                }
            }
            Shape::Map { values } => {
                Ty::atom(format!("{{ [key: string]: {} }}", self.ts_ref(values).text))
            }
            Shape::Record { fields, additional } => {
                let mut parts: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        format!(
                            "{}{}: {}",
                            prop_key(&f.wire_name),
                            optional_mark(f.presence),
                            self.field_ts(f).text
                        )
                    })
                    .collect();
                if let Some(sig) = self.index_signature(fields, additional) {
                    parts.push(sig);
                }
                // Never empty: a record without fields has an index signature.
                Ty::atom(format!("{{ {} }}", parts.join("; ")))
            }
            Shape::Union(u) => union_of(u.variants.iter().map(|v| self.variant_ts(u, v)).collect()),
            Shape::Intersection { members } => match members.len() {
                0 => Ty::atom("unknown"),
                1 => self.ts_ref(&members[0]),
                _ => Ty {
                    text: members
                        .iter()
                        .map(|m| self.ts_ref(m).at(Prec::Atom))
                        .collect::<Vec<_>>()
                        .join(" & "),
                    prec: Prec::Intersection,
                },
            },
            Shape::Nullable { inner } => self.ts_ref(inner).nullable(),
            Shape::Any => Ty::atom("unknown"),
            Shape::Never => Ty::atom("never"),
        }
    }

    /// A field's value type, with `| null` for nullable presences.
    pub(crate) fn field_ts(&self, f: &Field) -> Ty {
        let t = self.ts_ref(&f.ty);
        match f.presence {
            Presence::RequiredNullable | Presence::OptionalNullable => t.nullable(),
            Presence::Required | Presence::Optional => t,
        }
    }

    /// The index signature of a record, if it admits extra properties.
    fn index_signature(&self, fields: &[Field], additional: &Additional) -> Option<String> {
        match additional {
            Additional::Closed if fields.is_empty() => Some("[key: string]: never".into()),
            Additional::Closed => None,
            Additional::Open => Some("[key: string]: unknown".into()),
            // Declared fields must be assignable to the index signature, so
            // a record with fields keeps `unknown`; Zod checks the extras.
            Additional::Typed { .. } if !fields.is_empty() => Some("[key: string]: unknown".into()),
            Additional::Typed { values } => {
                Some(format!("[key: string]: {}", self.ts_ref(values).text))
            }
        }
    }

    /// A union variant, narrowed to its tag when the union is tagged and the
    /// variant does not already fix the tag.
    fn variant_ts(&self, u: &Union, v: &Variant) -> Ty {
        let base = self.ts_ref(&v.ty);
        match self.narrowing(u, v) {
            Some((prop, tag)) => Ty {
                text: format!(
                    "{} & {{ {}: {} }}",
                    base.at(Prec::Atom),
                    prop_key(prop),
                    string_lit(tag)
                ),
                prec: Prec::Intersection,
            },
            None => base,
        }
    }

    /// The (property, tag) a tagged variant must be narrowed with: none when
    /// the union is not tagged or the variant record already requires that
    /// constant.
    fn narrowing<'u>(&self, u: &'u Union, v: &'u Variant) -> Option<(&'u str, &'u str)> {
        if u.strategy != UnionStrategy::Tagged {
            return None;
        }
        let prop = u.discriminator.as_ref()?.property.as_str();
        let tag = v.tag.as_deref()?;
        let fixed = self.record_of(&v.ty).is_some_and(|(fields, _)| {
            fields.iter().any(|f| {
                f.wire_name == prop
                    && f.presence == Presence::Required
                    && matches!(&f.ty, TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Const { value } if value.as_str() == Some(tag)))
            })
        });
        (!fixed).then_some((prop, tag))
    }

    /// The record behind a reference (following one named type).
    fn record_of<'r>(&'r self, r: &'r TypeRef) -> Option<(&'r [Field], &'r Additional)> {
        record_of(self.plan, r)
    }

    // ------------------------------------------------------------- zod

    pub(crate) fn zod_ref(&self, r: &TypeRef) -> String {
        match r {
            TypeRef::Named(id) => match self.type_name(id) {
                Some(name) if self.lazy(id) => format!("z.lazy(() => {name})"),
                Some(name) => name,
                None => "z.unknown()".into(),
            },
            TypeRef::Inline(s) => self.zod_shape(s),
        }
    }

    pub(crate) fn zod_shape(&self, s: &Shape) -> String {
        match s {
            Shape::Primitive {
                primitive,
                constraints,
            } => zod_primitive(primitive, constraints),
            Shape::Enum { values, .. } => {
                let vals: Vec<&Value> = values.iter().map(|v| &v.value).collect();
                if vals.is_empty() {
                    "z.never()".into()
                } else if vals.iter().all(|v| v.is_string()) {
                    format!(
                        "z.enum([{}])",
                        vals.iter()
                            .map(|v| json_lit(v))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else if vals.iter().all(|v| !v.is_object() && !v.is_array()) {
                    format!(
                        "z.literal([{}])",
                        vals.iter()
                            .map(|v| json_lit(v))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    format!(
                        "z.union([{}])",
                        vals.iter()
                            .map(|v| zod_const(v))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Shape::Const { value } => zod_const(value),
            Shape::Array {
                items, min, max, ..
            } => {
                let mut out = format!("z.array({})", self.zod_ref(items));
                if let Some(n) = min {
                    out.push_str(&format!(".min({n})"));
                }
                if let Some(n) = max {
                    out.push_str(&format!(".max({n})"));
                }
                out
            }
            Shape::Map { values } => format!("z.record(z.string(), {})", self.zod_ref(values)),
            Shape::Record { fields, additional } => {
                let body: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", prop_key(&f.wire_name), self.field_zod(f)))
                    .collect();
                record_ctor(
                    &format!("{{ {} }}", body.join(", ")),
                    body.is_empty(),
                    additional,
                    self,
                )
            }
            Shape::Union(u) => self.zod_union(u),
            Shape::Intersection { members } => match members.len() {
                0 => "z.unknown()".into(),
                _ => {
                    let mut it = members.iter();
                    let mut out = it.next().map(|m| self.zod_ref(m)).unwrap_or_default();
                    for m in it {
                        out = format!("z.intersection({out}, {})", self.zod_ref(m));
                    }
                    out
                }
            },
            Shape::Nullable { inner } => format!("{}.nullable()", self.zod_ref(inner)),
            Shape::Any => "z.unknown()".into(),
            Shape::Never => "z.never()".into(),
        }
    }

    /// A field's schema with its presence applied.
    pub(crate) fn field_zod(&self, f: &Field) -> String {
        let base = self.zod_ref(&f.ty);
        match f.presence {
            Presence::Required => base,
            Presence::RequiredNullable => format!("{base}.nullable()"),
            Presence::Optional => format!("{base}.exactOptional()"),
            Presence::OptionalNullable => format!("{base}.nullable().exactOptional()"),
        }
    }

    fn zod_union(&self, u: &Union) -> String {
        if u.variants.is_empty() {
            return "z.never()".into();
        }
        if u.strategy == UnionStrategy::Tagged
            && let Some(d) = &u.discriminator
            && let Some(options) = self.discriminated_options(u)
        {
            return format!(
                "z.discriminatedUnion({}, [{}])",
                string_lit(&d.property),
                options.join(", ")
            );
        }
        let parts: Vec<String> = u
            .variants
            .iter()
            .map(|v| {
                let base = self.zod_ref(&v.ty);
                match self.narrowing(u, v) {
                    Some((prop, tag)) => format!(
                        "{base}.and(z.object({{ {}: z.literal({}) }}))",
                        prop_key(prop),
                        string_lit(tag)
                    ),
                    None => base,
                }
            })
            .collect();
        format!("z.union([{}])", parts.join(", "))
    }

    /// The options of `z.discriminatedUnion`, when every variant is an
    /// object schema whose type TypeScript can see (declared earlier in
    /// this module, not annotated as part of a cycle) and whose tags are
    /// distinct. Otherwise `None`, and the union is a `z.union` of narrowed
    /// variants.
    fn discriminated_options(&self, u: &Union) -> Option<Vec<String>> {
        let prop = &u.discriminator.as_ref()?.property;
        let mut tags: Vec<&str> = vec![];
        let mut out = vec![];
        for v in &u.variants {
            let tag = v.tag.as_deref()?;
            if tags.contains(&tag) {
                return None;
            }
            tags.push(tag);
            let narrowed = self.narrowing(u, v).is_some();
            match &v.ty {
                TypeRef::Named(id) => {
                    let info = self.plan.types.get(id)?;
                    let visible = self.home == Some(info.ns.as_str())
                        && !info.cyclic
                        && Some(info.scc) != self.scc;
                    if !visible || self.record_of(&v.ty).is_none() {
                        return None;
                    }
                    if narrowed {
                        out.push(format!(
                            "{}.extend({{ {}: z.literal({}) }})",
                            info.name,
                            prop_key(prop),
                            string_lit(tag)
                        ));
                    } else {
                        out.push(info.name.clone());
                    }
                }
                TypeRef::Inline(s) => {
                    let Shape::Record { fields, additional } = s.as_ref() else {
                        return None;
                    };
                    let mut body: Vec<String> = fields
                        .iter()
                        .filter(|f| !(narrowed && &f.wire_name == prop))
                        .map(|f| format!("{}: {}", prop_key(&f.wire_name), self.field_zod(f)))
                        .collect();
                    if narrowed {
                        body.push(format!(
                            "{}: z.literal({})",
                            prop_key(prop),
                            string_lit(tag)
                        ));
                    }
                    out.push(record_ctor(
                        &format!("{{ {} }}", body.join(", ")),
                        body.is_empty(),
                        additional,
                        self,
                    ));
                }
            }
        }
        Some(out)
    }
}

/// `z.strictObject`, `z.looseObject` or `z.object(...).catchall(...)` over
/// an object literal of field schemas.
fn record_ctor(shape: &str, empty: bool, additional: &Additional, cx: &TypeCx<'_, '_>) -> String {
    let shape = if empty { "{}" } else { shape };
    match additional {
        Additional::Closed => format!("z.strictObject({shape})"),
        Additional::Open => format!("z.looseObject({shape})"),
        Additional::Typed { values } => {
            format!("z.object({shape}).catchall({})", cx.zod_ref(values))
        }
    }
}

fn optional_mark(p: Presence) -> &'static str {
    match p {
        Presence::Optional | Presence::OptionalNullable => "?",
        Presence::Required | Presence::RequiredNullable => "",
    }
}

pub(crate) fn ts_primitive(p: &Primitive) -> &'static str {
    match p {
        Primitive::String { .. } => "string",
        Primitive::Int32
        | Primitive::Int64
        | Primitive::Integer
        | Primitive::Float
        | Primitive::Double
        | Primitive::Number => "number",
        Primitive::Bool => "boolean",
        Primitive::Bytes => "Uint8Array",
    }
}

/// The Zod schema of a primitive with its constraints. String formats use
/// Zod's checks only where they accept every valid wire value; patterns go
/// through `withPattern`, which skips a pattern the engine cannot compile.
pub(crate) fn zod_primitive(p: &Primitive, c: &Constraints) -> String {
    let mut out = match p {
        Primitive::String { format } => match format {
            Some(StringFormat::Uuid) => "z.guid()".to_string(),
            Some(StringFormat::Email) => "z.email({ pattern: z.regexes.unicodeEmail })".to_string(),
            Some(StringFormat::DateTime) => "z.iso.datetime({ offset: true })".to_string(),
            Some(StringFormat::Date) => "z.iso.date()".to_string(),
            Some(StringFormat::Ipv4) => "z.ipv4()".to_string(),
            Some(StringFormat::Ipv6) => "z.ipv6()".to_string(),
            _ => "z.string()".to_string(),
        },
        Primitive::Int32 => "z.int32()".to_string(),
        Primitive::Int64 | Primitive::Integer => "integer()".to_string(),
        Primitive::Float | Primitive::Double | Primitive::Number => "z.number()".to_string(),
        Primitive::Bool => return "z.boolean()".to_string(),
        Primitive::Bytes => return "z.instanceof(Uint8Array)".to_string(),
    };
    if matches!(p, Primitive::String { .. }) {
        if let Some(n) = c.min_length {
            out.push_str(&format!(".min({n})"));
        }
        if let Some(n) = c.max_length {
            out.push_str(&format!(".max({n})"));
        }
        if let Some(pattern) = &c.pattern {
            out = format!("withPattern({out}, {})", string_lit(pattern));
        }
    } else {
        for (method, v) in [
            ("gte", &c.minimum),
            ("lte", &c.maximum),
            ("gt", &c.exclusive_minimum),
            ("lt", &c.exclusive_maximum),
            ("multipleOf", &c.multiple_of),
        ] {
            if let Some(n) = v {
                out.push_str(&format!(".{method}({n})"));
            }
        }
    }
    out
}

/// The schema admitting exactly one JSON value.
pub(crate) fn zod_const(v: &Value) -> String {
    match v {
        Value::Null => "z.null()".into(),
        Value::Array(items) => format!(
            "z.tuple([{}])",
            items.iter().map(zod_const).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "z.strictObject({{ {} }})",
            map.iter()
                .map(|(k, v)| format!("{}: {}", prop_key(k), zod_const(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => format!("z.literal({})", json_lit(v)),
    }
}

/// Doc lines for a field: its description plus the facts its type does not
/// show.
pub(crate) fn field_doc(plan: &Plan<'_>, f: &Field) -> String {
    let mut notes: Vec<String> = vec![];
    if f.read_only {
        notes.push("Read-only: set by the server, ignored in requests.".into());
    }
    if f.write_only {
        notes.push("Write-only: never returned by the server.".into());
    }
    if f.sensitive {
        notes.push("Sensitive: never log or persist this value.".into());
    }
    if let Some(shape) = resolve(plan, &f.ty) {
        notes.extend(shape_notes(shape));
    }
    if let Some(d) = &f.default {
        notes.push(format!("@defaultValue `{}`", json_lit(d)));
    }
    if f.deprecated {
        notes.push("@deprecated".into());
    }
    paragraphs(std::iter::once(doc_text(f.doc.as_ref())).chain(notes))
}

/// The record behind a reference (following one named type).
pub(crate) fn record_of<'r>(
    plan: &'r Plan<'_>,
    r: &'r TypeRef,
) -> Option<(&'r [Field], &'r Additional)> {
    match resolve(plan, r)? {
        Shape::Record { fields, additional } => Some((fields, additional)),
        _ => None,
    }
}

/// The shape behind a reference (following one named type).
pub(crate) fn resolve<'r>(plan: &'r Plan<'_>, r: &'r TypeRef) -> Option<&'r Shape> {
    match r {
        TypeRef::Inline(s) => Some(s),
        TypeRef::Named(id) => plan.ir.types.get(id).map(|t| &t.shape),
    }
}

/// Facts about a shape that its TypeScript type cannot express.
pub(crate) fn shape_notes(shape: &Shape) -> Vec<String> {
    let mut notes = vec![];
    match shape {
        Shape::Primitive {
            primitive: Primitive::Int64,
            ..
        } => notes.push(
            "A 64-bit integer: values beyond 2^53 lose precision as a JavaScript number.".into(),
        ),
        Shape::Primitive {
            primitive: Primitive::Integer,
            ..
        } => notes.push(
            "An integer of unspecified size: values beyond 2^53 lose precision as a JavaScript number.".into(),
        ),
        Shape::Primitive {
            primitive: Primitive::String { format: Some(f) },
            ..
        } => notes.push(format!("Format: `{}`.", format_name(f))),
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
        StringFormat::Byte => "byte (base64)".into(),
        StringFormat::Password => "password".into(),
        StringFormat::Other(s) => s.clone(),
    }
}

/// The source of `src/models/<file>.ts` for one namespace.
pub(crate) fn models_file(plan: &Plan<'_>, ns: &str, header: &str) -> String {
    let model = plan.model_ns(ns);
    let ids: &[TypeId] = model.map_or(&[], |m| m.types.as_slice());
    let mut body = Writer::new("  ");
    let mut imports = Imports::new();
    let mut uses = Uses::default();
    for id in ids {
        let Some(nt) = plan.ir.types.get(id) else {
            continue;
        };
        let Some(info) = plan.types.get(id) else {
            continue;
        };
        let cx = TypeCx {
            plan,
            home: Some(ns),
            scc: Some(info.scc),
        };
        body.blank();
        write_named(&mut body, &cx, nt, &info.name, info.cyclic);
        uses.add_shape(plan, Some(ns), &nt.shape);
    }
    if !ids.is_empty() {
        imports.add("zod", "z");
    }
    if uses.integer {
        imports.add("../internal.js", "integer");
    }
    if uses.pattern {
        imports.add("../internal.js", "withPattern");
    }
    let mut w = Writer::new("  ");
    w.line(header);
    w.blank();
    w.line(format!("// Types and schemas of the `{ns}` namespace."));
    w.blank();
    write_imports(&mut w, &imports);
    for other in &uses.namespaces {
        w.line(format!(
            "import * as {} from \"./{}.js\";",
            plan.alias(other),
            plan.model_ns(other).map_or("", |m| m.file.as_str())
        ));
    }
    let mut out = w.finish();
    out.push('\n');
    let text = body.finish();
    if text.trim().is_empty() {
        out.push_str("export {};\n");
    } else {
        out.push_str(&text);
    }
    out
}

/// Write `import { a, b } from "m";` lines.
/// Write `import { a, b } from "m";` lines: packages before relative
/// modules, and per module the value import before the type-only one.
pub(crate) fn write_imports(w: &mut Writer, imports: &Imports) {
    let values: Vec<(&str, Vec<&str>)> = imports.values().collect();
    let types: Vec<(&str, Vec<&str>)> = imports.types().collect();
    let mut modules: Vec<&str> = values.iter().chain(&types).map(|(m, _)| *m).collect();
    modules.sort_by_key(|m| (m.starts_with('.'), *m));
    modules.dedup();
    for m in modules {
        if let Some((_, names)) = values.iter().find(|(v, _)| *v == m) {
            w.line(format!(
                "import {{ {} }} from {};",
                names.join(", "),
                string_lit(m)
            ));
        }
        if let Some((_, names)) = types.iter().find(|(t, _)| *t == m) {
            w.line(format!(
                "import type {{ {} }} from {};",
                names.join(", "),
                string_lit(m)
            ));
        }
    }
}

/// What a module needs to import for the shapes it renders.
#[derive(Debug, Clone, Default)]
pub(crate) struct Uses {
    /// The `integer()` helper.
    pub integer: bool,
    /// The `withPattern()` helper.
    pub pattern: bool,
    /// Model namespaces referenced (other than the module's own).
    pub namespaces: std::collections::BTreeSet<String>,
}

impl Uses {
    /// Record what rendering `r` from `home` needs.
    pub(crate) fn add_ref(&mut self, plan: &Plan<'_>, home: Option<&str>, r: &TypeRef) {
        match r {
            TypeRef::Named(id) => {
                if let Some(t) = plan.types.get(id)
                    && Some(t.ns.as_str()) != home
                {
                    self.namespaces.insert(t.ns.clone());
                }
            }
            TypeRef::Inline(s) => self.add_shape(plan, home, s),
        }
    }

    /// Record what rendering `shape` from `home` needs.
    pub(crate) fn add_shape(&mut self, plan: &Plan<'_>, home: Option<&str>, shape: &Shape) {
        match shape {
            Shape::Primitive {
                primitive,
                constraints,
            } => {
                self.integer |= matches!(primitive, Primitive::Int64 | Primitive::Integer);
                self.pattern |=
                    matches!(primitive, Primitive::String { .. }) && constraints.pattern.is_some();
            }
            Shape::Array { items, .. } => self.add_ref(plan, home, items),
            Shape::Map { values } => self.add_ref(plan, home, values),
            Shape::Nullable { inner } => self.add_ref(plan, home, inner),
            Shape::Record { fields, additional } => {
                for f in fields {
                    self.add_ref(plan, home, &f.ty);
                }
                if let Additional::Typed { values } = additional {
                    self.add_ref(plan, home, values);
                }
            }
            Shape::Union(u) => {
                for v in &u.variants {
                    self.add_ref(plan, home, &v.ty);
                }
            }
            Shape::Intersection { members } => {
                for m in members {
                    self.add_ref(plan, home, m);
                }
            }
            Shape::Enum { .. } | Shape::Const { .. } | Shape::Any | Shape::Never => {}
        }
    }
}

/// One named type: doc, type alias and schema constant.
fn write_named(w: &mut Writer, cx: &TypeCx<'_, '_>, nt: &NamedType, name: &str, cyclic: bool) {
    let notes = shape_notes(&nt.shape);
    w.doc(
        CommentStyle::JsDoc,
        &paragraphs(std::iter::once(doc_text(nt.doc.as_ref())).chain(notes)),
    );
    match &nt.shape {
        Shape::Record { fields, additional } => {
            w.line(format!("export type {name} = {{"));
            w.indent();
            for f in fields {
                w.doc(CommentStyle::JsDoc, &field_doc(cx.plan, f));
                w.line(format!(
                    "{}{}: {};",
                    prop_key(&f.wire_name),
                    optional_mark(f.presence),
                    cx.field_ts(f).text
                ));
            }
            if let Some(sig) = cx.index_signature(fields, additional) {
                w.line(format!("{sig};"));
            }
            w.dedent();
            w.line("};");
            let ctor = match additional {
                Additional::Closed => "z.strictObject(".to_string(),
                Additional::Open => "z.looseObject(".to_string(),
                Additional::Typed { .. } => "z.object(".to_string(),
            };
            let close = match additional {
                Additional::Typed { values } => format!("}}).catchall({})", cx.zod_ref(values)),
                _ => "})".to_string(),
            };
            let decl = schema_decl(name, cyclic);
            if fields.is_empty() {
                w.line(format!(
                    "{decl}{ctor}{{{close}{};",
                    schema_tail(name, cyclic)
                ));
            } else {
                w.line(format!("{decl}{ctor}{{"));
                w.indent();
                for f in fields {
                    w.line(format!("{}: {},", prop_key(&f.wire_name), cx.field_zod(f)));
                }
                w.dedent();
                w.line(format!("{close}{};", schema_tail(name, cyclic)));
            }
        }
        shape => {
            w.line(format!("export type {name} = {};", cx.ts_shape(shape).text));
            w.line(format!(
                "{}{}{};",
                schema_decl(name, cyclic),
                cx.zod_shape(shape),
                schema_tail(name, cyclic)
            ));
        }
    }
}

fn schema_decl(name: &str, cyclic: bool) -> String {
    if cyclic {
        format!("export const {name}: z.ZodType<{name}> = ")
    } else {
        format!("export const {name} = ")
    }
}

fn schema_tail(name: &str, cyclic: bool) -> String {
    if cyclic {
        String::new()
    } else {
        format!(" satisfies z.ZodType<{name}>")
    }
}
