// SPDX-License-Identifier: AGPL-3.0-only
//! `tools.json`: a provider-neutral function-calling manifest. One tool per
//! callable operation (planned and hidden operations are never tools) and
//! per macro: `name` (`<namespace>_<resources>_<method>`, snake_case, at
//! most 64 characters, unique), `description` (the compacted doc plus the
//! safety tier and idempotency rule) and `parameters`, the JSON Schema of
//! the arguments object exactly as the SDK takes it (`tungsten_emit::args`).
//! Tool-specific facts the schema cannot carry are under `x-tungsten`.

use serde_json::{Map, Value, json};
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

pub(crate) fn tools_json(model: &Model<'_>) -> String {
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

/// The arguments object schema of `op`, self-contained (`$defs` inside).
fn parameters(ir: &Ir, op: &Operation) -> Value {
    let mut b = SchemaBuilder::new(ir, schema_options());
    let layout = args_layout(ir, op);
    let root = b.args(&layout);
    without_dialect(b.finish(root))
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
    Value::Object(meta)
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
    if let (Some(Value::Object(add)), Some(Value::Object(props))) =
        (input.get("add"), params.get_mut("properties"))
    {
        for (name, schema) in add {
            props.insert(name.clone(), schema.clone());
        }
    }
    json!({
        "name": m.tool,
        "description": format!("{} [{}; macro]", macro_summary(m.mac), safety(m.mac.safety)),
        "parameters": params,
        "x-tungsten": { "macro": m.mac.name.0, "safety": safety(m.mac.safety) },
    })
}
