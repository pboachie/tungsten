// SPDX-License-Identifier: AGPL-3.0-only
//! Manifest text plus the location of every node, keyed by JSON Pointer.

use std::collections::BTreeMap;

use tungsten_core::{Diagnostics, SourceFile, SourceId, Span};

use crate::pointer;

/// The manifest as read, with a map from JSON Pointer to the byte offset
/// where that node starts (for a mapping entry: where its key starts).
///
/// Diagnostics from this crate carry pointers but no spans, because the
/// manifest is not part of any [`tungsten_core::SourceMap`] at parse time.
/// A caller that renders locations either asks [`ManifestSource::line_col`]
/// or adds [`ManifestSource::text`] to its source map and calls
/// [`ManifestSource::attach_spans`] with the returned id.
#[derive(Debug, Clone)]
pub struct ManifestSource {
    file: SourceFile,
    nodes: BTreeMap<String, u32>,
}

impl ManifestSource {
    pub(crate) fn new(name: &str, text: &str, nodes: BTreeMap<String, u32>) -> Self {
        Self {
            file: SourceFile::new(SourceId(0), name, text),
            nodes,
        }
    }

    /// The manifest name as given to the loader.
    pub fn name(&self) -> &str {
        &self.file.name
    }

    /// The full manifest text (empty when the file could not be read).
    pub fn text(&self) -> &str {
        &self.file.text
    }

    /// Byte offset where the node at `pointer` starts. A pointer that does
    /// not exist in the document (for example a missing required field)
    /// resolves to its nearest existing ancestor. `None` when no node of
    /// the path is known (empty or unparsable document).
    pub fn offset(&self, pointer: &str) -> Option<u32> {
        let mut current = pointer;
        loop {
            if let Some(&offset) = self.nodes.get(current) {
                return Some(offset);
            }
            current = pointer::parent(current)?;
        }
    }

    /// 1-based line and column (in bytes, like
    /// [`tungsten_core::SourceFile::line_col`]) of the node at `pointer`,
    /// resolved as in [`ManifestSource::offset`].
    pub fn line_col(&self, pointer: &str) -> Option<(u32, u32)> {
        self.offset(pointer).map(|o| self.line_col_at(o))
    }

    /// 1-based line and column of a byte offset.
    pub fn line_col_at(&self, offset: u32) -> (u32, u32) {
        self.file.line_col(offset.min(self.file.text.len() as u32))
    }

    /// The span of the node at `pointer` for a source map in which this
    /// manifest's text was added as `source`: from the node's start to the
    /// end of that line.
    pub fn span(&self, source: SourceId, pointer: &str) -> Option<Span> {
        let start = self.offset(pointer)? as usize;
        let text = self.text();
        let end = text[start..]
            .find(['\n', '\r'])
            .map_or(text.len(), |i| start + i);
        Some(Span::new(source, start, end))
    }

    /// Fill the span of every label that points into this manifest (same
    /// file name) and has no span yet.
    pub fn attach_spans(&self, source: SourceId, diagnostics: &mut Diagnostics) {
        for diagnostic in &mut diagnostics.0 {
            for label in &mut diagnostic.labels {
                if label.span.is_none() && label.file == self.name() {
                    label.span = self.span(source, &label.pointer);
                }
            }
        }
    }
}
