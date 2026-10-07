// SPDX-License-Identifier: AGPL-3.0-only
//! YAML → `serde_json::Value` with spans, on top of the yaml-rust2 event
//! parser.
//!
//! - Scalars resolve with the YAML 1.2 core schema (`on`/`yes` stay
//!   strings); `.inf`/`.nan` stay strings because JSON cannot hold them.
//! - Anchors and aliases are expanded under a node budget and a byte
//!   budget (string, key and number text), so a "billion laughs" document,
//!   or one that aliases a huge scalar many times, fails fast with TG0105
//!   instead of exhausting memory; expanded nodes take the span of the
//!   alias that produced them.
//!   Merge keys (`<<`) are applied with explicit keys taking
//!   precedence.
//! - Mapping keys must be scalars; duplicate keys and multi-document streams
//!   are errors (TG0102).
//! - yaml-rust2 markers count characters; they are converted to byte offsets
//!   so spans agree with [`tungsten_core::SourceFile`].
//! - A leading UTF-8 byte order mark is skipped (as JSON does), and spans
//!   still count it.

use std::collections::HashMap;

use serde_json::{Map, Number, Value};
use tungsten_core::{SourceId, Span};
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::{Marker, ScanError, TScalarStyle};

use crate::escape_token;
use crate::parse::{ParseError, Parsed};
use crate::spans::SpanIndex;

/// Maximum number of nodes alias expansion may add to one document.
pub(crate) const MAX_ALIAS_NODES: usize = 1_000_000;

/// Maximum bytes of text (strings, keys, numbers) alias expansion may add
/// to one document: twice the default input size limit.
pub(crate) const MAX_ALIAS_BYTES: usize = 64 * 1024 * 1024;

const CORE_TAG: &str = "tag:yaml.org,2002:";

pub(crate) fn parse_yaml(
    text: &str,
    source: SourceId,
    max_depth: usize,
) -> Result<Parsed, ParseError> {
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut offsets = Offsets::new(text);
    offsets.char_shift = usize::from(body.len() != text.len());
    YamlBuilder {
        text,
        source,
        max_depth,
        offsets,
        spans: SpanIndex::default(),
        pointer: String::new(),
        stack: vec![],
        anchors: HashMap::new(),
        alias_nodes: 0,
        alias_bytes: 0,
    }
    .run(&mut Parser::new_from_str(body))
}

/// Character index → byte offset, with a moving cursor (markers are mostly
/// increasing).
struct Offsets<'a> {
    text: &'a str,
    ascii: bool,
    chars: usize,
    bytes: usize,
    /// Characters of `text` before the parsed body (a byte order mark).
    char_shift: usize,
}

impl<'a> Offsets<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            ascii: text.is_ascii(),
            chars: 0,
            bytes: 0,
            char_shift: 0,
        }
    }

    fn byte(&mut self, char_index: usize) -> usize {
        let char_index = char_index.saturating_add(self.char_shift);
        if self.ascii {
            return char_index.min(self.text.len());
        }
        while self.chars < char_index {
            let Some(c) = self.text[self.bytes..].chars().next() else {
                break;
            };
            self.bytes += c.len_utf8();
            self.chars += 1;
        }
        while self.chars > char_index {
            let c = self.text[..self.bytes]
                .chars()
                .next_back()
                .expect("cursor is past the start");
            self.bytes -= c.len_utf8();
            self.chars -= 1;
        }
        self.bytes
    }
}

/// A node definition remembered for aliases, with its node count, text
/// size and depth.
struct Anchor {
    value: Value,
    nodes: usize,
    bytes: usize,
    depth: usize,
}

/// Number of nodes in `value`, bytes of its text (strings, keys, numbers)
/// and its nesting depth (a scalar has depth 0).
fn measure(value: &Value) -> (usize, usize, usize) {
    let (mut nodes, mut bytes, mut depth) = (0, 0, 0);
    let mut stack = vec![(value, 0)];
    while let Some((v, d)) = stack.pop() {
        nodes += 1;
        depth = depth.max(d);
        match v {
            Value::Object(m) => {
                bytes += m.keys().map(String::len).sum::<usize>();
                stack.extend(m.values().map(|c| (c, d + 1)));
            }
            Value::Array(a) => stack.extend(a.iter().map(|c| (c, d + 1))),
            Value::String(s) => bytes += s.len(),
            Value::Number(n) => bytes += n.to_string().len(),
            Value::Bool(_) | Value::Null => bytes += 1,
        }
    }
    (nodes, bytes, depth)
}

enum Frame {
    Mapping {
        map: Map<String, Value>,
        start: usize,
        end: usize,
        base: usize,
        anchor: usize,
        /// The pending key, and whether it is a plain `<<` merge key.
        key: Option<(String, bool)>,
        /// Values of `<<` keys with their relative spans, in order.
        merges: Vec<(Value, Vec<(String, Span)>)>,
    },
    Sequence {
        items: Vec<Value>,
        start: usize,
        end: usize,
        base: usize,
        anchor: usize,
    },
}

impl Frame {
    /// Widen the frame's extent to include a child node.
    fn include(&mut self, child_start: usize, child_end: usize) {
        match self {
            Frame::Mapping { start, end, .. } | Frame::Sequence { start, end, .. } => {
                *start = (*start).min(child_start);
                *end = (*end).max(child_end);
            }
        }
    }
}

struct YamlBuilder<'a> {
    text: &'a str,
    source: SourceId,
    max_depth: usize,
    offsets: Offsets<'a>,
    spans: SpanIndex,
    pointer: String,
    stack: Vec<Frame>,
    anchors: HashMap<usize, Anchor>,
    alias_nodes: usize,
    alias_bytes: usize,
}

impl YamlBuilder<'_> {
    fn run(mut self, parser: &mut Parser<std::str::Chars<'_>>) -> Result<Parsed, ParseError> {
        let mut root: Option<Value> = None;
        let mut documents = 0;
        loop {
            let (event, mark) = parser.next_token().map_err(|e| self.scan_error(&e))?;
            let at = self.offsets.byte(mark.index());
            let completed = match event {
                Event::StreamEnd => break,
                Event::StreamStart | Event::DocumentEnd | Event::Nothing => continue,
                Event::DocumentStart => {
                    documents += 1;
                    if documents > 1 {
                        return Err(self.error(
                            "TG0102",
                            "multiple YAML documents in one file are not supported",
                            at,
                        ));
                    }
                    continue;
                }
                Event::Scalar(value, style, anchor, tag) => {
                    if self.take_key(&value, style, at)? {
                        self.remember_key(anchor, value);
                        continue;
                    }
                    let end = self.scalar_end(parser, &value, style, at);
                    let v = resolve_scalar(value, style, tag.as_ref());
                    self.spans
                        .insert(self.pointer.clone(), Span::new(self.source, at, end));
                    self.remember(anchor, &v);
                    (v, end)
                }
                Event::Alias(id) => {
                    self.reject_complex_key(at)?;
                    self.expand_alias(id, at)?
                }
                Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _) => {
                    self.reject_complex_key(at)?;
                    if self.stack.len() + 1 > self.max_depth {
                        return Err(self.depth_error(at));
                    }
                    let base = self.pointer.len();
                    self.stack
                        .push(if matches!(event, Event::MappingStart(..)) {
                            Frame::Mapping {
                                map: Map::new(),
                                start: at,
                                end: at,
                                base,
                                anchor,
                                key: None,
                                merges: vec![],
                            }
                        } else {
                            Frame::Sequence {
                                items: vec![],
                                start: at,
                                end: at,
                                base,
                                anchor,
                            }
                        });
                    self.enter_next_item();
                    continue;
                }
                Event::MappingEnd | Event::SequenceEnd => self.close(at)?,
            };
            if let Some(v) = self.attach(completed) {
                root = Some(v);
            }
        }
        let value = root.unwrap_or(Value::Null);
        if self.spans.get("").is_none() {
            self.spans
                .insert(String::new(), Span::new(self.source, 0, 0));
        }
        Ok(Parsed {
            value,
            spans: self.spans,
        })
    }

    /// When the innermost frame is a mapping waiting for a key, consume this
    /// scalar as the key and return true.
    fn take_key(
        &mut self,
        value: &str,
        style: TScalarStyle,
        at: usize,
    ) -> Result<bool, ParseError> {
        let Some(Frame::Mapping {
            map,
            key,
            base,
            start,
            ..
        }) = self.stack.last_mut()
        else {
            return Ok(false);
        };
        if key.is_some() {
            return Ok(false);
        }
        *start = (*start).min(at);
        let merge = value == "<<" && style == TScalarStyle::Plain;
        if !merge && map.contains_key(value) {
            let pointer = format!("{}/{}", &self.pointer[..*base], escape_token(value));
            return Err(ParseError {
                code: "TG0102",
                message: format!("invalid YAML: duplicate key \"{value}\""),
                pointer,
                span: Some(self.span_at(at)),
            });
        }
        let base = *base;
        *key = Some((value.to_string(), merge));
        self.pointer.truncate(base);
        self.pointer.push('/');
        self.pointer.push_str(&escape_token(value));
        Ok(true)
    }

    fn reject_complex_key(&self, at: usize) -> Result<(), ParseError> {
        if let Some(Frame::Mapping { key: None, .. }) = self.stack.last() {
            return Err(self.error("TG0102", "invalid YAML: mapping keys must be scalars", at));
        }
        Ok(())
    }

    /// Point `self.pointer` at the next element when the innermost frame is
    /// a sequence (mappings set it when their key arrives).
    fn enter_next_item(&mut self) {
        if let Some(Frame::Sequence { items, base, .. }) = self.stack.last() {
            let (base, next) = (*base, items.len());
            self.pointer.truncate(base);
            self.pointer.push_str(&format!("/{next}"));
        }
    }

    /// Byte offset just past a scalar starting at `at`.
    fn scalar_end(
        &mut self,
        parser: &mut Parser<std::str::Chars<'_>>,
        value: &str,
        style: TScalarStyle,
        at: usize,
    ) -> usize {
        let rest = &self.text[at..];
        let line_end = rest.find('\n').map_or(self.text.len(), |i| at + i);
        match style {
            TScalarStyle::Plain if rest.starts_with(value) => at + value.len(),
            TScalarStyle::Plain => trim_end(self.text, at, line_end),
            TScalarStyle::SingleQuoted => {
                let bytes = rest.as_bytes();
                let mut i = 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        return at + i + 1;
                    }
                    i += 1;
                }
                self.text.len()
            }
            TScalarStyle::DoubleQuoted => {
                let bytes = rest.as_bytes();
                let mut i = 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => return at + i + 1,
                        _ => i += 1,
                    }
                }
                self.text.len()
            }
            TScalarStyle::Literal | TScalarStyle::Folded => {
                let next = match parser.peek() {
                    Ok((_, m)) => m.index(),
                    Err(_) => usize::MAX,
                };
                let next = self.offsets.byte(next).max(at);
                trim_end(self.text, at, next)
            }
        }
    }

    fn remember(&mut self, anchor: usize, value: &Value) {
        if anchor == 0 {
            return;
        }
        let (nodes, bytes, depth) = measure(value);
        self.anchors.insert(
            anchor,
            Anchor {
                value: value.clone(),
                nodes,
                bytes,
                depth,
            },
        );
    }

    /// An anchored mapping key can be aliased as a plain string value.
    fn remember_key(&mut self, anchor: usize, key: String) {
        if anchor != 0 {
            self.anchors.insert(
                anchor,
                Anchor {
                    nodes: 1,
                    bytes: key.len(),
                    depth: 0,
                    value: Value::String(key),
                },
            );
        }
    }

    fn expand_alias(&mut self, id: usize, at: usize) -> Result<(Value, usize), ParseError> {
        let Some(anchor) = self.anchors.get(&id) else {
            return Err(self.error("TG0102", "invalid YAML: unknown alias", at));
        };
        self.alias_nodes += anchor.nodes;
        self.alias_bytes += anchor.bytes;
        if self.alias_nodes > MAX_ALIAS_NODES {
            return Err(self.error(
                "TG0105",
                &format!("YAML alias expansion exceeds the limit of {MAX_ALIAS_NODES} nodes"),
                at,
            ));
        }
        if self.alias_bytes > MAX_ALIAS_BYTES {
            return Err(self.error(
                "TG0105",
                &format!("YAML alias expansion exceeds the limit of {MAX_ALIAS_BYTES} bytes"),
                at,
            ));
        }
        if self.stack.len() + anchor.depth > self.max_depth {
            return Err(self.depth_error(at));
        }
        let value = anchor.value.clone();
        let name_len = self.text[at..]
            .char_indices()
            .skip(1)
            .find(|(_, c)| c.is_whitespace() || matches!(c, ',' | ']' | '}'))
            .map_or(self.text.len() - at, |(i, _)| i);
        // Nodes inside the expansion have no spans of their own; lookups fall
        // back to the alias site.
        let end = at + name_len;
        self.spans
            .insert(self.pointer.clone(), Span::new(self.source, at, end));
        Ok((value, end))
    }

    /// Close the innermost container at marker offset `at`.
    fn close(&mut self, at: usize) -> Result<(Value, usize), ParseError> {
        let frame = self
            .stack
            .pop()
            .expect("parser balances start and end events");
        let (value, start, end, anchor) = match frame {
            Frame::Mapping {
                mut map,
                start,
                end,
                base,
                anchor,
                merges,
                ..
            } => {
                self.pointer.truncate(base);
                self.apply_merges(&mut map, merges, at)?;
                (Value::Object(map), start, end, anchor)
            }
            Frame::Sequence {
                items,
                start,
                end,
                base,
                anchor,
            } => {
                self.pointer.truncate(base);
                (Value::Array(items), start, end, anchor)
            }
        };
        // Flow collections end at their closing bracket; block collections
        // end with their last child.
        let end = match self.text.as_bytes().get(at) {
            Some(b'}' | b']') => at + 1,
            _ => end.max(start),
        };
        self.spans
            .insert(self.pointer.clone(), Span::new(self.source, start, end));
        self.remember(anchor, &value);
        Ok((value, end))
    }

    fn apply_merges(
        &mut self,
        map: &mut Map<String, Value>,
        merges: Vec<(Value, Vec<(String, Span)>)>,
        at: usize,
    ) -> Result<(), ParseError> {
        for (value, spans) in merges {
            let sources: Vec<(String, Map<String, Value>)> = match value {
                Value::Object(m) => vec![(String::new(), m)],
                Value::Array(items) => items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| match v {
                        Value::Object(m) => Ok((format!("/{i}"), m)),
                        _ => Err(()),
                    })
                    .collect::<Result<_, _>>()
                    .map_err(|()| self.merge_error(at))?,
                _ => return Err(self.merge_error(at)),
            };
            for (prefix, source) in sources {
                for (k, v) in source {
                    if map.contains_key(&k) {
                        continue;
                    }
                    let member = format!("{prefix}/{}", escape_token(&k));
                    let target = format!("{}/{}", self.pointer, escape_token(&k));
                    for (suffix, span) in &spans {
                        if let Some(rest) = suffix.strip_prefix(&member)
                            && (rest.is_empty() || rest.starts_with('/'))
                        {
                            self.spans.insert(format!("{target}{rest}"), *span);
                        }
                    }
                    map.insert(k, v);
                }
            }
        }
        Ok(())
    }

    /// Attach a completed node to its parent. Returns the root when the
    /// node has no parent.
    fn attach(&mut self, (value, end): (Value, usize)) -> Option<Value> {
        let child = self.spans.get(&self.pointer);
        let Some(frame) = self.stack.last_mut() else {
            return Some(value);
        };
        if let Some(child) = child {
            frame.include(child.start as usize, end);
        }
        match frame {
            Frame::Mapping {
                map, key, merges, ..
            } => {
                let (k, merge) = key.take().expect("a value follows its key");
                if merge {
                    let spans = self.spans.take_subtree(&self.pointer);
                    merges.push((value, spans));
                } else {
                    map.insert(k, value);
                }
            }
            Frame::Sequence { items, .. } => {
                items.push(value);
            }
        }
        self.enter_next_item();
        None
    }

    fn span_at(&self, at: usize) -> Span {
        let at = at.min(self.text.len());
        let len = self.text[at..].chars().next().map_or(0, char::len_utf8);
        Span::new(self.source, at, at + len)
    }

    fn error(&self, code: &'static str, message: &str, at: usize) -> ParseError {
        ParseError {
            code,
            message: message.to_string(),
            pointer: self.pointer.clone(),
            span: Some(self.span_at(at)),
        }
    }

    fn depth_error(&self, at: usize) -> ParseError {
        self.error(
            "TG0105",
            &format!("nesting depth exceeds the limit of {}", self.max_depth),
            at,
        )
    }

    fn merge_error(&self, at: usize) -> ParseError {
        self.error(
            "TG0102",
            "invalid YAML: a merge key (<<) value must be a mapping or a sequence of mappings",
            at,
        )
    }

    fn scan_error(&mut self, e: &ScanError) -> ParseError {
        let m: &Marker = e.marker();
        let at = self.offsets.byte(m.index());
        // yaml-rust2 bounds flow nesting itself; report that as the limit it is.
        if e.info().contains("recursion limit") {
            return self.error(
                "TG0105",
                &format!(
                    "nesting depth exceeds the YAML parser's limit ({})",
                    e.info()
                ),
                at,
            );
        }
        self.error("TG0102", &format!("invalid YAML: {}", e.info()), at)
    }
}

fn trim_end(text: &str, start: usize, end: usize) -> usize {
    let end = end.min(text.len()).max(start);
    start + text[start..end].trim_end().len()
}

/// Resolve a scalar with the YAML 1.2 core schema, honoring explicit core
/// tags (`!!str`, `!!int`, `!!float`, `!!bool`, `!!null`).
fn resolve_scalar(value: String, style: TScalarStyle, tag: Option<&Tag>) -> Value {
    let core = tag
        .filter(|t| t.handle == CORE_TAG || t.handle == "!!")
        .map(|t| t.suffix.as_str());
    match core {
        Some("str") | Some("binary") | Some("timestamp") => return Value::String(value),
        Some("null") => return Value::Null,
        Some("bool") => return parse_bool(&value).map_or(Value::String(value), Value::Bool),
        Some("int") | Some("float") => {
            return parse_number(&value).map_or(Value::String(value), Value::Number);
        }
        _ => {}
    }
    if style != TScalarStyle::Plain || tag.is_some() {
        return Value::String(value);
    }
    if matches!(value.as_str(), "" | "~" | "null" | "Null" | "NULL") {
        return Value::Null;
    }
    if let Some(b) = parse_bool(&value) {
        return Value::Bool(b);
    }
    parse_number(&value).map_or(Value::String(value), Value::Number)
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// Core-schema integers (decimal, `0o`, `0x`) and floats. Infinities and
/// NaN are not representable in JSON and yield `None`.
fn parse_number(s: &str) -> Option<Number> {
    if let Some(oct) = s.strip_prefix("0o") {
        return u64::from_str_radix(oct, 8).ok().map(Number::from);
    }
    if let Some(hex) = s.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).ok().map(Number::from);
    }
    let unsigned = s.strip_prefix(['-', '+']).unwrap_or(s);
    if !unsigned.is_empty() && unsigned.bytes().all(|b| b.is_ascii_digit()) {
        let digits = s.strip_prefix('+').unwrap_or(s);
        if let Ok(i) = digits.parse::<i64>() {
            return Some(Number::from(i));
        }
        if let Ok(u) = digits.parse::<u64>() {
            return Some(Number::from(u));
        }
        return digits.parse::<f64>().ok().and_then(Number::from_f64);
    }
    if is_core_float(unsigned) {
        return s.parse::<f64>().ok().and_then(Number::from_f64);
    }
    None
}

/// `( \.[0-9]+ | [0-9]+ ( \.[0-9]* )? ) ( [eE] [-+]? [0-9]+ )?`
fn is_core_float(s: &str) -> bool {
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa, None),
    };
    let all_digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = match frac {
        Some(f) => all_digits(int) && all_digits(f) && !(int.is_empty() && f.is_empty()),
        None => !int.is_empty() && all_digits(int),
    };
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && all_digits(e)
    });
    mantissa_ok && exponent_ok
}
