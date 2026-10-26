// SPDX-License-Identifier: AGPL-3.0-only
//! Incoming webhooks (OpenAPI 3.1+ `webhooks`).
//!
//! Each operation of a `webhooks` entry is a request the API sends to the
//! application. Only its event identity (the entry key and method) and its
//! JSON request body are compiled: the body becomes a typed payload, and
//! targets emit receiving helpers for it. Parameters, responses and agent
//! metadata describe the receiver's side and are not compiled.

use serde_json::Value;
use tungsten_ir::{SourceRef, Webhook, WebhookPayload};
use tungsten_openapi::RefTarget;

use crate::bodies;
use crate::ctx::{Ctx, child, doc_of, flag, str_of};
use crate::names::pascal;
use crate::operations::METHODS;
use crate::pathitem::PathItem;

/// The webhooks of the document `doc`, in document order.
pub(crate) fn build(
    cx: &mut Ctx<'_>,
    namespace: &str,
    doc: tungsten_openapi::DocId,
) -> Vec<Webhook> {
    let at = RefTarget {
        doc,
        pointer: "/webhooks".into(),
    };
    let Some(Value::Object(entries)) = cx.get(&at) else {
        return vec![];
    };
    let keys: Vec<String> = entries.keys().cloned().collect();
    let mut out = vec![];
    for key in keys {
        if cx.ws.is_cancelled() {
            break;
        }
        let Some(entry) = PathItem::resolve(cx, &child(&at, &key)) else {
            continue;
        };
        if !entry.item.is_object() {
            continue;
        }
        let ops: Vec<_> = METHODS
            .iter()
            .filter_map(|(word, method)| {
                let (target, op) = entry.operation(word)?;
                op.is_object().then(|| (*word, *method, target, op.clone()))
            })
            .collect();
        let several = ops.len() > 1;
        for (word, method, target, op) in ops {
            let mut name = tungsten_ir::Ident::new(key.clone());
            if several {
                name.words.push(word.to_string());
            }
            let hint = pascal(&name.words.join(" "));
            let payload = payload(cx, namespace, &target, &hint);
            out.push(Webhook {
                name,
                method,
                operation_id: str_of(&op, "operationId")
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string),
                doc: doc_of(&op),
                payload,
                source: SourceRef {
                    file: cx.ws.name(target.doc).to_string(),
                    pointer: target.pointer.clone(),
                },
            });
        }
    }
    out
}

/// The JSON request body of the webhook operation at `op`, typed.
fn payload(
    cx: &mut Ctx<'_>,
    namespace: &str,
    op: &RefTarget,
    hint: &str,
) -> Option<WebhookPayload> {
    let at = child(op, "requestBody");
    cx.get(&at)?;
    let (target, value) = cx.deref_value(&at)?;
    let required = flag(value, "required");
    let (media_type, schema) = bodies::json_schema(cx, &target)?;
    let ty = cx.tb.type_ref(namespace, &schema, &[hint, "Webhook"]);
    Some(WebhookPayload {
        media_type,
        ty,
        required,
    })
}
