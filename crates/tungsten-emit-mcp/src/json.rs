// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic JSON layout for `manifest.json`: objects one key per line,
//! two-space indent; arrays of scalars (or of small arrays of scalars, such
//! as BM25 postings) inline when they fit in [`WIDTH`] columns and filled
//! line by line otherwise, so the file stays short and diffs stay readable.

use serde_json::Value;

/// Target line width of filled arrays.
const WIDTH: usize = 100;

/// `value` laid out, with a final newline.
pub(crate) fn pretty(value: &Value) -> String {
    let mut out = String::new();
    write(value, 0, &mut out);
    out.push('\n');
    out
}

fn scalar(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

/// An array whose elements are scalars or non-empty arrays of scalars,
/// rendered inline: `[1, 2]`, `[[0, 1], [3, 2]]`.
fn flat(items: &[Value]) -> Option<Vec<String>> {
    items
        .iter()
        .map(|item| match item {
            Value::Array(inner) if inner.iter().all(is_scalar) => Some(format!(
                "[{}]",
                inner.iter().map(scalar).collect::<Vec<_>>().join(", ")
            )),
            v if is_scalar(v) => Some(scalar(v)),
            _ => None,
        })
        .collect()
}

fn write(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent + 2);
    match value {
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            out.push_str("{\n");
            let last = map.len() - 1;
            for (i, (key, v)) in map.iter().enumerate() {
                out.push_str(&pad);
                out.push_str(&scalar(&Value::String(key.clone())));
                out.push_str(": ");
                write(v, indent + 2, out);
                if i != last {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&" ".repeat(indent));
            out.push('}');
        }
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => match flat(items) {
            Some(parts) => {
                let inline = format!("[{}]", parts.join(", "));
                if indent + inline.len() <= WIDTH {
                    out.push_str(&inline);
                    return;
                }
                out.push_str("[\n");
                let mut line = String::new();
                let last = parts.len() - 1;
                for (i, part) in parts.iter().enumerate() {
                    let piece = if i == last {
                        part.clone()
                    } else {
                        format!("{part},")
                    };
                    if !line.is_empty() && pad.len() + line.len() + 1 + piece.len() > WIDTH {
                        out.push_str(&pad);
                        out.push_str(&line);
                        out.push('\n');
                        line.clear();
                    }
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(&piece);
                }
                out.push_str(&pad);
                out.push_str(&line);
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                out.push(']');
            }
            None => {
                out.push_str("[\n");
                let last = items.len() - 1;
                for (i, v) in items.iter().enumerate() {
                    out.push_str(&pad);
                    write(v, indent + 2, out);
                    if i != last {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&" ".repeat(indent));
                out.push(']');
            }
        },
        v => out.push_str(&scalar(v)),
    }
}
