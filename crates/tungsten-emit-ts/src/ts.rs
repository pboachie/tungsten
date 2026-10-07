// SPDX-License-Identifier: AGPL-3.0-only
//! TypeScript lexical helpers: string literals, property keys, JSON values
//! as JavaScript literals and doc text.

use serde_json::Value;
use tungsten_ir::Doc;

/// A double-quoted string literal. JSON string syntax is valid TypeScript
/// string syntax (U+2028 and U+2029 included since ES2019).
pub(crate) fn string_lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Whether `s` is an ASCII identifier name (`[A-Za-z_$][A-Za-z0-9_$]*`).
/// Reserved words are identifier names too: they are valid property names.
pub(crate) fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// A property key in an object literal or object type. `__proto__` is
/// written as a computed key: as a plain key in an object literal it would
/// set the prototype instead of defining a property.
pub(crate) fn prop_key(name: &str) -> String {
    if name == "__proto__" {
        format!("[{}]", string_lit(name))
    } else if is_identifier(name) {
        name.to_string()
    } else {
        string_lit(name)
    }
}

/// A JSON value as a JavaScript expression (compact, one line).
pub(crate) fn json_lit(v: &Value) -> String {
    match v {
        Value::Null | Value::Bool(_) | Value::Number(_) => v.to_string(),
        Value::String(s) => string_lit(s),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(json_lit).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) if map.is_empty() => "{}".to_string(),
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", prop_key(k), json_lit(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
    }
}

/// A JSON value as a TypeScript literal type (`"a"`, `1`, `{ a: true }`).
pub(crate) fn json_type(v: &Value) -> String {
    match v {
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(json_type).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) if map.is_empty() => "{ [key: string]: never }".to_string(),
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", prop_key(k), json_type(v)))
                .collect();
            format!("{{ {} }}", parts.join("; "))
        }
        _ => json_lit(v),
    }
}

/// The text of a doc: the full description, or the summary when there is
/// no description, or both when the description does not repeat the
/// summary.
pub(crate) fn doc_text(doc: Option<&Doc>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    let summary = doc.summary.as_deref().map(str::trim).unwrap_or("");
    let description = doc.description.as_deref().map(str::trim).unwrap_or("");
    match (summary.is_empty(), description.is_empty()) {
        (_, true) => summary.to_string(),
        (true, false) => description.to_string(),
        (false, false) if description.starts_with(summary) => description.to_string(),
        (false, false) => format!("{summary}\n\n{description}"),
    }
}

/// The summary line of a doc (summary, else the description's first line).
pub(crate) fn doc_summary(doc: Option<&Doc>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    match doc.summary.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => doc
            .description
            .as_deref()
            .and_then(|d| d.trim().lines().next())
            .unwrap_or("")
            .to_string(),
    }
}

/// Join doc paragraphs, skipping empty ones.
pub(crate) fn paragraphs<I, S>(parts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parts
        .into_iter()
        .map(|p| p.as_ref().trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A JavaScript value tree printed one entry per line when it does not fit
/// on one line. Used for descriptor literals.
#[derive(Debug, Clone)]
pub(crate) enum Js {
    /// Code written verbatim.
    Raw(String),
    Object(Vec<(String, Js)>),
    Array(Vec<Js>),
}

impl Js {
    pub(crate) fn str(s: &str) -> Js {
        Js::Raw(string_lit(s))
    }
    pub(crate) fn opt_str(s: Option<&str>) -> Js {
        s.map_or_else(|| Js::Raw("null".into()), Js::str)
    }
    pub(crate) fn json(v: &Value) -> Js {
        match v {
            Value::Array(items) => Js::Array(items.iter().map(Js::json).collect()),
            Value::Object(map) => {
                Js::Object(map.iter().map(|(k, v)| (k.clone(), Js::json(v))).collect())
            }
            _ => Js::Raw(json_lit(v)),
        }
    }
    pub(crate) fn bool(b: bool) -> Js {
        Js::Raw(b.to_string())
    }
    pub(crate) fn num(n: impl std::fmt::Display) -> Js {
        Js::Raw(n.to_string())
    }
    pub(crate) fn strs<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Js {
        Js::Array(items.into_iter().map(|s| Js::str(s.as_ref())).collect())
    }
    pub(crate) fn obj(entries: Vec<(&str, Js)>) -> Js {
        Js::Object(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    fn inline(&self) -> String {
        match self {
            Js::Raw(s) => s.clone(),
            Js::Array(items) => {
                let parts: Vec<String> = items.iter().map(Js::inline).collect();
                format!("[{}]", parts.join(", "))
            }
            Js::Object(entries) if entries.is_empty() => "{}".into(),
            Js::Object(entries) => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", prop_key(k), v.inline()))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
        }
    }

    /// Render with `indent` per level, starting at column `col` (used to
    /// decide whether a value fits on one line of 100 columns).
    pub(crate) fn render(&self, indent: &str, col: usize) -> String {
        if let Js::Raw(s) = self {
            return s.clone();
        }
        let flat = self.inline();
        if col + flat.len() <= 100 && !flat.contains('\n') {
            return flat;
        }
        match self {
            Js::Raw(s) => s.clone(),
            Js::Array(items) => {
                let mut out = String::from("[\n");
                for item in items {
                    out.push_str(indent);
                    out.push_str(&indent_rest(&item.render(indent, indent.len()), indent));
                    out.push_str(",\n");
                }
                out.push(']');
                out
            }
            Js::Object(entries) => {
                let mut out = String::from("{\n");
                for (k, v) in entries {
                    let key = prop_key(k);
                    out.push_str(indent);
                    out.push_str(&key);
                    out.push_str(": ");
                    let rendered = v.render(indent, indent.len() + key.len() + 2);
                    out.push_str(&indent_rest(&rendered, indent));
                    out.push_str(",\n");
                }
                out.push('}');
                out
            }
        }
    }
}

/// Indent every line after the first by `indent`.
fn indent_rest(text: &str, indent: &str) -> String {
    let mut lines = text.split('\n');
    let mut out = lines.next().unwrap_or("").to_string();
    for l in lines {
        out.push('\n');
        if !l.is_empty() {
            out.push_str(indent);
        }
        out.push_str(l);
    }
    out
}
