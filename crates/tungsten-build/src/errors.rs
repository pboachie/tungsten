// SPDX-License-Identifier: AGPL-3.0-only
//! Error models (planning/03 "Errors"), read from the normalized
//! documents. Each namespace gets its own model, because each document
//! declares its own error schema (ZROtext: `public.Error` and
//! `sealed.Error` with different code sets, `workflow.WorkflowError` with
//! the code one level down):
//!
//! - envelope: the named schema referenced by the most error responses
//!   with JSON content across the namespace's callable operations; ties go
//!   to the lexicographically smallest type id. A named schema is a
//!   `components/schemas` entry or any other `$ref` target (an error
//!   schema kept in a shared file, which a component may adopt);
//! - code field: at the top of the envelope and in each object property
//!   one level down (`error.code`), the first of `code`, `error_code`,
//!   `type`, `error` that is a string property, with the message field
//!   from `message`, `detail`, `error_description` next to it. A level
//!   whose code admits more than one value wins over one whose code is a
//!   single constant (`type: "error"` is a tag, not an error code), then a
//!   level with a message field wins, then the top level. A property that
//!   is a `oneOf` / `anyOf` of objects counts as a level when every
//!   variant has the same code field (the discriminator property first);
//!   the variants' values are the codes (`error.type` for the Claude API);
//! - codes: the code field's `enum` (or `const`), each with the exact error statuses of
//!   responses carrying the envelope whose description names the code as
//!   a word followed by `:` or whitespace. Sorted by code.
//!
//! The API-wide model ([`merge`]) lists every code of every namespace with
//! the union of its statuses, and keeps the envelope, code field and
//! message field only when all namespaces that have one agree.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use tungsten_ir::{ErrorCode, ErrorModel, StatusMatch, TypeId, TypeRef};
use tungsten_openapi::{RefTarget, is_named_schema, split_pointer};

use crate::ctx::{Ctx, child};
use crate::responses::RawResponse;

const CODE_FIELDS: [&str; 4] = ["code", "error_code", "type", "error"];
const MESSAGE_FIELDS: [&str; 3] = ["message", "detail", "error_description"];

/// Build the error model of `namespace` from the responses of its
/// callable operations.
pub(crate) fn build(cx: &mut Ctx<'_>, namespace: &str, ops: &[&[RawResponse]]) -> ErrorModel {
    let Some(envelope) = envelope(cx, namespace, ops) else {
        return ErrorModel::default();
    };
    let id = type_id(cx, namespace, &envelope);
    let Some((code_field, message_field, values)) = code_field(cx, &envelope) else {
        return ErrorModel {
            envelope: id,
            ..ErrorModel::default()
        };
    };
    let mut codes: Vec<ErrorCode> = values
        .into_iter()
        .map(|code| ErrorCode {
            statuses: statuses_for(cx, ops, &envelope, &code),
            code,
        })
        .collect();
    codes.sort_by(|a, b| a.code.cmp(&b.code));
    codes.dedup_by(|a, b| a.code == b.code);
    ErrorModel {
        envelope: id,
        code_field: Some(code_field),
        message_field,
        codes,
    }
}

/// The API-wide model from the namespaces' models.
pub(crate) fn merge<'m>(models: impl IntoIterator<Item = &'m ErrorModel>) -> ErrorModel {
    let models: Vec<&ErrorModel> = models.into_iter().collect();
    let mut codes: BTreeMap<&str, Vec<u16>> = BTreeMap::new();
    for m in &models {
        for c in &m.codes {
            codes
                .entry(c.code.as_str())
                .or_default()
                .extend(&c.statuses);
        }
    }
    ErrorModel {
        envelope: agreed(models.iter().map(|m| m.envelope.as_ref())),
        code_field: agreed(models.iter().map(|m| m.code_field.as_ref())),
        message_field: agreed(models.iter().map(|m| m.message_field.as_ref())),
        codes: codes
            .into_iter()
            .map(|(code, mut statuses)| {
                statuses.sort_unstable();
                statuses.dedup();
                ErrorCode {
                    code: code.to_string(),
                    statuses,
                }
            })
            .collect(),
    }
}

/// The value every present entry has, when there is one and they agree.
fn agreed<'v, T: PartialEq + Clone + 'v>(values: impl Iterator<Item = Option<&'v T>>) -> Option<T> {
    let mut present = values.flatten();
    let first = present.next()?;
    present.all(|v| v == first).then(|| first.clone())
}

/// The most referenced named schema among JSON error responses.
fn envelope(cx: &Ctx<'_>, namespace: &str, ops: &[&[RawResponse]]) -> Option<RefTarget> {
    let mut counts: BTreeMap<RefTarget, usize> = BTreeMap::new();
    for responses in ops {
        for raw in responses.iter().filter(|r| r.is_error()) {
            let Some(target) = raw.json_schema.as_ref().and_then(|s| cx.deref(s)) else {
                continue;
            };
            if is_named_schema(&target.pointer) || cx.ws.graph.nodes.contains(&target) {
                *counts.entry(target).or_insert(0) += 1;
            }
        }
    }
    let best = counts.values().copied().max()?;
    counts
        .into_iter()
        .filter(|(_, n)| *n == best)
        .map(|(target, _)| (tie_key(cx, namespace, &target), target))
        .min()
        .map(|(_, target)| target)
}

/// The type id of a component schema, or `namespace.Name` when the type
/// builder has not named it.
fn tie_key(cx: &Ctx<'_>, namespace: &str, target: &RefTarget) -> String {
    cx.tb.type_id_for(target).map_or_else(
        || {
            let name = split_pointer(&target.pointer).pop().unwrap_or_default();
            format!("{namespace}.{name}")
        },
        |id| id.0,
    )
}

fn type_id(cx: &mut Ctx<'_>, namespace: &str, target: &RefTarget) -> Option<TypeId> {
    match cx.tb.type_ref(namespace, target, &[]) {
        TypeRef::Named(id) => Some(id),
        TypeRef::Inline(_) => cx.tb.type_id_for(target),
    }
}

/// (code field path, message field path, enum values of the code field).
fn code_field(cx: &Ctx<'_>, envelope: &RefTarget) -> Option<(String, Option<String>, Vec<String>)> {
    let (target, schema) = cx.deref_value(envelope)?;
    let props = schema.get("properties")?.as_object()?;
    let mut levels = vec![fields_in(cx, &target, props, "", None)];
    for outer in props.keys() {
        let Some((inner_target, inner)) = cx.deref_value(&property(&target, outer)) else {
            continue;
        };
        let prefix = format!("{outer}.");
        if let Some(inner_props) = inner.get("properties").and_then(Value::as_object) {
            levels.push(fields_in(cx, &inner_target, inner_props, &prefix, None));
        } else {
            levels.push(union_fields(cx, &inner_target, inner, &prefix));
        }
    }
    // The first level with the best (not a single constant, has a message).
    let score = |(_, message, values): &(String, Option<String>, Vec<String>)| {
        (values.len() != 1, message.is_some())
    };
    let mut best: Option<(String, Option<String>, Vec<String>)> = None;
    for found in levels.into_iter().flatten() {
        if best.as_ref().is_none_or(|b| score(&found) > score(b)) {
            best = Some(found);
        }
    }
    best
}

fn fields_in(
    cx: &Ctx<'_>,
    target: &RefTarget,
    props: &Map<String, Value>,
    prefix: &str,
    preferred: Option<&str>,
) -> Option<(String, Option<String>, Vec<String>)> {
    let string_prop = |name: &&&str| {
        props.contains_key(**name)
            && cx
                .deref_value(&property(target, name))
                .is_some_and(|(_, s)| is_stringish(s))
    };
    let code = preferred
        .iter()
        .find(string_prop)
        .or_else(|| CODE_FIELDS.iter().find(string_prop))?;
    let values = cx
        .deref_value(&property(target, code))
        .and_then(|(_, s)| {
            s.get("enum")
                .and_then(Value::as_array)
                .cloned()
                .or_else(|| s.get("const").map(|c| vec![c.clone()]))
        })
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let message = MESSAGE_FIELDS
        .iter()
        .find(string_prop)
        .map(|m| format!("{prefix}{m}"));
    Some((format!("{prefix}{code}"), message, values))
}

/// The code field of a union of objects (`oneOf` / `anyOf`), the shape of
/// an error kept as a tagged union: the discriminator property (else the
/// usual code names) must be a string in every variant; the codes are the
/// union of the variants' values (any string when one variant admits any),
/// the message field the one every variant names.
fn union_fields(
    cx: &Ctx<'_>,
    target: &RefTarget,
    schema: &Value,
    prefix: &str,
) -> Option<(String, Option<String>, Vec<String>)> {
    let key = ["oneOf", "anyOf"]
        .into_iter()
        .find(|k| schema.get(*k).is_some_and(Value::is_array))?;
    let members = schema.get(key)?.as_array()?;
    let discriminator = schema
        .get("discriminator")
        .and_then(|d| d.get("propertyName"))
        .and_then(Value::as_str);
    let mut path: Option<String> = None;
    let mut message: Option<Option<String>> = None;
    let mut values: Vec<String> = Vec::new();
    let mut any_string = false;
    for index in 0..members.len() {
        let member = child(&child(target, key), &index.to_string());
        let (variant, vschema) = cx.deref_value(&member)?;
        let vprops = vschema.get("properties")?.as_object()?;
        let (found, found_message, found_values) =
            fields_in(cx, &variant, vprops, prefix, discriminator)?;
        if path.get_or_insert_with(|| found.clone()) != &found {
            return None;
        }
        // The message field counts only when every variant names it.
        message = Some(match message.take() {
            Some(m) if m != found_message => None,
            _ => found_message,
        });
        any_string |= found_values.is_empty();
        values.extend(found_values);
    }
    if any_string {
        values.clear();
    }
    values.sort();
    values.dedup();
    Some((path?, message.flatten(), values))
}

fn property(object: &RefTarget, name: &str) -> RefTarget {
    child(&child(object, "properties"), name)
}

/// A string schema: `type` includes `string`, or every `enum` value or the
/// `const` is a string.
fn is_stringish(schema: &Value) -> bool {
    let typed = match schema.get("type") {
        Some(Value::String(t)) => t == "string",
        Some(Value::Array(ts)) => ts.iter().any(|t| t == "string"),
        _ => false,
    };
    let enumerated = schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|vs| !vs.is_empty() && vs.iter().all(Value::is_string));
    typed || enumerated || schema.get("const").is_some_and(Value::is_string)
}

/// Exact error statuses of responses carrying the envelope whose
/// description names `code`.
fn statuses_for(
    cx: &Ctx<'_>,
    ops: &[&[RawResponse]],
    envelope: &RefTarget,
    code: &str,
) -> Vec<u16> {
    let mut out: Vec<u16> = ops
        .iter()
        .flat_map(|responses| responses.iter())
        .filter(|r| r.is_error())
        .filter(|r| {
            r.json_schema
                .as_ref()
                .and_then(|s| cx.deref(s))
                .is_some_and(|t| t == *envelope)
        })
        .filter_map(|r| match r.status {
            StatusMatch::Exact(s) if mentions(&r.description, code) => Some(s),
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether `text` contains `code` as a word (not preceded by a word
/// character) immediately followed by `:` or whitespace.
pub(crate) fn mentions(text: &str, code: &str) -> bool {
    if code.is_empty() {
        return false;
    }
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(code).any(|(i, _)| {
        let before_ok = text[..i].chars().next_back().is_none_or(|c| !word(c));
        let after = text[i + code.len()..].chars().next();
        before_ok && after.is_some_and(|c| c == ':' || c.is_whitespace())
    })
}
