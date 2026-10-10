// SPDX-License-Identifier: AGPL-3.0-only
//! Byte offsets and LSP positions (zero-based lines, UTF-16 columns).

use lsp_types::{Position, Range};

/// Line starts of one text, for converting between byte offsets and LSP
/// positions.
#[derive(Debug, Clone, Default)]
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { starts }
    }

    /// Number of lines (a final line without a newline counts).
    pub fn lines(&self) -> usize {
        self.starts.len()
    }

    /// Byte offset where a zero-based line starts, clamped to the text.
    pub fn line_start(&self, line: usize, text: &str) -> usize {
        self.starts.get(line).copied().unwrap_or(text.len())
    }

    /// Byte range of a line without its line terminator.
    pub fn line_range(&self, line: usize, text: &str) -> (usize, usize) {
        let start = self.line_start(line, text);
        let end = self.starts.get(line + 1).copied().unwrap_or(text.len());
        let trimmed = text[start..end].trim_end_matches(['\n', '\r']).len();
        (start, start + trimmed)
    }

    pub fn position(&self, offset: usize, text: &str) -> Position {
        let offset = offset.min(text.len());
        let line = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let start = self.starts[line];
        let mut end = offset;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let col: usize = text[start..end].chars().map(char::len_utf16).sum();
        Position::new(line as u32, col as u32)
    }

    pub fn range(&self, start: usize, end: usize, text: &str) -> Range {
        Range::new(self.position(start, text), self.position(end, text))
    }

    /// Byte offset of a position; a column past the end of the line is the
    /// end of the line.
    pub fn offset(&self, pos: Position, text: &str) -> usize {
        let (start, end) = self.line_range(pos.line as usize, text);
        let mut units = 0u32;
        for (i, c) in text[start..end].char_indices() {
            if units >= pos.character {
                return start + i;
            }
            units += c.len_utf16() as u32;
        }
        end
    }
}
