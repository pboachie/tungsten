// SPDX-License-Identifier: AGPL-3.0-only
//! References, boolean expressions and predicates in macros and
//! verification hooks.
//!
//! - A reference is a string `$<root>` or `$<root>.<field>...`; the root is
//!   an identifier, each field a non-empty run without `.` or whitespace.
//! - An expression is JSON in which strings starting with `$` are
//!   references, an object whose only key is `expr` is a boolean
//!   (`<ref> in [<literal>, ...]`, `<ref> == <literal>`, `<ref> !=
//!   <literal>`), and anything else is a literal. Literals are JSON; strings
//!   may also be single-quoted. The canonical form renders literals as
//!   compact JSON (`$s.state in ["unknown","delivery_unknown"]`).
//! - A predicate maps field paths to exactly one test: `{in: [...]}`,
//!   `{equals: x}` or `{contains: {...}}`.

use serde_json::{Map, Value};

/// A parsed reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ref {
    pub root: String,
    pub path: Vec<String>,
}

impl Ref {
    pub fn parse(text: &str) -> Result<Ref, String> {
        let body = text
            .strip_prefix('$')
            .ok_or_else(|| format!("`{text}` is not a reference (it must start with `$`)"))?;
        let mut parts = body.split('.');
        let root = parts.next().unwrap_or_default();
        if !is_identifier(root) {
            return Err(format!(
                "`{text}` is not a valid reference: `$` must be followed by a name"
            ));
        }
        let mut path = vec![];
        for part in parts {
            if part.is_empty() || part.chars().any(char::is_whitespace) {
                return Err(format!(
                    "`{text}` is not a valid reference: empty or blank field name"
                ));
            }
            path.push(part.to_string());
        }
        Ok(Ref {
            root: root.to_string(),
            path,
        })
    }
}

pub(crate) fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A boolean expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoolExpr {
    pub subject: Ref,
    subject_text: String,
    op: Op,
    operand: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    In,
    Eq,
    Ne,
}

impl BoolExpr {
    pub fn parse(text: &str) -> Result<BoolExpr, String> {
        let trimmed = text.trim();
        let (subject_text, rest) = trimmed
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("expression `{text}` needs `<ref> in [...]`, `<ref> == <value>` or `<ref> != <value>`"))?;
        let subject = Ref::parse(subject_text)?;
        let rest = rest.trim_start();
        let (op, operand) = if let Some(r) = rest.strip_prefix("==") {
            (Op::Eq, r)
        } else if let Some(r) = rest.strip_prefix("!=") {
            (Op::Ne, r)
        } else if let Some(r) = rest
            .strip_prefix("in")
            .filter(|r| r.starts_with(char::is_whitespace) || r.starts_with('['))
        {
            (Op::In, r)
        } else {
            return Err(format!(
                "expression `{text}`: expected `in`, `==` or `!=` after `{subject_text}`"
            ));
        };
        let operand = literal(operand.trim()).map_err(|e| format!("expression `{text}`: {e}"))?;
        if op == Op::In && !operand.is_array() {
            return Err(format!("expression `{text}`: `in` needs a list"));
        }
        Ok(BoolExpr {
            subject,
            subject_text: subject_text.to_string(),
            op,
            operand,
        })
    }

    /// Canonical text: single spaces, literals as compact JSON.
    pub fn canonical(&self) -> String {
        let op = match self.op {
            Op::In => "in",
            Op::Eq => "==",
            Op::Ne => "!=",
        };
        format!("{} {op} {}", self.subject_text, self.operand)
    }
}

/// A JSON literal in which strings may be single-quoted.
fn literal(text: &str) -> Result<Value, String> {
    if text.is_empty() {
        return Err("missing value".into());
    }
    let mut json = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                json.push('"');
                let mut escaped = false;
                for c in chars.by_ref() {
                    json.push(c);
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        break;
                    }
                }
            }
            '\'' => {
                let mut s = String::new();
                let mut closed = false;
                let mut escaped = false;
                for c in chars.by_ref() {
                    if escaped {
                        s.push(c);
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '\'' {
                        closed = true;
                        break;
                    } else {
                        s.push(c);
                    }
                }
                if !closed {
                    return Err("unterminated string".into());
                }
                json.push_str(&Value::String(s).to_string());
            }
            c => json.push(c),
        }
    }
    serde_json::from_str(&json).map_err(|_| format!("`{text}` is not a JSON value"))
}

/// The expression object `{expr: "..."}`, when `value` is one.
pub(crate) fn expr_text(value: &Value) -> Option<&str> {
    match value {
        Value::Object(map) if map.len() == 1 => map.get("expr").and_then(Value::as_str),
        _ => None,
    }
}

/// One problem in an expression: pointer suffix (relative to the
/// expression) and message.
pub(crate) type Problem = (String, String);

/// Every reference in an expression with its pointer suffix, and the
/// problems of malformed references and expressions.
pub(crate) fn references(value: &Value) -> (Vec<(String, Ref)>, Vec<Problem>) {
    let mut refs = vec![];
    let mut problems = vec![];
    walk(value, String::new(), &mut refs, &mut problems);
    (refs, problems)
}

fn walk(value: &Value, at: String, refs: &mut Vec<(String, Ref)>, problems: &mut Vec<Problem>) {
    if let Some(text) = expr_text(value) {
        match BoolExpr::parse(text) {
            Ok(e) => refs.push((format!("{at}/expr"), e.subject)),
            Err(e) => problems.push((format!("{at}/expr"), e)),
        }
        return;
    }
    match value {
        Value::String(s) if s.starts_with('$') => match Ref::parse(s) {
            Ok(r) => refs.push((at, r)),
            Err(e) => problems.push((at, e)),
        },
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk(item, format!("{at}/{i}"), refs, problems);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                walk(v, crate::report::child(&at, k), refs, problems);
            }
        }
        _ => {}
    }
}

/// The expression with every `{expr}` in canonical form. Call only on
/// expressions without problems.
pub(crate) fn canonical(value: &Value) -> Value {
    if let Some(text) = expr_text(value) {
        let text = BoolExpr::parse(text).map_or_else(|_| text.to_string(), |e| e.canonical());
        return serde_json::json!({ "expr": text });
    }
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect())
        }
        other => other.clone(),
    }
}

/// Problems of a predicate's shape, with pointer suffixes.
pub(crate) fn predicate_problems(predicate: &Map<String, Value>) -> Vec<Problem> {
    let mut out = vec![];
    for (field, test) in predicate {
        let at = crate::report::child("", field);
        if field.is_empty() || field.split('.').any(str::is_empty) {
            out.push((at.clone(), format!("`{field}` is not a field path")));
        }
        let Some((kind, operand)) = test
            .as_object()
            .filter(|t| t.len() == 1)
            .and_then(|t| t.iter().next())
        else {
            out.push((
                at,
                format!("the test of `{field}` must be one of {{in: [...]}}, {{equals: x}}, {{contains: {{...}}}}"),
            ));
            continue;
        };
        let ok = match kind.as_str() {
            "in" => operand.is_array(),
            "equals" => true,
            "contains" => operand.is_object(),
            _ => false,
        };
        if !ok {
            out.push((
                crate::report::child(&at, kind),
                format!(
                    "the test of `{field}` must be one of {{in: [...]}}, {{equals: x}}, {{contains: {{...}}}}; found `{kind}`"
                ),
            ));
        }
    }
    out
}
