// SPDX-License-Identifier: AGPL-3.0-only
//! Walking the published JSON Schemas of the manifests.
//!
//! The schemas (`tungsten schema tungsten`, `tungsten schema agent`) are
//! generated from the types the compiler deserializes, so completion built
//! on them follows the compiler instead of a copy of its key lists.

use serde_json::Value;

use crate::scan::Seg;

/// A schema and the nodes a path of keys and items leads to. A node can be
/// several alternatives (`anyOf`, `oneOf`), so a lookup returns a list.
#[derive(Debug)]
pub struct Schema {
    root: Value,
}

/// One property of an object schema.
#[derive(Debug, Clone)]
pub struct Property {
    pub name: String,
    pub description: Option<String>,
}

/// One allowed value of an enum.
#[derive(Debug, Clone)]
pub struct Choice {
    pub value: String,
    pub description: Option<String>,
}

impl Schema {
    pub fn new(root: Value) -> Self {
        Self { root }
    }

    /// The alternatives at `path`.
    pub fn at<'a>(&'a self, path: &[Seg]) -> Vec<&'a Value> {
        let mut nodes = self.expand(&self.root);
        for seg in path {
            let mut next = vec![];
            for node in &nodes {
                match seg {
                    Seg::Key(key) => {
                        if let Some(child) = node.get("properties").and_then(|p| p.get(key)) {
                            next.extend(self.expand(child));
                        } else if let Some(extra) =
                            node.get("additionalProperties").filter(|v| v.is_object())
                        {
                            next.extend(self.expand(extra));
                        }
                    }
                    Seg::Item(_) => {
                        if let Some(items) = node.get("items") {
                            next.extend(self.expand(items));
                        }
                    }
                }
            }
            nodes = next;
        }
        nodes
    }

    /// `node` with references followed and alternatives flattened.
    fn expand<'a>(&'a self, node: &'a Value) -> Vec<&'a Value> {
        let mut out = vec![];
        self.expand_into(node, &mut out, 0);
        out
    }

    fn expand_into<'a>(&'a self, node: &'a Value, out: &mut Vec<&'a Value>, depth: usize) {
        if depth > 16 {
            return;
        }
        if let Some(target) = node
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix('#'))
            .and_then(|p| self.root.pointer(p))
        {
            self.expand_into(target, out, depth + 1);
        }
        for key in ["anyOf", "oneOf", "allOf"] {
            if let Some(list) = node.get(key).and_then(Value::as_array) {
                for alt in list {
                    self.expand_into(alt, out, depth + 1);
                }
            }
        }
        if node.get("type").and_then(Value::as_str) == Some("null") {
            return;
        }
        let composite = node.get("$ref").is_some()
            || ["anyOf", "oneOf", "allOf"]
                .iter()
                .any(|k| node.get(*k).is_some());
        let concrete = ["properties", "enum", "const"]
            .iter()
            .any(|k| node.get(*k).is_some());
        if !composite || concrete {
            out.push(node);
        }
    }

    /// Properties of the object alternatives at `path`.
    pub fn properties(&self, path: &[Seg]) -> Vec<Property> {
        let mut out: Vec<Property> = vec![];
        for node in self.at(path) {
            let Some(props) = node.get("properties").and_then(Value::as_object) else {
                continue;
            };
            for (name, def) in props {
                if out.iter().any(|p| &p.name == name) {
                    continue;
                }
                out.push(Property {
                    name: name.clone(),
                    description: self.description_of(def),
                });
            }
        }
        out
    }

    /// Allowed string values at `path`, from enums and constants.
    pub fn choices(&self, path: &[Seg]) -> Vec<Choice> {
        let mut out: Vec<Choice> = vec![];
        let mut push = |value: &str, description: Option<String>| {
            if !out.iter().any(|c| c.value == value) {
                out.push(Choice {
                    value: value.to_string(),
                    description,
                });
            }
        };
        for node in self.at(path) {
            if let Some(list) = node.get("enum").and_then(Value::as_array) {
                for v in list.iter().filter_map(Value::as_str) {
                    push(v, None);
                }
            }
            if let Some(v) = node.get("const").and_then(Value::as_str) {
                push(
                    v,
                    node.get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                );
            }
        }
        out
    }

    /// Whether a boolean is allowed at `path`.
    pub fn is_boolean(&self, path: &[Seg]) -> bool {
        self.at(path).iter().any(|n| match n.get("type") {
            Some(Value::String(t)) => t == "boolean",
            Some(Value::Array(ts)) => ts.iter().any(|t| t == "boolean"),
            _ => false,
        })
    }

    /// Whether the node at `path` is an object with properties (as opposed
    /// to a scalar), so a sequence item there can start a mapping.
    pub fn has_properties(&self, path: &[Seg]) -> bool {
        self.at(path).iter().any(|n| {
            n.get("properties")
                .is_some_and(|p| p.as_object().is_some_and(|o| !o.is_empty()))
        })
    }

    /// The documentation of the node at `path`: the description beside a
    /// reference wins over the referenced type's.
    pub fn description(&self, path: &[Seg]) -> Option<String> {
        let (last, parent) = path.split_last()?;
        for node in self.at(parent) {
            let child = match last {
                Seg::Key(k) => node
                    .get("properties")
                    .and_then(|p| p.get(k))
                    .or_else(|| node.get("additionalProperties").filter(|v| v.is_object())),
                Seg::Item(_) => node.get("items"),
            };
            if let Some(text) = child.and_then(|c| self.description_of(c)) {
                return Some(text);
            }
        }
        None
    }

    fn description_of(&self, def: &Value) -> Option<String> {
        let own = def.get("description").and_then(Value::as_str);
        let text = own.map(str::to_string).or_else(|| {
            self.expand(def).iter().find_map(|n| {
                n.get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })?;
        Some(
            text.trim()
                .replace("\n\n", "\u{0}")
                .replace('\n', " ")
                .replace('\u{0}', "\n\n"),
        )
    }
}
