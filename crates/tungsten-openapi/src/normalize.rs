// SPDX-License-Identifier: AGPL-3.0-only
//! OpenAPI 3.0 → 3.1 schema normalization.
//!
//! Applied to every schema location of a 3.0 document (and to the parts of
//! external files a 3.0 document references):
//!
//! - `nullable: true` with `type: T` becomes `type: [T, "null"]`; an `enum`
//!   next to it gets `null` appended so the enum still admits null.
//! - `nullable: true` without `type` (`$ref`, `allOf`, enum-only, ...)
//!   becomes `anyOf: [<schema without nullable>, {type: "null"}]`, keeping
//!   annotations (`title`, `description`, `default`, ...) on the wrapper.
//! - `nullable: false` (or any non-`true` value) is dropped.
//! - Boolean `exclusiveMinimum`/`exclusiveMaximum` take the numeric form.
//! - `example: X` becomes `examples: [X]`.
//!
//! Schemas are rewritten bottom-up so a wrapper never hides an unprocessed
//! child. Wrapping moves members to `<schema>/anyOf/0`; each such move is
//! recorded so that `$ref` pointers written against the original layout keep
//! resolving (see [`translate`]).

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::spans::SpanIndex;
use crate::walk::{Kind, walk};
use crate::{join_pointer, unescape_token};

/// Members that stay on the wrapper when a nullable schema is wrapped in
/// `anyOf`: they annotate the whole (nullable) value.
const ANNOTATIONS: &[&str] = &[
    "title",
    "description",
    "default",
    "deprecated",
    "readOnly",
    "writeOnly",
    "examples",
    "externalDocs",
];

/// Members of the schema at `from` moved under `to` by a nullable wrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Move {
    pub from: String,
    pub to: String,
    pub keys: BTreeSet<String>,
}

/// Map a pointer written against the pre-normalization layout to the
/// current one by replaying `moves` in order.
pub(crate) fn translate(moves: &[Move], pointer: &str) -> String {
    let mut p = pointer.to_string();
    for m in moves {
        let Some(rest) = p.strip_prefix(m.from.as_str()) else {
            continue;
        };
        let Some(tail) = rest.strip_prefix('/') else {
            continue;
        };
        let first = tail.split('/').next().unwrap_or_default();
        if m.keys.contains(&unescape_token(first)) {
            p = format!("{}{rest}", m.to);
        }
    }
    p
}

/// Normalize every schema in the subtree at `pointer`, which is an object
/// of `kind`. Records spans for rewritten nodes and moves for wraps.
pub(crate) fn normalize(
    root: &mut Value,
    spans: &mut SpanIndex,
    moves: &mut Vec<Move>,
    pointer: &str,
    kind: Kind,
) {
    let Some(start) = root.pointer(pointer) else {
        return;
    };
    let mut schemas = vec![];
    walk(start, pointer, kind, |node| {
        if node.kind == Kind::Schema {
            schemas.push(node.pointer.clone());
        }
        true
    });
    // Reverse pre-order: every schema is rewritten after all schemas below it.
    for p in schemas.into_iter().rev() {
        let Some(Value::Object(map)) = root.pointer_mut(&p) else {
            continue;
        };
        exclusive_bound(map, spans, &p, "exclusiveMinimum", "minimum");
        exclusive_bound(map, spans, &p, "exclusiveMaximum", "maximum");
        example_to_examples(map, spans, &p);
        if let Some(m) = nullable(map, spans, &p) {
            moves.push(m);
        }
    }
}

fn exclusive_bound(
    map: &mut Map<String, Value>,
    spans: &mut SpanIndex,
    p: &str,
    key: &str,
    bound: &str,
) {
    let Some(Value::Bool(flag)) = map.get(key) else {
        return;
    };
    let key_ptr = join_pointer(p, key);
    match map.get(bound) {
        Some(n @ Value::Number(_)) if *flag => {
            let n = n.clone();
            map.insert(key.to_string(), n);
            map.shift_remove(bound);
            spans.move_subtree(&join_pointer(p, bound), &key_ptr);
        }
        _ => {
            map.shift_remove(key);
            spans.take_subtree(&key_ptr);
        }
    }
}

fn example_to_examples(map: &mut Map<String, Value>, spans: &mut SpanIndex, p: &str) {
    if !map.contains_key("example") {
        return;
    }
    let example_ptr = join_pointer(p, "example");
    let examples_ptr = join_pointer(p, "examples");
    match map.get("examples") {
        Some(Value::Array(_)) => {
            let example = map.shift_remove("example").unwrap_or_default();
            if let Some(Value::Array(list)) = map.get_mut("examples") {
                if list.contains(&example) {
                    spans.take_subtree(&example_ptr);
                } else {
                    spans.move_subtree(&example_ptr, &format!("{examples_ptr}/{}", list.len()));
                    list.push(example);
                }
            }
        }
        // A non-array `examples` is invalid; leave both members untouched.
        Some(_) => {}
        None => {
            let entries = std::mem::take(map);
            for (k, v) in entries {
                if k == "example" {
                    map.insert("examples".into(), Value::Array(vec![v]));
                } else {
                    map.insert(k, v);
                }
            }
            let span = spans.get(&example_ptr);
            spans.move_subtree(&example_ptr, &format!("{examples_ptr}/0"));
            if let Some(span) = span {
                spans.insert(examples_ptr, span);
            }
        }
    }
}

fn nullable(map: &mut Map<String, Value>, spans: &mut SpanIndex, p: &str) -> Option<Move> {
    let flag = map.shift_remove("nullable")?;
    let null_span = spans
        .take_subtree(&join_pointer(p, "nullable"))
        .into_iter()
        .next()
        .map(|(_, s)| s);
    if flag != Value::Bool(true) {
        return None;
    }
    let type_ptr = join_pointer(p, "type");
    match map.get_mut("type") {
        Some(Value::String(t)) => {
            if t != "null" {
                let t = std::mem::take(t);
                map.insert("type".into(), json!([t, "null"]));
                if let Some(s) = spans.get(&type_ptr) {
                    spans.insert(format!("{type_ptr}/0"), s);
                    spans.insert(format!("{type_ptr}/1"), null_span.unwrap_or(s));
                }
            }
            add_null_to_enum(map, spans, p, null_span);
            None
        }
        Some(Value::Array(types)) => {
            if !types.iter().any(|t| t == "null") {
                types.push(Value::String("null".into()));
                if let Some(s) = null_span {
                    spans.insert(format!("{type_ptr}/{}", types.len() - 1), s);
                }
            }
            add_null_to_enum(map, spans, p, null_span);
            None
        }
        // An invalid `type`; nothing sensible to rewrite.
        Some(_) => None,
        None => wrap(map, spans, p, null_span),
    }
}

fn add_null_to_enum(
    map: &mut Map<String, Value>,
    spans: &mut SpanIndex,
    p: &str,
    null_span: Option<tungsten_core::Span>,
) {
    if let Some(Value::Array(values)) = map.get_mut("enum")
        && !values.contains(&Value::Null)
    {
        values.push(Value::Null);
        if let Some(s) = null_span {
            spans.insert(
                format!("{}/{}", join_pointer(p, "enum"), values.len() - 1),
                s,
            );
        }
    }
}

/// `{..., nullable: true}` without `type` → `{annotations..., anyOf: [{rest},
/// {type: "null"}]}`.
fn wrap(
    map: &mut Map<String, Value>,
    spans: &mut SpanIndex,
    p: &str,
    null_span: Option<tungsten_core::Span>,
) -> Option<Move> {
    let mut outer = Map::new();
    let mut inner = Map::new();
    for (k, v) in std::mem::take(map) {
        if ANNOTATIONS.contains(&k.as_str()) {
            outer.insert(k, v);
        } else {
            if inner.is_empty() {
                // Placeholder that fixes the wrapper's member order.
                outer.insert("anyOf".into(), Value::Null);
            }
            inner.insert(k, v);
        }
    }
    if inner.is_empty() {
        // Nothing but annotations: the schema already admits null.
        *map = outer;
        return None;
    }
    let keys: BTreeSet<String> = inner.keys().cloned().collect();
    outer.insert(
        "anyOf".into(),
        Value::Array(vec![Value::Object(inner), json!({ "type": "null" })]),
    );
    *map = outer;

    let any_of = join_pointer(p, "anyOf");
    let to = format!("{any_of}/0");
    for k in &keys {
        spans.move_subtree(&join_pointer(p, k), &join_pointer(&to, k));
    }
    if let Some(s) = spans.get(p) {
        spans.insert(any_of.clone(), s);
        spans.insert(to.clone(), s);
        let ns = null_span.unwrap_or(s);
        spans.insert(format!("{any_of}/1"), ns);
        spans.insert(format!("{any_of}/1/type"), ns);
    }
    // A wrapped bare `$ref` moves nothing a pointer could address.
    (keys.iter().any(|k| k != "$ref")).then(|| Move {
        from: p.to_string(),
        to,
        keys,
    })
}
