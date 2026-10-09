// SPDX-License-Identifier: AGPL-3.0-only
//! Structural walk over an OpenAPI document.
//!
//! Every object node is visited with the kind of OpenAPI object it is, so
//! that `$ref`s are only recognized where the specification allows them
//! (never inside `example` values or extensions) and schema rewrites only
//! touch schemas (a property named `example` is not the `example` keyword).

use serde_json::Value;

use crate::join_pointer;

/// The kind of OpenAPI object found at a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    Document,
    PathItem,
    Operation,
    Parameter,
    Header,
    RequestBody,
    Response,
    MediaType,
    Encoding,
    Callback,
    Example,
    Link,
    SecurityScheme,
    Schema,
}

/// One visited object node.
#[derive(Debug)]
pub(crate) struct Node<'v> {
    pub pointer: String,
    pub kind: Kind,
    pub value: &'v serde_json::Map<String, Value>,
}

impl Node<'_> {
    /// The `$ref` member, when present (a string or not).
    pub(crate) fn reference(&self) -> Option<&Value> {
        self.value.get("$ref")
    }
}

const METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

const COMPONENTS: &[(&str, Kind)] = &[
    ("schemas", Kind::Schema),
    ("responses", Kind::Response),
    ("parameters", Kind::Parameter),
    ("examples", Kind::Example),
    ("requestBodies", Kind::RequestBody),
    ("headers", Kind::Header),
    ("securitySchemes", Kind::SecurityScheme),
    ("links", Kind::Link),
    ("callbacks", Kind::Callback),
    ("pathItems", Kind::PathItem),
];

/// Schema keywords holding one subschema (or, for `items`, an array of them
/// in older drafts).
const SCHEMA_SINGLE: &[&str] = &[
    "items",
    "additionalProperties",
    "not",
    "if",
    "then",
    "else",
    "contains",
    "propertyNames",
    "unevaluatedItems",
    "unevaluatedProperties",
    "additionalItems",
    "contentSchema",
];
/// Schema keywords holding a map of subschemas.
const SCHEMA_MAPS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];
/// Schema keywords holding an array of subschemas.
const SCHEMA_LISTS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];

/// Walk the subtree at `pointer` (whose value is `value`) as an object of
/// `kind`, in document pre-order. `visit` returns whether to descend into
/// the node's children. Non-object values are skipped (boolean schemas have
/// nothing to rewrite or reference).
pub(crate) fn walk<'v>(
    value: &'v Value,
    pointer: &str,
    kind: Kind,
    mut visit: impl FnMut(&Node<'v>) -> bool,
) {
    let mut stack: Vec<(String, Kind, &'v Value)> = vec![(pointer.to_string(), kind, value)];
    while let Some((pointer, kind, value)) = stack.pop() {
        let Value::Object(map) = value else {
            continue;
        };
        let node = Node {
            pointer,
            kind,
            value: map,
        };
        if !visit(&node) {
            continue;
        }
        // A Reference Object outside schemas has no other meaningful members.
        if kind != Kind::Schema && node.reference().is_some() {
            continue;
        }
        let mut children = vec![];
        children_of(&node, &mut children);
        stack.extend(children.into_iter().rev());
    }
}

fn children_of<'v>(node: &Node<'v>, out: &mut Vec<(String, Kind, &'v Value)>) {
    let map = node.value;
    let p = node.pointer.as_str();
    let one = |out: &mut Vec<_>, key: &str, kind: Kind| {
        if let Some(v) = map.get(key) {
            out.push((join_pointer(p, key), kind, v));
        }
    };
    let each = |out: &mut Vec<_>, key: &str, kind: Kind, skip_extensions: bool| {
        if let Some(Value::Object(m)) = map.get(key) {
            let base = join_pointer(p, key);
            for (k, v) in m {
                if !(skip_extensions && k.starts_with("x-")) {
                    out.push((join_pointer(&base, k), kind, v));
                }
            }
        }
    };
    let list = |out: &mut Vec<_>, key: &str, kind: Kind| {
        if let Some(Value::Array(items)) = map.get(key) {
            let base = join_pointer(p, key);
            for (i, v) in items.iter().enumerate() {
                out.push((format!("{base}/{i}"), kind, v));
            }
        }
    };
    match node.kind {
        Kind::Document => {
            each(out, "paths", Kind::PathItem, true);
            each(out, "webhooks", Kind::PathItem, false);
            if let Some(Value::Object(components)) = map.get("components") {
                let base = join_pointer(p, "components");
                for (section, kind) in COMPONENTS {
                    if let Some(Value::Object(m)) = components.get(*section) {
                        let section = join_pointer(&base, section);
                        for (k, v) in m {
                            out.push((join_pointer(&section, k), *kind, v));
                        }
                    }
                }
            }
        }
        Kind::PathItem => {
            list(out, "parameters", Kind::Parameter);
            for m in METHODS {
                one(out, m, Kind::Operation);
            }
        }
        Kind::Operation => {
            list(out, "parameters", Kind::Parameter);
            one(out, "requestBody", Kind::RequestBody);
            each(out, "responses", Kind::Response, true);
            each(out, "callbacks", Kind::Callback, false);
        }
        Kind::Callback => {
            for (k, v) in map {
                if !k.starts_with("x-") {
                    out.push((join_pointer(p, k), Kind::PathItem, v));
                }
            }
        }
        Kind::Parameter | Kind::Header => {
            one(out, "schema", Kind::Schema);
            each(out, "content", Kind::MediaType, false);
            each(out, "examples", Kind::Example, false);
        }
        Kind::RequestBody => each(out, "content", Kind::MediaType, false),
        Kind::Response => {
            each(out, "headers", Kind::Header, false);
            each(out, "content", Kind::MediaType, false);
            each(out, "links", Kind::Link, false);
        }
        Kind::MediaType => {
            one(out, "schema", Kind::Schema);
            each(out, "examples", Kind::Example, false);
            each(out, "encoding", Kind::Encoding, false);
        }
        Kind::Encoding => each(out, "headers", Kind::Header, false),
        Kind::Example | Kind::Link | Kind::SecurityScheme => {}
        Kind::Schema => {
            for key in SCHEMA_SINGLE {
                match map.get(*key) {
                    Some(Value::Array(_)) if *key == "items" => list(out, key, Kind::Schema),
                    Some(_) => one(out, key, Kind::Schema),
                    None => {}
                }
            }
            for key in SCHEMA_MAPS {
                each(out, key, Kind::Schema, false);
            }
            for key in SCHEMA_LISTS {
                list(out, key, Kind::Schema);
            }
        }
    }
}
