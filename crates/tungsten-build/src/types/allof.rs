// SPDX-License-Identifier: AGPL-3.0-only
//! `allOf`.
//!
//! Members are the `allOf` items, a `$ref` next to them (3.1 applies
//! both), the other sibling keywords (`properties`, `required`,
//! `additionalProperties`, ...) as one more member, and a sibling
//! `oneOf`/`anyOf` as another. Members with only annotations or unsupported
//! keywords are dropped (`allOf: [{$ref: X}, {description: ...}]`,
//! `allOf: [{if: ..., then: ...}]`). Then:
//!
//! - no member left: the schema's own `type`;
//! - one member: that member, so `$ref` plus annotations is the
//!   referenced type;
//! - otherwise every member must read as a record (a record, a map, or
//!   any). Fields are merged in member order. A field declared by several
//!   members must have the same type in each (`any` yields to the other
//!   type), or one must refine the other without changing its JSON type:
//!   two primitives of one kind (a format or a size may be added; the
//!   constraints are conjoined: the larger minimum, the smaller maximum,
//!   the later `pattern`), two arrays of the same items (bounds conjoined),
//!   or a primitive and an enum or const of its JSON type (the enum or
//!   const is kept). It is required when any member requires it and
//!   nullable only when every member admits `null`. The merged record is
//!   closed when any member is closed (that member forbids every property it does not
//!   declare, and the generated type should not invite them), else typed
//!   by the first member with typed extras, else open.
//! - a member that is not a record, or a field whose types differ, keeps
//!   the members as an `Intersection` with TG0302.
//!
//! Inheritance polymorphism: a base schema whose `oneOf`/`anyOf` lists its
//! subtypes, each of which is `allOf: [{$ref: Base}, ...]`. A subtype takes
//! only the base's own record keywords (its union is the base's type, not
//! a part of every subtype), and a base whose union members all extend it
//! is that union alone (each variant already carries the base fields).
//!
//! Referenced members that are already built reuse their shape instead of
//! being converted again, and a member referenced twice counts once, so
//! flattening stays linear in the number of schemas.

use serde_json::Value;
use tungsten_core::Diagnostic;
use tungsten_ir::{Additional, Constraints, Field, Primitive, Shape, TypeRef};
use tungsten_openapi::RefTarget;

use super::convert::{FieldAt, presence, split_presence, union_members};
use super::schema::{self, JsonType, Object};
use super::{At, Body, Conv, Resolved, State, TypeBuilder, child, names};

/// How deep `same_type` compares inline shapes.
const MAX_COMPARE_DEPTH: usize = 16;

#[derive(Debug)]
enum Member {
    /// A schema at a pointer (an `allOf` item or a `$ref` target).
    At(RefTarget),
    /// Sibling keywords of the `allOf` schema itself.
    View(Object),
}

/// A member read as a record.
#[derive(Debug)]
struct Parts {
    fields: Vec<Field>,
    additional: Additional,
    nullable: bool,
}

fn is_any(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::Inline(shape) if **shape == Shape::Any)
}

impl<'a> TypeBuilder<'a> {
    pub(super) fn convert_all_of(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        hint: &[String],
    ) -> Conv {
        let mut members = self.all_of_members(target, map);
        match members.len() {
            0 => self.convert_typed(ns, target, map, hint),
            1 => {
                let only = members.remove(0);
                self.member_conv(ns, target, only, hint)
            }
            _ => self.merge(ns, target, members, hint),
        }
    }

    fn all_of_members(&mut self, target: &RefTarget, map: &Object) -> Vec<Member> {
        let ws = self.ws;
        let mut members = vec![];
        // A schema referenced by several members applies once.
        let mut seen: Vec<RefTarget> = vec![];
        let mut push_at = |members: &mut Vec<Member>, at: RefTarget| {
            let key = ws.deref(&at).unwrap_or_else(|| at.clone());
            if !seen.contains(&key) {
                seen.push(key);
                members.push(Member::At(at));
            }
        };
        if let Some(to) = map
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| ws.resolve(target.doc, r))
        {
            push_at(&mut members, to);
        }
        match map.get("allOf") {
            Some(Value::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    let at = child(target, &["allOf", &i.to_string()]);
                    match item {
                        Value::Object(m) if schema::is_inert(m) => self.report_unsupported(&at, m),
                        Value::Bool(true) => {}
                        _ => push_at(&mut members, at),
                    }
                }
            }
            Some(_) => self.report(
                Diagnostic::info("TG0305", "`allOf` is not an array; ignored"),
                &target.keyword("allOf"),
            ),
            None => {}
        }
        let object_only = schema::non_null_types(map).is_some_and(|t| t == [JsonType::Object]);
        let rest: Object = map
            .iter()
            .filter(|(k, _)| {
                schema::is_structural(k)
                    && !matches!(k.as_str(), "$ref" | "allOf" | "oneOf" | "anyOf")
                    && !(k.as_str() == "type" && object_only)
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // A base whose union members all extend it is that union: every
        // variant already carries the base's own fields.
        if !rest.is_empty() && !self.is_inheritance_base(target, map) {
            members.push(Member::View(rest));
        }
        let union: Object = map
            .iter()
            .filter(|(k, _)| matches!(k.as_str(), "oneOf" | "anyOf" | "discriminator"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if union.contains_key("oneOf") || union.contains_key("anyOf") {
            members.push(Member::View(union));
        }
        members
    }

    /// Whether every non-null `oneOf`/`anyOf` member of the schema at
    /// `target` is a subtype that extends it through `allOf` (or a 3.1
    /// sibling `$ref`).
    fn is_inheritance_base(&self, target: &RefTarget, map: &Object) -> bool {
        let Some((_, members)) = union_members(target, map) else {
            return false;
        };
        let mut subtypes = members.iter().filter(|m| !self.is_null_only(m)).peekable();
        subtypes.peek().is_some() && subtypes.all(|m| self.extends(m, target))
    }

    /// Whether the schema at `member` (after `$ref`s) lists `base` among
    /// its `allOf` members or its sibling `$ref`.
    fn extends(&self, member: &RefTarget, base: &RefTarget) -> bool {
        let ws = self.ws;
        let Some(sub) = ws.deref(member) else {
            return false;
        };
        let Some(map) = ws.get(&sub).and_then(Value::as_object) else {
            return false;
        };
        let is_base = |at: RefTarget| ws.deref(&at).as_ref() == Some(base);
        let items = map.get("allOf").and_then(Value::as_array);
        let by_all_of = (0..items.map_or(0, Vec::len))
            .any(|i| is_base(child(&sub, &["allOf", &i.to_string()])));
        let by_ref = map
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| ws.resolve(sub.doc, r))
            .is_some_and(is_base);
        by_all_of || by_ref
    }

    /// Whether a `oneOf`/`anyOf` member of the schema `map` at `at` is
    /// `whole` (after `$ref`s).
    fn lists_member(&self, at: &RefTarget, map: &Object, whole: &RefTarget) -> bool {
        union_members(at, map).is_some_and(|(_, members)| {
            members
                .iter()
                .any(|m| self.ws.deref(m).as_ref() == Some(whole))
        })
    }

    /// The type of a lone member. An inline member is converted in place
    /// (the `allOf` schema takes its shape); a reference stays a reference.
    fn member_conv(
        &mut self,
        ns: &str,
        target: &RefTarget,
        member: Member,
        hint: &[String],
    ) -> Conv {
        let ws = self.ws;
        match member {
            Member::View(map) => self.convert_object(ns, target, &map, hint),
            Member::At(at) => match ws.get(&at) {
                Some(value)
                    if !self.by_target.contains_key(&at)
                        && !ws.graph.nodes.contains(&at)
                        && value.get("$ref").is_none() =>
                {
                    self.convert(ns, &at, value, hint)
                }
                _ => Conv::of(self.resolve(ns, &at, hint)),
            },
        }
    }

    fn merge(
        &mut self,
        ns: &str,
        target: &RefTarget,
        members: Vec<Member>,
        hint: &[String],
    ) -> Conv {
        let mut parts = vec![];
        for (i, m) in members.iter().enumerate() {
            match self.member_parts(ns, target, m, hint, target) {
                Some(p) => parts.push(p),
                None => {
                    let reason = format!("member {} is not an object schema", i + 1);
                    return self.intersection(ns, target, &members, hint, &reason);
                }
            }
        }
        let nullable = parts.iter().all(|p| p.nullable);
        let mut fields: Vec<Field> = vec![];
        let mut closed = false;
        let mut typed = None;
        for p in parts {
            for f in p.fields {
                match fields.iter_mut().find(|g| g.wire_name == f.wire_name) {
                    None => fields.push(f),
                    Some(g) => {
                        let wire = f.wire_name.clone();
                        if !self.merge_field(g, f) {
                            let reason = format!(
                                "property `{wire}` has different types in different members"
                            );
                            return self.intersection(ns, target, &members, hint, &reason);
                        }
                    }
                }
            }
            match p.additional {
                Additional::Closed => closed = true,
                Additional::Typed { values } => {
                    typed.get_or_insert(values);
                }
                Additional::Open => {}
            }
        }
        let additional = match (closed, typed) {
            (true, _) => Additional::Closed,
            (false, Some(values)) => Additional::Typed { values },
            (false, None) => Additional::Open,
        };
        let fields = fields
            .into_iter()
            .map(|field| FieldAt {
                field,
                pointer: target.keyword("allOf"),
            })
            .collect();
        Conv {
            body: Body::Shape(Shape::Record {
                fields: self.finish_fields(fields),
                additional,
            }),
            nullable,
        }
    }

    /// Merge `f` into `g` (same wire name). False when the types conflict.
    fn merge_field(&self, g: &mut Field, f: Field) -> bool {
        let (g_required, g_nullable) = split_presence(g.presence);
        let (f_required, f_nullable) = split_presence(f.presence);
        if is_any(&g.ty) && !is_any(&f.ty) {
            g.ty = f.ty;
            g.constraints = f.constraints;
        } else if is_any(&f.ty) || self.same_type(&g.ty, &f.ty, 0) {
        } else if let Some(ty) = self.refine(&g.ty, &f.ty) {
            g.ty = ty;
            g.constraints = conjoin(&g.constraints, &f.constraints);
        } else {
            return false;
        }
        g.presence = presence(g_required || f_required, g_nullable && f_nullable);
        g.read_only |= f.read_only;
        g.write_only |= f.write_only;
        g.deprecated |= f.deprecated;
        g.sensitive |= f.sensitive;
        if g.default.is_none() {
            g.default = f.default;
        }
        if g.doc.is_none() {
            g.doc = f.doc;
        }
        true
    }

    /// A member read as a record. `whole` is the `allOf` schema being
    /// flattened.
    fn member_parts(
        &mut self,
        ns: &str,
        target: &RefTarget,
        member: &Member,
        hint: &[String],
        whole: &RefTarget,
    ) -> Option<Parts> {
        match member {
            Member::View(map) => {
                let conv = self.convert_object(ns, target, map, hint);
                self.parts_of(ns, conv, hint, whole)
            }
            Member::At(at) => self.parts_at(ns, at, hint, whole),
        }
    }

    /// The member at `at` (after `$ref`s) read as a record. A referenced
    /// schema that is already built gives its shape; one still being built
    /// (a reference cycle) is converted again under its own name (its
    /// nested inline types are memoized, so they are shared with its own
    /// named type). A base whose `oneOf`/`anyOf` lists `whole` contributes
    /// only its own record keywords.
    fn parts_at(
        &mut self,
        ns: &str,
        at: &RefTarget,
        hint: &[String],
        whole: &RefTarget,
    ) -> Option<Parts> {
        let ws = self.ws;
        let resolved = ws.deref(at)?;
        let value = ws.get(&resolved)?;
        let named = resolved != *at
            || self.by_target.contains_key(&resolved)
            || ws.graph.nodes.contains(&resolved);
        let (hint, id) = if named {
            let id = self.register(ns, &resolved, hint);
            (self.name_words(&id), Some(id))
        } else {
            (hint.to_vec(), None)
        };
        if !self.expanding.insert(resolved.clone()) {
            return None;
        }
        let parts = match value.as_object() {
            Some(map) if self.lists_member(&resolved, map, whole) => {
                let own: Object = map
                    .iter()
                    .filter(|(k, _)| !matches!(k.as_str(), "oneOf" | "anyOf" | "discriminator"))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let conv = self.convert_object(ns, &resolved, &own, &hint);
                self.parts_of(ns, conv, &hint, whole)
            }
            _ => {
                let conv = match id.and_then(|id| self.built(&id)) {
                    Some(conv) => conv,
                    None => self.convert(ns, &resolved, value, &hint),
                };
                self.parts_of(ns, conv, &hint, whole)
            }
        };
        self.expanding.remove(&resolved);
        parts
    }

    /// The converted schema of a named type whose build has finished.
    fn built(&self, id: &super::TypeId) -> Option<Conv> {
        let e = self.entries.get(id)?;
        if e.state != State::Done {
            return None;
        }
        let body = match &e.alias {
            Some(other) => Body::Named(other.clone()),
            None => Body::Shape(e.ty.shape.clone()),
        };
        Some(Conv {
            body,
            nullable: e.nullable,
        })
    }

    fn parts_of(
        &mut self,
        ns: &str,
        conv: Conv,
        hint: &[String],
        whole: &RefTarget,
    ) -> Option<Parts> {
        let (fields, additional) = match conv.body {
            Body::Shape(Shape::Record { fields, additional }) => (fields, additional),
            Body::Shape(Shape::Map { values }) if is_any(&values) => (vec![], Additional::Open),
            Body::Shape(Shape::Map { values }) => (vec![], Additional::Typed { values }),
            Body::Shape(Shape::Any) => (vec![], Additional::Open),
            Body::Named(id) => {
                let at = self.entries.get(&id)?.target.clone();
                let mut parts = self.parts_at(ns, &at, hint, whole)?;
                parts.nullable |= conv.nullable;
                return Some(parts);
            }
            Body::Shape(_) => return None,
        };
        Some(Parts {
            fields,
            additional,
            nullable: conv.nullable,
        })
    }

    /// Keep the members as an intersection (TG0302).
    fn intersection(
        &mut self,
        ns: &str,
        target: &RefTarget,
        members: &[Member],
        hint: &[String],
        reason: &str,
    ) -> Conv {
        let mut refs = vec![];
        let mut nullable = true;
        for (i, m) in members.iter().enumerate() {
            let words = names::concat(hint, &["part".to_string(), (i + 1).to_string()]);
            let resolved = match m {
                Member::At(at) => self.resolve(ns, at, &words),
                Member::View(map) => {
                    let conv = self.convert_object(ns, target, map, &words);
                    Resolved {
                        ty: self.place(ns, target, conv.body, &words, conv.nullable, false),
                        nullable: conv.nullable,
                    }
                }
            };
            nullable &= resolved.nullable;
            refs.push(resolved.ty);
        }
        self.report(
            Diagnostic::warning(
                "TG0302",
                format!(
                    "allOf cannot be flattened into one record: {reason}; kept as an intersection"
                ),
            ),
            &target.doc_pointer(),
        );
        Conv {
            body: Body::Shape(Shape::Intersection { members: refs }),
            nullable,
        }
    }

    /// The narrower of two field types where one refines the other without
    /// changing its JSON type (see the module docs), or `None`.
    fn refine(&self, a: &TypeRef, b: &TypeRef) -> Option<TypeRef> {
        let inline = |shape: Shape| TypeRef::Inline(Box::new(shape));
        if let (TypeRef::Inline(x), TypeRef::Inline(y)) = (a, b) {
            match (x.as_ref(), y.as_ref()) {
                (
                    Shape::Primitive {
                        primitive: p,
                        constraints: c,
                    },
                    Shape::Primitive {
                        primitive: q,
                        constraints: d,
                    },
                ) => {
                    return Some(inline(Shape::Primitive {
                        primitive: narrower_primitive(p, q)?,
                        constraints: conjoin(c, d),
                    }));
                }
                (
                    Shape::Array {
                        items: i,
                        min: a_min,
                        max: a_max,
                        unique: a_unique,
                    },
                    Shape::Array {
                        items: j,
                        min: b_min,
                        max: b_max,
                        unique: b_unique,
                    },
                ) => {
                    if !self.same_type(i, j, 0) {
                        return None;
                    }
                    return Some(inline(Shape::Array {
                        items: i.clone(),
                        min: tighter(*a_min, *b_min, u64::max),
                        max: tighter(*a_max, *b_max, u64::min),
                        unique: *a_unique || *b_unique,
                    }));
                }
                _ => {}
            }
        }
        // A primitive and an enum or const of its JSON type: the values.
        let (primitive, values) = match (a, b) {
            (TypeRef::Inline(x), other) | (other, TypeRef::Inline(x))
                if matches!(x.as_ref(), Shape::Primitive { .. }) =>
            {
                (x.as_ref(), other)
            }
            _ => return None,
        };
        let Shape::Primitive { primitive, .. } = primitive else {
            return None;
        };
        let admits = match self.shape_of(values)? {
            Shape::Enum { base, .. } => json_type(base) == json_type(primitive),
            Shape::Const { value } => {
                let t = JsonType::of(value);
                t == json_type(primitive)
                    || (t == JsonType::Integer && json_type(primitive) == JsonType::Number)
            }
            _ => false,
        };
        admits.then(|| values.clone())
    }

    // ----- structural type equality -------------------------------------

    /// Whether two references denote the same type. Distinct component
    /// types differ; inline shapes and types registered for inline schemas
    /// compare structurally.
    pub(super) fn same_type(&self, a: &TypeRef, b: &TypeRef, depth: usize) -> bool {
        if a == b {
            return true;
        }
        if depth > MAX_COMPARE_DEPTH {
            return false;
        }
        match (self.structural_shape(a), self.structural_shape(b)) {
            (Some(x), Some(y)) => self.same_shape(x, y, depth + 1),
            _ => false,
        }
    }

    /// The shape compared structurally: `None` for component types.
    fn structural_shape<'s>(&'s self, r: &'s TypeRef) -> Option<&'s Shape> {
        match r {
            TypeRef::Named(id) if self.is_component(id) => None,
            _ => self.shape_of(r),
        }
    }

    fn same_shape(&self, a: &Shape, b: &Shape, depth: usize) -> bool {
        let same = |x: &TypeRef, y: &TypeRef| self.same_type(x, y, depth);
        match (a, b) {
            (
                Shape::Array {
                    items: x,
                    min: a_min,
                    max: a_max,
                    unique: a_unique,
                },
                Shape::Array {
                    items: y,
                    min: b_min,
                    max: b_max,
                    unique: b_unique,
                },
            ) => a_min == b_min && a_max == b_max && a_unique == b_unique && same(x, y),
            (Shape::Map { values: x }, Shape::Map { values: y }) => same(x, y),
            (Shape::Nullable { inner: x }, Shape::Nullable { inner: y }) => same(x, y),
            (
                Shape::Record {
                    fields: fa,
                    additional: aa,
                },
                Shape::Record {
                    fields: fb,
                    additional: ab,
                },
            ) => {
                let additional = match (aa, ab) {
                    (Additional::Typed { values: x }, Additional::Typed { values: y }) => {
                        same(x, y)
                    }
                    (x, y) => x == y,
                };
                additional
                    && fa.len() == fb.len()
                    && fa.iter().zip(fb).all(|(f, g)| {
                        f.wire_name == g.wire_name && f.presence == g.presence && same(&f.ty, &g.ty)
                    })
            }
            (Shape::Union(x), Shape::Union(y)) => {
                x.strategy == y.strategy
                    && x.variants.len() == y.variants.len()
                    && x.variants
                        .iter()
                        .zip(&y.variants)
                        .all(|(v, w)| v.tag == w.tag && same(&v.ty, &w.ty))
            }
            (Shape::Intersection { members: x }, Shape::Intersection { members: y }) => {
                x.len() == y.len() && x.iter().zip(y).all(|(m, n)| same(m, n))
            }
            (x, y) => x == y,
        }
    }
}

/// The JSON type a primitive's values have.
fn json_type(p: &Primitive) -> JsonType {
    match p {
        Primitive::String { .. } | Primitive::Bytes => JsonType::String,
        Primitive::Int32 | Primitive::Int64 | Primitive::Integer => JsonType::Integer,
        Primitive::Float | Primitive::Double | Primitive::Number => JsonType::Number,
        Primitive::Bool => JsonType::Boolean,
    }
}

/// The narrower of two primitives of one kind: equal ones, a string and
/// the same string with a format, an unsized integer or number and a sized
/// one, a number and an integer.
fn narrower_primitive(p: &Primitive, q: &Primitive) -> Option<Primitive> {
    use Primitive::*;
    Some(match (p, q) {
        _ if p == q => p.clone(),
        (String { format: None }, String { format: Some(_) }) => q.clone(),
        (String { format: Some(_) }, String { format: None }) => p.clone(),
        (Integer | Number, Int32 | Int64) | (Number, Integer) => q.clone(),
        (Int32 | Int64, Integer | Number) | (Integer, Number) => p.clone(),
        (Number, Float | Double) => q.clone(),
        (Float | Double, Number) => p.clone(),
        _ => return None,
    })
}

/// Both bounds apply: the tighter one by `pick` (min or max).
fn tighter<T: Copy>(a: Option<T>, b: Option<T>, pick: fn(T, T) -> T) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(pick(x, y)),
        (x, y) => x.or(y),
    }
}

/// The tighter of two numeric bounds: the larger lower bound or the
/// smaller upper bound.
fn tighter_number(
    a: &Option<serde_json::Number>,
    b: &Option<serde_json::Number>,
    lower: bool,
) -> Option<serde_json::Number> {
    match (a, b) {
        (Some(x), Some(y)) => {
            let (fx, fy) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
            let x_wins = if lower { fx >= fy } else { fx <= fy };
            Some(if x_wins { x.clone() } else { y.clone() })
        }
        (x, y) => x.clone().or_else(|| y.clone()),
    }
}

/// The constraints of two `allOf` members that both apply. Bounds take the
/// tighter value; `pattern` and `multipleOf` keep the later member's when
/// both are set (one field can carry only one).
fn conjoin(c: &Constraints, d: &Constraints) -> Constraints {
    Constraints {
        pattern: d.pattern.clone().or_else(|| c.pattern.clone()),
        min_length: tighter(c.min_length, d.min_length, u64::max),
        max_length: tighter(c.max_length, d.max_length, u64::min),
        minimum: tighter_number(&c.minimum, &d.minimum, true),
        maximum: tighter_number(&c.maximum, &d.maximum, false),
        exclusive_minimum: tighter_number(&c.exclusive_minimum, &d.exclusive_minimum, true),
        exclusive_maximum: tighter_number(&c.exclusive_maximum, &d.exclusive_maximum, false),
        multiple_of: d.multiple_of.clone().or_else(|| c.multiple_of.clone()),
    }
}
