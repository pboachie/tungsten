// SPDX-License-Identifier: AGPL-3.0-only
//! RPC unflattening (FR-I7): one HTTP operation whose JSON request body is
//! a `oneOf` of objects told apart by a `const` method field becomes one
//! operation per method. Reads the normalized documents directly; the
//! types of the parameters come from the type builder later.
//!
//! A member may require other envelope members only when their schema is
//! a constant (`jsonrpc: {const: "2.0"}`); the binding carries them so the
//! runtime sends them verbatim. A required member that is not a constant
//! (JSON-RPC `id`) has no place in an unflattened method, so the envelope
//! is not unflattened (TG0506) rather than silently dropping it.

use std::collections::BTreeMap;

use serde_json::Value;
use tungsten_config::RpcUnflatten;
use tungsten_core::Diagnostic;
use tungsten_ir::Doc;
use tungsten_openapi::RefTarget;

use crate::bodies;
use crate::ctx::{Ctx, child, doc, pointer, str_of};

/// One method of an unflattened envelope.
#[derive(Debug, Clone)]
pub(crate) struct RpcVariant {
    /// Discriminator value sent on the wire (`workflow.action.send`).
    pub value: String,
    /// The value without `name_strip_prefix` (`action.send`).
    pub local_id: String,
    /// The variant schema (after `$ref`s).
    pub variant: RefTarget,
    /// The variant's parameters subschema, when it declares one.
    pub params: Option<RefTarget>,
    /// Whether the variant requires the parameters member.
    pub params_required: bool,
    /// Other required members with a constant value.
    pub constants: BTreeMap<String, Value>,
    /// Media type of the envelope's JSON body.
    pub media_type: String,
    pub doc: Option<Doc>,
}

/// The variants of the envelope operation `op` described by `rpc` (the
/// manifest entry of input `input_index`). Every problem is a TG0506
/// error and yields `None`.
pub(crate) fn variants(
    cx: &mut Ctx<'_>,
    rpc: &RpcUnflatten,
    input_index: usize,
    op: &RefTarget,
) -> Option<Vec<RpcVariant>> {
    let manifest_at = pointer(["inputs", &input_index.to_string(), "rpc_unflatten"]);
    match collect(cx, rpc, op) {
        Ok(variants) => Some(variants),
        Err((message, at)) => {
            let d = Diagnostic::error(
                "TG0506",
                format!(
                    "rpc_unflatten {} {}: {message}",
                    rpc.method.to_ascii_uppercase(),
                    rpc.path
                ),
            );
            let span = cx.ws.span(&at);
            let d = d.at(cx.ws.name(at.doc), &at.pointer, span);
            cx.report_manifest(d, &manifest_at);
            None
        }
    }
}

/// Report an `rpc_unflatten` entry whose path and method name no
/// operation (TG0506).
pub(crate) fn report_missing(cx: &mut Ctx<'_>, rpc: &RpcUnflatten, input_index: usize) {
    let at = pointer(["inputs", &input_index.to_string(), "rpc_unflatten", "path"]);
    cx.report_manifest(
        Diagnostic::error(
            "TG0506",
            format!(
                "rpc_unflatten names {} {}, which is not a callable operation of the input",
                rpc.method.to_ascii_uppercase(),
                rpc.path
            ),
        ),
        &at,
    );
}

type Failure = (String, RefTarget);

fn collect(cx: &Ctx<'_>, rpc: &RpcUnflatten, op: &RefTarget) -> Result<Vec<RpcVariant>, Failure> {
    let body_at = child(op, "requestBody");
    let fail = |message: String, at: &RefTarget| -> Failure { (message, at.clone()) };
    let (body, _) = cx
        .deref_value(&body_at)
        .ok_or_else(|| fail("the operation has no request body".into(), op))?;
    let (media_type, schema_at) = bodies::json_schema(cx, &body)
        .ok_or_else(|| fail("the request body has no JSON schema".into(), &body))?;
    let (schema, value) = cx.deref_value(&schema_at).ok_or_else(|| {
        fail(
            "the request body schema does not resolve".into(),
            &schema_at,
        )
    })?;
    let Some(Value::Array(members)) = value.get("oneOf") else {
        return Err(fail(
            "the request body schema is not a oneOf".into(),
            &schema,
        ));
    };
    let one_of = child(&schema, "oneOf");
    let prefix = rpc.name_strip_prefix.as_deref().unwrap_or("");
    let mut out: Vec<RpcVariant> = vec![];
    for i in 0..members.len() {
        let member_at = child(&one_of, &i.to_string());
        let (variant, v) = cx
            .deref_value(&member_at)
            .ok_or_else(|| fail(format!("oneOf member {i} does not resolve"), &member_at))?;
        let tag_at = child(&child(&variant, "properties"), &rpc.discriminator);
        let tag = cx
            .deref_value(&tag_at)
            .and_then(|(_, t)| const_string(t))
            .ok_or_else(|| {
                fail(
                    format!(
                        "oneOf member {i} has no string const property `{}`",
                        rpc.discriminator
                    ),
                    &variant,
                )
            })?;
        if out.iter().any(|o| o.value == tag) {
            return Err(fail(
                format!("discriminator value `{tag}` appears in more than one oneOf member"),
                &variant,
            ));
        }
        let local_id = tag.strip_prefix(prefix).unwrap_or(&tag).to_string();
        if local_id.is_empty() || local_id.split('.').any(str::is_empty) {
            return Err(fail(
                format!("discriminator value `{tag}` does not leave a dotted method name"),
                &variant,
            ));
        }
        let params_at = child(&child(&variant, "properties"), &rpc.params);
        let params = cx.get(&params_at).map(|_| params_at.clone());
        let required: Vec<&str> = v
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let params_required = required.contains(&rpc.params.as_str());
        let mut constants = BTreeMap::new();
        for name in required {
            if name == rpc.discriminator || name == rpc.params {
                continue;
            }
            let prop_at = child(&child(&variant, "properties"), name);
            let value = cx
                .deref_value(&prop_at)
                .and_then(|(_, p)| constant(p))
                .ok_or_else(|| {
                    fail(
                        format!(
                            "oneOf member {i} requires `{name}`, which is neither `{}`, `{}` nor a constant, so an unflattened method could not send it",
                            rpc.discriminator, rpc.params
                        ),
                        &variant,
                    )
                })?;
            constants.insert(name.to_string(), value);
        }
        let params_doc = params
            .as_ref()
            .and_then(|p| cx.deref_value(p))
            .and_then(|(_, p)| str_of(p, "description"));
        out.push(RpcVariant {
            value: tag.clone(),
            local_id,
            variant,
            params,
            params_required,
            constants,
            media_type: media_type.clone(),
            doc: doc(None, str_of(v, "description").or(params_doc)),
        });
    }
    if out.is_empty() {
        return Err(fail("the oneOf has no members".into(), &schema));
    }
    Ok(out)
}

/// A `const` of any JSON value, or the single value of a one-element `enum`.
fn constant(schema: &Value) -> Option<Value> {
    if let Some(c) = schema.get("const") {
        return Some(c.clone());
    }
    match schema.get("enum").and_then(Value::as_array) {
        Some(values) if values.len() == 1 => Some(values[0].clone()),
        _ => None,
    }
}

/// A string `const`, or the single value of a one-element string `enum`.
fn const_string(schema: &Value) -> Option<String> {
    if let Some(c) = schema.get("const") {
        return c.as_str().map(str::to_string);
    }
    match schema.get("enum").and_then(Value::as_array) {
        Some(values) if values.len() == 1 => values[0].as_str().map(str::to_string),
        _ => None,
    }
}
