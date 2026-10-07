// SPDX-License-Identifier: AGPL-3.0-only
//! Source files, byte spans and line/column lookup.

use serde::{Deserialize, Serialize};

/// Index of a file inside a [`SourceMap`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub struct SourceId(pub u32);

/// A half-open byte range `[start, end)` inside one source file.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub struct Span {
    pub source: SourceId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(source: SourceId, start: usize, end: usize) -> Self {
        Self {
            source,
            start: start as u32,
            end: end as u32,
        }
    }
}

/// One loaded input file. `name` is the path as the user gave it (relative
/// paths stay relative so output is machine-independent).
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub id: SourceId,
    pub name: String,
    pub text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(id: SourceId, name: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        Self {
            id,
            name: name.into(),
            text,
            line_starts,
        }
    }

    /// 1-based (line, column) of a byte offset. Columns count bytes.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (line as u32 + 1, offset - self.line_starts[line] + 1)
    }

    /// The full text of a 1-based line, without the trailing newline.
    pub fn line_text(&self, line: u32) -> &str {
        let idx = (line.saturating_sub(1)) as usize;
        let Some(&start) = self.line_starts.get(idx) else {
            return "";
        };
        let end = self
            .line_starts
            .get(idx + 1)
            .map(|e| *e as usize)
            .unwrap_or(self.text.len());
        self.text[start as usize..end].trim_end_matches(['\n', '\r'])
    }
}

/// All source files of one compilation.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> SourceId {
        let id = SourceId(self.files.len() as u32);
        self.files.push(SourceFile::new(id, name, text));
        id
    }

    pub fn get(&self, id: SourceId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    /// `name:line:col` for a span, used in plain-text and JSON diagnostics.
    pub fn location(&self, span: Span) -> String {
        let f = self.get(span.source);
        let (l, c) = f.line_col(span.start);
        format!("{}:{}:{}", f.name, l, c)
    }
}
