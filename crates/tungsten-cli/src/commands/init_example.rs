// SPDX-License-Identifier: AGPL-3.0-only
//! Canonical SDK argument examples from the ordinary emitter layout.
use serde_json::{Map, Value};
use tungsten_emit::args::{BodyArg, args_layout};
use tungsten_ir::{BodyEncoding, Ir, Operation};

pub(super) fn arguments(ir: &Ir, operation: &Operation) -> Result<Value, String> {
    let examples = tungsten_mock::RequestExamples::new(ir.clone());
    let layout = args_layout(ir, operation);
    let mut args = Map::new();
    for param in layout.params {
        if param.param.required {
            args.insert(
                param.key.clone(),
                examples.value(&param.param.ty, &operation.id.0, &param.key)?,
            );
        }
    }
    if layout.body_required
        && let Some(body) = layout.body
    {
        let content = match &body {
            BodyArg::Merged { content, .. } | BodyArg::Arg { content, .. } => content,
        };
        if content.encoding != BodyEncoding::Json {
            return Err("automatic local examples currently require JSON request bodies".into());
        }
        let value = examples.value(&content.ty, &operation.id.0, "body")?;
        match body {
            BodyArg::Arg { key, .. } => {
                args.insert(key, value);
            }
            BodyArg::Merged { fields, .. } => {
                let object = value
                    .as_object()
                    .ok_or("local JSON body example must be an object")?;
                for field in fields {
                    if let Some(value) = object.get(&field.wire_name) {
                        args.insert(field.wire_name.clone(), value.clone());
                    }
                }
            }
        }
    }
    Ok(Value::Object(args))
}
