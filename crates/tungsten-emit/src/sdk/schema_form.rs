// SPDX-License-Identifier: AGPL-3.0-only
//! The runtime schema form: IR types as the validators of every SDK runtime
//! built on the descriptor document read them (the JSON form of the Go
//! runtime's `Schema`). The rules a runtime applies are specified in
//! [`super::descriptors`].

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;
use tungsten_ir::{
    Additional, Constraints, Field, Ir, Presence, Primitive, Shape, StringFormat, TypeId, TypeRef,
    UnionStrategy,
};

/// The kind of a runtime schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchemaKind {
    /// Any JSON value.
    Any,
    /// No value at all.
    Never,
    String,
    /// An integer: a JSON number without a fractional part, at most 2^53 - 1
    /// in magnitude; `bits` 32 narrows it to a signed 32-bit integer.
    Integer,
    /// Any finite JSON number.
    Number,
    Boolean,
    /// Binary data: the runtime's byte type, or a string (base64) in JSON.
    Bytes,
    /// One of `values` (JSON equality).
    Enum,
    /// Exactly `values[0]`.
    Const,
    /// A JSON array of `items`.
    Array,
    /// A JSON object whose every member is an `items`.
    Map,
    /// A JSON object with `fields` and `additional` members.
    Object,
    /// One of `variants`: by `tag` when set, else the first that accepts.
    Union,
    /// Every one of `variants`.
    All,
    /// `null`, or an `inner`.
    Nullable,
    /// The named type `ref` (an entry of the document's `defs`).
    Ref,
    /// The constraints of this schema applied to whatever kind the value
    /// has (string constraints to a string, number constraints to a
    /// number): field-level constraints next to a field's type.
    Limits,
}

/// What an `object` schema does with members it does not list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdditionalMembers {
    /// Ignored.
    Open,
    /// Refused (`unknown member <name>`, reported last).
    Closed,
    /// Checked against `extra`.
    Schema,
}

/// One schema of the runtime schema form. Only the members its kind uses
/// are present.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RuntimeSchema {
    pub kind: SchemaKind,
    /// String format: `uuid`, `email`, `date-time`, `date`, `ipv4` and
    /// `ipv6` are checked; any other name is documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// ECMAScript regular expression source, unanchored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Integer width: 32 or 64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<u8>,
    /// String length bounds, in UTF-16 code units (as the TypeScript
    /// runtime counts them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_minimum: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_maximum: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple_of: Option<serde_json::Number>,
    /// The values of an enum, the value of a const.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<serde_json::Value>>,
    /// Items of an array, member values of a map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<RuntimeSchema>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    /// Items must be pairwise different (JSON equality).
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
    /// Members of an object, in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<SchemaField>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional: Option<AdditionalMembers>,
    /// Schema of the members an object does not list (`additional:
    /// schema`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Box<RuntimeSchema>>,
    /// Variants of a union, members of an `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<RuntimeSchema>>,
    /// The discriminator member of a tagged union.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The tag value of each variant, in variant order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// The type of a nullable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner: Option<Box<RuntimeSchema>>,
    /// The type id a `ref` names.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One member of an object schema.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SchemaField {
    /// The member's name in the value checked: the wire name in a model,
    /// the target's argument name in an operation's `request`.
    pub name: String,
    /// The arguments layout key (`request` schemas only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The name on the wire.
    pub wire: String,
    pub schema: RuntimeSchema,
    /// The member must be present.
    pub required: bool,
    /// `null` is accepted in place of a value.
    pub nullable: bool,
}

impl RuntimeSchema {
    /// A schema of this kind with nothing else set.
    pub fn of(kind: SchemaKind) -> Self {
        Self {
            kind,
            format: None,
            pattern: None,
            bits: None,
            min_length: None,
            max_length: None,
            minimum: None,
            maximum: None,
            exclusive_minimum: None,
            exclusive_maximum: None,
            multiple_of: None,
            values: None,
            items: None,
            min_items: None,
            max_items: None,
            unique: false,
            fields: None,
            additional: None,
            extra: None,
            variants: None,
            tag: None,
            tags: None,
            inner: None,
            reference: None,
        }
    }

    fn constrained(mut self, c: &Constraints) -> Self {
        self.pattern = c.pattern.clone();
        self.min_length = c.min_length;
        self.max_length = c.max_length;
        self.minimum = c.minimum.clone();
        self.maximum = c.maximum.clone();
        self.exclusive_minimum = c.exclusive_minimum.clone();
        self.exclusive_maximum = c.exclusive_maximum.clone();
        self.multiple_of = c.multiple_of.clone();
        self
    }
}

/// The JSON Schema `format` name of an IR string format; `None` for the
/// formats that only document (`byte`, `password`).
pub fn format_name(f: &StringFormat) -> Option<String> {
    Some(
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
            StringFormat::Byte | StringFormat::Password => return None,
            StringFormat::Other(name) => name.as_str(),
        }
        .to_string(),
    )
}

/// Renders IR types into the runtime schema form, collecting the named
/// types they reference for the document's `defs`.
#[derive(Debug)]
pub struct SchemaFormBuilder<'a> {
    ir: &'a Ir,
    referenced: BTreeSet<TypeId>,
}

impl<'a> SchemaFormBuilder<'a> {
    pub fn new(ir: &'a Ir) -> Self {
        Self {
            ir,
            referenced: BTreeSet::new(),
        }
    }

    /// The schema of a type reference: a `ref` for a named type.
    pub fn type_ref(&mut self, ty: &TypeRef) -> RuntimeSchema {
        match ty {
            TypeRef::Named(id) => {
                if self.ir.types.get(id).is_none() {
                    return RuntimeSchema::of(SchemaKind::Any);
                }
                self.referenced.insert(id.clone());
                RuntimeSchema {
                    reference: Some(id.0.clone()),
                    ..RuntimeSchema::of(SchemaKind::Ref)
                }
            }
            TypeRef::Inline(shape) => self.shape(shape),
        }
    }

    /// The schema of a shape.
    pub fn shape(&mut self, shape: &Shape) -> RuntimeSchema {
        match shape {
            Shape::Primitive {
                primitive,
                constraints,
            } => primitive_schema(primitive).constrained(constraints),
            Shape::Enum { values, .. } => RuntimeSchema {
                values: Some(values.iter().map(|v| v.value.clone()).collect()),
                ..RuntimeSchema::of(SchemaKind::Enum)
            },
            Shape::Const { value } => RuntimeSchema {
                values: Some(vec![value.clone()]),
                ..RuntimeSchema::of(SchemaKind::Const)
            },
            Shape::Array {
                items,
                min,
                max,
                unique,
            } => RuntimeSchema {
                items: Some(Box::new(self.type_ref(items))),
                min_items: *min,
                max_items: *max,
                unique: *unique,
                ..RuntimeSchema::of(SchemaKind::Array)
            },
            Shape::Map { values } => RuntimeSchema {
                items: Some(Box::new(self.type_ref(values))),
                ..RuntimeSchema::of(SchemaKind::Map)
            },
            Shape::Record { fields, additional } => {
                let fields = fields
                    .iter()
                    .map(|f| {
                        let schema = self.type_ref(&f.ty);
                        self.field(f.wire_name.clone(), None, f.wire_name.clone(), schema, f)
                    })
                    .collect();
                let (additional, extra) = match additional {
                    Additional::Open => (AdditionalMembers::Open, None),
                    Additional::Closed => (AdditionalMembers::Closed, None),
                    Additional::Typed { values } => (
                        AdditionalMembers::Schema,
                        Some(Box::new(self.type_ref(values))),
                    ),
                };
                RuntimeSchema {
                    fields: Some(fields),
                    additional: Some(additional),
                    extra,
                    ..RuntimeSchema::of(SchemaKind::Object)
                }
            }
            Shape::Union(u) => {
                let variants: Vec<RuntimeSchema> =
                    u.variants.iter().map(|v| self.type_ref(&v.ty)).collect();
                let tags: Vec<String> = u.variants.iter().filter_map(|v| v.tag.clone()).collect();
                let tagged = u.strategy == UnionStrategy::Tagged
                    && u.discriminator
                        .as_ref()
                        .is_some_and(|d| !d.property.is_empty())
                    && !variants.is_empty()
                    && tags.len() == variants.len();
                RuntimeSchema {
                    variants: Some(variants),
                    tag: if tagged {
                        u.discriminator.as_ref().map(|d| d.property.clone())
                    } else {
                        None
                    },
                    tags: tagged.then_some(tags),
                    ..RuntimeSchema::of(SchemaKind::Union)
                }
            }
            Shape::Intersection { members } => RuntimeSchema {
                variants: Some(members.iter().map(|m| self.type_ref(m)).collect()),
                ..RuntimeSchema::of(SchemaKind::All)
            },
            Shape::Nullable { inner } => RuntimeSchema {
                inner: Some(Box::new(self.type_ref(inner))),
                ..RuntimeSchema::of(SchemaKind::Nullable)
            },
            Shape::Any => RuntimeSchema::of(SchemaKind::Any),
            Shape::Never => RuntimeSchema::of(SchemaKind::Never),
        }
    }

    /// One object member for a record field: its presence, and the
    /// constraints written on the field (unless its inline primitive type
    /// carries them already) as a `limits` check next to its type.
    pub fn field(
        &mut self,
        name: String,
        key: Option<String>,
        wire: String,
        schema: RuntimeSchema,
        f: &Field,
    ) -> SchemaField {
        let own = matches!(&f.ty, TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Primitive { constraints, .. } if !constraints.is_empty()));
        let schema = if f.constraints.is_empty() || own {
            schema
        } else {
            limits(schema, &f.constraints)
        };
        SchemaField {
            name,
            key,
            wire,
            schema,
            required: matches!(f.presence, Presence::Required | Presence::RequiredNullable),
            nullable: matches!(
                f.presence,
                Presence::RequiredNullable | Presence::OptionalNullable
            ),
        }
    }

    /// The schemas of every named type referenced so far and of the types
    /// they reference, by id.
    pub fn finish(mut self) -> BTreeMap<String, RuntimeSchema> {
        let mut defs: BTreeMap<String, RuntimeSchema> = BTreeMap::new();
        loop {
            let pending: Vec<TypeId> = self
                .referenced
                .iter()
                .filter(|id| !defs.contains_key(&id.0))
                .cloned()
                .collect();
            if pending.is_empty() {
                break;
            }
            for id in pending {
                let Some(t) = self.ir.types.get(&id) else {
                    defs.insert(id.0.clone(), RuntimeSchema::of(SchemaKind::Any));
                    continue;
                };
                let schema = self.shape(&t.shape);
                defs.insert(id.0.clone(), schema);
            }
        }
        defs
    }
}

/// `schema` and a `limits` check of `c`, as an `all`.
pub fn limits(schema: RuntimeSchema, c: &Constraints) -> RuntimeSchema {
    RuntimeSchema {
        variants: Some(vec![
            schema,
            RuntimeSchema::of(SchemaKind::Limits).constrained(c),
        ]),
        ..RuntimeSchema::of(SchemaKind::All)
    }
}

fn primitive_schema(p: &Primitive) -> RuntimeSchema {
    match p {
        Primitive::String { format } => RuntimeSchema {
            format: format.as_ref().and_then(format_name),
            ..RuntimeSchema::of(SchemaKind::String)
        },
        Primitive::Int32 => RuntimeSchema {
            bits: Some(32),
            ..RuntimeSchema::of(SchemaKind::Integer)
        },
        Primitive::Int64 => RuntimeSchema {
            bits: Some(64),
            ..RuntimeSchema::of(SchemaKind::Integer)
        },
        Primitive::Integer => RuntimeSchema::of(SchemaKind::Integer),
        Primitive::Float | Primitive::Double | Primitive::Number => {
            RuntimeSchema::of(SchemaKind::Number)
        }
        Primitive::Bool => RuntimeSchema::of(SchemaKind::Boolean),
        Primitive::Bytes => RuntimeSchema::of(SchemaKind::Bytes),
    }
}
