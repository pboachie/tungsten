// SPDX-License-Identifier: AGPL-3.0-only
//! The IR type system.
//!
//! Types are either named (registered once in the [`TypeTable`] under a
//! [`TypeId`]) or inline [`Shape`]s. A record field keeps required,
//! nullable and optional apart through [`Presence`], so each target language
//! can represent all four combinations faithfully.

use serde::{Deserialize, Serialize};

use crate::{Doc, Ident, SourceRef, ir_struct};

/// Globally unique, stable type id: `namespace.Name` (for example
/// `public.WebhookEndpointView`). Inline schemas that need a name get one
/// derived from their position (`public.SubmitAlphaMessageBody`).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct TypeId(pub String);

ir_struct! {
    /// All named types, sorted by id.
    #[derive(Default)]
    pub struct TypeTable {
        pub types: Vec<NamedType>,
    }
}

impl TypeTable {
    pub fn get(&self, id: &TypeId) -> Option<&NamedType> {
        self.types
            .binary_search_by(|t| t.id.cmp(id))
            .ok()
            .map(|i| &self.types[i])
    }
    /// Sort by id; call after inserting.
    pub fn sort(&mut self) {
        self.types.sort_by(|a, b| a.id.cmp(&b.id));
    }
}

ir_struct! {
    pub struct NamedType {
        pub id: TypeId,
        pub name: Ident,
        pub namespace: String,
        pub shape: Shape,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
        /// True when the type participates in a reference cycle.
        #[serde(default)]
        pub recursive: bool,
        pub origin: SourceRef,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TypeRef {
    Named(TypeId),
    Inline(Box<Shape>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Primitive {
    String {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<StringFormat>,
    },
    Int32,
    Int64,
    /// `integer` with no format: emitters choose the widest safe integer.
    Integer,
    Float,
    Double,
    /// `number` with no format.
    Number,
    Bool,
    /// `format: binary` or a non-JSON request body.
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StringFormat {
    Uuid,
    DateTime,
    Date,
    Time,
    Duration,
    Email,
    Uri,
    Hostname,
    Ipv4,
    Ipv6,
    Byte,
    Password,
    /// Any other format string, preserved.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shape {
    Primitive {
        primitive: Primitive,
        #[serde(default, skip_serializing_if = "Constraints::is_empty")]
        constraints: Constraints,
    },
    Enum {
        base: Primitive,
        values: Vec<EnumValue>,
    },
    Const {
        value: serde_json::Value,
    },
    Array {
        items: TypeRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
        #[serde(default)]
        unique: bool,
    },
    /// `additionalProperties: <schema>` with no fixed properties.
    Map {
        values: TypeRef,
    },
    Record {
        fields: Vec<Field>,
        additional: Additional,
    },
    Union(Union),
    /// `allOf` that could not be flattened (TG0302).
    Intersection {
        members: Vec<TypeRef>,
    },
    Nullable {
        inner: TypeRef,
    },
    Any,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Additional {
    /// `additionalProperties: false`.
    Closed,
    /// Unspecified or `true`.
    Open,
    /// Fixed properties plus typed extras.
    Typed { values: TypeRef },
}

ir_struct! {
    pub struct EnumValue {
        pub value: serde_json::Value,
        pub name: Ident,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Required,
    RequiredNullable,
    Optional,
    OptionalNullable,
}

ir_struct! {
    pub struct Field {
        pub wire_name: String,
        pub name: Ident,
        /// The field type with nullability removed; nullability lives in
        /// `presence`.
        pub ty: TypeRef,
        pub presence: Presence,
        #[serde(default)]
        pub read_only: bool,
        #[serde(default)]
        pub write_only: bool,
        #[serde(default)]
        pub deprecated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub default: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
        #[serde(default, skip_serializing_if = "Constraints::is_empty")]
        pub constraints: Constraints,
        /// From `x-agent-sensitive` or agent.yml; never logged.
        #[serde(default)]
        pub sensitive: bool,
    }
}

ir_struct! {
    #[derive(Default)]
    pub struct Constraints {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pattern: Option<String>,
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
    }
}

impl Constraints {
    pub fn is_empty(&self) -> bool {
        *self == Constraints::default()
    }
}

ir_struct! {
    pub struct Union {
        pub variants: Vec<Variant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub discriminator: Option<Discriminator>,
        pub strategy: UnionStrategy,
    }
}

ir_struct! {
    pub struct Variant {
        pub name: Ident,
        pub ty: TypeRef,
        /// Discriminator value selecting this variant, when tagged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tag: Option<String>,
    }
}

ir_struct! {
    pub struct Discriminator {
        pub property: String,
        /// Explicit mapping from the spec, or derived from `const` fields.
        pub mapping: Vec<(String, TypeId)>,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnionStrategy {
    /// Discriminator property (declared or detected from `const`).
    Tagged,
    /// No discriminator: try variants in the listed order (TG0301).
    Untagged,
    /// Union of literals / primitives that a value check can tell apart.
    Literal,
}
