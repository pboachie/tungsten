// SPDX-License-Identifier: AGPL-3.0-only
//! The JSONPath subset used by OpenAPI Overlay 1.0 targets.
//!
//! Supported: `$`, `.name`, `['name']` / `["name"]` (with `,` unions),
//! `[n]` (negative counts from the end, `,` unions), `[*]`, `.*`,
//! recursive descent `..name` / `..*` / `..[...]`, and filters
//! `[?(@.key == literal)]`, `[?(@.key != literal)]`, `[?(@.key)]` where the
//! relative path may chain `.name`, `['name']` and `[n]` and literals are
//! strings, numbers, booleans or `null`. The parentheses are optional
//! (RFC 9535 style `[?@.key == 'v']`).
//!
//! Selection returns JSON Pointers in selection order without duplicates.

use std::collections::HashSet;

use serde_json::Value;

use crate::join_pointer;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JsonPath {
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq)]
enum Segment {
    Child(Selector),
    Descendant(Selector),
}

#[derive(Debug, Clone, PartialEq)]
enum Selector {
    Names(Vec<String>),
    Indices(Vec<i64>),
    Wildcard,
    Filter(Filter),
}

#[derive(Debug, Clone, PartialEq)]
struct Filter {
    path: Vec<Step>,
    test: Option<(Comparison, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Name(String),
    Index(i64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Comparison {
    Eq,
    Ne,
}

impl JsonPath {
    pub(crate) fn parse(expr: &str) -> Result<Self, String> {
        let mut p = Cursor {
            chars: expr.chars().collect(),
            pos: 0,
        };
        p.skip_ws();
        if !p.eat('$') {
            return Err("a JSONPath must start with '$'".into());
        }
        let mut segments = vec![];
        loop {
            p.skip_ws();
            match p.peek() {
                None => break,
                Some('.') if p.peek_at(1) == Some('.') => {
                    p.pos += 2;
                    let sel = if p.peek() == Some('[') {
                        p.bracket()?
                    } else {
                        p.dot_member()?
                    };
                    segments.push(Segment::Descendant(sel));
                }
                Some('.') => {
                    p.pos += 1;
                    segments.push(Segment::Child(p.dot_member()?));
                }
                Some('[') => segments.push(Segment::Child(p.bracket()?)),
                Some(c) => return Err(format!("unexpected '{c}' at position {}", p.pos)),
            }
        }
        Ok(Self { segments })
    }

    /// Pointers of every node selected in `root`. `Err` when a step of the
    /// selection holds more than [`MAX_SELECTED`] nodes.
    pub(crate) fn select(&self, root: &Value) -> Result<Vec<String>, String> {
        let mut current: Vec<(String, &Value)> = vec![(String::new(), root)];
        for segment in &self.segments {
            let mut next = vec![];
            match segment {
                Segment::Child(sel) => {
                    for (p, v) in &current {
                        apply(sel, p, v, &mut next);
                    }
                }
                Segment::Descendant(sel) => {
                    // One walk of the document applies `sel` to every node
                    // inside a current subtree, so nested current nodes do
                    // not walk their subtrees again.
                    let roots: HashSet<&str> = current.iter().map(|(p, _)| p.as_str()).collect();
                    let mut stack = vec![(String::new(), root, false)];
                    while let Some((p, v, inside)) = stack.pop() {
                        let inside = inside || roots.contains(p.as_str());
                        if inside {
                            apply(sel, &p, v, &mut next);
                            if next.len() > MAX_SELECTED {
                                return Err(too_many());
                            }
                        }
                        stack.extend(
                            children(&p, v)
                                .into_iter()
                                .rev()
                                .map(|(cp, cv)| (cp, cv, inside)),
                        );
                    }
                }
            }
            let mut seen = HashSet::new();
            next.retain(|(p, _)| seen.insert(p.clone()));
            if next.len() > MAX_SELECTED {
                return Err(too_many());
            }
            current = next;
        }
        Ok(current.into_iter().map(|(p, _)| p).collect())
    }
}

/// Most nodes one step of a JSONPath selection may hold.
pub(crate) const MAX_SELECTED: usize = 1_000_000;

fn too_many() -> String {
    format!("the selection exceeds the limit of {MAX_SELECTED} nodes")
}

fn children<'v>(pointer: &str, v: &'v Value) -> Vec<(String, &'v Value)> {
    match v {
        Value::Object(m) => m
            .iter()
            .map(|(k, c)| (join_pointer(pointer, k), c))
            .collect(),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, c)| (format!("{pointer}/{i}"), c))
            .collect(),
        _ => vec![],
    }
}

fn index_of(len: usize, i: i64) -> Option<usize> {
    let i = if i < 0 {
        i.checked_add(i64::try_from(len).ok()?)?
    } else {
        i
    };
    usize::try_from(i).ok().filter(|&i| i < len)
}

fn apply<'v>(sel: &Selector, pointer: &str, v: &'v Value, out: &mut Vec<(String, &'v Value)>) {
    match sel {
        Selector::Names(names) => {
            if let Value::Object(m) = v {
                for name in names {
                    if let Some(c) = m.get(name) {
                        out.push((join_pointer(pointer, name), c));
                    }
                }
            }
        }
        Selector::Indices(indices) => {
            if let Value::Array(a) = v {
                for &i in indices {
                    if let Some(i) = index_of(a.len(), i) {
                        out.push((format!("{pointer}/{i}"), &a[i]));
                    }
                }
            }
        }
        Selector::Wildcard => out.extend(children(pointer, v)),
        Selector::Filter(f) => out.extend(
            children(pointer, v)
                .into_iter()
                .filter(|(_, c)| f.matches(c)),
        ),
    }
}

impl Filter {
    fn matches(&self, node: &Value) -> bool {
        let mut cur = Some(node);
        for step in &self.path {
            cur = match (step, cur) {
                (Step::Name(n), Some(Value::Object(m))) => m.get(n),
                (Step::Index(i), Some(Value::Array(a))) => index_of(a.len(), *i).map(|i| &a[i]),
                _ => None,
            };
        }
        match (&self.test, cur) {
            (None, found) => found.is_some(),
            (Some((Comparison::Eq, lit)), Some(v)) => same(v, lit),
            (Some((Comparison::Eq, _)), None) => false,
            (Some((Comparison::Ne, lit)), found) => !found.is_some_and(|v| same(v, lit)),
        }
    }
}

/// JSON equality where numbers compare by value (`1 == 1.0`).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

struct Cursor {
    chars: Vec<char>,
    pos: usize,
}

impl Cursor {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, c: char) -> Result<(), String> {
        self.skip_ws();
        if self.eat(c) {
            Ok(())
        } else {
            Err(match self.peek() {
                Some(found) => format!("expected '{c}' at position {}, found '{found}'", self.pos),
                None => format!("expected '{c}' at the end of the path"),
            })
        }
    }
    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    /// After `.` or `..`: `*` or a member name.
    fn dot_member(&mut self) -> Result<Selector, String> {
        if self.eat('*') {
            return Ok(Selector::Wildcard);
        }
        Ok(Selector::Names(vec![self.name()?]))
    }

    fn name(&mut self) -> Result<String, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if matches!(
                c,
                '.' | '[' | ']' | '(' | ')' | '\'' | '"' | '=' | '!' | ','
            ) || c.is_whitespace()
            {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(format!("expected a member name at position {start}"));
        }
        Ok(self.chars[start..self.pos].iter().collect())
    }

    fn bracket(&mut self) -> Result<Selector, String> {
        self.expect('[')?;
        self.skip_ws();
        let sel = match self.peek() {
            Some('*') => {
                self.pos += 1;
                Selector::Wildcard
            }
            Some('?') => {
                self.pos += 1;
                Selector::Filter(self.filter()?)
            }
            Some('\'' | '"') => {
                let mut names = vec![self.quoted()?];
                while self.skip_ws_then(',') {
                    self.skip_ws();
                    names.push(self.quoted()?);
                }
                Selector::Names(names)
            }
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let mut indices = vec![self.integer()?];
                while self.skip_ws_then(',') {
                    self.skip_ws();
                    indices.push(self.integer()?);
                }
                Selector::Indices(indices)
            }
            Some(c) => {
                return Err(format!(
                    "unexpected '{c}' in brackets at position {}",
                    self.pos
                ));
            }
            None => return Err("unterminated '['".into()),
        };
        self.expect(']')?;
        Ok(sel)
    }

    fn skip_ws_then(&mut self, c: char) -> bool {
        self.skip_ws();
        self.eat(c)
    }

    fn filter(&mut self) -> Result<Filter, String> {
        self.skip_ws();
        let parens = self.eat('(');
        self.skip_ws();
        if !self.eat('@') {
            return Err(format!(
                "a filter must start with '@' at position {}",
                self.pos
            ));
        }
        let mut path = vec![];
        loop {
            match self.peek() {
                Some('.') => {
                    self.pos += 1;
                    path.push(Step::Name(self.name()?));
                }
                Some('[') => {
                    self.pos += 1;
                    self.skip_ws();
                    let step = match self.peek() {
                        Some('\'' | '"') => Step::Name(self.quoted()?),
                        _ => Step::Index(self.integer()?),
                    };
                    self.expect(']')?;
                    path.push(step);
                }
                _ => break,
            }
        }
        self.skip_ws();
        let test = match (self.peek(), self.peek_at(1)) {
            (Some('='), Some('=')) => Some(Comparison::Eq),
            (Some('!'), Some('=')) => Some(Comparison::Ne),
            _ => None,
        };
        let test = match test {
            Some(op) => {
                self.pos += 2;
                self.skip_ws();
                Some((op, self.literal()?))
            }
            None => None,
        };
        if parens {
            self.expect(')')?;
        }
        Ok(Filter { path, test })
    }

    fn literal(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('\'' | '"') => Ok(Value::String(self.quoted()?)),
            _ => {
                let start = self.pos;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.'))
                {
                    self.pos += 1;
                }
                let word: String = self.chars[start..self.pos].iter().collect();
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "null" => Ok(Value::Null),
                    _ => serde_json::from_str::<serde_json::Number>(&word)
                        .map(Value::Number)
                        .map_err(|_| format!("invalid literal '{word}' at position {start}")),
                }
            }
        }
    }

    fn integer(&mut self) -> Result<i64, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<i64>()
            .map_err(|_| format!("invalid index at position {start}"))
    }

    fn quoted(&mut self) -> Result<String, String> {
        let quote = self.peek().ok_or("expected a quoted name")?;
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err(format!("unterminated string at position {start}")),
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    let c = match self.peek_at(1) {
                        Some(c @ ('\\' | '\'' | '"' | '/')) => c,
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        _ => return Err(format!("invalid escape at position {}", self.pos)),
                    };
                    out.push(c);
                    self.pos += 2;
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }
}
