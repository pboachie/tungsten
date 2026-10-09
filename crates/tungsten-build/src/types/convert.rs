// SPDX-License-Identifier: AGPL-3.0-only
//! Conversion of one schema object to a shape: references, nullability,
//! primitives, enums, arrays, maps and records. Unions and `allOf` live in
//! their own modules.

use serde_json::Value;
use tungsten_core::Diagnostic;
use tungsten_ir::naming::Role;
use tungsten_ir::{
    Additional, Constraints, EnumValue, Field, Ident, Presence, Primitive, Shape, TypeRef, Union,
    UnionStrategy, Variant,
};
use tungsten_openapi::RefTarget;

use super::schema::{self, Inferred, JsonType, Object};
use super::{At, Body, Conv, Resolved, TypeBuilder, child, names};

/// How a schema admits `null`.
#[derive(Debug, Clone, Copy)]
struct Nullness {
    /// `null` is one of the admitted values.
    nullable: bool,
    /// `null` is the only admitted value.
    only_null: bool,
}

/// One record field with the pointer of the property that declared it.
#[derive(Debug)]
pub(super) struct FieldAt {
    pub field: Field,
    pub pointer: (tungsten_openapi::DocId, String),
}

/// `Presence` from requiredness and nullability.
pub(super) fn presence(required: bool, nullable: bool) -> Presence {
    match (required, nullable) {
        (true, false) => Presence::Required,
        (true, true) => Presence::RequiredNullable,
        (false, false) => Presence::Optional,
        (false, true) => Presence::OptionalNullable,
    }
}

/// Requiredness and nullability of a `Presence`.
pub(super) fn split_presence(p: Presence) -> (bool, bool) {
    match p {
        Presence::Required => (true, false),
        Presence::RequiredNullable => (true, true),
        Presence::Optional => (false, false),
        Presence::OptionalNullable => (false, true),
    }
}

impl<'a> TypeBuilder<'a> {
    /// Convert the schema `value` found at `target`. Never registers
    /// `target` itself (the caller places the result).
    pub(super) fn convert(
        &mut self,
        ns: &str,
        target: &RefTarget,
        value: &Value,
        hint: &[String],
    ) -> Conv {
        match value {
            Value::Bool(true) => Conv::shape(Shape::Any),
            Value::Bool(false) => Conv::shape(Shape::Never),
            Value::Object(map) => self.convert_object(ns, target, map, hint),
            _ => {
                self.report(
                    Diagnostic::info(
                        "TG0305",
                        "schema is neither an object nor a boolean; treated as any",
                    ),
                    &target.doc_pointer(),
                );
                Conv::shape(Shape::Any)
            }
        }
    }

    /// Convert a schema object. `map` is the object at `target`, or a view
    /// of some of its keywords (an `allOf` sibling member).
    pub(super) fn convert_object(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        hint: &[String],
    ) -> Conv {
        self.report_unsupported(target, map);
        let has_ref = map.contains_key("$ref");
        if has_ref && !map.keys().any(|k| k != "$ref" && schema::is_structural(k)) {
            return self.convert_ref(ns, target, map);
        }
        let nulls = self.nullness(target, map);
        if nulls.only_null {
            return Conv::shape(Shape::Const { value: Value::Null });
        }
        // 3.1: a `$ref` next to other keywords applies both, like `allOf`.
        let mut conv =
            if has_ref || map.contains_key("allOf") || self.union_has_record_siblings(map) {
                self.convert_all_of(ns, target, map, hint)
            } else if let Some((key, members)) = union_members(target, map) {
                self.convert_union(ns, target, map, key, members, hint)
            } else if let Some(values) = map.get("enum") {
                self.convert_enum(target, map, values)
            } else if let Some(value) = map.get("const") {
                Conv::shape(Shape::Const {
                    value: value.clone(),
                })
            } else {
                self.convert_typed(ns, target, map, hint)
            };
        conv.nullable |= nulls.nullable;
        conv
    }

    /// `oneOf`/`anyOf` next to fixed properties: both apply, so the schema
    /// is handled as an `allOf` of the properties and the union.
    fn union_has_record_siblings(&self, map: &Object) -> bool {
        (map.contains_key("oneOf") || map.contains_key("anyOf"))
            && ["properties", "additionalProperties", "items"]
                .iter()
                .any(|k| map.contains_key(*k))
    }

    /// A `$ref` with only annotations next to it.
    fn convert_ref(&mut self, ns: &str, target: &RefTarget, map: &Object) -> Conv {
        let resolved = map
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| self.ws.resolve(target.doc, r));
        // A dangling, remote or traversing $ref was reported by the frontend.
        let Some(to) = resolved else {
            return Conv::shape(Shape::Any);
        };
        let id = self.register(ns, &to, &[]);
        Conv {
            nullable: self.is_nullable(&id),
            body: Body::Named(id),
        }
    }

    // ----- nullability --------------------------------------------------

    /// Where `null` is admitted: `type` containing `"null"`, `enum`
    /// containing `null`, `const: null`, or a `{type: "null"}` member of
    /// `oneOf`/`anyOf`.
    fn nullness(&self, target: &RefTarget, map: &Object) -> Nullness {
        let types = schema::declared_types(map).ok().flatten();
        let type_null = types.as_ref().is_some_and(|t| t.contains(&JsonType::Null));
        let type_only_null = types
            .as_ref()
            .is_some_and(|t| !t.is_empty() && t.iter().all(|t| *t == JsonType::Null));
        let values = map.get("enum").and_then(Value::as_array);
        let enum_null = values.is_some_and(|v| v.contains(&Value::Null));
        let enum_only_null = values.is_some_and(|v| !v.is_empty() && v.iter().all(Value::is_null));
        let const_null = map.get("const") == Some(&Value::Null);
        let (union_null, union_only_null) = match union_members(target, map) {
            Some((_, members)) => {
                let nulls = members.iter().filter(|m| self.is_null_only(m)).count();
                (nulls > 0, nulls > 0 && nulls == members.len())
            }
            None => (false, false),
        };
        Nullness {
            nullable: type_null || enum_null || const_null || union_null,
            only_null: type_only_null || enum_only_null || const_null || union_only_null,
        }
    }

    /// Whether the schema at `target` (after `$ref`s) admits only `null`.
    pub(super) fn is_null_only(&self, target: &RefTarget) -> bool {
        let Some(t) = self.ws.deref(target) else {
            return false;
        };
        let Some(Value::Object(map)) = self.ws.get(&t) else {
            return false;
        };
        let types_null = schema::declared_types(map)
            .ok()
            .flatten()
            .is_some_and(|t| !t.is_empty() && t.iter().all(|t| *t == JsonType::Null));
        let enum_null = map
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty() && v.iter().all(Value::is_null));
        types_null || enum_null || map.get("const") == Some(&Value::Null)
    }

    /// A syntactic estimate of whether the schema at `target` admits
    /// `null`, used for a named type before it is converted (references
    /// met while converting a cycle). Conversion sets the final value.
    /// Every schema of a followed alias chain shares the answer, which is
    /// memoized so long chains are walked once.
    pub(super) fn admits_null(&mut self, target: &RefTarget) -> bool {
        let mut cur = target.clone();
        let mut path = vec![];
        let answer = loop {
            if let Some(&known) = self.null_estimates.get(&cur) {
                break known;
            }
            if path.contains(&cur) {
                break false;
            }
            path.push(cur.clone());
            let Some(Value::Object(map)) = self.ws.get(&cur) else {
                break false;
            };
            if let Some(r) = map.get("$ref").and_then(Value::as_str) {
                let annotated_only = !map.keys().any(|k| k != "$ref" && schema::is_structural(k));
                match self.ws.resolve(cur.doc, r) {
                    Some(next) if annotated_only => {
                        cur = next;
                        continue;
                    }
                    _ => break false,
                }
            }
            let n = self.nullness(&cur, map);
            break n.nullable && !n.only_null;
        };
        for t in path {
            self.null_estimates.insert(t, answer);
        }
        answer
    }

    // ----- typed schemas ------------------------------------------------

    /// A schema without composition, `enum` or `const`: by `type`, or by
    /// the keywords present when `type` is absent.
    pub(super) fn convert_typed(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        hint: &[String],
    ) -> Conv {
        match schema::declared_types(map) {
            Err(bad) => {
                self.report(
                    Diagnostic::info(
                        "TG0305",
                        format!("`type` {bad} is not a JSON type; treated as any"),
                    ),
                    &target.keyword("type"),
                );
                Conv::shape(Shape::Any)
            }
            Ok(Some(types)) => {
                let types: Vec<JsonType> =
                    types.into_iter().filter(|t| *t != JsonType::Null).collect();
                match types.as_slice() {
                    [] => {
                        self.report(
                            Diagnostic::info("TG0305", "`type` lists no type; treated as any"),
                            &target.keyword("type"),
                        );
                        Conv::shape(Shape::Any)
                    }
                    [one] => self.convert_type(ns, target, map, *one, hint),
                    many => self.convert_multi_type(ns, target, map, many, hint),
                }
            }
            Ok(None) => match schema::infer_type(map) {
                Inferred::One(t) => self.convert_type(ns, target, map, t, hint),
                Inferred::Ambiguous => {
                    self.report(
                        Diagnostic::info(
                            "TG0305",
                            "schema has keywords of several types and no `type`; treated as any",
                        ),
                        &target.doc_pointer(),
                    );
                    Conv::shape(Shape::Any)
                }
                Inferred::Unknown => {
                    if let Some(key) = map.keys().find(|k| schema::is_structural(k)) {
                        let message =
                            format!("schema has `{key}` but no determinable type; treated as any");
                        self.report(Diagnostic::info("TG0305", message), &target.doc_pointer());
                    }
                    Conv::shape(Shape::Any)
                }
            },
        }
    }

    /// The shape of a schema read as one JSON type.
    pub(super) fn convert_type(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        ty: JsonType,
        hint: &[String],
    ) -> Conv {
        let format = map.get("format").and_then(Value::as_str);
        let primitive = |primitive| Shape::Primitive {
            primitive,
            constraints: schema::constraints_of(map),
        };
        Conv::shape(match ty {
            JsonType::String => primitive(match schema::content_primitive(map) {
                Some(p) if format.is_none() => p,
                _ => self.string_primitive(target, format),
            }),
            JsonType::Integer => primitive(schema::integer_primitive(format)),
            JsonType::Number => primitive(schema::number_primitive(format)),
            JsonType::Boolean => Shape::Primitive {
                primitive: Primitive::Bool,
                constraints: Constraints::default(),
            },
            JsonType::Array => self.convert_array(ns, target, map, hint),
            JsonType::Object => self.convert_record(ns, target, map, hint),
            JsonType::Null => Shape::Const { value: Value::Null },
        })
    }

    /// The string primitive for a format, reporting unknown formats.
    fn string_primitive(&mut self, target: &RefTarget, format: Option<&str>) -> Primitive {
        let (primitive, unknown) = schema::string_primitive(format);
        if unknown && let Some(f) = format {
            self.report(
                Diagnostic::info(
                    "TG0304",
                    format!("unknown string format `{f}`; kept as a plain string"),
                ),
                &target.keyword("format"),
            );
        }
        primitive
    }

    /// `type: [A, B, ...]` without `null`: a union with one variant per
    /// type, each reading the schema as that type.
    fn convert_multi_type(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        types: &[JsonType],
        hint: &[String],
    ) -> Conv {
        let mut candidates = vec![];
        for (index, ty) in types.iter().enumerate() {
            let words = names::extend(hint, ty.name());
            let conv = self.convert_type(ns, target, map, *ty, &words);
            let ty_ref = self.place(ns, target, conv.body, &words, false, false);
            candidates.push(super::union::Candidate {
                name: Ident::new(ty.name()),
                ty: ty_ref,
                index,
                tag: None,
            });
        }
        Conv {
            body: self.finish_union(target, None, None, candidates),
            nullable: false,
        }
    }

    fn convert_array(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        hint: &[String],
    ) -> Shape {
        let items = match map.get("items") {
            Some(Value::Object(_) | Value::Bool(_)) if !map.contains_key("prefixItems") => self
                .resolve(ns, &child(target, &["items"]), &names::extend(hint, "item"))
                .positional(),
            Some(Value::Array(_)) => {
                self.report(
                    Diagnostic::info(
                        "TG0303",
                        "tuple-form `items` is not modelled and was ignored; items are any",
                    ),
                    &target.keyword("items"),
                );
                TypeRef::Inline(Box::new(Shape::Any))
            }
            _ => TypeRef::Inline(Box::new(Shape::Any)),
        };
        Shape::Array {
            items,
            min: map.get("minItems").and_then(Value::as_u64),
            max: map.get("maxItems").and_then(Value::as_u64),
            unique: map.get("uniqueItems") == Some(&Value::Bool(true)),
        }
    }

    // ----- records and maps ---------------------------------------------

    /// `type: object`: a record when it has properties or required names,
    /// a map otherwise (`Record` with no fields when it is closed).
    fn convert_record(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        hint: &[String],
    ) -> Shape {
        let properties = map.get("properties").and_then(Value::as_object);
        let mut required: Vec<&str> = vec![];
        for name in map
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !required.contains(&name) {
                required.push(name);
            }
        }
        let additional = self.additional(ns, target, map, hint);
        if properties.is_none_or(|p| p.is_empty()) && required.is_empty() {
            return match additional {
                Additional::Closed => Shape::Record {
                    fields: vec![],
                    additional,
                },
                Additional::Open => Shape::Map {
                    values: TypeRef::Inline(Box::new(Shape::Any)),
                },
                Additional::Typed { values } => Shape::Map { values },
            };
        }
        let mut fields = vec![];
        for (name, property) in properties.into_iter().flatten() {
            let at = child(target, &["properties", name]);
            let resolved = self.resolve(ns, &at, &names::extend(hint, name));
            let field = self.field(
                name,
                &at,
                property,
                resolved,
                required.contains(&name.as_str()),
            );
            fields.push(FieldAt {
                field,
                pointer: at.doc_pointer(),
            });
        }
        for name in required {
            if properties.is_some_and(|p| p.contains_key(name)) {
                continue;
            }
            // Required but undeclared: present with any value.
            fields.push(FieldAt {
                field: Field {
                    wire_name: name.to_string(),
                    name: Ident::new(name),
                    ty: TypeRef::Inline(Box::new(Shape::Any)),
                    presence: Presence::Required,
                    read_only: false,
                    write_only: false,
                    deprecated: false,
                    default: None,
                    doc: None,
                    constraints: Constraints::default(),
                    sensitive: false,
                },
                pointer: target.keyword("required"),
            });
        }
        Shape::Record {
            fields: self.finish_fields(fields),
            additional,
        }
    }

    /// `additionalProperties`: absent, `true` or `{}` is open, `false` is
    /// closed, a schema types the extra values.
    fn additional(
        &mut self,
        ns: &str,
        target: &RefTarget,
        map: &Object,
        hint: &[String],
    ) -> Additional {
        match map.get("additionalProperties") {
            Some(Value::Bool(false)) => Additional::Closed,
            Some(Value::Object(m)) if !schema::is_inert(m) || m.contains_key("$ref") => {
                let values = self
                    .resolve(
                        ns,
                        &child(target, &["additionalProperties"]),
                        &names::extend(hint, "value"),
                    )
                    .positional();
                Additional::Typed { values }
            }
            _ => Additional::Open,
        }
    }

    /// One record field. Annotations come from the property schema;
    /// `readOnly`, `writeOnly` and `x-agent-sensitive` also from the schema
    /// it references.
    fn field(
        &mut self,
        name: &str,
        at: &RefTarget,
        property: &Value,
        resolved: Resolved,
        required: bool,
    ) -> Field {
        let ws = self.ws;
        let own = property.as_object();
        let referenced = own
            .filter(|m| m.contains_key("$ref"))
            .and_then(|_| ws.deref(at))
            .and_then(|t| ws.get(&t))
            .and_then(Value::as_object);
        let flag = |key: &str| {
            [own, referenced]
                .into_iter()
                .flatten()
                .any(|m| m.get(key) == Some(&Value::Bool(true)))
        };
        let constraints = match &resolved.ty {
            TypeRef::Inline(shape) => match shape.as_ref() {
                Shape::Primitive { constraints, .. } => constraints.clone(),
                _ => own.map(schema::constraints_of).unwrap_or_default(),
            },
            TypeRef::Named(_) => own.map(schema::constraints_of).unwrap_or_default(),
        };
        Field {
            wire_name: name.to_string(),
            name: Ident::new(name),
            presence: presence(required, resolved.nullable),
            ty: resolved.ty,
            read_only: flag("readOnly"),
            write_only: flag("writeOnly"),
            deprecated: own.is_some_and(|m| m.get("deprecated") == Some(&Value::Bool(true))),
            // `default: null` is dropped: the IR cannot tell it from no default.
            default: own
                .and_then(|m| m.get("default"))
                .filter(|v| !v.is_null())
                .cloned(),
            doc: own.and_then(schema::doc_of),
            constraints,
            sensitive: flag("x-agent-sensitive"),
        }
    }

    /// Make field names unique within one record (TG0401 per renamed field).
    pub(super) fn finish_fields(&mut self, fields: Vec<FieldAt>) -> Vec<Field> {
        let mut idents: Vec<Ident> = fields.iter().map(|f| f.field.name.clone()).collect();
        let changed = names::disambiguate_all(&mut idents, Role::Field);
        for &i in &changed {
            let message = format!(
                "field `{}` collides with another field of the same record; renamed to `{}`",
                fields[i].field.wire_name,
                idents[i].snake()
            );
            self.report(Diagnostic::warning("TG0401", message), &fields[i].pointer);
        }
        fields
            .into_iter()
            .zip(idents)
            .map(|(mut f, ident)| {
                f.field.name = ident;
                f.field
            })
            .collect()
    }

    // ----- enums --------------------------------------------------------

    /// `enum` without `null` (nullability is handled by the caller). One
    /// value is a `Const`; values of one primitive type are an `Enum`;
    /// values of mixed types are a literal union of `Const`s.
    fn convert_enum(&mut self, target: &RefTarget, map: &Object, values: &Value) -> Conv {
        let Some(all) = values.as_array() else {
            self.report(
                Diagnostic::info("TG0305", "`enum` is not an array; treated as any"),
                &target.keyword("enum"),
            );
            return Conv::shape(Shape::Any);
        };
        if all.is_empty() {
            self.report(
                Diagnostic::warning("TG0307", "`enum` is empty, so the schema admits no value"),
                &target.keyword("enum"),
            );
            return Conv::shape(Shape::Never);
        }
        let mut distinct: Vec<Value> = vec![];
        for v in all.iter().filter(|v| !v.is_null()) {
            if !distinct.contains(v) {
                distinct.push(v.clone());
            }
        }
        if let [only] = distinct.as_slice() {
            return Conv::shape(Shape::Const {
                value: only.clone(),
            });
        }
        let declared = schema::non_null_types(map);
        let base = match declared.as_deref() {
            Some([one]) => Some(*one),
            _ => {
                let first = distinct.first().map(JsonType::of);
                let all_same = distinct.iter().all(|v| Some(JsonType::of(v)) == first);
                let numeric = distinct
                    .iter()
                    .all(|v| matches!(JsonType::of(v), JsonType::Integer | JsonType::Number));
                if all_same {
                    first
                } else if numeric {
                    Some(JsonType::Number)
                } else {
                    None
                }
            }
        };
        let format = map.get("format").and_then(Value::as_str);
        let primitive = match base {
            Some(JsonType::String) => Some(self.string_primitive(target, format)),
            Some(JsonType::Integer) => Some(schema::integer_primitive(format)),
            Some(JsonType::Number) => Some(schema::number_primitive(format)),
            Some(JsonType::Boolean) => Some(Primitive::Bool),
            _ => None,
        };
        Conv::shape(match primitive {
            Some(base) => Shape::Enum {
                base,
                values: self.enum_values(target, distinct),
            },
            None => self.literal_enum(target, distinct),
        })
    }

    /// Enum values named after their text, made unique (TG0401).
    fn enum_values(&mut self, target: &RefTarget, values: Vec<Value>) -> Vec<EnumValue> {
        let mut idents: Vec<Ident> = values
            .iter()
            .map(|v| Ident::new(schema::value_text(v)))
            .collect();
        self.disambiguate_variants(target, "enum value", &mut idents);
        values
            .into_iter()
            .zip(idents)
            .map(|(value, name)| EnumValue {
                value,
                name,
                doc: None,
            })
            .collect()
    }

    /// An `enum` of values of different JSON types: a literal union.
    fn literal_enum(&mut self, target: &RefTarget, values: Vec<Value>) -> Shape {
        let mut idents: Vec<Ident> = values
            .iter()
            .map(|v| Ident::new(schema::value_text(v)))
            .collect();
        self.disambiguate_variants(target, "enum value", &mut idents);
        let variants = values
            .into_iter()
            .zip(idents)
            .map(|(value, name)| Variant {
                name,
                ty: TypeRef::Inline(Box::new(Shape::Const { value })),
                tag: None,
            })
            .collect();
        Shape::Union(Union {
            variants,
            discriminator: None,
            strategy: UnionStrategy::Literal,
        })
    }

    /// Make enum value or union variant names unique (TG0401 at `target`).
    pub(super) fn disambiguate_variants(
        &mut self,
        target: &RefTarget,
        what: &str,
        idents: &mut [Ident],
    ) {
        let changed = names::disambiguate_all(idents, Role::EnumVariant);
        for i in changed {
            let message = format!(
                "{what} `{}` collides with another {what} of the same type; renamed to `{}`",
                idents[i].wire,
                idents[i].pascal()
            );
            self.report(
                Diagnostic::warning("TG0401", message),
                &target.doc_pointer(),
            );
        }
    }

    // ----- diagnostics --------------------------------------------------

    /// TG0303 for every keyword tungsten ignores.
    pub(super) fn report_unsupported(&mut self, target: &RefTarget, map: &Object) {
        let keys: Vec<String> = schema::unsupported_keywords(map)
            .map(str::to_string)
            .collect();
        for key in keys {
            let shown = if key == "if" {
                "if/then/else"
            } else {
                key.as_str()
            };
            self.report(
                Diagnostic::info(
                    "TG0303",
                    format!("`{shown}` is not modelled and was ignored"),
                ),
                &target.keyword(&key),
            );
        }
    }
}

/// The `oneOf` (or else `anyOf`) member targets of a schema.
pub(super) fn union_members(
    target: &RefTarget,
    map: &Object,
) -> Option<(&'static str, Vec<RefTarget>)> {
    let key = ["oneOf", "anyOf"]
        .into_iter()
        .find(|k| matches!(map.get(*k), Some(Value::Array(_))))?;
    let len = map.get(key).and_then(Value::as_array).map_or(0, Vec::len);
    let members = (0..len)
        .map(|i| child(target, &[key, &i.to_string()]))
        .collect();
    Some((key, members))
}
