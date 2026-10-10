// SPDX-License-Identifier: Apache-2.0
//! Dotted flags: `--field.sub value` sets one member of an object, map or
//! union argument whose flag takes JSON text (`--field '{...}'`). Both forms
//! can be mixed (the dotted members are applied on top of the JSON text).
//!
//! A map's keys and a union's members cannot be known to the command tree,
//! so the dotted tokens are taken out of the command line before clap sees
//! it and applied to the arguments of the command that runs. The type of a
//! value comes from the operation's compact JSON Schema (string, integer,
//! number, boolean, or JSON text for a nested object or list); where the
//! schema does not say, a value that is JSON is taken as JSON and any other
//! text is a string. A list member is given by repeating the flag.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::ctx::{Ctx, Fail};
use crate::spec::{CliFlag, CliSpec, FlagKind};
use crate::values::{json_from, number};

/// One `--flag.path value` of the command line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dotted {
    /// The flag (`filter`), as written without dashes.
    pub flag: String,
    /// The members below the argument (`["created", "after"]`).
    pub path: Vec<String>,
    pub text: String,
}

impl Dotted {
    fn shown(&self) -> String {
        format!("--{}.{}", self.flag, self.path.join("."))
    }
}

/// Flags that can take dotted members: those whose value is JSON text.
fn names(spec: &CliSpec) -> BTreeSet<&str> {
    let all = spec
        .ops
        .iter()
        .flat_map(|o| o.flags.iter())
        .chain(spec.macros.iter().flat_map(|m| m.flags.iter()));
    all.filter(|f| !f.sensitive && is_json(&f.kind))
        .map(|f| f.flag.as_str())
        .collect()
}

fn is_json(kind: &FlagKind) -> bool {
    match kind {
        FlagKind::Json => true,
        FlagKind::Array(inner) => **inner == FlagKind::Json,
        _ => false,
    }
}

/// Take the dotted tokens out of `argv` (program name first). A token is
/// `--name.path value` or `--name.path=value` where `name` is a JSON flag
/// of the table; everything after a bare `--` is left alone.
pub(crate) fn split(
    spec: &CliSpec,
    argv: Vec<String>,
) -> Result<(Vec<String>, Vec<Dotted>), String> {
    let names = names(spec);
    if names.is_empty() {
        return Ok((argv, Vec::new()));
    }
    let mut kept = Vec::with_capacity(argv.len());
    let mut found = Vec::new();
    let mut tokens = argv.into_iter();
    kept.extend(tokens.next());
    while let Some(token) = tokens.next() {
        if token == "--" {
            kept.push(token);
            kept.extend(tokens.by_ref());
            break;
        }
        let parsed = token
            .strip_prefix("--")
            .and_then(|body| body.split_once('.'))
            .filter(|(name, _)| names.contains(name));
        let Some((name, tail)) = parsed else {
            kept.push(token);
            continue;
        };
        let (path_text, inline) = match tail.split_once('=') {
            Some((path, value)) => (path, Some(value.to_string())),
            None => (tail, None),
        };
        let path: Vec<String> = path_text.split('.').map(str::to_string).collect();
        if path.iter().any(String::is_empty) {
            return Err(format!("--{name}.{path_text}: a member name is empty"));
        }
        let text = match inline {
            Some(text) => text,
            None => tokens
                .next()
                .ok_or_else(|| format!("--{name}.{path_text} needs a value"))?,
        };
        found.push(Dotted {
            flag: name.to_string(),
            path,
            text,
        });
    }
    Ok((kept, found))
}

/// The names of the flags the command line gave dotted members to.
pub(crate) fn flag_names(found: &[Dotted]) -> BTreeSet<String> {
    found.iter().map(|d| d.flag.clone()).collect()
}

// ---------------------------------------------------------------- schema

const DEPTH: usize = 16;

/// Follow a local `$ref` (`#/$defs/x`) of the schema document.
fn resolve<'a>(root: &'a Value, mut node: &'a Value) -> &'a Value {
    for _ in 0..DEPTH {
        let Some(pointer) = node
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix('#'))
        else {
            return node;
        };
        match root.pointer(pointer) {
            Some(next) => node = next,
            None => return node,
        }
    }
    node
}

/// The schema of member `key` of an object, map or union schema.
fn member<'a>(root: &'a Value, node: &'a Value, key: &str, depth: usize) -> Option<&'a Value> {
    if depth > DEPTH {
        return None;
    }
    let node = resolve(root, node);
    if let Some(found) = node.get("properties").and_then(|p| p.get(key)) {
        return Some(found);
    }
    for keyword in ["allOf", "oneOf", "anyOf"] {
        let alternatives = node.get(keyword).and_then(Value::as_array);
        for alternative in alternatives.into_iter().flatten() {
            if let Some(found) = member(root, alternative, key, depth + 1) {
                return Some(found);
            }
        }
    }
    node.get("additionalProperties").filter(|v| v.is_object())
}

/// The JSON type a schema names (`null` is ignored in a type list).
fn type_of<'a>(root: &'a Value, node: &'a Value) -> Option<&'a str> {
    let node = resolve(root, node);
    match node.get("type") {
        Some(Value::String(t)) => return Some(t),
        Some(Value::Array(list)) => {
            let mut kinds = list
                .iter()
                .filter_map(Value::as_str)
                .filter(|t| *t != "null");
            if let (Some(only), None) = (kinds.next(), kinds.next()) {
                return Some(only);
            }
            return None;
        }
        _ => {}
    }
    if let Some(values) = node.get("enum").and_then(Value::as_array)
        && !values.is_empty()
        && values.iter().all(Value::is_string)
    {
        return Some("string");
    }
    if node.get("properties").is_some() || node.get("additionalProperties").is_some() {
        return Some("object");
    }
    node.get("items").map(|_| "array")
}

fn items<'a>(root: &'a Value, node: &'a Value) -> Option<&'a Value> {
    resolve(root, node).get("items")
}

// ---------------------------------------------------------------- values

fn failure(d: &Dotted, message: impl std::fmt::Display) -> Fail {
    Fail::Usage(format!("{}: {message}", d.shown()))
}

/// Whether the schema accepts `null` (`type: [.., "null"]` or `nullable`).
fn allows_null(root: &Value, node: &Value) -> bool {
    let node = resolve(root, node);
    node.get("nullable").and_then(Value::as_bool) == Some(true)
        || node
            .get("type")
            .and_then(Value::as_array)
            .is_some_and(|list| list.iter().any(|t| t.as_str() == Some("null")))
}

/// One value of text, typed by the schema `node`.
fn scalar(
    ctx: &mut Ctx<'_>,
    text: &str,
    root: &Value,
    node: Option<&Value>,
) -> Result<Value, Fail> {
    if text == "null" && node.is_some_and(|n| allows_null(root, n)) {
        return Ok(Value::Null);
    }
    match node.and_then(|n| type_of(root, n)) {
        Some("string") => Ok(Value::String(text.to_string())),
        Some("integer") => {
            let n = number(text).map_err(Fail::Usage)?;
            if n.is_i64() || n.is_u64() {
                Ok(Value::Number(n))
            } else {
                Err(Fail::Usage(format!("`{text}` is not an integer")))
            }
        }
        Some("number") => number(text).map(Value::Number).map_err(Fail::Usage),
        Some("boolean") => match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(Fail::Usage(format!("`{text}` is not `true` or `false`"))),
        },
        Some("null") if text == "null" => Ok(Value::Null),
        Some("null") => Err(Fail::Usage(format!("`{text}` is not `null`"))),
        Some("object" | "array") => json_from(ctx, text),
        _ if text == "-" || text.starts_with('@') => json_from(ctx, text),
        _ => Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))),
    }
}

/// Put `value` below `object` at `path` (objects are made on the way). A
/// list member is pushed.
fn put(
    object: &mut Map<String, Value>,
    path: &[String],
    value: Value,
    push: bool,
) -> Result<(), String> {
    let Some((last, parents)) = path.split_last() else {
        return Ok(());
    };
    let mut at = object;
    for key in parents {
        let entry = at
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        at = entry
            .as_object_mut()
            .ok_or_else(|| format!("`{key}` already holds a value that is not an object"))?;
    }
    if push {
        let entry = at
            .entry(last.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        entry
            .as_array_mut()
            .ok_or_else(|| format!("`{last}` already holds a value that is not a list"))?
            .push(value);
    } else {
        at.insert(last.clone(), value);
    }
    Ok(())
}

/// Apply the dotted members to the arguments of the command (`flags` and
/// `schema` are the table's), then check the flags that were left to this
/// step because the dotted members can satisfy them.
pub(crate) fn apply(
    ctx: &mut Ctx<'_>,
    flags: &[CliFlag],
    schema: &Value,
    found: Vec<Dotted>,
    args: &mut Map<String, Value>,
) -> Result<(), Fail> {
    let mut seen: BTreeSet<(String, Vec<String>)> = BTreeSet::new();
    for d in found {
        let Some(flag) = flags.iter().find(|f| f.flag == d.flag && is_json(&f.kind)) else {
            return Err(Fail::Usage(format!(
                "unexpected argument '{}' found: this command has no JSON flag --{}",
                d.shown(),
                d.flag
            )));
        };
        if matches!(flag.kind, FlagKind::Array(_)) {
            return Err(failure(
                &d,
                format!(
                    "--{} is a list: repeat --{} with one JSON item each",
                    flag.flag, flag.flag
                ),
            ));
        }
        let mut node = schema.get("properties").and_then(|p| p.get(&flag.arg));
        for key in &d.path {
            node = node.and_then(|n| member(schema, n, key, 0));
        }
        let list = node.is_some_and(|n| type_of(schema, n) == Some("array"));
        let whole = d.text == "-" || d.text.starts_with(['@', '[']);
        let (value, push) = if list && !whole {
            let item = node.and_then(|n| items(schema, n));
            (scalar(ctx, &d.text, schema, item), true)
        } else {
            (scalar(ctx, &d.text, schema, node), false)
        };
        let value = value.map_err(|f| match f {
            Fail::Usage(m) => failure(&d, m),
            other => other,
        })?;
        if !push && !seen.insert((d.flag.clone(), d.path.clone())) {
            return Err(failure(&d, "given more than once"));
        }
        let root = args
            .entry(flag.arg.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(object) = root.as_object_mut() else {
            return Err(failure(
                &d,
                format!("--{} was given a value that is not an object", flag.flag),
            ));
        };
        put(object, &d.path, value, push).map_err(|m| failure(&d, m))?;
    }
    let missing: Vec<String> = flags
        .iter()
        .filter(|f| f.required && !f.sensitive && !args.contains_key(&f.arg))
        .map(|f| format!("--{} <JSON>", f.flag))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Fail::Usage(format!(
            "the following required arguments were not provided: {}",
            missing.join(", ")
        )))
    }
}
