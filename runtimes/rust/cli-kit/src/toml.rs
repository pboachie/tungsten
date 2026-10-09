// SPDX-License-Identifier: Apache-2.0
//! The subset of TOML the configuration file uses: comments, `[table.path]`
//! headers, `key = value` lines with bare, quoted and dotted keys, strings
//! (basic with escapes and literal), integers, floats, booleans and
//! single-line arrays. Anything else (multi-line strings and arrays, inline
//! tables, arrays of tables, dates) is an error naming the line, so a file
//! is never half understood.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Array(Vec<Value>),
}

impl Value {
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// A number, or a string holding one.
    pub(crate) fn as_seconds(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Doc {
    entries: Vec<(Vec<String>, Value)>,
    tables: BTreeSet<Vec<String>>,
}

impl Doc {
    pub(crate) fn get(&self, path: &[&str]) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(p, _)| p.iter().map(String::as_str).eq(path.iter().copied()))
            .map(|(_, v)| v)
    }

    /// Whether a table (declared by a header or holding a key) exists.
    pub(crate) fn has_table(&self, path: &[&str]) -> bool {
        let starts = |p: &Vec<String>, n: usize| {
            p.len() >= n
                && p.iter()
                    .map(String::as_str)
                    .take(n)
                    .eq(path.iter().copied())
        };
        self.tables.iter().any(|t| starts(t, path.len()))
            || self
                .entries
                .iter()
                .any(|(p, _)| p.len() > path.len() && starts(p, path.len()))
    }

    /// The keys directly inside a table, in file order.
    pub(crate) fn keys(&self, path: &[&str]) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(p, _)| {
                p.len() == path.len() + 1
                    && p.iter()
                        .map(String::as_str)
                        .take(path.len())
                        .eq(path.iter().copied())
            })
            .map(|(p, _)| p[path.len()].as_str())
            .collect()
    }
}

/// Parse `text`; the error names the 1-based line.
pub(crate) fn parse(text: &str) -> Result<Doc, String> {
    let mut doc = Doc::default();
    let mut table: Vec<String> = Vec::new();
    for (n, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let at = |msg: String| format!("line {}: {msg}", n + 1);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if rest.starts_with('[') {
                return Err(at("arrays of tables are not supported".into()));
            }
            let (path, after) = key_path(rest, ']').map_err(&at)?;
            end_of_line(after).map_err(&at)?;
            doc.tables.insert(path.clone());
            table = path;
            continue;
        }
        let (key, after) = key_path(line, '=').map_err(&at)?;
        let (value, after) = value(after.trim_start()).map_err(&at)?;
        end_of_line(after).map_err(&at)?;
        let mut path = table.clone();
        path.extend(key);
        if doc.entries.iter().any(|(p, _)| *p == path) {
            return Err(at(format!("duplicate key `{}`", path.join("."))));
        }
        doc.entries.push((path, value));
    }
    Ok(doc)
}

fn end_of_line(rest: &str) -> Result<(), String> {
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(format!("unexpected `{rest}`"))
    }
}

/// A dotted key up to `close` (`]` or `=`), returning the rest after it.
fn key_path(s: &str, close: char) -> Result<(Vec<String>, &str), String> {
    let mut parts = Vec::new();
    let mut rest = s.trim_start();
    loop {
        let (part, after) = if let Some(quoted) = rest.strip_prefix('"') {
            basic_string(quoted)?
        } else if let Some(quoted) = rest.strip_prefix('\'') {
            literal_string(quoted)?
        } else {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .unwrap_or(rest.len());
            if end == 0 {
                return Err(format!("expected a key, found `{rest}`"));
            }
            (rest[..end].to_string(), &rest[end..])
        };
        parts.push(part);
        rest = after.trim_start();
        if let Some(r) = rest.strip_prefix('.') {
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix(close) {
            return Ok((parts, r));
        } else {
            return Err(format!("expected `{close}` after the key"));
        }
    }
}

fn value(s: &str) -> Result<(Value, &str), String> {
    if s.starts_with("\"\"\"") || s.starts_with("'''") {
        return Err("multi-line strings are not supported".into());
    }
    if let Some(r) = s.strip_prefix('"') {
        let (v, rest) = basic_string(r)?;
        return Ok((Value::Str(v), rest));
    }
    if let Some(r) = s.strip_prefix('\'') {
        let (v, rest) = literal_string(r)?;
        return Ok((Value::Str(v), rest));
    }
    if let Some(r) = s.strip_prefix('[') {
        return array(r);
    }
    if s.starts_with('{') {
        return Err("inline tables are not supported".into());
    }
    let end = s
        .find(|c: char| c == ',' || c == ']' || c == '#' || c.is_whitespace())
        .unwrap_or(s.len());
    let (word, rest) = s.split_at(end);
    let v = match word {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "" => return Err("missing value".into()),
        w => number(w)?,
    };
    Ok((v, rest))
}

fn number(word: &str) -> Result<Value, String> {
    let plain = word.replace('_', "");
    if let Ok(i) = plain.parse::<i64>() {
        return Ok(Value::Int(i));
    }
    let looks_numeric = plain
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'));
    match plain.parse::<f64>() {
        Ok(f) if looks_numeric && f.is_finite() => Ok(Value::Float(f)),
        _ => Err(format!("unsupported value `{word}`")),
    }
}

fn array(s: &str) -> Result<(Value, &str), String> {
    let mut items = Vec::new();
    let mut rest = s.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix(']') {
            return Ok((Value::Array(items), r));
        }
        if rest.is_empty() || rest.starts_with('#') {
            return Err("multi-line arrays are not supported".into());
        }
        let (v, after) = value(rest)?;
        items.push(v);
        rest = after.trim_start();
        if let Some(r) = rest.strip_prefix(',') {
            rest = r.trim_start();
        } else if !rest.starts_with(']') {
            return Err("expected `,` or `]` in the array".into());
        }
    }
}

fn literal_string(s: &str) -> Result<(String, &str), String> {
    match s.find('\'') {
        Some(end) => Ok((s[..end].to_string(), &s[end + 1..])),
        None => Err("unterminated string".into()),
    }
}

fn basic_string(s: &str) -> Result<(String, &str), String> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, &s[i + 1..])),
            '\\' => {
                let (_, e) = chars.next().ok_or("unterminated string")?;
                match e {
                    'b' => out.push('\u{8}'),
                    't' => out.push('\t'),
                    'n' => out.push('\n'),
                    'f' => out.push('\u{c}'),
                    'r' => out.push('\r'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'u' | 'U' => {
                        let len = if e == 'u' { 4 } else { 8 };
                        let mut code = 0u32;
                        for _ in 0..len {
                            let (_, h) = chars.next().ok_or("unterminated string")?;
                            let digit = h.to_digit(16).ok_or("invalid unicode escape")?;
                            code = code.checked_mul(16).ok_or("invalid unicode escape")? + digit;
                        }
                        out.push(char::from_u32(code).ok_or("invalid unicode escape")?);
                    }
                    other => return Err(format!("unknown escape `\\{other}`")),
                }
            }
            c => out.push(c),
        }
    }
    Err("unterminated string".into())
}
