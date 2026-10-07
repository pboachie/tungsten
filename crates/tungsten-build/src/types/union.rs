// SPDX-License-Identifier: AGPL-3.0-only
//! `oneOf`/`anyOf` (planning/03 "Unions").
//!
//! `{type: "null"}` members only make the union nullable. With one member
//! left the union is that member. Otherwise the strategy is, in order:
//!
//! 1. `Tagged` by an explicit `discriminator` (mapping values resolve like
//!    `$ref`s or name a component; variants without a mapping entry use
//!    their component name, or their `const` value of the property).
//! 2. `Tagged` on a property every variant record requires with a distinct
//!    `const` (or one-value `enum`), with the mapping derived from those
//!    values. This is the RPC envelope shape (ZROtext workflow tools).
//! 3. `Literal` when every variant is a primitive, enum or const and no two
//!    can accept the same JSON value.
//! 4. `Untagged`, with candidates ordered most constrained first (more
//!    required fields, then closed records, then more `const` fields, then
//!    spec order) and TG0301.
//!
//! Variant names: the referenced type's name, else the member's `const`
//! tag value, else its kind for inline primitives, else `Variant<n>`.

use std::cmp::Reverse;

use serde_json::Value;
use tungsten_core::Diagnostic;
use tungsten_ir::{
    Additional, Discriminator, Field, Ident, Presence, Primitive, Shape, TypeRef, Union,
    UnionStrategy, Variant,
};
use tungsten_openapi::{DocId, RefTarget, join_pointer};

use super::schema::{self, JsonType, Object};
use super::{At, Body, Conv, TypeBuilder, child, names};

/// A union member after conversion.
#[derive(Debug)]
pub(super) struct Candidate {
    pub name: Ident,
    pub ty: TypeRef,
    /// Position in the spec's member list.
    pub index: usize,
    /// The member's value of the union's raw tag property, if any.
    pub tag: Option<String>,
}

/// A tag property found on the raw member schemas, with each member's value.
#[derive(Debug)]
struct RawTags {
    property: String,
    values: Vec<String>,
}

/// The JSON values a variant can accept, for the `Literal` check.
enum Accepts {
    Types(Vec<JsonType>),
    Values(Vec<Value>),
}

impl Accepts {
    fn of(shape: &Shape) -> Option<Self> {
        Some(match shape {
            Shape::Primitive { primitive, .. } => Self::Types(match primitive {
                Primitive::String { .. } | Primitive::Bytes => vec![JsonType::String],
                Primitive::Int32 | Primitive::Int64 | Primitive::Integer => vec![JsonType::Integer],
                Primitive::Float | Primitive::Double | Primitive::Number => {
                    vec![JsonType::Number, JsonType::Integer]
                }
                Primitive::Bool => vec![JsonType::Boolean],
            }),
            Shape::Enum { values, .. } => {
                Self::Values(values.iter().map(|v| v.value.clone()).collect())
            }
            Shape::Const { value } => Self::Values(vec![value.clone()]),
            _ => return None,
        })
    }

    fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Types(a), Self::Types(b)) => a.iter().any(|t| b.contains(t)),
            (Self::Values(a), Self::Values(b)) => a.iter().any(|v| b.contains(v)),
            (Self::Values(values), Self::Types(types))
            | (Self::Types(types), Self::Values(values)) => {
                values.iter().any(|v| types.contains(&JsonType::of(v)))
            }
        }
    }
}

impl<'a> TypeBuilder<'a> {
    /// A schema with `oneOf` (or `anyOf`) members at `members`.
    pub(super) fn convert_union(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        key: &str,
        members: Vec<RefTarget>,
        hint: &[String],
    ) -> Conv {
        if key == "oneOf" && map.contains_key("anyOf") {
            self.report(
                Diagnostic::info(
                    "TG0303",
                    "`anyOf` next to `oneOf` is not modelled and was ignored",
                ),
                &target.keyword("anyOf"),
            );
        }
        if members.is_empty() {
            self.report(
                Diagnostic::warning(
                    "TG0307",
                    format!("`{key}` has no members, so the schema admits no value"),
                ),
                &target.keyword(key),
            );
            return Conv::shape(Shape::Never);
        }
        let mut nullable = false;
        let mut kept = vec![];
        for (index, m) in members.into_iter().enumerate() {
            if self.is_null_only(&m) {
                nullable = true;
            } else {
                kept.push((index, m));
            }
        }
        if let [(_, only)] = kept.as_slice() {
            let mut conv = Conv::of(self.resolve(ns, only, hint));
            conv.nullable |= nullable;
            return conv;
        }
        if kept.is_empty() {
            return Conv::shape(Shape::Const { value: Value::Null });
        }
        let raw = self.raw_tags(&kept);
        let mut candidates = vec![];
        for (pos, (index, m)) in kept.iter().enumerate() {
            let tag = raw.as_ref().map(|r| r.values[pos].clone());
            let label = match &tag {
                Some(t) => names::extend(hint, t),
                None => names::concat(hint, &["variant".to_string(), (index + 1).to_string()]),
            };
            let resolved = self.resolve(ns, m, &label);
            nullable |= resolved.nullable;
            let is_ref = self.ws.get(m).and_then(|v| v.get("$ref")).is_some();
            let referenced = match &resolved.ty {
                TypeRef::Named(id) if is_ref => self.entries.get(id).map(|e| e.ty.name.clone()),
                _ => None,
            };
            let name = match (referenced, &tag, &resolved.ty) {
                (Some(name), _, _) => name,
                (None, Some(t), _) => Ident::new(t.as_str()),
                (None, None, TypeRef::Inline(shape)) => kind_name(shape, *index),
                (None, None, TypeRef::Named(_)) => variant_name(*index),
            };
            candidates.push(Candidate {
                name,
                ty: resolved.ty,
                index: *index,
                tag,
            });
        }
        let discriminator = map.get("discriminator").and_then(Value::as_object);
        let raw_property = raw.map(|r| r.property);
        Conv {
            body: self.finish_union(target, discriminator, raw_property, candidates),
            nullable,
        }
    }

    /// Classify converted candidates (see the module docs).
    ///
    /// `raw_property` is a tag property found on the raw member schemas
    /// (each candidate carries its value). It decides `Tagged` when the
    /// converted variants cannot be inspected yet: a variant that is still
    /// being built because the union is part of a cycle.
    pub(super) fn finish_union(
        &mut self,
        target: &RefTarget,
        discriminator: Option<&Object>,
        raw_property: Option<String>,
        candidates: Vec<Candidate>,
    ) -> Body {
        let mut variants: Vec<Candidate> = vec![];
        for c in candidates {
            if !variants.iter().any(|v| v.ty == c.ty) {
                variants.push(c);
            }
        }
        if variants.len() == 1 {
            return Body::of(variants.remove(0).ty);
        }
        if let Some(disc) = discriminator
            && let Some((property, tags)) = self.explicit_tags(target, disc, &variants)
        {
            return self.tagged(target, variants, property, tags);
        }
        if let Some((property, tags)) = self.const_tags(&variants) {
            let tags = tags.into_iter().map(|t| vec![t]).collect();
            return self.tagged(target, variants, property, tags);
        }
        let raw_tags: Option<Vec<Vec<String>>> = variants
            .iter()
            .map(|v| match (&v.ty, &v.tag) {
                (TypeRef::Named(_), Some(tag)) => Some(vec![tag.clone()]),
                _ => None,
            })
            .collect();
        if let (Some(property), Some(tags)) = (raw_property, raw_tags) {
            return self.tagged(target, variants, property, tags);
        }
        if self.is_literal(&variants) {
            let n = variants.len();
            return Body::Shape(self.union_shape(
                target,
                variants,
                vec![None; n],
                None,
                UnionStrategy::Literal,
            ));
        }
        self.untagged(target, variants)
    }

    fn tagged(
        &mut self,
        target: &RefTarget,
        variants: Vec<Candidate>,
        property: String,
        tags: Vec<Vec<String>>,
    ) -> Body {
        let mut mapping = vec![];
        for (v, values) in variants.iter().zip(&tags) {
            if let TypeRef::Named(id) = &v.ty {
                mapping.extend(values.iter().map(|t| (t.clone(), id.clone())));
            }
        }
        let first: Vec<Option<String>> = tags.into_iter().map(|t| t.into_iter().next()).collect();
        let discriminator = Discriminator { property, mapping };
        Body::Shape(self.union_shape(
            target,
            variants,
            first,
            Some(discriminator),
            UnionStrategy::Tagged,
        ))
    }

    fn untagged(&mut self, target: &RefTarget, mut variants: Vec<Candidate>) -> Body {
        variants.sort_by_cached_key(|v| self.sniff_key(v));
        let order: Vec<String> = variants.iter().map(|v| v.name.pascal()).collect();
        self.report(
            Diagnostic::warning(
                "TG0301",
                format!(
                    "untagged union: the runtime sniffs {} candidates in order ({})",
                    variants.len(),
                    order.join(", ")
                ),
            )
            .with_help("add a discriminator, or a const property that tells the variants apart"),
            &target.doc_pointer(),
        );
        let n = variants.len();
        Body::Shape(self.union_shape(
            target,
            variants,
            vec![None; n],
            None,
            UnionStrategy::Untagged,
        ))
    }

    fn union_shape(
        &mut self,
        target: &RefTarget,
        variants: Vec<Candidate>,
        tags: Vec<Option<String>>,
        discriminator: Option<Discriminator>,
        strategy: UnionStrategy,
    ) -> Shape {
        let mut idents: Vec<Ident> = variants.iter().map(|v| v.name.clone()).collect();
        self.disambiguate_variants(target, "union variant", &mut idents);
        let variants = variants
            .into_iter()
            .zip(idents)
            .zip(tags)
            .map(|((v, name), tag)| Variant {
                name,
                ty: v.ty,
                tag,
            })
            .collect();
        Shape::Union(Union {
            variants,
            discriminator,
            strategy,
        })
    }

    /// Sniffing order: more required fields, closed records, more `const`
    /// fields, then spec order.
    fn sniff_key(&self, v: &Candidate) -> (Reverse<usize>, bool, Reverse<usize>, usize) {
        match self.shape_of(&v.ty) {
            Some(Shape::Record { fields, additional }) => {
                let required = fields
                    .iter()
                    .filter(|f| {
                        matches!(f.presence, Presence::Required | Presence::RequiredNullable)
                    })
                    .count();
                let consts = fields
                    .iter()
                    .filter(|f| matches!(self.shape_of(&f.ty), Some(Shape::Const { .. })))
                    .count();
                (
                    Reverse(required),
                    *additional != Additional::Closed,
                    Reverse(consts),
                    v.index,
                )
            }
            Some(Shape::Const { .. }) => (Reverse(0), true, Reverse(1), v.index),
            _ => (Reverse(0), true, Reverse(0), v.index),
        }
    }

    fn is_literal(&self, variants: &[Candidate]) -> bool {
        let accepts: Option<Vec<Accepts>> = variants
            .iter()
            .map(|v| self.shape_of(&v.ty).and_then(Accepts::of))
            .collect();
        let Some(accepts) = accepts else {
            return false;
        };
        accepts
            .iter()
            .enumerate()
            .all(|(i, a)| accepts[i + 1..].iter().all(|b| !a.overlaps(b)))
    }

    // ----- tags ---------------------------------------------------------

    /// The tag values of an explicit discriminator, one list per variant.
    /// `None` (with TG0306) when the discriminator cannot apply.
    fn explicit_tags(
        &mut self,
        target: &RefTarget,
        disc: &Object,
        variants: &[Candidate],
    ) -> Option<(String, Vec<Vec<String>>)> {
        let Some(property) = disc.get("propertyName").and_then(Value::as_str) else {
            self.report(
                Diagnostic::warning("TG0306", "discriminator has no `propertyName`; ignored"),
                &target.keyword("discriminator"),
            );
            return None;
        };
        if let Some(v) = variants.iter().find(|v| matches!(v.ty, TypeRef::Inline(_))) {
            self.report(
                Diagnostic::warning(
                    "TG0306",
                    format!(
                        "discriminator ignored: variant `{}` is not an object schema",
                        v.name.pascal()
                    ),
                ),
                &target.keyword("discriminator"),
            );
            return None;
        }
        let mut tags: Vec<Vec<String>> = vec![vec![]; variants.len()];
        let mapping = disc.get("mapping").and_then(Value::as_object);
        for (value, reference) in mapping.into_iter().flatten() {
            let at = child(target, &["discriminator", "mapping", value]).doc_pointer();
            let shown = schema::value_text(reference);
            let id = reference
                .as_str()
                .and_then(|r| self.mapping_target(target.doc, r))
                .and_then(|t| self.type_id_for(&t));
            let Some(id) = id else {
                let message = format!(
                    "discriminator mapping `{value}` → `{shown}` does not resolve to a schema; entry ignored"
                );
                self.report(Diagnostic::warning("TG0306", message), &at);
                continue;
            };
            match variants
                .iter()
                .position(|v| v.ty == TypeRef::Named(id.clone()))
            {
                Some(p) => tags[p].push(value.clone()),
                None => {
                    let message = format!(
                        "discriminator mapping `{value}` → `{shown}` is not a variant of this union; entry ignored"
                    );
                    self.report(Diagnostic::warning("TG0306", message), &at);
                }
            }
        }
        for (v, values) in variants.iter().zip(tags.iter_mut()) {
            if !values.is_empty() {
                continue;
            }
            let TypeRef::Named(id) = &v.ty else {
                continue;
            };
            let implicit = if self.is_component(id) {
                self.entries.get(id).map(|e| e.ty.name.wire.clone())
            } else {
                self.const_field(&v.ty, property)
            };
            match implicit {
                Some(t) => values.push(t),
                None => {
                    let message = format!(
                        "union variant `{}` has no discriminator value",
                        v.name.pascal()
                    );
                    self.report(
                        Diagnostic::warning("TG0306", message),
                        &target.keyword("discriminator"),
                    );
                }
            }
        }
        Some((property.to_string(), tags))
    }

    /// A mapping value: a `$ref`-style reference, or a bare component name.
    fn mapping_target(&self, doc: DocId, reference: &str) -> Option<RefTarget> {
        if reference.contains('#') || reference.contains('/') {
            self.ws.resolve(doc, reference)
        } else {
            let pointer = join_pointer("/components/schemas", reference);
            self.ws.resolve(doc, &format!("#{pointer}"))
        }
    }

    /// A property every variant record requires with a distinct literal
    /// value, and those values in variant order.
    fn const_tags(&self, variants: &[Candidate]) -> Option<(String, Vec<String>)> {
        let records: Vec<&[Field]> = variants
            .iter()
            .map(|v| self.record_fields(&v.ty))
            .collect::<Option<_>>()?;
        'property: for f in records[0] {
            let mut values: Vec<String> = vec![];
            for fields in &records {
                let Some(g) = fields.iter().find(|g| g.wire_name == f.wire_name) else {
                    continue 'property;
                };
                let tag = (g.presence == Presence::Required)
                    .then(|| self.literal_text(&g.ty))
                    .flatten();
                match tag {
                    Some(t) if !values.contains(&t) => values.push(t),
                    _ => continue 'property,
                }
            }
            return Some((f.wire_name.clone(), values));
        }
        None
    }

    /// The literal value of `property` in the record behind `ty`.
    fn const_field(&self, ty: &TypeRef, property: &str) -> Option<String> {
        let field = self
            .record_fields(ty)?
            .iter()
            .find(|f| f.wire_name == property)?;
        self.literal_text(&field.ty)
    }

    /// The text of a scalar `Const` type.
    fn literal_text(&self, ty: &TypeRef) -> Option<String> {
        match self.shape_of(ty)? {
            Shape::Const { value } if is_scalar(value) => Some(schema::value_text(value)),
            _ => None,
        }
    }

    /// A property every raw member schema (after `$ref`s) requires with a
    /// distinct literal value. Names inline variants before they are
    /// converted, and tags unions whose variants are still being built.
    fn raw_tags(&self, members: &[(usize, RefTarget)]) -> Option<RawTags> {
        let ws = self.ws;
        let resolved: Vec<(RefTarget, &Object)> = members
            .iter()
            .map(|(_, m)| {
                let t = ws.deref(m)?;
                let map = ws.get(&t)?.as_object()?;
                Some((t, map))
            })
            .collect::<Option<_>>()?;
        let first = resolved[0].1.get("properties")?.as_object()?;
        'property: for name in first.keys() {
            let mut values: Vec<String> = vec![];
            for (t, map) in &resolved {
                if !requires(map, name) {
                    continue 'property;
                }
                let prop = ws
                    .deref(&child(t, &["properties", name]))
                    .and_then(|p| ws.get(&p))
                    .and_then(Value::as_object);
                match prop.and_then(raw_literal) {
                    Some(v) if !values.contains(&v) => values.push(v),
                    _ => continue 'property,
                }
            }
            return Some(RawTags {
                property: name.clone(),
                values,
            });
        }
        None
    }
}

fn requires(map: &Object, property: &str) -> bool {
    map.get("required")
        .and_then(Value::as_array)
        .is_some_and(|r| r.iter().any(|v| v.as_str() == Some(property)))
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

/// The literal of `const: x` or a one-value `enum`, as text.
fn raw_literal(map: &Object) -> Option<String> {
    if let Some(v) = map.get("const").filter(|v| is_scalar(v)) {
        return Some(schema::value_text(v));
    }
    match map.get("enum")?.as_array()?.as_slice() {
        [v] if is_scalar(v) => Some(schema::value_text(v)),
        _ => None,
    }
}

/// The name of an inline (unregistered) variant from its kind.
fn kind_name(shape: &Shape, index: usize) -> Ident {
    let name = match shape {
        Shape::Primitive { primitive, .. } => match primitive {
            Primitive::String { .. } => "String",
            Primitive::Bytes => "Bytes",
            Primitive::Int32 | Primitive::Int64 | Primitive::Integer => "Integer",
            Primitive::Float | Primitive::Double | Primitive::Number => "Number",
            Primitive::Bool => "Boolean",
        },
        Shape::Const { value } => return Ident::new(schema::value_text(value)),
        Shape::Array { .. } => "Array",
        Shape::Map { .. } => "Map",
        Shape::Any => "Any",
        _ => return variant_name(index),
    };
    Ident::new(name)
}

/// `Variant<n>` for the member at spec position `index`.
fn variant_name(index: usize) -> Ident {
    Ident::new(format!("Variant{}", index + 1))
}
