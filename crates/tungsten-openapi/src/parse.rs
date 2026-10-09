// SPDX-License-Identifier: AGPL-3.0-only
//! Text → `serde_json::Value` with a pointer → span index.
//!
//! JSON goes through a small iterative parser that records the byte span of
//! every value; YAML goes through the yaml-rust2 event parser (see
//! [`crate::yaml`]). Both enforce the nesting-depth limit while parsing, so
//! hostile input never reaches recursive code.

use serde_json::{Map, Number, Value};
use tungsten_core::{Diagnostic, SourceId, Span};

use crate::escape_token;
use crate::spans::SpanIndex;

/// A parsed file.
#[derive(Debug, Clone)]
pub(crate) struct Parsed {
    pub value: Value,
    pub spans: SpanIndex,
}

/// Why a file could not be parsed. Converted to a diagnostic by the caller,
/// who knows the file's display name.
#[derive(Debug, Clone)]
pub(crate) struct ParseError {
    pub code: &'static str,
    pub message: String,
    pub pointer: String,
    pub span: Option<Span>,
}

impl ParseError {
    pub(crate) fn into_diagnostic(self, file: &str) -> Diagnostic {
        Diagnostic::error(self.code, self.message).at(file, self.pointer, self.span)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Json,
    Yaml,
}

const BOM: &str = "\u{feff}";

/// The format of a file: by extension when it has a known one, otherwise by
/// content (a document starting with `{` or `[` is JSON).
pub(crate) fn detect_format(name: &str, text: &str) -> Format {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".json") {
        Format::Json
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        Format::Yaml
    } else {
        match text.trim_start_matches(BOM).trim_start().as_bytes().first() {
            Some(b'{' | b'[') => Format::Json,
            _ => Format::Yaml,
        }
    }
}

/// Parse `text` as JSON or YAML according to [`detect_format`].
pub(crate) fn parse(
    name: &str,
    text: &str,
    source: SourceId,
    max_depth: usize,
) -> Result<Parsed, ParseError> {
    match detect_format(name, text) {
        Format::Json => parse_json(text, source, max_depth),
        Format::Yaml => crate::yaml::parse_yaml(text, source, max_depth),
    }
}

/// Parse JSON (RFC 8259). Duplicate object keys are an error.
pub(crate) fn parse_json(
    text: &str,
    source: SourceId,
    max_depth: usize,
) -> Result<Parsed, ParseError> {
    JsonParser {
        text,
        bytes: text.as_bytes(),
        pos: if text.starts_with(BOM) { BOM.len() } else { 0 },
        source,
        max_depth,
        spans: SpanIndex::default(),
        pointer: String::new(),
    }
    .run()
}

/// An open container while parsing. `base` is the length of the pointer of
/// the container itself; member pointers extend it.
enum Frame {
    Object {
        map: Map<String, Value>,
        start: usize,
        base: usize,
        key: String,
    },
    Array {
        items: Vec<Value>,
        start: usize,
        base: usize,
    },
}

struct JsonParser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    source: SourceId,
    max_depth: usize,
    spans: SpanIndex,
    /// Pointer of the value being parsed.
    pointer: String,
}

impl JsonParser<'_> {
    fn run(mut self) -> Result<Parsed, ParseError> {
        let mut stack: Vec<Frame> = vec![];
        'value: loop {
            self.skip_ws();
            let start = self.pos;
            let mut value = match self.bytes.get(self.pos) {
                Some(b'{') => {
                    self.check_depth(stack.len() + 1)?;
                    self.pos += 1;
                    self.skip_ws();
                    if self.bytes.get(self.pos) == Some(&b'}') {
                        self.pos += 1;
                        self.record(start);
                        Value::Object(Map::new())
                    } else {
                        let base = self.pointer.len();
                        let key = self.member_key()?;
                        stack.push(Frame::Object {
                            map: Map::new(),
                            start,
                            base,
                            key,
                        });
                        continue 'value;
                    }
                }
                Some(b'[') => {
                    self.check_depth(stack.len() + 1)?;
                    self.pos += 1;
                    self.skip_ws();
                    if self.bytes.get(self.pos) == Some(&b']') {
                        self.pos += 1;
                        self.record(start);
                        Value::Array(vec![])
                    } else {
                        let base = self.pointer.len();
                        self.pointer.push_str("/0");
                        stack.push(Frame::Array {
                            items: vec![],
                            start,
                            base,
                        });
                        continue 'value;
                    }
                }
                Some(b'"') => {
                    let s = self.string()?;
                    self.record(start);
                    Value::String(s)
                }
                Some(b't') => self.literal("true", Value::Bool(true))?,
                Some(b'f') => self.literal("false", Value::Bool(false))?,
                Some(b'n') => self.literal("null", Value::Null)?,
                Some(b'-' | b'0'..=b'9') => {
                    let n = self.number()?;
                    self.record(start);
                    Value::Number(n)
                }
                Some(_) => return Err(self.error("expected a JSON value", self.pos)),
                None => return Err(self.error("unexpected end of input", self.pos)),
            };
            // `value` is complete: attach it to its parent, closing every
            // container that ends right after it.
            loop {
                let Some(frame) = stack.last_mut() else {
                    self.skip_ws();
                    if self.pos < self.bytes.len() {
                        return Err(
                            self.error("unexpected characters after the document", self.pos)
                        );
                    }
                    return Ok(Parsed {
                        value,
                        spans: self.spans,
                    });
                };
                match frame {
                    Frame::Object { map, base, key, .. } => {
                        if map.contains_key(key.as_str()) {
                            return Err(ParseError {
                                code: "TG0102",
                                message: format!("duplicate key \"{key}\""),
                                pointer: self.pointer.clone(),
                                span: self.spans.get(&self.pointer),
                            });
                        }
                        map.insert(std::mem::take(key), value);
                        self.skip_ws();
                        match self.bytes.get(self.pos) {
                            Some(b',') => {
                                self.pos += 1;
                                self.skip_ws();
                                self.pointer.truncate(*base);
                                *key = self.member_key()?;
                                continue 'value;
                            }
                            Some(b'}') => {
                                self.pos += 1;
                                self.pointer.truncate(*base);
                            }
                            _ => return Err(self.error("expected ',' or '}'", self.pos)),
                        }
                    }
                    Frame::Array { items, base, .. } => {
                        items.push(value);
                        self.skip_ws();
                        match self.bytes.get(self.pos) {
                            Some(b',') => {
                                self.pos += 1;
                                self.pointer.truncate(*base);
                                self.pointer.push_str(&format!("/{}", items.len()));
                                continue 'value;
                            }
                            Some(b']') => {
                                self.pos += 1;
                                self.pointer.truncate(*base);
                            }
                            _ => return Err(self.error("expected ',' or ']'", self.pos)),
                        }
                    }
                }
                let (closed, start) = match stack.pop().expect("frame is on the stack") {
                    Frame::Object { map, start, .. } => (Value::Object(map), start),
                    Frame::Array { items, start, .. } => (Value::Array(items), start),
                };
                self.record(start);
                value = closed;
            }
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), ParseError> {
        if depth > self.max_depth {
            return Err(ParseError {
                code: "TG0105",
                message: format!("nesting depth exceeds the limit of {}", self.max_depth),
                pointer: self.pointer.clone(),
                span: Some(self.span_at(self.pos)),
            });
        }
        Ok(())
    }

    /// Parse `"key" :` and point `self.pointer` at the member's value.
    fn member_key(&mut self) -> Result<String, ParseError> {
        if self.bytes.get(self.pos) != Some(&b'"') {
            return Err(self.error("expected a string key", self.pos));
        }
        let key = self.string()?;
        self.skip_ws();
        if self.bytes.get(self.pos) != Some(&b':') {
            return Err(self.error("expected ':' after the key", self.pos));
        }
        self.pos += 1;
        self.pointer.push('/');
        self.pointer.push_str(&escape_token(&key));
        Ok(key)
    }

    fn record(&mut self, start: usize) {
        self.spans.insert(
            self.pointer.clone(),
            Span::new(self.source, start, self.pos),
        );
    }

    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.pos) {
            self.pos += 1;
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, ParseError> {
        let start = self.pos;
        if !self.bytes[self.pos..].starts_with(word.as_bytes()) {
            return Err(self.error("expected a JSON value", self.pos));
        }
        self.pos += word.len();
        self.record(start);
        Ok(value)
    }

    fn number(&mut self) -> Result<Number, ParseError> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let from = p.pos;
            while p.bytes.get(p.pos).is_some_and(u8::is_ascii_digit) {
                p.pos += 1;
            }
            p.pos > from
        };
        if self.bytes.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.bytes.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.error("invalid number", start)),
        }
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if !digits(self) {
                return Err(self.error("invalid number: expected digits after '.'", start));
            }
        }
        if let Some(b'e' | b'E') = self.bytes.get(self.pos) {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.bytes.get(self.pos) {
                self.pos += 1;
            }
            if !digits(self) {
                return Err(self.error("invalid number: expected exponent digits", start));
            }
        }
        serde_json::from_str::<Number>(&self.text[start..self.pos])
            .map_err(|_| self.error("number out of range", start))
    }

    fn string(&mut self) -> Result<String, ParseError> {
        let open = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let run = self.pos;
            while let Some(&b) = self.bytes.get(self.pos) {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // Runs stop only at ASCII bytes, so both ends are char boundaries.
            out.push_str(&self.text[run..self.pos]);
            match self.bytes.get(self.pos) {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    let esc = self.pos;
                    let c = match self.bytes.get(esc + 1) {
                        Some(b'u') => {
                            self.pos = esc + 2;
                            self.unicode_escape(esc)?
                        }
                        Some(&b) => {
                            let c = match b {
                                b'"' => '"',
                                b'\\' => '\\',
                                b'/' => '/',
                                b'b' => '\u{8}',
                                b'f' => '\u{c}',
                                b'n' => '\n',
                                b'r' => '\r',
                                b't' => '\t',
                                _ => return Err(self.error("invalid escape sequence", esc)),
                            };
                            self.pos = esc + 2;
                            c
                        }
                        None => return Err(self.error("unterminated string", open)),
                    };
                    out.push(c);
                }
                Some(_) => {
                    return Err(self.error("control character in string", self.pos));
                }
                None => return Err(self.error("unterminated string", open)),
            }
        }
    }

    /// After `\u`: four hex digits, combining a surrogate pair when present.
    /// Leaves `pos` after the last hex digit.
    fn unicode_escape(&mut self, esc: usize) -> Result<char, ParseError> {
        let hi = self.hex4(esc)?;
        let code = if (0xD800..0xDC00).contains(&hi) {
            if !self.bytes[self.pos..].starts_with(b"\\u") {
                return Err(self.error("unpaired surrogate in \\u escape", esc));
            }
            self.pos += 2;
            let lo = self.hex4(esc)?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(self.error("unpaired surrogate in \\u escape", esc));
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else if (0xDC00..0xE000).contains(&hi) {
            return Err(self.error("unpaired surrogate in \\u escape", esc));
        } else {
            hi
        };
        char::from_u32(code).ok_or_else(|| self.error("invalid \\u escape", esc))
    }

    fn hex4(&mut self, esc: usize) -> Result<u32, ParseError> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| self.error("invalid \\u escape", esc))?;
        self.pos += 4;
        Ok(u32::from_str_radix(digits, 16).expect("validated hex digits"))
    }

    fn span_at(&self, at: usize) -> Span {
        let len = self.text[at.min(self.text.len())..]
            .chars()
            .next()
            .map_or(0, char::len_utf8);
        Span::new(self.source, at, at + len)
    }

    fn error(&self, message: &str, at: usize) -> ParseError {
        ParseError {
            code: "TG0102",
            message: format!("invalid JSON: {message}"),
            pointer: self.pointer.clone(),
            span: Some(self.span_at(at)),
        }
    }
}
