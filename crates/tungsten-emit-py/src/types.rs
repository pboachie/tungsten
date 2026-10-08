// SPDX-License-Identifier: AGPL-3.0-only
//! Python types for IR shapes, and the `<module>/models/<namespace>.py`
//! files.
//!
//! A shape renders in two flavors. The schema flavor is what Pydantic
//! validates: constraints as `Annotated` metadata, `_internal.Bytes`
//! (base64 in JSON), `_internal.Int32`, discriminated and left-to-right
//! unions. The hint flavor is the plain type a signature shows (`str`,
//! `bytes`, `A | B`). Models are written in the schema flavor; method
//! signatures and `Result` types in the hint flavor.
//!
//! Records become `_internal.Model` classes; every other named type is a
//! PEP 695 `type` alias, which Python evaluates lazily, so declaration
//! order never matters and recursion needs no special casing. Models refer
//! to other namespaces through module aliases imported at the end of their
//! module, and `models/__init__.py` resolves every forward reference once
//! all namespaces are loaded.

use std::cell::RefCell;
use std::collections::BTreeSet;

use serde_json::Value;
use tungsten_emit::Writer;
use tungsten_ir::{
    Additional, Constraints, Field, NamedType, Presence, Primitive, Shape, StringFormat, TypeId,
    TypeRef, Union, UnionStrategy,
};

use crate::plan::{Plan, field_names};
use crate::py::{
    PyImports, after_imports, doc_text, docstring, dunder_all, dunder_all_cmp, json_lit,
    paragraphs, string_lit, two_blank,
};

/// A rendered Python type: the members of a union, in order (one member
/// for a non-union type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PyTy {
    members: Vec<String>,
}

impl PyTy {
    pub(crate) fn one(text: impl Into<String>) -> PyTy {
        PyTy {
            members: vec![text.into()],
        }
    }

    pub(crate) fn any() -> PyTy {
        PyTy::one("Any")
    }

    pub(crate) fn is_any(&self) -> bool {
        self.members.iter().any(|m| m == "Any")
    }

    /// The union of `parts`, flattened and without duplicates; `Any`
    /// absorbs every other member.
    pub(crate) fn union(parts: impl IntoIterator<Item = PyTy>) -> PyTy {
        let mut members: Vec<String> = vec![];
        for p in parts {
            for m in p.members {
                if !members.contains(&m) {
                    members.push(m);
                }
            }
        }
        if members.iter().any(|m| m == "Any") {
            return PyTy::any();
        }
        PyTy { members }
    }

    /// `T | None`.
    pub(crate) fn nullable(&self) -> PyTy {
        PyTy::union([self.clone(), PyTy::one("None")])
    }

    /// The members in source order: as given, `None` last (ruff RUF036).
    fn ordered(&self) -> Vec<&str> {
        let mut parts: Vec<&str> = self
            .members
            .iter()
            .map(String::as_str)
            .filter(|m| *m != "None")
            .collect();
        if self.members.iter().any(|m| m == "None") {
            parts.push("None");
        }
        parts
    }

    /// The type as source.
    pub(crate) fn text(&self) -> String {
        if self.members.is_empty() {
            return "Never".into();
        }
        self.ordered().join(" | ")
    }

    fn literal_values(&self) -> Option<Vec<String>> {
        let mut out = vec![];
        for m in &self.members {
            if m == "None" {
                continue;
            }
            let inner = m.strip_prefix("Literal[")?.strip_suffix(']')?;
            out.push(inner.to_string());
        }
        Some(out)
    }
}

/// Which rendering of a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    /// The plain type for signatures and results.
    Hint,
    /// The validated type for models and validators.
    Schema,
}

/// What a module imports for the types it renders.
#[derive(Debug, Clone, Default)]
pub(crate) struct Uses {
    /// Names from `typing`.
    pub typing: BTreeSet<&'static str>,
    /// Names from `pydantic`.
    pub pydantic: BTreeSet<&'static str>,
    /// Names from `tungsten_runtime`.
    pub runtime: BTreeSet<&'static str>,
    /// The package's `_internal` module.
    pub internal: bool,
    /// Model namespaces referenced (other than the module's own).
    pub namespaces: BTreeSet<String>,
}

impl Uses {
    pub(crate) fn merge(&mut self, other: &Uses) {
        self.typing.extend(other.typing.iter().copied());
        self.pydantic.extend(other.pydantic.iter().copied());
        self.runtime.extend(other.runtime.iter().copied());
        self.internal |= other.internal;
        self.namespaces.extend(other.namespaces.iter().cloned());
    }

    /// Add the imports of these uses to `imports`. `parent` is the
    /// relative path of the package root from the module (`.` or `..`);
    /// `models` the relative path of the models package.
    pub(crate) fn add_to(
        &self,
        plan: &Plan<'_>,
        imports: &mut PyImports,
        parent: &str,
        models: &str,
    ) {
        for t in &self.typing {
            imports.add("typing", t);
        }
        for p in &self.pydantic {
            imports.add("pydantic", p);
        }
        for r in &self.runtime {
            imports.add("tungsten_runtime", r);
        }
        if self.internal {
            imports.add(parent, "_internal");
        }
        for ns in &self.namespaces {
            if let Some(m) = plan.model_ns(ns) {
                imports.add_as(models, &m.file, &m.alias);
            }
        }
    }
}

/// Renders shapes from one module's point of view.
#[derive(Debug)]
pub(crate) struct Cx<'p, 'a> {
    pub plan: &'p Plan<'a>,
    /// The model namespace being written; `None` outside `models/`.
    pub home: Option<&'p str>,
    pub uses: RefCell<Uses>,
}

impl<'p, 'a> Cx<'p, 'a> {
    pub(crate) fn new(plan: &'p Plan<'a>, home: Option<&'p str>) -> Self {
        Cx {
            plan,
            home,
            uses: RefCell::new(Uses::default()),
        }
    }

    fn typing(&self, name: &'static str) {
        self.uses.borrow_mut().typing.insert(name);
    }

    fn pydantic(&self, name: &'static str) {
        self.uses.borrow_mut().pydantic.insert(name);
    }

    pub(crate) fn runtime(&self, name: &'static str) {
        self.uses.borrow_mut().runtime.insert(name);
    }

    fn internal(&self, name: &str) -> String {
        self.uses.borrow_mut().internal = true;
        format!("_internal.{name}")
    }

    fn any(&self) -> PyTy {
        self.typing("Any");
        PyTy::any()
    }

    fn never(&self, flavor: Flavor) -> PyTy {
        match flavor {
            Flavor::Hint => {
                self.typing("Never");
                PyTy::one("Never")
            }
            Flavor::Schema => PyTy::one(self.internal("Never")),
        }
    }

    /// `Annotated[base, meta...]`.
    fn annotated(&self, base: &PyTy, meta: &[String]) -> PyTy {
        if meta.is_empty() {
            return base.clone();
        }
        self.typing("Annotated");
        PyTy::one(format!("Annotated[{}, {}]", base.text(), meta.join(", ")))
    }

    /// The name of a named type as written in this module.
    pub(crate) fn type_name(&self, id: &TypeId) -> Option<String> {
        let info = self.plan.types.get(id)?;
        if self.home == Some(info.ns.as_str()) {
            return Some(info.name.clone());
        }
        let m = self.plan.model_ns(&info.ns)?;
        self.uses.borrow_mut().namespaces.insert(info.ns.clone());
        Some(format!("{}.{}", m.alias, info.name))
    }

    pub(crate) fn ty(&self, r: &TypeRef, flavor: Flavor) -> PyTy {
        match r {
            TypeRef::Named(id) => self.type_name(id).map_or_else(|| self.any(), PyTy::one),
            TypeRef::Inline(s) => self.shape(s, flavor),
        }
    }

    pub(crate) fn shape(&self, s: &Shape, flavor: Flavor) -> PyTy {
        match s {
            Shape::Primitive {
                primitive,
                constraints,
            } => self.primitive(primitive, constraints, flavor),
            Shape::Enum { values, .. } => {
                let values: Vec<&Value> = values.iter().map(|v| &v.value).collect();
                self.one_of(&values, flavor)
            }
            Shape::Const { value } => self.one_of(&[value], flavor),
            Shape::Array {
                items, min, max, ..
            } => {
                let list = PyTy::one(format!("list[{}]", self.ty(items, flavor).text()));
                if flavor == Flavor::Hint {
                    return list;
                }
                let mut args = vec![];
                if let Some(n) = min {
                    args.push(format!("min_length={n}"));
                }
                if let Some(n) = max {
                    args.push(format!("max_length={n}"));
                }
                self.with_field(&list, &args)
            }
            Shape::Map { values } => {
                PyTy::one(format!("dict[str, {}]", self.ty(values, flavor).text()))
            }
            Shape::Record { fields, additional } => match additional {
                // An inline record has no class (TG0733): a mapping of its
                // extras' type when it has no fixed fields.
                Additional::Typed { values } if fields.is_empty() => {
                    PyTy::one(format!("dict[str, {}]", self.ty(values, flavor).text()))
                }
                _ => {
                    self.typing("Any");
                    PyTy::one("dict[str, Any]")
                }
            },
            Shape::Union(u) => self.union(u, flavor),
            Shape::Intersection { members } => match members.len() {
                0 => self.any(),
                1 => self.ty(&members[0], flavor),
                _ if flavor == Flavor::Hint => self.any(),
                _ => {
                    let parts: Vec<String> = members
                        .iter()
                        .map(|m| self.value(&self.ty(m, Flavor::Schema)))
                        .collect();
                    let all_of = format!(
                        "{}(lambda: ({},))",
                        self.internal("all_of"),
                        parts.join(", ")
                    );
                    let base = self.any();
                    self.annotated(&base, &[all_of])
                }
            },
            Shape::Nullable { inner } => self.ty(inner, flavor).nullable(),
            Shape::Any => self.any(),
            Shape::Never => self.never(flavor),
        }
    }

    /// `Annotated[base, Field(args)]`, or `base` without arguments.
    fn with_field(&self, base: &PyTy, args: &[String]) -> PyTy {
        if args.is_empty() {
            return base.clone();
        }
        self.pydantic("Field");
        self.annotated(base, &[format!("Field({})", args.join(", "))])
    }

    fn primitive(&self, p: &Primitive, c: &Constraints, flavor: Flavor) -> PyTy {
        let hint = flavor == Flavor::Hint;
        match p {
            Primitive::String {
                format: Some(StringFormat::Byte),
            }
            | Primitive::Bytes => {
                if hint {
                    PyTy::one("bytes")
                } else {
                    PyTy::one(self.internal("Bytes"))
                }
            }
            Primitive::String { .. } => {
                let base = PyTy::one("str");
                if hint {
                    return base;
                }
                let mut args = vec![];
                if let Some(n) = c.min_length {
                    args.push(format!("min_length={n}"));
                }
                if let Some(n) = c.max_length {
                    args.push(format!("max_length={n}"));
                }
                let mut meta = vec![];
                if !args.is_empty() {
                    self.pydantic("Field");
                    meta.push(format!("Field({})", args.join(", ")));
                }
                if let Some(pattern) = &c.pattern {
                    meta.push(format!(
                        "{}({})",
                        self.internal("pattern"),
                        string_lit(pattern)
                    ));
                }
                self.annotated(&base, &meta)
            }
            Primitive::Bool => PyTy::one("bool"),
            Primitive::Int32 | Primitive::Int64 | Primitive::Integer => {
                if hint {
                    return PyTy::one("int");
                }
                let base = if *p == Primitive::Int32 {
                    PyTy::one(self.internal("Int32"))
                } else {
                    PyTy::one("int")
                };
                self.with_field(&base, &number_args(c))
            }
            Primitive::Float | Primitive::Double | Primitive::Number => {
                let base = PyTy::one("float");
                if hint {
                    return base;
                }
                self.with_field(&base, &number_args(c))
            }
        }
    }

    /// The type admitting exactly `values`: a `Literal` of the scalars that
    /// can be literals (strings, integers, booleans) plus `None`, or, when
    /// a value cannot be a literal (a float, an object, an array),
    /// `_internal.one_of` over all of them.
    fn one_of(&self, values: &[&Value], flavor: Flavor) -> PyTy {
        if values.is_empty() {
            return self.never(flavor);
        }
        let literal = |v: &Value| match v {
            Value::String(_) | Value::Bool(_) => Some(json_lit(v)),
            Value::Number(n) if n.is_i64() || n.is_u64() => Some(n.to_string()),
            _ => None,
        };
        let mut lits: Vec<String> = vec![];
        let mut null = false;
        let mut other = false;
        for v in values {
            match (v, literal(v)) {
                (Value::Null, _) => null = true,
                (_, Some(l)) => {
                    if !lits.contains(&l) {
                        lits.push(l);
                    }
                }
                (_, None) => other = true,
            }
        }
        if other {
            if flavor == Flavor::Hint {
                return self.any();
            }
            let list = format!(
                "[{}]",
                values
                    .iter()
                    .map(|v| json_lit(v))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let base = self.any();
            let check = format!("{}({list})", self.internal("one_of"));
            return self.annotated(&base, &[check]);
        }
        let mut parts = vec![];
        if !lits.is_empty() {
            self.typing("Literal");
            parts.push(PyTy::one(format!("Literal[{}]", lits.join(", "))));
        }
        if null {
            parts.push(PyTy::one("None"));
        }
        PyTy::union(parts)
    }

    fn union(&self, u: &Union, flavor: Flavor) -> PyTy {
        if u.variants.is_empty() {
            return self.never(flavor);
        }
        if flavor == Flavor::Hint {
            return PyTy::union(u.variants.iter().map(|v| self.ty(&v.ty, Flavor::Hint)));
        }
        match u.strategy {
            UnionStrategy::Tagged
                if u.discriminator.is_some() && u.variants.iter().all(|v| v.tag.is_some()) =>
            {
                self.tagged(u)
            }
            UnionStrategy::Literal => {
                let joined = PyTy::union(u.variants.iter().map(|v| self.ty(&v.ty, Flavor::Schema)));
                match joined.literal_values() {
                    Some(values) if !values.is_empty() => {
                        let null = joined.members.iter().any(|m| m == "None");
                        let mut parts = vec![PyTy::one(format!("Literal[{}]", values.join(", ")))];
                        if null {
                            parts.push(PyTy::one("None"));
                        }
                        PyTy::union(parts)
                    }
                    _ => joined,
                }
            }
            UnionStrategy::Tagged | UnionStrategy::Untagged => {
                let joined = PyTy::union(u.variants.iter().map(|v| self.ty(&v.ty, Flavor::Schema)));
                if joined.members.len() < 2 {
                    return joined;
                }
                self.pydantic("Field");
                self.annotated(
                    &joined,
                    &["Field(union_mode=\"left_to_right\")".to_string()],
                )
            }
        }
    }

    /// A tagged union: Pydantic's discriminator on a field when every
    /// variant is a distinct model that fixes the tag as a required
    /// literal under one attribute name, else a callable discriminator
    /// that reads the tag's wire name (`_internal.tag`) with each variant
    /// labelled by its tag.
    fn tagged(&self, u: &Union) -> PyTy {
        let Some(d) = &u.discriminator else {
            return self.any();
        };
        let prop = d.property.as_str();
        let mut attr: Option<String> = None;
        let mut field_form = true;
        let mut seen_tags: Vec<&str> = vec![];
        let mut seen_ids: Vec<&TypeId> = vec![];
        for v in &u.variants {
            let tag = v.tag.as_deref().unwrap_or_default();
            let fixed = match &v.ty {
                TypeRef::Named(id) => match self.plan.ir.types.get(id).map(|t| &t.shape) {
                    Some(Shape::Record { fields, .. }) => {
                        let names = field_names(fields.iter().map(|f| &f.name));
                        fields.iter().zip(names).find_map(|(f, name)| {
                            let is_tag = f.wire_name == prop
                                && f.presence == Presence::Required
                                && matches!(&f.ty, TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Const { value } if value.as_str() == Some(tag)));
                            is_tag.then_some((id, name))
                        })
                    }
                    _ => None,
                },
                TypeRef::Inline(_) => None,
            };
            match fixed {
                Some((id, name))
                    if !seen_tags.contains(&tag)
                        && !seen_ids.contains(&id)
                        && attr.as_ref().is_none_or(|a| *a == name) =>
                {
                    seen_tags.push(tag);
                    seen_ids.push(id);
                    attr = Some(name);
                }
                _ => field_form = false,
            }
        }
        if field_form && let Some(attr) = attr {
            let joined = PyTy::union(u.variants.iter().map(|v| self.ty(&v.ty, Flavor::Schema)));
            self.pydantic("Field");
            return self.annotated(
                &joined,
                &[format!("Field(discriminator={})", string_lit(&attr))],
            );
        }
        self.pydantic("Tag");
        let labelled: Vec<PyTy> = u
            .variants
            .iter()
            .map(|v| {
                let base = self.ty(&v.ty, Flavor::Schema);
                let tag = format!("Tag({})", string_lit(v.tag.as_deref().unwrap_or_default()));
                self.typing("Annotated");
                PyTy::one(format!("Annotated[{}, {tag}]", base.text()))
            })
            .collect();
        let joined = PyTy::union(labelled);
        let disc = format!("{}({})", self.internal("tag"), string_lit(prop));
        self.annotated(&joined, &[disc])
    }

    /// A field's type with its presence applied (without `Unset`), as a
    /// validator or a non-optional annotation needs it.
    pub(crate) fn field_value(&self, f: &Field, flavor: Flavor) -> PyTy {
        let t = self.ty(&f.ty, flavor);
        match f.presence {
            Presence::RequiredNullable | Presence::OptionalNullable => t.nullable(),
            Presence::Required | Presence::Optional => t,
        }
    }

    /// `t` as a runtime expression (a validator's type, a tuple element).
    /// Type checkers reject `|` between `Annotated[...]` and another type
    /// outside annotations, so such a union is written `_internal.union(...)`.
    pub(crate) fn value(&self, t: &PyTy) -> String {
        if t.members.len() > 1 && t.members.iter().any(|m| m.starts_with("Annotated[")) {
            format!("{}({})", self.internal("union"), t.ordered().join(", "))
        } else {
            t.text()
        }
    }

    /// `T | Unset` for a value that may be left out (`T` itself when it is
    /// `Any`).
    pub(crate) fn omittable(&self, t: &PyTy) -> PyTy {
        if t.is_any() {
            return t.clone();
        }
        self.runtime("Unset");
        PyTy::union([t.clone(), PyTy::one("Unset")])
    }
}

/// `ge=`, `le=`, `gt=`, `lt=`, `multiple_of=` arguments of a number.
fn number_args(c: &Constraints) -> Vec<String> {
    let mut args = vec![];
    for (name, v) in [
        ("ge", &c.minimum),
        ("le", &c.maximum),
        ("gt", &c.exclusive_minimum),
        ("lt", &c.exclusive_maximum),
        ("multiple_of", &c.multiple_of),
    ] {
        if let Some(n) = v {
            args.push(format!("{name}={n}"));
        }
    }
    args
}

/// The shape behind a reference (following one named type).
pub(crate) fn resolve<'r>(plan: &'r Plan<'_>, r: &'r TypeRef) -> Option<&'r Shape> {
    match r {
        TypeRef::Inline(s) => Some(s),
        TypeRef::Named(id) => plan.ir.types.get(id).map(|t| &t.shape),
    }
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

/// Facts about a shape that its Python type does not show.
pub(crate) fn shape_notes(shape: &Shape) -> Vec<String> {
    let mut notes = vec![];
    match shape {
        Shape::Primitive {
            primitive: Primitive::String { format: Some(f) },
            ..
        } => match f {
            StringFormat::Byte => notes.push("Base64 on the wire; `bytes` here.".into()),
            f => notes.push(format!("Format: `{}`.", format_name(f))),
        },
        Shape::Primitive {
            primitive: Primitive::Bytes,
            ..
        } => notes.push("Binary; base64 when carried in JSON.".into()),
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

/// The docstring of a field: its description plus the facts its type does
/// not show.
pub(crate) fn field_doc(plan: &Plan<'_>, f: &Field, attr: &str) -> String {
    let mut notes: Vec<String> = vec![];
    if attr != f.wire_name {
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
    if let Some(shape) = resolve(plan, &f.ty) {
        notes.extend(shape_notes(shape));
    }
    if let Some(d) = &f.default {
        notes.push(format!(
            "Default (applied by the server): `{}`.",
            json_lit(d)
        ));
    }
    if f.deprecated {
        notes.push("Deprecated.".into());
    }
    paragraphs(std::iter::once(doc_text(f.doc.as_ref())).chain(notes))
}

/// `Field(...)` arguments for a model field: its default (`UNSET` when it
/// may be left out) and its wire name when it differs. Both aliases are set
/// separately rather than `alias=`, so type checkers take the Python name
/// as the `__init__` parameter.
fn field_spec(cx: &Cx<'_, '_>, f: &Field, attr: &str) -> Option<String> {
    let optional = matches!(f.presence, Presence::Optional | Presence::OptionalNullable);
    if attr == f.wire_name {
        return optional.then(|| {
            cx.runtime("UNSET");
            "UNSET".to_string()
        });
    }
    cx.pydantic("Field");
    let wire = string_lit(&f.wire_name);
    let mut args = vec![];
    if optional {
        cx.runtime("UNSET");
        args.push("default=UNSET".to_string());
    }
    args.push(format!("validation_alias={wire}"));
    args.push(format!("serialization_alias={wire}"));
    Some(format!("Field({})", args.join(", ")))
}

/// The annotation of a model field (schema flavor, presence applied).
fn field_annotation(cx: &Cx<'_, '_>, f: &Field) -> String {
    let t = cx.field_value(f, Flavor::Schema);
    match f.presence {
        Presence::Optional | Presence::OptionalNullable => cx.omittable(&t).text(),
        Presence::Required | Presence::RequiredNullable => t.text(),
    }
}

/// One named type: a model class for a record, a `type` alias otherwise.
fn write_named(w: &mut Writer, cx: &Cx<'_, '_>, nt: &NamedType, name: &str) {
    let notes = shape_notes(&nt.shape);
    let doc = paragraphs(std::iter::once(doc_text(nt.doc.as_ref())).chain(notes));
    match &nt.shape {
        Shape::Record { fields, additional } => {
            cx.uses.borrow_mut().internal = true;
            w.line(format!("class {name}(_internal.Model):"));
            w.indent();
            docstring(w, &doc);
            if !doc.is_empty() {
                w.blank();
            }
            cx.pydantic("ConfigDict");
            let extra = match additional {
                Additional::Closed => "forbid",
                Additional::Open | Additional::Typed { .. } => "allow",
            };
            w.line(format!("model_config = ConfigDict(extra=\"{extra}\")"));
            if let Additional::Typed { values } = additional {
                w.blank();
                cx.pydantic("Field");
                let v = cx.ty(values, Flavor::Schema).text();
                w.line(format!(
                    "__pydantic_extra__: dict[str, {v}] = Field(init=False)  # pyright: ignore[reportIncompatibleVariableOverride]"
                ));
                docstring(w, "Additional properties, each validated as this type.");
            }
            let names = field_names(fields.iter().map(|f| &f.name));
            // A blank line after the configuration and around documented
            // fields; undocumented fields stay together.
            let mut spaced = true;
            for (f, attr) in fields.iter().zip(&names) {
                let doc = field_doc(cx.plan, f, attr);
                if spaced || !doc.is_empty() {
                    w.blank();
                }
                let ann = field_annotation(cx, f);
                match field_spec(cx, f, attr) {
                    Some(spec) => w.line(format!("{attr}: {ann} = {spec}")),
                    None => w.line(format!("{attr}: {ann}")),
                };
                docstring(w, &doc);
                spaced = !doc.is_empty();
            }
            w.dedent();
        }
        shape => {
            let t = cx.shape(shape, Flavor::Schema);
            w.line(format!("type {name} = {}", t.text()));
            docstring(w, &doc);
        }
    }
}

/// The source of `<module>/models/<file>.py` for one namespace.
pub(crate) fn models_file(plan: &Plan<'_>, ns: &str, header: &str) -> String {
    let model = plan.model_ns(ns);
    let ids: &[TypeId] = model.map_or(&[], |m| m.types.as_slice());
    let cx = Cx::new(plan, Some(ns));
    let mut body = Writer::new("    ");
    for id in ids {
        let (Some(nt), Some(info)) = (plan.ir.types.get(id), plan.types.get(id)) else {
            continue;
        };
        two_blank(&mut body);
        write_named(&mut body, &cx, nt, &info.name);
    }
    let uses = cx.uses.into_inner();
    let mut imports = PyImports::default();
    imports.add("__future__", "annotations");
    let mut local = uses.clone();
    local.namespaces.clear();
    local.add_to(plan, &mut imports, "..", ".");

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    docstring(&mut w, &format!("Models of the `{ns}` namespace."));
    w.blank();
    w.line(imports.render());
    let mut out = w.finish();
    let text = body.finish();
    let text = text.trim_start_matches('\n');
    if !text.trim().is_empty() {
        out.push_str(after_imports(text));
        out.push_str(text);
    }
    if !uses.namespaces.is_empty() {
        let mut cross = PyImports::default();
        for other in &uses.namespaces {
            if let Some(m) = plan.model_ns(other) {
                cross.add_as(".", &m.file, &m.alias);
            }
        }
        out.push_str("\n\n# Imported last: models of different namespaces refer to each other.\n");
        for line in cross.render().lines() {
            out.push_str(line);
            out.push_str("  # noqa: E402\n");
        }
    }
    out
}

/// The source of `<module>/models/__init__.py`: every namespace module,
/// forward references resolved once all are loaded, and, for a
/// single-namespace API, every type re-exported.
pub(crate) fn models_init(plan: &Plan<'_>, header: &str) -> String {
    let mut imports = PyImports::default();
    imports.add("__future__", "annotations");
    imports.add("..", "_internal");
    for m in &plan.models {
        imports.add(".", &m.file);
    }
    let single = (plan.ir.namespaces.len() == 1)
        .then(|| {
            plan.ir
                .namespaces
                .first()
                .and_then(|n| plan.model_ns(&n.name.wire))
        })
        .flatten();
    let mut exported: Vec<String> = plan.models.iter().map(|m| m.file.clone()).collect();
    if let Some(m) = single {
        for id in &m.types {
            if let Some(info) = plan.types.get(id) {
                imports.add(&format!(".{}", m.file), &info.name);
                exported.push(info.name.clone());
            }
        }
    }
    exported.sort_by(|a, b| dunder_all_cmp(a, b));
    exported.dedup();
    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    let doc = if single.is_some() {
        "The API's models (also importable from their namespace module)."
    } else {
        "The API's models, one module per namespace."
    };
    docstring(&mut w, doc);
    w.blank();
    w.line(imports.render());
    w.blank();
    w.line(format!(
        "_internal.rebuild({})",
        plan.models
            .iter()
            .map(|m| m.file.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    w.blank();
    w.line(dunder_all(&exported));
    w.finish()
}
