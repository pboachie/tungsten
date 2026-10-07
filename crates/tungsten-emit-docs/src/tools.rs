// SPDX-License-Identifier: AGPL-3.0-only
//! `tools.json`: a provider-neutral function-calling manifest. One tool per
//! callable operation (planned and hidden operations are never tools) and
//! per macro: `name` (`<namespace>_<resources>_<method>`, snake_case, at
//! most 64 characters, unique), `description` (the compacted doc plus the
//! safety tier and idempotency rule) and `parameters`, the JSON Schema of
//! the arguments object exactly as the SDK takes it (`tungsten_emit::args`).
//! Tool-specific facts the schema cannot carry are under `x-tungsten`.
//!
//! Parameters are compacted without losing constraints: a scalar
//! sub-schema repeated within one tool (the same UUID pattern on eight
//! fields) moves to `$defs` once when that makes the tool smaller. A tool
//! still over the schema budget (agent.yml
//! `defaults.disclosure.schema_budget_tokens`, measured as characters / 4)
//! is TG0713.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::args::args_layout;
use tungsten_emit::header_text;
use tungsten_emit::schema::{SchemaBuilder, SchemaOptions, Usage, collapse_whitespace};
use tungsten_ir::{IdempotencyKind, Ir, Operation, OperationStatus, Safety};

use crate::model::{MacroDocs, Model, idempotency_kind, macro_summary, method, safety, summary};

/// How tool parameter schemas are rendered: request side, descriptions cut
/// to two sentences, named types inlined four levels deep.
fn schema_options() -> SchemaOptions {
    SchemaOptions {
        inline_depth: 4,
        descriptions: true,
        max_sentences: Some(2),
        usage: Usage::Request,
    }
}

/// TG0713 for each tool whose `{name, description, parameters}` is over the
/// schema budget after compaction.
pub(crate) fn budget_warnings(model: &Model<'_>) -> Diagnostics {
    let budget = model.ir.agent.disclosure.schema_budget_tokens as usize;
    let mut d = Diagnostics::new();
    for tool in tools(model) {
        let sent = json!({ "name": tool["name"], "description": tool["description"], "parameters": tool["parameters"] });
        let tokens = serde_json::to_string(&sent)
            .unwrap_or_default()
            .chars()
            .count()
            .div_ceil(4);
        if tokens > budget {
            d.push(
                Diagnostic::warning(
                    "TG0713",
                    format!(
                        "tool `{}` is about {tokens} tokens, over the schema budget of {budget}",
                        tool["name"].as_str().unwrap_or_default()
                    ),
                )
                .with_help("shorten descriptions (disclosure.prune for operations, the spec for fields), split the operation, or raise defaults.disclosure.schema_budget_tokens in agent.yml"),
            );
        }
    }
    d
}

/// Every tool: callable operations that are not hidden, then macros.
fn tools(model: &Model<'_>) -> Vec<Value> {
    let ir = model.ir;
    let mut tools = vec![];
    for (_, c) in model.callable().filter(|(_, c)| !c.op.agent.hidden) {
        tools.push(json!({
            "name": c.tool,
            "description": description(c.op),
            "parameters": parameters(ir, c.op),
            "x-tungsten": operation_meta(c.op),
        }));
    }
    for m in &model.macros {
        tools.push(macro_tool(model, m));
    }
    tools
}

pub(crate) fn tools_json(model: &Model<'_>) -> String {
    let ir = model.ir;
    let tools = tools(model);
    let doc = json!({
        "$comment": header_text(ir).replace('\n', " "),
        "api": ir.api.name.wire,
        "version": ir.api.version,
        "tools": tools,
    });
    let mut text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    text.push('\n');
    text
}

/// The arguments object schema of `op`, self-contained (`$defs` inside),
/// with repeated scalar sub-schemas hoisted.
fn parameters(ir: &Ir, op: &Operation) -> Value {
    let mut b = SchemaBuilder::new(ir, schema_options());
    let layout = args_layout(ir, op);
    let root = b.args(&layout);
    hoist_repeats(without_dialect(b.finish(root)))
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
fn hoist_repeats(mut schema: Value) -> Value {
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

/// Tool parameters carry no `$schema`: several function-calling APIs
/// reject keywords they do not list.
fn without_dialect(mut schema: Value) -> Value {
    if let Value::Object(o) = &mut schema {
        o.shift_remove("$schema");
    }
    schema
}

/// The compacted doc (or summary) followed by the rules an agent must know
/// before calling: tier, confirmation, idempotency, gate, one-time secrets.
pub(crate) fn description(op: &Operation) -> String {
    let a = &op.agent;
    let base = if a.compact_doc.trim().is_empty() {
        summary(op)
    } else {
        collapse_whitespace(&a.compact_doc)
    };
    let mut notes = vec![safety(a.safety).to_string()];
    match a.safety {
        Safety::Destructive => notes.push("needs confirmation".into()),
        Safety::Irreversible => notes.push("needs a confirmation token from a preview".into()),
        Safety::ReadOnly | Safety::Mutating => {}
    }
    let p = &a.idempotency;
    match p.policy {
        IdempotencyKind::CallerOwned => {
            let header = p.header.as_deref().unwrap_or("idempotency key");
            let required = if p.persist_required { " required" } else { "" };
            notes.push(format!(
                "caller-owned {header}{required}: persist it and reuse it on every retry"
            ));
        }
        IdempotencyKind::Auto => notes.push("idempotency key generated per call".into()),
        IdempotencyKind::ContentHash => notes.push("idempotent by body content".into()),
        IdempotencyKind::ContentIdentity => {
            notes.push("retry only by resending identical bytes".into())
        }
        IdempotencyKind::None if a.safety != Safety::ReadOnly => {
            notes.push("not idempotent: never retry blindly".into())
        }
        IdempotencyKind::None => {}
    }
    if let OperationStatus::Gated { gate } = &op.status {
        notes.push(format!(
            "gated by {} ({} by default)",
            gate.env_var,
            if gate.default_on { "on" } else { "off" }
        ));
    }
    if a.shown_once {
        notes.push("returns a secret once".into());
    }
    if op.deprecated {
        notes.push("deprecated".into());
    }
    format!("{base} [{}]", notes.join("; "))
}

fn operation_meta(op: &Operation) -> Value {
    let mut meta = Map::new();
    meta.insert("operation".into(), json!(op.id.0));
    meta.insert("method".into(), json!(method(op.method)));
    meta.insert("path".into(), json!(op.path.raw));
    if let Some(rpc) = &op.rpc {
        meta.insert(
            "rpc".into(),
            json!({ "field": rpc.discriminator_field, "value": rpc.discriminator_value }),
        );
    }
    meta.insert("safety".into(), json!(safety(op.agent.safety)));
    meta.insert(
        "idempotency".into(),
        json!(idempotency_kind(op.agent.idempotency.policy)),
    );
    if let OperationStatus::Gated { gate } = &op.status {
        meta.insert("gate".into(), json!(gate.env_var));
    }
    if let Some(cluster) = &op.agent.cluster {
        meta.insert("cluster".into(), json!(cluster));
    }
    Value::Object(meta)
}

/// The rules of a macro an agent must know before calling it: the tier,
/// macro-level confirmation, caller-owned keys its steps need, one-time
/// secrets in its output.
fn macro_description(model: &Model<'_>, m: &MacroDocs<'_>) -> String {
    let mac = m.mac;
    let mut notes = vec![safety(mac.safety).to_string(), "macro".to_string()];
    match mac.safety {
        Safety::Destructive => notes.push("needs confirmation for the whole run".into()),
        Safety::Irreversible => {
            notes.push("needs a confirmation token from a preview of the macro".into())
        }
        Safety::ReadOnly | Safety::Mutating => {}
    }
    let steps = mac.steps.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut keyed: Vec<&str> = vec![];
    for step in steps {
        let Some(op) = step
            .get("operation")
            .and_then(Value::as_str)
            .and_then(|id| model.find(id))
            .map(|(_, c)| c.op)
        else {
            continue;
        };
        if op.agent.idempotency.policy == IdempotencyKind::CallerOwned
            && !keyed.contains(&op.id.0.as_str())
        {
            keyed.push(&op.id.0);
        }
    }
    if !keyed.is_empty() {
        notes.push(format!(
            "caller-owned idempotency key for {}: persist it and reuse it on every retry",
            keyed.join(", ")
        ));
    }
    if mac.shown_once {
        notes.push("returns a secret once".into());
    }
    format!("{} [{}]", macro_summary(mac), notes.join("; "))
}

/// A macro tool: the arguments of the operation it extends plus the
/// inputs it adds (canonical macro input `{extends, add}`).
fn macro_tool(model: &Model<'_>, m: &MacroDocs<'_>) -> Value {
    let ir = model.ir;
    let input = &m.mac.input;
    let base = input
        .get("extends")
        .and_then(Value::as_str)
        .and_then(|id| model.find(id))
        .map(|(_, c)| c.op);
    let mut params = match base {
        Some(op) => parameters(ir, op),
        None => json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    };
    if let Some(Value::Object(add)) = input.get("add") {
        if let Some(Value::Object(props)) = params.get_mut("properties") {
            for (name, schema) in add {
                props.insert(name.clone(), schema.clone());
            }
        }
        // An added input without a default is required, as in the SDK's
        // macro input type (the runtime applies defaults only).
        let needed: Vec<&String> = add
            .iter()
            .filter(|(_, schema)| schema.get("default").is_none())
            .map(|(name, _)| name)
            .collect();
        if !needed.is_empty()
            && let Value::Object(root) = &mut params
        {
            let required = root
                .entry("required")
                .or_insert_with(|| Value::Array(vec![]));
            if let Value::Array(list) = required {
                for name in needed {
                    if !list.iter().any(|v| v.as_str() == Some(name.as_str())) {
                        list.push(json!(name));
                    }
                }
            }
        }
    }
    json!({
        "name": m.tool,
        "description": macro_description(model, m),
        "parameters": params,
        "x-tungsten": macro_meta(m),
    })
}

fn macro_meta(m: &MacroDocs<'_>) -> Value {
    let mut meta = Map::new();
    meta.insert("macro".into(), json!(m.mac.name.0));
    meta.insert("safety".into(), json!(safety(m.mac.safety)));
    if let Some(cluster) = &m.mac.cluster {
        meta.insert("cluster".into(), json!(cluster));
    }
    if !m.mac.sensitive_response_fields.is_empty() {
        meta.insert(
            "sensitive_response_fields".into(),
            json!(m.mac.sensitive_response_fields),
        );
    }
    if m.mac.shown_once {
        meta.insert("shown_once".into(), json!(true));
    }
    Value::Object(meta)
}
