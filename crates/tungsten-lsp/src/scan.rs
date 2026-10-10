// SPDX-License-Identifier: AGPL-3.0-only
//! A forgiving line scanner for block-style YAML.
//!
//! Editors send buffers that do not parse: a key being typed, a half
//! written list. The frontend rejects those as a whole, so completion and
//! hover work from this scanner instead. It recovers, for every line, the
//! path of keys and sequence items that encloses it, and understands
//! one-line flow collections (`{ a: 1, b: [x, y] }`). It does not validate
//! anything: diagnostics come only from the compiler.

use crate::position::LineIndex;

/// Marks the items of a flow sequence, which are not told apart.
pub const FLOW_ITEM: u32 = u32::MAX;

/// One step of a path into a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    /// A sequence item; the number tells the items of a document apart.
    Item(u32),
}

/// The shape of a path without item numbers: `tools[].safety`.
pub fn shape(path: &[Seg]) -> String {
    let mut out = String::new();
    for seg in path {
        match seg {
            Seg::Key(k) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(k);
            }
            Seg::Item(_) => out.push_str("[]"),
        }
    }
    out
}

/// A key, a scalar sequence item or a flow key found by [`scan`].
#[derive(Debug, Clone)]
pub struct Entry {
    pub line: usize,
    /// Path including this entry's own key (and its item segment).
    pub path: Vec<Seg>,
    pub key: Option<String>,
    /// Absolute byte range of the key text, without quotes.
    pub key_range: Option<(usize, usize)>,
    /// Absolute byte range of the value text after the colon (or of a
    /// scalar item), trimmed and without a trailing comment.
    pub value_range: Option<(usize, usize)>,
    /// Column of the first character of the line's content (the dash of an
    /// item).
    pub indent: usize,
    pub item: bool,
    pub flow: bool,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub entries: Vec<Entry>,
}

impl Scan {
    /// The entry whose key contains `offset`, flow keys included.
    pub fn key_at(&self, offset: usize) -> Option<&Entry> {
        self.entries.iter().find(|e| {
            e.key_range
                .is_some_and(|(s, end)| s <= offset && offset <= end)
        })
    }

    /// The entry whose value contains `offset`.
    pub fn value_at(&self, offset: usize) -> Option<&Entry> {
        self.entries
            .iter()
            .filter(|e| {
                e.value_range
                    .is_some_and(|(s, end)| s <= offset && offset <= end)
            })
            .min_by_key(|e| e.value_range.map_or(usize::MAX, |(s, end)| end - s))
    }

    /// Names of the keys directly below `path`, from block lines and flow
    /// maps alike.
    pub fn children(&self, path: &[Seg]) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| e.path.len() == path.len() + 1 && e.path.starts_with(path))
            .filter_map(|e| e.key.as_deref())
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stack {
    frames: Vec<(usize, Seg)>,
    next_item: u32,
    /// Lines deeper than this column are the body of a block scalar.
    block: Option<usize>,
}

impl Stack {
    /// The path enclosing a line that starts at `col` (the dash column for
    /// an item), without changing the stack.
    pub fn parent_for(&self, col: usize, item: bool) -> Vec<Seg> {
        let mut copy = self.clone();
        copy.pop_for(col, item);
        copy.frames.iter().map(|(_, s)| s.clone()).collect()
    }

    fn pop_for(&mut self, col: usize, item: bool) {
        while let Some((c, seg)) = self.frames.last() {
            let pop = if item {
                *c > col || (*c == col && matches!(seg, Seg::Item(_)))
            } else {
                *c >= col
            };
            if !pop {
                break;
            }
            self.frames.pop();
        }
    }

    fn path(&self) -> Vec<Seg> {
        self.frames.iter().map(|(_, s)| s.clone()).collect()
    }
}

/// What the cursor line looks like up to the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Key,
    Value,
}

/// The completion context at a cursor.
#[derive(Debug, Clone)]
pub struct Context {
    pub slot: Slot,
    /// Key slot: the mapping the key belongs to. Value slot: the value's
    /// own path (`tools[].safety`).
    pub path: Vec<Seg>,
    /// The cursor starts a sequence item (`- |`): the item may be a mapping
    /// (key slot) or a scalar (value slot) — the caller decides by schema.
    pub item: bool,
    /// Absolute byte range of the word under the cursor.
    pub word: (usize, usize),
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '$' | '*' | '/')
}

/// Scan a whole text.
pub fn scan(text: &str, index: &LineIndex) -> Scan {
    run(text, index, None).0
}

fn run(text: &str, index: &LineIndex, stop: Option<usize>) -> (Scan, Stack) {
    let mut scan = Scan::default();
    let mut stack = Stack::default();
    let lines = stop.unwrap_or_else(|| index.lines());
    for line in 0..lines {
        let (start, end) = index.line_range(line, text);
        feed(&mut scan, &mut stack, line, start, &text[start..end]);
    }
    (scan, stack)
}

/// Split `s` into its leading columns, the dash of a sequence item and the
/// rest: `(indent, dash, content_col)`.
fn leading(s: &str) -> (usize, bool, usize) {
    let indent = s.len() - s.trim_start_matches(' ').len();
    let rest = &s[indent..];
    if rest == "-" || rest.starts_with("- ") {
        let after = &rest[1..];
        let gap = after.len() - after.trim_start_matches(' ').len();
        (indent, true, indent + 1 + gap)
    } else {
        (indent, false, indent)
    }
}

/// The byte index of the `:` that ends a key in `s`, when `s` starts with
/// one. Returns `(key_start, key_end, colon)`.
fn split_key(s: &str) -> Option<(usize, usize, usize)> {
    let first = s.chars().next()?;
    if matches!(first, '{' | '[' | '#' | '|' | '>' | '&' | '*' | '!') {
        return None;
    }
    if first == '"' || first == '\'' {
        let close = s[1..].find(first)? + 1;
        let after = &s[close + 1..];
        let trimmed = after.trim_start_matches(' ');
        return trimmed
            .starts_with(':')
            .then(|| (1, close, close + 1 + (after.len() - trimmed.len())));
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            let key_end = s[..i].trim_end().len();
            return (key_end > 0).then_some((0, key_end, i));
        }
    }
    None
}

/// `s` without a trailing ` # comment` (quotes respected).
fn strip_comment(s: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if (c == '"' || c == '\'') && prev == ' ' {
                    quote = Some(c);
                } else if c == '#' && prev == ' ' {
                    return s[..i].trim_end();
                }
            }
        }
        prev = c;
    }
    s.trim_end()
}

fn feed(scan: &mut Scan, stack: &mut Stack, line: usize, base: usize, s: &str) {
    let (indent, item, content) = leading(s);
    let trimmed = &s[indent..];
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return;
    }
    if let Some(block) = stack.block {
        if indent > block {
            return;
        }
        stack.block = None;
    }
    if indent == 0 && (s.starts_with("---") || s.starts_with("...")) {
        stack.frames.clear();
        return;
    }
    stack.pop_for(indent, item);
    if item {
        stack.next_item += 1;
        stack.frames.push((indent, Seg::Item(stack.next_item)));
    }
    let body = strip_comment(&s[content..]);
    let mut entry = Entry {
        line,
        path: vec![],
        key: None,
        key_range: None,
        value_range: None,
        indent,
        item,
        flow: false,
    };
    let value_text;
    let value_col;
    match split_key(body) {
        Some((ks, ke, colon)) => {
            let key = body[ks..ke].to_string();
            stack.frames.push((content, Seg::Key(key.clone())));
            entry.key_range = Some((base + content + ks, base + content + ke));
            entry.key = Some(key);
            let after = &body[colon + 1..];
            let gap = after.len() - after.trim_start_matches(' ').len();
            value_text = after.trim_start_matches(' ');
            value_col = content + colon + 1 + gap;
        }
        None => {
            value_text = body;
            value_col = content;
        }
    }
    entry.path = stack.path();
    if !value_text.is_empty() {
        entry.value_range = Some((base + value_col, base + value_col + value_text.len()));
        if value_text.starts_with(['|', '>']) {
            stack.block = Some(indent);
        }
    }
    let flow_path = entry.path.clone();
    let value_at = base + value_col;
    scan.entries.push(entry);
    if value_text.starts_with(['{', '[']) {
        let (keys, _) = flow_walk(value_text, true);
        for k in keys {
            let mut path = flow_path.clone();
            path.extend(k.rel.iter().cloned());
            scan.entries.push(Entry {
                line,
                path,
                key: k.key,
                key_range: Some((value_at + k.start, value_at + k.end)),
                value_range: k.value.map(|(s, e)| (value_at + s, value_at + e)),
                indent,
                item: false,
                flow: true,
            });
        }
    }
}

/// A key found inside a flow collection; offsets are relative to the text
/// given to [`flow_walk`].
#[derive(Debug, Clone)]
pub struct FlowKey {
    /// Path from the flow root, including the key.
    pub rel: Vec<Seg>,
    pub key: Option<String>,
    pub start: usize,
    pub end: usize,
    pub value: Option<(usize, usize)>,
}

/// Where a flow walk ended.
#[derive(Debug, Clone)]
pub struct FlowEnd {
    pub slot: Slot,
    /// Key slot: the map's path from the flow root. Value slot: the value's
    /// path.
    pub rel: Vec<Seg>,
}

struct Frame {
    map: bool,
    rel: Vec<Seg>,
    key: Option<String>,
    in_key: bool,
}

fn flow_rel(frames: &[Frame]) -> Vec<Seg> {
    match frames.last() {
        None => vec![],
        Some(f) if f.map => {
            let mut rel = f.rel.clone();
            if let Some(k) = &f.key {
                rel.push(Seg::Key(k.clone()));
            }
            rel
        }
        Some(f) => {
            let mut rel = f.rel.clone();
            rel.push(Seg::Item(FLOW_ITEM));
            rel
        }
    }
}

/// Walk a flow collection. With `whole`, the keys found in `text`; without,
/// `text` is the prefix before a cursor and the second value says what the
/// cursor is in (`None` when it is not inside an unclosed collection).
pub fn flow_walk(text: &str, whole: bool) -> (Vec<FlowKey>, Option<FlowEnd>) {
    let bytes = text.as_bytes();
    let mut frames: Vec<Frame> = vec![];
    let mut keys = vec![];
    let mut i = 0;
    // The plain or quoted token being read: (start, end, text).
    let mut pending: Option<(usize, usize, String)> = None;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            ' ' | '\t' => i += 1,
            '{' | '[' => {
                let rel = flow_rel(&frames);
                frames.push(Frame {
                    map: c == '{',
                    rel,
                    key: None,
                    in_key: true,
                });
                pending = None;
                i += 1;
            }
            '}' | ']' => {
                frames.pop();
                pending = None;
                i += 1;
            }
            ',' => {
                if let Some(f) = frames.last_mut() {
                    f.in_key = true;
                    f.key = None;
                }
                pending = None;
                i += 1;
            }
            '"' | '\'' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] as char != c {
                    if c == '"' && bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let end = i.min(bytes.len());
                pending = Some((start + 1, end, text[start + 1..end].to_string()));
                i = end + 1;
            }
            ':' if next_is_sep(bytes, i) => {
                if let (Some(f), Some((s, e, name))) = (frames.last_mut(), pending.take())
                    && f.map
                    && f.in_key
                {
                    let mut rel = f.rel.clone();
                    rel.push(Seg::Key(name.clone()));
                    keys.push(FlowKey {
                        rel,
                        key: Some(name.clone()),
                        start: s,
                        end: e,
                        value: value_range(text, i + 1),
                    });
                    f.key = Some(name);
                    f.in_key = false;
                }
                i += 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() {
                    let b = bytes[i] as char;
                    if matches!(b, ',' | '{' | '}' | '[' | ']')
                        || (b == ':' && next_is_sep(bytes, i))
                    {
                        break;
                    }
                    i += 1;
                }
                let token = text[start..i].trim_end();
                pending = Some((start, start + token.len(), token.to_string()));
            }
        }
    }
    if whole {
        return (keys, None);
    }
    let end = frames.last().map(|f| {
        let rel = flow_rel(&frames);
        if f.map && f.in_key {
            FlowEnd {
                slot: Slot::Key,
                rel: f.rel.clone(),
            }
        } else {
            FlowEnd {
                slot: Slot::Value,
                rel,
            }
        }
    });
    (keys, end)
}

fn next_is_sep(bytes: &[u8], i: usize) -> bool {
    bytes
        .get(i + 1)
        .is_none_or(|b| matches!(b, b' ' | b',' | b'}' | b']'))
}

/// The trimmed token after `from` up to the next top-level separator.
fn value_range(text: &str, from: usize) -> Option<(usize, usize)> {
    let rest = &text[from..];
    let lead = rest.len() - rest.trim_start().len();
    let start = from + lead;
    let mut depth = 0i32;
    let mut end = text.len();
    for (i, c) in text[start..].char_indices() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => {
                if depth == 0 {
                    end = start + i;
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 0 => {
                end = start + i;
                break;
            }
            _ => {}
        }
    }
    let token = text[start..end].trim_end();
    (!token.is_empty()).then_some((start, start + token.len()))
}

/// The word (letters, digits and `_-.$*/`) around `offset`, as an absolute
/// byte range; `None` when the cursor is not on one.
pub fn word_at(text: &str, index: &LineIndex, offset: usize) -> Option<(usize, usize)> {
    let offset = offset.min(text.len());
    let line = index.position(offset, text).line as usize;
    let (start, end) = index.line_range(line, text);
    let (s, e) = word_around(&text[start..end], start, offset - start);
    (s < e).then_some((s, e))
}

/// The word around column `col` of a line.
fn word_around(line: &str, line_start: usize, col: usize) -> (usize, usize) {
    let mut s = col;
    while s > 0 {
        let prev = line[..s].chars().next_back().filter(|c| is_word(*c));
        match prev {
            Some(c) => s -= c.len_utf8(),
            None => break,
        }
    }
    let mut e = col;
    while let Some(c) = line[e..].chars().next().filter(|c| is_word(*c)) {
        e += c.len_utf8();
    }
    (line_start + s, line_start + e)
}

/// The completion context at byte `offset` of `text`; `None` where nothing
/// should be offered (comments, block scalars, free text).
pub fn context(text: &str, index: &LineIndex, offset: usize) -> Option<Context> {
    let offset = offset.min(text.len());
    let pos = index.position(offset, text);
    let line = pos.line as usize;
    let (start, end) = index.line_range(line, text);
    let s = &text[start..end];
    let col = offset.clamp(start, end) - start;
    let prefix = &s[..col];
    let (_, stack) = run(text, index, Some(line));
    let (indent, item, content) = leading(prefix);
    let current = if prefix.trim().is_empty() {
        col
    } else {
        indent
    };
    if stack.block.is_some_and(|b| current > b) || prefix[indent..].starts_with('#') {
        return None;
    }
    let word = word_around(s, start, col);
    let body = &prefix[content.min(prefix.len())..];
    let mut base = stack.parent_for(indent, item);
    if item {
        base.push(Seg::Item(stack.next_item + 1));
    }
    let Some((ks, ke, colon)) = split_key(&format!("{body}x")) else {
        return Some(Context {
            slot: Slot::Key,
            path: base,
            item,
            word,
        });
    };
    base.push(Seg::Key(body[ks..ke].to_string()));
    let value = &body[colon + 1..];
    if let Some(open) = value.find(['{', '[']) {
        let (_, end) = flow_walk(&value[open..], false);
        let end = end?;
        base.extend(end.rel);
        return Some(Context {
            slot: end.slot,
            path: base,
            item: false,
            word,
        });
    }
    Some(Context {
        slot: Slot::Value,
        path: base,
        item: false,
        word,
    })
}
