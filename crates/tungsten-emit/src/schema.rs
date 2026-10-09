// SPDX-License-Identifier: AGPL-3.0-only
//! IR types rendered as JSON Schema (draft 2020-12), for tool manifests,
//! MCP tool schemas and documentation.
//!
//! Mapping:
//! - primitives: `string` with `format` (`uuid`, `date-time`, ...),
//!   `integer`/`number` with `format` for sized kinds, `boolean`; `byte`
//!   strings and `bytes` are strings with `contentEncoding: base64`;
//!   constraints keep their JSON Schema names;
//! - enums `enum` over the base type, constants `const`, arrays `items`
//!   with bounds, maps `additionalProperties`;
//! - records: `properties` by wire name, `required` from presence,
//!   `additionalProperties: false` when closed or the extras' schema when
//!   typed; nullable presence becomes a type array (`["string", "null"]`)
//!   or, when the schema has no `type`, `anyOf` with `{"type": "null"}`;
//! - unions: tagged ones are `oneOf` whose variants pin the discriminator
//!   with `const`; literal unions of constants are one `enum`; the others
//!   are `anyOf` (their variants may overlap); intersections are `allOf`;
//! - `any` is `{}` and `never` is `{"not": {}}`.
//!
//! Named types are inlined up to [`SchemaOptions::inline_depth`] levels;
//! deeper references, recursive types and types already moved to `$defs`
//! are `$ref`s to `#/$defs/<type id>`. [`SchemaBuilder::finish`] attaches
//! the collected `$defs`.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use tungsten_ir::{
    Additional, Doc, Field, Ir, Presence, Primitive, Shape, StringFormat, TypeId, TypeRef, Union,
    UnionStrategy,
};

use crate::args::{ArgsLayout, BodyArg};

/// The draft 2020-12 meta-schema URI.
pub const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Which side of the wire a schema describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    /// Every field, with `readOnly`/`writeOnly` annotations.
    Any,
    /// What a client sends: `readOnly` fields are left out.
    Request,
    /// What a client receives: `writeOnly` fields are left out.
    Response,
}

/// How [`SchemaBuilder`] renders.
#[derive(Debug, Clone)]
pub struct SchemaOptions {
    /// Levels of named types inlined into a schema; references below that
    /// depth become `$ref`s into `$defs`. 0 references every named type.
    pub inline_depth: usize,
    /// Keep `description`s from the spec.
    pub descriptions: bool,
    /// Cut each description to its first sentences; `None` keeps it whole.
    pub max_sentences: Option<usize>,
    pub usage: Usage,
}

impl Default for SchemaOptions {
    fn default() -> Self {
        Self {
            inline_depth: 2,
            descriptions: true,
            max_sentences: None,
            usage: Usage::Any,
        }
    }
}

/// Renders IR types into JSON Schema values sharing one `$defs` table.
#[derive(Debug)]
pub struct SchemaBuilder<'a> {
    ir: &'a Ir,
    opts: SchemaOptions,
    /// `$defs` entries by type id; `Null` while the entry is being built.
    defs: BTreeMap<String, Value>,
    /// Named types being inlined, innermost last.
    stack: Vec<&'a TypeId>,
}

impl<'a> SchemaBuilder<'a> {
    pub fn new(ir: &'a Ir, opts: SchemaOptions) -> Self {
        Self {
            ir,
            opts,
            defs: BTreeMap::new(),
            stack: vec![],
        }
    }

    pub fn options(&self) -> &SchemaOptions {
        &self.opts
    }

    /// The schema of a type reference.
    pub fn type_ref(&mut self, ty: &'a TypeRef) -> Value {
        match ty {
            TypeRef::Inline(shape) => self.shape(shape),
            TypeRef::Named(id) => self.named(id),
        }
    }

    fn named(&mut self, id: &'a TypeId) -> Value {
        let ir = self.ir;
        let Some(named) = ir.types.get(id) else {
            return json!({});
        };
        let by_ref = named.recursive
            || self.stack.contains(&id)
            || self.stack.len() >= self.opts.inline_depth
            || self.defs.contains_key(&id.0);
        if by_ref {
            self.define(id);
            return json!({ "$ref": format!("#/$defs/{}", escape_pointer(&id.0)) });
        }
        self.stack.push(id);
        let mut schema = self.shape(&named.shape);
        self.stack.pop();
        if let Some(text) = self.description(named.doc.as_ref()) {
            insert_absent(&mut schema, "description", Value::String(text));
        }
        schema
    }

    /// Build the `$defs` entry of `id` once, with a fresh inlining depth.
    fn define(&mut self, id: &'a TypeId) {
        if self.defs.contains_key(&id.0) {
            return;
        }
        let ir = self.ir;
        let Some(named) = ir.types.get(id) else {
            return;
        };
        self.defs.insert(id.0.clone(), Value::Null);
        let outer = std::mem::replace(&mut self.stack, vec![id]);
        let mut schema = self.shape(&named.shape);
        self.stack = outer;
        if let Some(text) = self.description(named.doc.as_ref()) {
            insert_absent(&mut schema, "description", Value::String(text));
        }
        self.defs.insert(id.0.clone(), schema);
    }

    /// The schema of a shape.
    pub fn shape(&mut self, shape: &'a Shape) -> Value {
        match shape {
            Shape::Primitive {
                primitive,
                constraints,
            } => {
                let mut schema = primitive_schema(primitive);
                apply_constraints(&mut schema, constraints);
                schema
            }
            Shape::Enum { base, values } => {
                let mut schema = primitive_schema(base);
                set(
                    &mut schema,
                    "enum",
                    values.iter().map(|v| v.value.clone()).collect(),
                );
                schema
            }
            Shape::Const { value } => json!({ "const": value }),
            Shape::Array {
                items,
                min,
                max,
                unique,
            } => {
                let mut schema = json!({ "type": "array", "items": self.type_ref(items) });
                if let Some(min) = min {
                    set(&mut schema, "minItems", json!(min));
                }
                if let Some(max) = max {
                    set(&mut schema, "maxItems", json!(max));
                }
                if *unique {
                    set(&mut schema, "uniqueItems", json!(true));
                }
                schema
            }
            Shape::Map { values } => {
                json!({ "type": "object", "additionalProperties": self.type_ref(values) })
            }
            Shape::Record { fields, additional } => self.record(fields, additional),
            Shape::Union(union) => self.union(union),
            Shape::Intersection { members } => {
                let all: Vec<Value> = members.iter().map(|m| self.type_ref(m)).collect();
                json!({ "allOf": all })
            }
            Shape::Nullable { inner } => nullable(self.type_ref(inner)),
            Shape::Any => json!({}),
            Shape::Never => json!({ "not": {} }),
        }
    }

    /// The schema of a record with these fields and extras.
    pub fn record(&mut self, fields: &'a [Field], additional: &'a Additional) -> Value {
        let mut properties = Map::new();
        let mut required = vec![];
        let usage = self.opts.usage;
        for field in fields.iter().filter(|f| includes(usage, f)) {
            if is_required(field.presence) {
                required.push(Value::String(field.wire_name.clone()));
            }
            properties.insert(field.wire_name.clone(), self.field(field));
        }
        let mut schema = json!({ "type": "object", "properties": properties });
        if !required.is_empty() {
            set(&mut schema, "required", Value::Array(required));
        }
        match additional {
            Additional::Closed => set(&mut schema, "additionalProperties", json!(false)),
            Additional::Typed { values } => {
                let values = self.type_ref(values);
                set(&mut schema, "additionalProperties", values);
            }
            Additional::Open => {}
        }
        schema
    }

    /// The schema of one record field: its type, constraints, annotations
    /// and nullability (whether it is required is the record's business).
    pub fn field(&mut self, field: &'a Field) -> Value {
        let mut schema = self.type_ref(&field.ty);
        apply_constraints(&mut schema, &field.constraints);
        if let Some(text) = self.description(field.doc.as_ref()) {
            set(&mut schema, "description", Value::String(text));
        }
        if let Some(default) = &field.default {
            set(&mut schema, "default", default.clone());
        }
        if field.deprecated {
            set(&mut schema, "deprecated", json!(true));
        }
        if self.opts.usage == Usage::Any {
            if field.read_only {
                set(&mut schema, "readOnly", json!(true));
            }
            if field.write_only {
                set(&mut schema, "writeOnly", json!(true));
            }
        }
        match field.presence {
            Presence::RequiredNullable | Presence::OptionalNullable => nullable(schema),
            Presence::Required | Presence::Optional => schema,
        }
    }

    fn union(&mut self, union: &'a Union) -> Value {
        let variants: Vec<Value> = union
            .variants
            .iter()
            .map(|v| {
                let schema = self.type_ref(&v.ty);
                match (&union.discriminator, &v.tag, union.strategy) {
                    (Some(d), Some(tag), UnionStrategy::Tagged) => {
                        pin_tag(schema, &d.property, tag)
                    }
                    _ => schema,
                }
            })
            .collect();
        let consts: Option<Vec<Value>> = variants
            .iter()
            .map(|v| match v.as_object() {
                Some(o) if o.len() == 1 => o.get("const").cloned(),
                _ => None,
            })
            .collect();
        match (union.strategy, consts) {
            (UnionStrategy::Literal, Some(values)) if !values.is_empty() => {
                json!({ "enum": values })
            }
            (UnionStrategy::Tagged, _) if union.discriminator.is_some() => {
                json!({ "oneOf": variants })
            }
            _ => json!({ "anyOf": variants }),
        }
    }

    /// The object schema of an operation's arguments (see
    /// [`crate::args`]): parameters by key, then the merged body fields or
    /// the body argument. Closed unless a merged body record is open.
    pub fn args(&mut self, layout: &ArgsLayout<'a>) -> Value {
        let mut properties = Map::new();
        let mut required = vec![];
        for p in &layout.params {
            let mut schema = self.type_ref(&p.param.ty);
            if let Some(text) = self.description(p.param.doc.as_ref()) {
                set(&mut schema, "description", Value::String(text));
            }
            if p.param.deprecated {
                set(&mut schema, "deprecated", json!(true));
            }
            if p.param.required {
                required.push(Value::String(p.key.clone()));
            }
            properties.insert(p.key.clone(), schema);
        }
        let mut closed = true;
        match &layout.body {
            Some(BodyArg::Merged {
                fields, additional, ..
            }) => {
                closed = matches!(additional, Additional::Closed);
                let usage = self.opts.usage;
                for field in fields.iter().filter(|f| includes(usage, f)) {
                    if layout.body_required && is_required(field.presence) {
                        required.push(Value::String(field.wire_name.clone()));
                    }
                    properties.insert(field.wire_name.clone(), self.field(field));
                }
            }
            Some(BodyArg::Arg { key, content }) => {
                if layout.body_required {
                    required.push(Value::String(key.clone()));
                }
                let mut schema = self.type_ref(&content.ty);
                insert_absent(
                    &mut schema,
                    "description",
                    Value::String(format!("Request body ({}).", content.media_type)),
                );
                properties.insert(key.clone(), schema);
            }
            None => {}
        }
        let mut schema = json!({ "type": "object", "properties": properties });
        if !required.is_empty() {
            set(&mut schema, "required", Value::Array(required));
        }
        if closed {
            set(&mut schema, "additionalProperties", json!(false));
        }
        schema
    }

    /// A description pruned per the options, or `None` when descriptions
    /// are off or the doc is empty. Prefers the full description, then the
    /// summary.
    pub fn description(&self, doc: Option<&Doc>) -> Option<String> {
        if !self.opts.descriptions {
            return None;
        }
        let doc = doc?;
        let text = doc.description.as_deref().or(doc.summary.as_deref())?;
        let text = match self.opts.max_sentences {
            Some(n) => prune_sentences(text, n),
            None => collapse_whitespace(text),
        };
        (!text.is_empty()).then_some(text)
    }

    /// The `$defs` built so far, by type id.
    pub fn defs(&self) -> &BTreeMap<String, Value> {
        &self.defs
    }

    /// `root` with `$schema` first and the collected `$defs` last (when
    /// any). `root` must be an object; anything else is returned as is.
    pub fn finish(self, root: Value) -> Value {
        let Value::Object(body) = root else {
            return root;
        };
        let mut out = Map::new();
        out.insert("$schema".into(), json!(DRAFT_2020_12));
        out.extend(body);
        if !self.defs.is_empty() {
            out.insert(
                "$defs".into(),
                Value::Object(self.defs.into_iter().collect()),
            );
        }
        Value::Object(out)
    }
}

/// A standalone schema of one type: `$schema`, the type and its `$defs`.
pub fn type_schema(ir: &Ir, ty: &TypeRef, opts: SchemaOptions) -> Value {
    let mut b = SchemaBuilder::new(ir, opts);
    let root = b.type_ref(ty);
    b.finish(root)
}

/// Whether a record field appears in schemas for `usage`.
pub fn includes(usage: Usage, field: &Field) -> bool {
    match usage {
        Usage::Any => true,
        Usage::Request => !field.read_only,
        Usage::Response => !field.write_only,
    }
}

/// Whether a presence makes a record field required.
pub fn is_required(presence: Presence) -> bool {
    matches!(presence, Presence::Required | Presence::RequiredNullable)
}

/// `schema` that also admits `null`: `"null"` added to its `type` (and its
/// `enum`), or `anyOf` with `{"type": "null"}` when it has no `type`.
/// `{}` already admits `null`.
pub fn nullable(schema: Value) -> Value {
    let Value::Object(mut o) = schema else {
        return schema;
    };
    if o.is_empty() {
        return Value::Object(o);
    }
    let null = Value::String("null".into());
    match o.get_mut("type") {
        Some(Value::String(t)) => {
            let t = std::mem::take(t);
            o.insert("type".into(), json!([t, "null"]));
        }
        Some(Value::Array(types)) => {
            if !types.contains(&null) {
                types.push(null);
            }
        }
        _ => return json!({ "anyOf": [Value::Object(o), { "type": "null" }] }),
    }
    if let Some(Value::Array(values)) = o.get_mut("enum")
        && !values.contains(&Value::Null)
    {
        values.push(Value::Null);
    }
    Value::Object(o)
}

/// The first `max` sentences of `text`, whitespace collapsed. A sentence
/// ends at `.`, `!` or `?` followed by whitespace or the end of the text.
pub fn prune_sentences(text: &str, max: usize) -> String {
    let text = collapse_whitespace(text);
    if max == 0 {
        return String::new();
    }
    let mut count = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let ends = matches!(c, '.' | '!' | '?')
            && chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        if ends {
            count += 1;
            if count == max {
                return text[..i + c.len_utf8()].to_string();
            }
        }
    }
    text
}

/// `text` on one line with runs of whitespace replaced by one space.
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// RFC 6901 escaping of one reference token.
fn escape_pointer(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn primitive_schema(p: &Primitive) -> Value {
    match p {
        Primitive::String { format } => {
            let mut schema = json!({ "type": "string" });
            match format {
                None => {}
                Some(StringFormat::Byte) => set(&mut schema, "contentEncoding", json!("base64")),
                Some(f) => set(&mut schema, "format", json!(format_name(f))),
            }
            schema
        }
        Primitive::Int32 => json!({ "type": "integer", "format": "int32" }),
        Primitive::Int64 => json!({ "type": "integer", "format": "int64" }),
        Primitive::Integer => json!({ "type": "integer" }),
        Primitive::Float => json!({ "type": "number", "format": "float" }),
        Primitive::Double => json!({ "type": "number", "format": "double" }),
        Primitive::Number => json!({ "type": "number" }),
        Primitive::Bool => json!({ "type": "boolean" }),
        Primitive::Bytes => json!({ "type": "string", "contentEncoding": "base64" }),
    }
}

fn format_name(f: &StringFormat) -> &str {
    match f {
        StringFormat::Uuid => "uuid",
        StringFormat::DateTime => "date-time",
        StringFormat::Date => "date",
        StringFormat::Time => "time",
        StringFormat::Duration => "duration",
        StringFormat::Email => "email",
        StringFormat::Uri => "uri",
        StringFormat::Hostname => "hostname",
        StringFormat::Ipv4 => "ipv4",
        StringFormat::Ipv6 => "ipv6",
        StringFormat::Byte => "byte",
        StringFormat::Password => "password",
        StringFormat::Other(other) => other,
    }
}

fn apply_constraints(schema: &mut Value, c: &tungsten_ir::Constraints) {
    let entries = [
        ("pattern", c.pattern.as_ref().map(|p| json!(p))),
        ("minLength", c.min_length.map(|n| json!(n))),
        ("maxLength", c.max_length.map(|n| json!(n))),
        ("minimum", c.minimum.as_ref().map(|n| json!(n))),
        ("maximum", c.maximum.as_ref().map(|n| json!(n))),
        (
            "exclusiveMinimum",
            c.exclusive_minimum.as_ref().map(|n| json!(n)),
        ),
        (
            "exclusiveMaximum",
            c.exclusive_maximum.as_ref().map(|n| json!(n)),
        ),
        ("multipleOf", c.multiple_of.as_ref().map(|n| json!(n))),
    ];
    for (key, value) in entries {
        if let Some(value) = value {
            set(schema, key, value);
        }
    }
}

/// A tagged variant whose discriminator property only admits `tag`: set in
/// place on an inline object schema, else by `allOf`.
fn pin_tag(schema: Value, property: &str, tag: &str) -> Value {
    let pinned = json!({ "const": tag });
    if let Value::Object(mut o) = schema {
        if let Some(Value::Object(props)) = o.get_mut("properties") {
            props.insert(property.to_string(), pinned);
            let required = o.entry("required").or_insert_with(|| json!([]));
            if let Value::Array(names) = required
                && !names.iter().any(|n| n == property)
            {
                names.push(json!(property));
            }
            return Value::Object(o);
        }
        return json!({
            "allOf": [Value::Object(o)],
            "properties": { property: pinned },
            "required": [property],
        });
    }
    schema
}

/// Set `key` on an object schema, replacing any value.
fn set(schema: &mut Value, key: &str, value: Value) {
    if let Value::Object(o) = schema {
        o.insert(key.to_string(), value);
    }
}

/// Set `key` on an object schema unless it is present.
fn insert_absent(schema: &mut Value, key: &str, value: Value) {
    if let Value::Object(o) = schema {
        o.entry(key.to_string()).or_insert(value);
    }
}
