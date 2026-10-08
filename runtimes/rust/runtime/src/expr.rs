// SPDX-License-Identifier: Apache-2.0
//! Predicates (verify `expect`/`terminal`, poll `until`) and the reference
//! expressions used by verification argument mappings and macros.
//!
//! Predicate: field path to `{in: [...]}` | `{equals: x}` | `{contains: {...}}`;
//! any other value is compared for equality. Every entry must hold.
//!
//! Expression: JSON in which a string starting with `$` is a reference
//! resolved against a scope (`$input.a.b`, `$<name>.a`, `$response.x`,
//! `$args.y`), an object `{expr: "<ref> in [..]" | "<ref> == x" | "<ref> != x"}`
//! evaluates to a boolean, and anything else is a literal.
//!
//! A dry evaluation (macro previews) knows the input but not the results of
//! the steps before: a reference to such a result evaluates to a placeholder
//! string, `<from step NAME: path>` (`<from step NAME>` for the whole
//! result), and so does an `expr` over one.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::util::{
    contains_subset, deep_equal, deep_equal_opt, get_path, get_path_str, json_text, split_path,
};

/// Bindings an expression can reference, by name without `$`.
pub type Scope = Map<String, Value>;

/// True when every entry of `predicate` holds on `value`. Malformed
/// predicates never hold; a null predicate always holds.
pub fn evaluate_predicate(predicate: &Value, value: Option<&Value>) -> bool {
    match predicate {
        Value::Null => true,
        Value::Object(entries) => entries
            .iter()
            .all(|(path, test)| holds(test, get_path_str(value, path))),
        _ => false,
    }
}

fn holds(test: &Value, actual: Option<&Value>) -> bool {
    if let Value::Object(map) = test
        && map.len() == 1
    {
        if let Some(candidates) = map.get("in") {
            return match candidates {
                Value::Array(list) => list.iter().any(|c| deep_equal_opt(Some(c), actual)),
                _ => false,
            };
        }
        if let Some(expected) = map.get("equals") {
            return deep_equal_opt(Some(expected), actual);
        }
        if let Some(pattern) = map.get("contains") {
            if let Value::String(text) = pattern {
                return match actual {
                    Some(Value::String(have)) => have.contains(text.as_str()),
                    Some(Value::Array(items)) => {
                        items.iter().any(|i| i.as_str() == Some(text.as_str()))
                    }
                    _ => false,
                };
            }
            if let Some(Value::Array(items)) = actual
                && !pattern.is_array()
            {
                return items
                    .iter()
                    .any(|item| contains_subset(Some(item), pattern));
            }
            return contains_subset(actual, pattern);
        }
    }
    deep_equal_opt(Some(test), actual)
}

/// Resolve one `$name.path` reference; `None` when the name is unbound.
pub fn resolve_ref(reference: &str, scope: &Scope) -> Option<Value> {
    let segments = split_path(reference.strip_prefix('$').unwrap_or(reference));
    let (name, rest) = segments.split_first()?;
    let bound = scope.get(name)?;
    get_path(Some(bound), rest).cloned()
}

/// Evaluate an expression against `scope`. Unknown references are `None`
/// (dropped from objects); malformed `expr` strings are `null`.
pub fn evaluate_expr(expr: &Value, scope: &Scope, depth: usize) -> Option<Value> {
    if depth > 64 {
        return Some(Value::Null);
    }
    match expr {
        Value::String(text) if text.starts_with('$') => resolve_ref(text, scope),
        Value::Array(items) => Some(Value::Array(
            items
                .iter()
                .map(|item| evaluate_expr(item, scope, depth + 1).unwrap_or(Value::Null))
                .collect(),
        )),
        Value::Object(map) => {
            if map.len() == 1
                && let Some(Value::String(source)) = map.get("expr")
            {
                return Some(evaluate_boolean(source, scope));
            }
            let mut out = Map::new();
            for (key, item) in map {
                if let Some(value) = evaluate_expr(item, scope, depth + 1) {
                    out.insert(key.clone(), value);
                }
            }
            Some(Value::Object(out))
        }
        other => Some(other.clone()),
    }
}

/// The placeholder a dry evaluation shows for a result not produced yet.
pub fn placeholder(name: &str, path: &str) -> String {
    if path.is_empty() {
        format!("<from step {name}>")
    } else {
        format!("<from step {name}: {path}>")
    }
}

static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^<from step [^<>]+>$").expect("static regex"));

/// The placeholders in `value`, at any depth, in order of appearance.
pub fn placeholders_in(value: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 64 {
        return;
    }
    match value {
        Value::String(text) if PLACEHOLDER.is_match(text) => out.push(text.clone()),
        Value::Array(items) => items
            .iter()
            .for_each(|item| placeholders_in(item, out, depth + 1)),
        Value::Object(map) => map
            .values()
            .for_each(|item| placeholders_in(item, out, depth + 1)),
        _ => {}
    }
}

/// Whether `value` is a placeholder, or holds one at any depth.
pub fn contains_placeholder(value: &Value) -> bool {
    let mut found = Vec::new();
    placeholders_in(value, &mut found, 0);
    !found.is_empty()
}

/// The pending binding a `$name.path` reference names, with the path as
/// written after the name; `None` when the name is not pending.
fn pending_ref(reference: &str, pending: &BTreeSet<String>) -> Option<(String, String)> {
    let name = split_path(reference.strip_prefix('$')?)
        .into_iter()
        .next()?;
    if !pending.contains(&name) || !reference.starts_with(&format!("${name}")) {
        return None;
    }
    let rest = &reference[name.len() + 1..];
    Some((name, rest.strip_prefix('.').unwrap_or(rest).to_owned()))
}

static DRY_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$[^\s=!]+").expect("static regex"));

/// [`evaluate_expr`] for a dry run: references to the `pending` names
/// (results of steps that have not run) become placeholders.
pub fn evaluate_dry(
    expr: &Value,
    scope: &Scope,
    pending: &BTreeSet<String>,
    depth: usize,
) -> Option<Value> {
    if depth > 64 {
        return Some(Value::Null);
    }
    match expr {
        Value::String(text) if text.starts_with('$') => match pending_ref(text, pending) {
            Some((name, path)) => Some(Value::String(placeholder(&name, &path))),
            None => resolve_ref(text, scope),
        },
        Value::Array(items) => Some(Value::Array(
            items
                .iter()
                .map(|item| evaluate_dry(item, scope, pending, depth + 1).unwrap_or(Value::Null))
                .collect(),
        )),
        Value::Object(map) => {
            if map.len() == 1
                && let Some(Value::String(raw)) = map.get("expr")
            {
                let source = raw.trim();
                let head = DRY_REF.find(source).map_or("", |m| m.as_str());
                return Some(match pending_ref(head, pending) {
                    Some((name, _)) => {
                        let rest = &source[name.len() + 1..];
                        Value::String(placeholder(&name, rest.strip_prefix('.').unwrap_or(rest)))
                    }
                    None => evaluate_boolean(source, scope),
                });
            }
            let mut out = Map::new();
            for (key, item) in map {
                if let Some(value) = evaluate_dry(item, scope, pending, depth + 1) {
                    out.insert(key.clone(), value);
                }
            }
            Some(Value::Object(out))
        }
        other => Some(other.clone()),
    }
}

/// A predicate in words, for remediation and preview text:
/// `state in ["delivered", "failed"]`, `endpoints contains {...}`, `a = 1`,
/// joined with "and"; "" for an empty predicate.
pub fn describe_predicate(predicate: &Value) -> String {
    let Value::Object(entries) = predicate else {
        return String::new();
    };
    let parts: Vec<String> = entries
        .iter()
        .map(|(path, test)| {
            if let Value::Object(map) = test
                && map.len() == 1
            {
                if let Some(Value::Array(list)) = map.get("in") {
                    let shown: Vec<String> = list.iter().map(json_text).collect();
                    return format!("{path} in [{}]", shown.join(", "));
                }
                if let Some(expected) = map.get("equals") {
                    return format!("{path} = {}", json_text(expected));
                }
                if let Some(pattern) = map.get("contains") {
                    return format!("{path} contains {}", json_text(pattern));
                }
            }
            format!("{path} = {}", json_text(test))
        })
        .collect();
    parts.join(" and ")
}

static SINGLE_QUOTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'((?:[^'\\]|\\.)*)'").expect("static regex"));

/// A JSON literal, also accepting single-quoted strings (`'a'`).
fn parse_literal(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Some(value);
    }
    let normalized = SINGLE_QUOTED.replace_all(trimmed, |caps: &regex::Captures<'_>| {
        json_text(&Value::String(caps[1].replace("\\'", "'")))
    });
    serde_json::from_str(&normalized).ok()
}

static BOOLEAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^\s*(\$[^\s=!]+)\s+(in|==|!=)\s+(.+)$").expect("static regex")
});

fn evaluate_boolean(source: &str, scope: &Scope) -> Value {
    let Some(caps) = BOOLEAN.captures(source) else {
        return Value::Null;
    };
    let Some(literal) = parse_literal(&caps[3]) else {
        return Value::Null;
    };
    let actual = resolve_ref(&caps[1], scope);
    if &caps[2] == "in" {
        return match literal {
            Value::Array(list) => Value::Bool(
                list.iter()
                    .any(|v| deep_equal_opt(Some(v), actual.as_ref())),
            ),
            _ => Value::Null,
        };
    }
    let equal = deep_equal(actual.as_ref().unwrap_or(&Value::Null), &literal);
    Value::Bool(if &caps[2] == "==" { equal } else { !equal })
}
