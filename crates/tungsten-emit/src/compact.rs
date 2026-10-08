// SPDX-License-Identifier: AGPL-3.0-only
//! Compact agent tool schemas, shared by every emitter that describes tools
//! to agents (`tools.json` and `llms-full.txt` in the docs target, the MCP
//! server's tool manifest), so all of them describe exactly the same
//! arguments object (planning/07 "MCP tool surface", NFR-3).
//!
//! A tool's parameters are the SDK's arguments object ([`crate::args`])
//! rendered on the request side with descriptions cut to two sentences and
//! named types inlined four levels deep, without `$schema` (several
//! function-calling APIs reject keywords they do not list). Compaction is
//! lossless: a scalar sub-schema repeated within one tool (the same UUID
//! pattern on eight fields) moves to `$defs` once when that makes the tool
//! smaller.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use tungsten_ir::{Ir, Macro, Operation, OperationStatus};

use crate::args::args_layout;
use crate::schema::{SchemaBuilder, SchemaOptions, Usage};

/// How tool schemas are rendered for `usage`: descriptions cut to two
/// sentences, named types inlined four levels deep.
pub fn tool_schema_options(usage: Usage) -> SchemaOptions {
    SchemaOptions {
        inline_depth: 4,
        descriptions: true,
        max_sentences: Some(2),
        usage,
    }
}

/// The arguments object schema of `op`, self-contained (`$defs` inside),
/// compacted.
pub fn operation_parameters(ir: &Ir, op: &Operation) -> Value {
    let mut b = SchemaBuilder::new(ir, tool_schema_options(Usage::Request));
    let layout = args_layout(ir, op);
    let root = b.args(&layout);
    hoist_repeats(without_dialect(b.finish(root)))
}

/// The input schema of a macro: the arguments of the operation it extends
/// (canonical macro input `{extends, add}`) plus the inputs it adds. An
/// added input without a `default` is required, as in the SDK's macro
/// input type (the runtime applies defaults only). A macro that extends no
/// callable operation starts from an empty closed object.
pub fn macro_parameters(ir: &Ir, mac: &Macro) -> Value {
    let input = &mac.input;
    let base = input
        .get("extends")
        .and_then(Value::as_str)
        .and_then(|id| callable_operation(ir, id));
    let mut params = match base {
        Some(op) => operation_parameters(ir, op),
        None => json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    };
    let Some(Value::Object(add)) = input.get("add") else {
        return params;
    };
    if let Some(Value::Object(props)) = params.get_mut("properties") {
        for (name, schema) in add {
            props.insert(name.clone(), schema.clone());
        }
    }
    let needed: Vec<&String> = add
        .iter()
        .filter(|(_, schema)| schema.get("default").is_none())
        .map(|(name, _)| name)
        .collect();
    if !needed.is_empty() {
        let mut list: Vec<Value> = params
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for name in needed {
            if !list.iter().any(|v| v.as_str() == Some(name.as_str())) {
                list.push(json!(name));
            }
        }
        set_required(&mut params, list);
    }
    params
}

/// Set an object schema's `required` list (a new one goes last).
pub fn set_required(schema: &mut Value, required: Vec<Value>) {
    if let Value::Object(obj) = schema {
        obj.insert("required".into(), Value::Array(required));
    }
}

/// A callable (not planned) operation by id.
pub fn callable_operation<'a>(ir: &'a Ir, id: &str) -> Option<&'a Operation> {
    ir.operations()
        .into_iter()
        .find(|op| op.id.0 == id && !matches!(op.status, OperationStatus::Planned { .. }))
}

/// Keys under which a schema holds sub-schemas: maps of them, single
/// ones, and arrays of them.
const SCHEMA_MAPS: &[&str] = &["properties", "$defs"];
const SCHEMA_ONE: &[&str] = &["items", "additionalProperties", "not"];
const SCHEMA_LISTS: &[&str] = &["anyOf", "oneOf", "allOf", "prefixItems"];

/// A scalar schema: no sub-schemas and no reference.
fn is_scalar(schema: &Map<String, Value>) -> bool {
    !schema.keys().any(|k| {
        SCHEMA_MAPS.contains(&k.as_str())
            || SCHEMA_ONE.contains(&k.as_str())
            || SCHEMA_LISTS.contains(&k.as_str())
            || k == "$ref"
    })
}

/// Visit every sub-schema below the root (depth first, document order).
fn walk_subschemas(schema: &mut Value, f: &mut dyn FnMut(&mut Value)) {
    let Value::Object(obj) = schema else { return };
    for (key, value) in obj.iter_mut() {
        let children: Vec<&mut Value> = match value {
            Value::Object(map) if SCHEMA_MAPS.contains(&key.as_str()) => map.values_mut().collect(),
            Value::Array(list) if SCHEMA_LISTS.contains(&key.as_str()) => list.iter_mut().collect(),
            v @ Value::Object(_) if SCHEMA_ONE.contains(&key.as_str()) => vec![v],
            _ => vec![],
        };
        for child in children {
            f(child);
            walk_subschemas(child, f);
        }
    }
}

/// Move scalar sub-schemas that occur at least twice to `$defs` when that
/// makes the schema shorter, replacing each occurrence with a `$ref`.
/// Lossless: every constraint stays, once. Names are `shared_<type>_<n>`
/// in order of first occurrence.
pub fn hoist_repeats(mut schema: Value) -> Value {
    let mut counts: Vec<(String, usize)> = vec![];
    walk_subschemas(&mut schema, &mut |s| {
        if let Value::Object(obj) = s
            && is_scalar(obj)
        {
            let key = serde_json::to_string(obj).unwrap_or_default();
            match counts.iter_mut().find(|(k, _)| *k == key) {
                Some((_, n)) => *n += 1,
                None => counts.push((key, 1)),
            }
        }
    });
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut hoisted: Vec<(String, Value)> = vec![];
    for (key, n) in counts {
        if n < 2 {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&key) else {
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("value");
        let name = format!("shared_{kind}_{}", hoisted.len() + 1);
        let reference = format!(r##"{{"$ref":"#/$defs/{name}"}}"##);
        let entry = name.len() + key.len() + 4;
        if n * key.len() <= n * reference.len() + entry {
            continue;
        }
        names.insert(key, name.clone());
        hoisted.push((name, value));
    }
    if hoisted.is_empty() {
        return schema;
    }
    walk_subschemas(&mut schema, &mut |s| {
        if let Value::Object(obj) = s
            && is_scalar(obj)
            && let Some(name) = names.get(&serde_json::to_string(obj).unwrap_or_default())
        {
            *s = json!({ "$ref": format!("#/$defs/{name}") });
        }
    });
    if let Value::Object(root) = &mut schema {
        let defs = root
            .entry("$defs")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(defs) = defs {
            for (name, value) in hoisted {
                defs.insert(name, value);
            }
        }
    }
    schema
}

/// `schema` without its `$schema` keyword.
pub fn without_dialect(mut schema: Value) -> Value {
    if let Value::Object(o) = &mut schema {
        o.shift_remove("$schema");
    }
    schema
}
