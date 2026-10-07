// SPDX-License-Identifier: AGPL-3.0-only
//! Span-aware YAML → JSON conversion.
//!
//! The manifest is read through yaml-rust2's event parser so every node's
//! position is known. The result is a `serde_json::Value` plus a map from
//! JSON Pointer to the byte offset where the node starts (for a mapping
//! entry, where its key starts).
//!
//! Accepted YAML is deliberately narrower than the full language:
//! - exactly one document (an empty file is `null`);
//! - anchors are allowed but aliases are refused, so a manifest cannot
//!   expand exponentially and every node has exactly one location;
//! - mapping keys must be scalars and unique; a key is used as written;
//! - plain scalars resolve with the YAML 1.2 core schema (null, booleans,
//!   integers, floats); quoted and block scalars are strings;
//! - only the core tags (`!!str`, `!!int`, `!!float`, `!!bool`, `!!null`,
//!   `!!map`, `!!seq`) and the non-specific `!` are accepted;
//! - nesting is limited to [`MAX_DEPTH`] levels.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};
use yaml_rust2::Yaml;
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use crate::pointer;

/// Deepest accepted nesting of mappings and sequences.
pub(crate) const MAX_DEPTH: usize = 64;

const CORE_TAG_HANDLE: &str = "tag:yaml.org,2002:";

/// A converted document: the JSON value and the start offset of each node.
#[derive(Debug)]
pub(crate) struct Converted {
    pub value: Value,
    pub positions: BTreeMap<String, u32>,
}

/// A YAML problem: what went wrong, the byte offset where it was detected
/// and the pointer of the innermost node being read at that point.
#[derive(Debug)]
pub(crate) struct YamlError {
    pub message: String,
    pub offset: u32,
    pub pointer: String,
    /// Positions recorded before the error, so the caller can still map
    /// pointers of the partial document.
    pub positions: BTreeMap<String, u32>,
}

pub(crate) fn to_json(text: &str) -> Result<Converted, Box<YamlError>> {
    let mut reader = Reader {
        parser: Parser::new_from_str(text),
        offsets: Offsets::new(text),
        positions: BTreeMap::new(),
    };
    match reader.document() {
        Ok(value) => Ok(Converted {
            value,
            positions: reader.positions,
        }),
        Err(mut err) => {
            err.positions = std::mem::take(&mut reader.positions);
            Err(err)
        }
    }
}

struct Reader<'a> {
    parser: Parser<std::str::Chars<'a>>,
    offsets: Offsets<'a>,
    positions: BTreeMap<String, u32>,
}

impl Reader<'_> {
    fn next(&mut self, pointer: &str) -> Result<(Event, u32), Box<YamlError>> {
        match self.parser.next_token() {
            Ok((event, mark)) => Ok((event, self.offsets.of(&mark))),
            Err(err) => {
                let offset = self.offsets.of(err.marker());
                Err(self.error(err.info().to_string(), offset, pointer))
            }
        }
    }

    fn error(&self, message: String, offset: u32, pointer: &str) -> Box<YamlError> {
        Box::new(YamlError {
            message,
            offset,
            pointer: pointer.to_string(),
            positions: BTreeMap::new(),
        })
    }

    /// The whole stream: zero or one document.
    fn document(&mut self) -> Result<Value, Box<YamlError>> {
        let mut value = Value::Null;
        let mut seen_document = false;
        loop {
            let (event, offset) = self.next("")?;
            match event {
                Event::StreamStart | Event::DocumentEnd | Event::Nothing => {}
                Event::StreamEnd => return Ok(value),
                Event::DocumentStart if seen_document => {
                    return Err(self.error(
                        "the manifest must be a single YAML document".into(),
                        offset,
                        "",
                    ));
                }
                Event::DocumentStart => {
                    seen_document = true;
                    value = self.node("", 0)?;
                }
                other => {
                    return Err(self.error(format!("unexpected YAML event {other:?}"), offset, ""));
                }
            }
        }
    }

    /// One node at `pointer`. Records its position unless the caller (a
    /// mapping entry) already recorded the key's position.
    fn node(&mut self, pointer: &str, depth: usize) -> Result<Value, Box<YamlError>> {
        let (event, offset) = self.next(pointer)?;
        self.positions.entry(pointer.to_string()).or_insert(offset);
        match event {
            Event::Scalar(text, style, _, tag) => {
                scalar(text, style, tag.as_ref()).map_err(|m| self.error(m, offset, pointer))
            }
            Event::SequenceStart(_, tag) => {
                self.check_depth(depth, offset, pointer)?;
                self.collection_tag(tag.as_ref(), "seq", offset, pointer)?;
                self.sequence(pointer, depth)
            }
            Event::MappingStart(_, tag) => {
                self.check_depth(depth, offset, pointer)?;
                self.collection_tag(tag.as_ref(), "map", offset, pointer)?;
                self.mapping(pointer, depth)
            }
            Event::Alias(_) => Err(self.error(
                "YAML aliases are not supported; repeat the value instead".into(),
                offset,
                pointer,
            )),
            other => Err(self.error(format!("unexpected YAML event {other:?}"), offset, pointer)),
        }
    }

    fn check_depth(&self, depth: usize, offset: u32, pointer: &str) -> Result<(), Box<YamlError>> {
        if depth >= MAX_DEPTH {
            return Err(self.error(
                format!("nesting deeper than {MAX_DEPTH} levels"),
                offset,
                pointer,
            ));
        }
        Ok(())
    }

    fn collection_tag(
        &self,
        tag: Option<&Tag>,
        core: &str,
        offset: u32,
        pointer: &str,
    ) -> Result<(), Box<YamlError>> {
        match tag {
            None => Ok(()),
            Some(t) if t.handle == CORE_TAG_HANDLE && t.suffix == core => Ok(()),
            Some(t) => Err(self.error(
                format!("unsupported YAML tag {}", tag_name(t)),
                offset,
                pointer,
            )),
        }
    }

    fn sequence(&mut self, pointer: &str, depth: usize) -> Result<Value, Box<YamlError>> {
        let mut items = vec![];
        loop {
            match self.parser.peek() {
                Ok((Event::SequenceEnd, _)) => {
                    self.next(pointer)?;
                    return Ok(Value::Array(items));
                }
                Ok(_) => {}
                Err(err) => {
                    let offset = self.offsets.of(err.marker());
                    return Err(self.error(err.info().to_string(), offset, pointer));
                }
            }
            let child = pointer::child(pointer, &items.len().to_string());
            items.push(self.node(&child, depth + 1)?);
        }
    }

    fn mapping(&mut self, pointer: &str, depth: usize) -> Result<Value, Box<YamlError>> {
        let mut map = Map::new();
        loop {
            let (event, offset) = self.next(pointer)?;
            let key = match event {
                Event::MappingEnd => return Ok(Value::Object(map)),
                Event::Scalar(key, ..) => key,
                Event::Alias(_) => {
                    return Err(self.error(
                        "YAML aliases are not supported; repeat the value instead".into(),
                        offset,
                        pointer,
                    ));
                }
                _ => {
                    return Err(self.error(
                        "mapping keys must be plain strings".into(),
                        offset,
                        pointer,
                    ));
                }
            };
            // A block mapping's start event is reported after its first
            // key; the first key is the better location for the mapping.
            if let Some(start) = self.positions.get_mut(pointer) {
                *start = (*start).min(offset);
            }
            let child = pointer::child(pointer, &key);
            if map.contains_key(&key) {
                return Err(self.error(format!("duplicate key `{key}`"), offset, &child));
            }
            self.positions.insert(child.clone(), offset);
            let value = self.node(&child, depth + 1)?;
            map.insert(key, value);
        }
    }
}

/// Resolve a scalar to JSON.
fn scalar(text: String, style: TScalarStyle, tag: Option<&Tag>) -> Result<Value, String> {
    match tag {
        None if style == TScalarStyle::Plain => Ok(resolve_plain(&text)),
        None => Ok(Value::String(text)),
        // The non-specific tag `!` forces a string.
        Some(t) if t.handle.is_empty() && t.suffix == "!" => Ok(Value::String(text)),
        Some(t) if t.handle == CORE_TAG_HANDLE => {
            let resolved = resolve_plain(&text);
            let ok = match t.suffix.as_str() {
                "str" => return Ok(Value::String(text)),
                "null" => resolved.is_null(),
                "bool" => resolved.is_boolean(),
                "int" => resolved.is_i64() || resolved.is_u64(),
                "float" => resolved.is_number(),
                _ => return Err(format!("unsupported YAML tag {}", tag_name(t))),
            };
            if ok {
                Ok(resolved)
            } else {
                Err(format!("`{text}` is not a valid {}", tag_name(t)))
            }
        }
        Some(t) => Err(format!("unsupported YAML tag {}", tag_name(t))),
    }
}

/// YAML 1.2 core schema resolution of a plain scalar.
fn resolve_plain(text: &str) -> Value {
    if matches!(text, "Null" | "NULL") {
        return Value::Null;
    }
    match Yaml::from_str(text) {
        Yaml::Null => Value::Null,
        Yaml::Boolean(b) => Value::Bool(b),
        Yaml::Integer(i) => Value::from(i),
        Yaml::Real(r) => r
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map_or_else(|| Value::String(text.to_string()), Value::Number),
        _ => Value::String(text.to_string()),
    }
}

fn tag_name(tag: &Tag) -> String {
    match tag.handle.as_str() {
        CORE_TAG_HANDLE => format!("!!{}", tag.suffix),
        handle => format!("{handle}{}", tag.suffix),
    }
}

/// Converts yaml-rust2 markers (1-based line, 0-based column counted in
/// characters) to byte offsets.
struct Offsets<'a> {
    text: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> Offsets<'a> {
    fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, line_starts }
    }

    fn of(&self, mark: &Marker) -> u32 {
        let Some(&start) = self.line_starts.get(mark.line().saturating_sub(1)) else {
            return self.text.len() as u32;
        };
        let line = &self.text[start..];
        let within = line
            .char_indices()
            .nth(mark.col())
            .map_or(line.len(), |(i, _)| i);
        (start + within) as u32
    }
}
