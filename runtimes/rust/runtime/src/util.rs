// SPDX-License-Identifier: Apache-2.0
//! Small pure helpers shared by the runtime modules: value inspection, field
//! paths, canonical JSON, hashing, encodings and redaction. The text forms
//! (`JSON.stringify`, canonical JSON, number formatting) are the ones the
//! TypeScript runtime writes, byte for byte, so digests, tokens and request
//! bodies agree across runtimes.

use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

use crate::value::{BINARY_KEY, Binary};

/// Placeholder written wherever a secret or sensitive value would appear.
pub const REDACTED: &str = "<redacted>";

/// Longest string kept in `Diagnostic.received_value`.
pub const MAX_RECEIVED_CHARS: usize = 200;

/// Deepest JSON nesting the runtime accepts in arguments.
pub const MAX_ARG_DEPTH: usize = 256;

/// The byte-wise standard base64 text of `bytes`.
pub fn base64_standard(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Unpadded URL-safe base64.
pub fn base64url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Lowercase hex SHA-256 of bytes.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 15));
    }
    out
}

fn hex_digit(n: u8) -> char {
    char::from(if n < 10 { b'0' + n } else { b'a' + n - 10 })
}

/// HMAC-SHA-256 of `message` under `key` (any key length).
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    // HMAC accepts keys of every length, so construction cannot fail.
    match Hmac::<Sha256>::new_from_slice(key) {
        Ok(mut mac) => {
            mac.update(message);
            mac.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// Constant-time comparison of two strings of known public length.
pub fn timing_safe_equal(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// Random bytes from the operating system.
pub fn random_bytes<const N: usize>() -> Option<[u8; N]> {
    let mut buffer = [0u8; N];
    getrandom::fill(&mut buffer).ok()?;
    Some(buffer)
}

/// A random UUID version 4 in canonical lowercase form.
pub fn uuid_v4() -> Option<String> {
    let mut b = random_bytes::<16>()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 15));
    }
    Some(out)
}

// ------------------------------------------------------------------ values

/// The [`Binary`] a tagged object stands for (`{"$tungsten_binary": ..}`).
pub fn binary_of(value: &Value) -> Option<Binary> {
    match value {
        Value::Object(map) if map.contains_key(BINARY_KEY) => Binary::deserialize(value).ok(),
        _ => None,
    }
}

/// Number of nested containers, counting `value` itself.
pub fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Split a field path (`a.b`, `items[0].id`, `items.0.id`) into segments.
pub fn split_path(path: &str) -> Vec<String> {
    if path.is_empty() || path == "." {
        return Vec::new();
    }
    let chars: Vec<char> = path.chars().collect();
    let mut dotted = String::with_capacity(path.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < chars.len() && chars[j] == ']' {
                dotted.push('.');
                dotted.extend(&chars[i + 1..j]);
                i = j + 1;
                continue;
            }
        }
        dotted.push(chars[i]);
        i += 1;
    }
    dotted
        .split('.')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn is_index(segment: &str) -> bool {
    !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit())
}

/// Read a field path from a JSON value; `None` when absent.
pub fn get_path<'a>(value: Option<&'a Value>, segments: &[String]) -> Option<&'a Value> {
    let mut current = value?;
    for segment in segments {
        current = match current {
            Value::Array(items) if is_index(segment) => {
                items.get(segment.parse::<usize>().ok()?)?
            }
            Value::Object(map) => map.get(segment)?,
            _ => return None,
        };
    }
    Some(current)
}

/// [`get_path`] with a textual path.
pub fn get_path_str<'a>(value: Option<&'a Value>, path: &str) -> Option<&'a Value> {
    get_path(value, &split_path(path))
}

fn numbers_equal(a: &Number, b: &Number) -> bool {
    match (a.as_i64(), b.as_i64()) {
        (Some(x), Some(y)) => x == y,
        _ => match (a.as_u64(), b.as_u64()) {
            (Some(x), Some(y)) => x == y,
            _ => a.as_f64() == b.as_f64(),
        },
    }
}

/// Structural equality over JSON values (`1` equals `1.0`; object member
/// order does not matter).
pub fn deep_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => numbers_equal(x, y),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| deep_equal(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|other| deep_equal(v, other)))
        }
        _ => a == b,
    }
}

/// [`deep_equal`] where either side may be absent (`undefined`).
pub fn deep_equal_opt(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => deep_equal(x, y),
        _ => false,
    }
}

/// Every field of `pattern` appears in `value` with an equal (recursively
/// contained) value. Arrays in `pattern` match when each element is
/// contained by some element of the corresponding array.
pub fn contains_subset(value: Option<&Value>, pattern: &Value) -> bool {
    match pattern {
        Value::Object(fields) => match value {
            Some(Value::Object(have)) => fields
                .iter()
                .all(|(k, p)| have.contains_key(k) && contains_subset(have.get(k), p)),
            _ => false,
        },
        Value::Array(wanted) => match value {
            Some(Value::Array(have)) => wanted
                .iter()
                .all(|p| have.iter().any(|v| contains_subset(Some(v), p))),
            _ => false,
        },
        _ => value.is_some_and(|v| deep_equal(v, pattern)),
    }
}

// ------------------------------------------------------- JSON text forms

/// A string literal as `JSON.stringify` writes it.
pub fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    push_js_string(&mut out, text);
    out
}

fn push_js_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str("\\u00");
                out.push(hex_digit((c as u32 >> 4) as u8));
                out.push(hex_digit((c as u32 & 15) as u8));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `String(value)` for a finite JavaScript number (ECMAScript
/// Number::toString): `1.0` is `1`, `1e-7` is `1e-7`, `1e21` is `1e+21`,
/// `-0` is `0`.
pub fn js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent_text) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent_text.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let k = digits.len() as i32;
    let n = exponent + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let mark = if e >= 0 { '+' } else { '-' };
        let head = if k == 1 {
            digits.to_owned()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!("{head}e{mark}{}", e.abs())
    };
    format!("{sign}{body}")
}

/// A JSON number as JavaScript writes it (`null` for a non-finite one).
pub fn number_text(number: &Number) -> String {
    if let Some(i) = number.as_i64() {
        return i.to_string();
    }
    if let Some(u) = number.as_u64() {
        return u.to_string();
    }
    match number.as_f64() {
        Some(f) if f.is_finite() => js_number(f),
        _ => "null".to_owned(),
    }
}

/// `JSON.stringify(value)`: members in insertion order, no whitespace,
/// numbers as JavaScript writes them.
pub fn json_text(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, &mut out, false);
    out
}

/// The canonical JSON every tungsten runtime digests (confirmation tokens,
/// `auto` idempotency logical ids, `content_hash` keys): object members
/// sorted by the UTF-16 code units of their names, no whitespace, numbers as
/// JavaScript writes them, binary values as `{"$bytes": <base64>}` (a file
/// with a name or type as `{"$file": {"bytes", "name", "type"}}`).
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, &mut out, true);
    out
}

fn write_json(value: &Value, out: &mut String, canonical: bool) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&number_text(n)),
        Value::String(s) => push_js_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out, canonical);
            }
            out.push(']');
        }
        Value::Object(map) => {
            if canonical && let Some(binary) = binary_of(value) {
                write_binary(&binary, out);
                return;
            }
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if canonical {
                entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            }
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_js_string(out, key);
                out.push(':');
                write_json(item, out, canonical);
            }
            out.push('}');
        }
    }
}

fn write_binary(binary: &Binary, out: &mut String) {
    let encoded = base64_standard(&binary.data);
    if binary.filename.is_none() && binary.content_type.is_none() {
        out.push_str("{\"$bytes\":");
        push_js_string(out, &encoded);
        out.push('}');
        return;
    }
    out.push_str("{\"$file\":{\"bytes\":");
    push_js_string(out, &encoded);
    out.push_str(",\"name\":");
    match &binary.filename {
        Some(name) => push_js_string(out, name),
        None => out.push_str("null"),
    }
    out.push_str(",\"type\":");
    match &binary.content_type {
        Some(kind) => push_js_string(out, kind),
        None => out.push_str("null"),
    }
    out.push_str("}}");
}

/// Length in UTF-16 code units, the unit JavaScript counts in.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first `max_units` UTF-16 code units of `text`, never splitting a
/// character.
pub fn take_units(text: &str, max_units: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for ch in text.chars() {
        used += ch.len_utf16();
        if used > max_units {
            break;
        }
        out.push(ch);
    }
    out
}

// ---------------------------------------------------------------- redaction

/// Whether a name very likely holds a secret (redacted even without
/// metadata).
pub fn looks_sensitive(name: &str) -> bool {
    let lower = name.to_lowercase();
    const WORDS: [&str; 15] = [
        "secret",
        "password",
        "passwd",
        "passphrase",
        "token",
        "apikey",
        "api-key",
        "api_key",
        "privatekey",
        "private-key",
        "private_key",
        "credential",
        "authorization",
        "cookie",
        "session",
    ];
    WORDS.iter().any(|word| lower.contains(word))
}

/// Deep copy of a JSON value with sensitive-looking keys redacted.
pub fn redact_sensitive_keys(value: &Value, depth: usize) -> Value {
    if depth > 32 {
        return Value::Null;
    }
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| redact_sensitive_keys(v, depth + 1))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, item) in map {
                let shown = if looks_sensitive(key) {
                    Value::String(REDACTED.to_owned())
                } else {
                    redact_sensitive_keys(item, depth + 1)
                };
                out.insert(key.clone(), shown);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn redact_at(value: &Value, segments: &[String]) -> Value {
    let Some((head, rest)) = segments.split_first() else {
        return Value::String(REDACTED.to_owned());
    };
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|v| redact_at(v, segments)).collect()),
        Value::Object(map) if map.contains_key(head) => {
            let mut out = map.clone();
            if let Some(slot) = out.get_mut(head) {
                *slot = redact_at(slot, rest);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Deep copy with every listed field path redacted; arrays on the way fan
/// out, so `items.secret` redacts the field in each item.
pub fn redact_paths(value: &Value, paths: &[String]) -> Value {
    let mut out = value.clone();
    for path in paths {
        let segments = split_path(path);
        if !segments.is_empty() {
            out = redact_at(&out, &segments);
        }
    }
    out
}

fn cut(text: &str) -> String {
    if utf16_len(text) > MAX_RECEIVED_CHARS {
        format!("{}…", take_units(text, MAX_RECEIVED_CHARS - 1))
    } else {
        text.to_owned()
    }
}

/// A value made safe for `Diagnostic.received_value`: redacted when
/// sensitive, binary summarised, strings and large values cut to 200
/// characters, sensitive-looking nested keys redacted.
pub fn envelope_value(value: &Value, sensitive: bool) -> Value {
    if sensitive {
        return Value::String(REDACTED.to_owned());
    }
    match value {
        Value::String(s) => Value::String(cut(s)),
        Value::Array(_) | Value::Object(_) => {
            if let Some(binary) = binary_of(value) {
                return Value::String(format!("<{} bytes>", binary.data.len()));
            }
            let json = json_text(&redact_sensitive_keys(value, 0));
            if utf16_len(&json) > MAX_RECEIVED_CHARS {
                Value::String(cut(&json))
            } else {
                serde_json::from_str(&json).unwrap_or(Value::Null)
            }
        }
        other => other.clone(),
    }
}

/// Secret values to scrub from texts, in insertion order without duplicates.
#[derive(Debug, Default, Clone)]
pub struct SecretSet {
    items: Vec<String>,
}

impl SecretSet {
    pub fn insert(&mut self, secret: impl Into<String>) {
        let secret = secret.into();
        if !secret.is_empty() && !self.items.contains(&secret) {
            self.items.push(secret);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(String::as_str)
    }
}

/// Replace every occurrence of the secrets (four characters or more) in a text.
pub fn scrub_text(text: &str, secrets: &SecretSet) -> String {
    let mut out = text.to_owned();
    for secret in secrets.iter() {
        if secret.chars().count() >= 4 && out.contains(secret) {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

/// [`scrub_text`] over every string of a JSON value.
pub fn scrub_value(value: &Value, secrets: &SecretSet) -> Value {
    match value {
        Value::String(s) => Value::String(scrub_text(s, secrets)),
        Value::Array(_) | Value::Object(_) => {
            let scrubbed = scrub_text(&json_text(value), secrets);
            serde_json::from_str(&scrubbed).unwrap_or_else(|_| Value::String(REDACTED.to_owned()))
        }
        other => other.clone(),
    }
}
