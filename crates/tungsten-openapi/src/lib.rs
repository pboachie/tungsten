// SPDX-License-Identifier: AGPL-3.0-only
//! OpenAPI 3.0/3.1 frontend (planning/02, planning/03).
//!
//! Loads documents (JSON or YAML) with a JSON-Pointer → byte-span index,
//! applies overlays, normalizes 3.0 constructs to 3.1 form, resolves
//! `$ref` across files, and builds the reference graph with cycle
//! information. `$ref`s are NOT inlined: the builder needs them to keep type
//! names.
//!
//! Pipeline per entry document: read (size limit) → parse with spans
//! (depth limit, YAML alias budget) → overlays → version check → 3.0→3.1
//! normalization → `$ref` resolution across files (loading referenced files
//! once each) → reference graph and cycles. Every problem is a diagnostic;
//! nothing here panics on bad input.

mod graph;
mod jsonpath;
mod load;
mod normalize;
mod overlay;
mod parse;
mod refs;
mod spans;
mod version;
mod walk;
mod yaml;

use std::collections::BTreeMap;
use std::path::PathBuf;

use tungsten_core::{Diagnostics, Digest, SourceId, SourceMap, Span};

pub use graph::{RefGraph, is_named_schema};
pub use load::{load, load_str};

/// Index into [`Workspace::documents`].
pub type DocId = usize;

/// The OpenAPI version a document declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecVersion {
    /// 3.0.x; the document has been normalized to 3.1 form.
    V30(String),
    V31(String),
    /// A file loaded only because another document referenced it.
    Fragment,
}

/// Limits and policies for loading.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// URL prefixes remote `$ref`s may fetch from. Empty: remote refs are
    /// refused with TG0202.
    pub allow_remote: Vec<String>,
    /// Maximum bytes per file (TG0105).
    pub max_bytes: usize,
    /// Maximum nesting depth (TG0105).
    pub max_depth: usize,
    /// Accept Swagger 2.0 by converting it (not implemented in 0.1; TG0103).
    pub convert_swagger2: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            allow_remote: vec![],
            max_bytes: 32 * 1024 * 1024,
            max_depth: 256,
            convert_swagger2: false,
        }
    }
}

/// One entry document to load, with the overlays to apply to it in order.
#[derive(Debug, Clone)]
pub struct LoadEntry {
    pub path: PathBuf,
    /// Name used in diagnostics and source refs (path as written by the
    /// user, relative to the project root).
    pub display_name: String,
    pub overlays: Vec<PathBuf>,
}

/// One loaded document.
#[derive(Debug, Clone)]
pub struct Document {
    pub source: SourceId,
    pub name: String,
    pub version: SpecVersion,
    /// The document after overlays and 3.0→3.1 normalization.
    pub root: serde_json::Value,
    /// Digest of the original file bytes.
    pub digest: Digest,
    spans: spans::SpanIndex,
    /// Absolute, lexically normalized path the file was loaded from; `None`
    /// for in-memory documents.
    path: Option<PathBuf>,
    /// Members moved by 3.0 nullable wraps, for translating pointers.
    moves: Vec<normalize::Move>,
}

impl Document {
    pub fn new(
        source: SourceId,
        name: String,
        version: SpecVersion,
        root: serde_json::Value,
        digest: Digest,
    ) -> Self {
        Self {
            source,
            name,
            version,
            root,
            digest,
            spans: Default::default(),
            path: None,
            moves: vec![],
        }
    }
    /// Span of the value at a JSON Pointer, if known. Pointers changed by
    /// overlays or normalization keep the span of the nearest original node.
    /// Values inserted by an overlay have spans in the overlay file.
    pub fn span(&self, pointer: &str) -> Option<Span> {
        self.spans.nearest(pointer)
    }
    /// Record a span; used by the loader.
    pub fn set_span(&mut self, pointer: String, span: Span) {
        self.spans.insert(pointer, span);
    }
    /// The value at a JSON Pointer.
    pub fn get(&self, pointer: &str) -> Option<&serde_json::Value> {
        self.root.pointer(pointer)
    }
}

/// A canonical reference target: document plus JSON Pointer (`""` is the root).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RefTarget {
    pub doc: DocId,
    pub pointer: String,
}

/// Everything loaded for one compilation.
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    pub sources: SourceMap,
    pub documents: Vec<Document>,
    /// Entry documents in the order given to [`load`]; fragments follow.
    pub entries: Vec<DocId>,
    /// One slot per [`LoadEntry`] given to [`load`], in that order: the
    /// entry's document, or `None` when it could not be loaded. The same
    /// file listed twice (with different overlays) is two documents.
    pub entry_docs: Vec<Option<DocId>>,
    /// Digest of the bytes of every overlay file that was read and applied,
    /// keyed by the path given in [`LoadEntry::overlays`].
    pub overlay_digests: BTreeMap<PathBuf, Digest>,
    pub graph: RefGraph,
    pub diagnostics: Diagnostics,
    /// Loaded files by absolute path (lexical and canonical forms).
    files: BTreeMap<PathBuf, DocId>,
}

impl Workspace {
    /// Resolve a `$ref` string found in document `from`. Handles local
    /// pointers (`#/components/schemas/X`) and relative files
    /// (`common.yaml#/X`, loaded by [`load`]), with percent-decoding and
    /// pointers written against the pre-normalization layout of 3.0
    /// documents. Returns `None` when the target does not exist or is
    /// remote (the loader has already reported TG0201/TG0202/TG0204).
    pub fn resolve(&self, from: DocId, reference: &str) -> Option<RefTarget> {
        refs::resolve(self, from, reference)
    }

    /// The value at a target.
    pub fn get(&self, target: &RefTarget) -> Option<&serde_json::Value> {
        self.documents.get(target.doc)?.get(&target.pointer)
    }

    /// Follow `$ref` chains starting at a target until a non-`$ref` value.
    /// Returns the final target, or `None` on a dangling or circular chain.
    /// Chains of any length are followed; only a repeated target stops one.
    pub fn deref(&self, target: &RefTarget) -> Option<RefTarget> {
        let mut cur = target.clone();
        let mut seen = std::collections::HashSet::new();
        loop {
            let v = self.get(&cur)?;
            match v.get("$ref").and_then(|r| r.as_str()) {
                Some(r) => {
                    if !seen.insert(cur.clone()) {
                        return None;
                    }
                    cur = self.resolve(cur.doc, r)?;
                }
                None => return Some(cur),
            }
        }
    }

    /// Span for a target, if known.
    pub fn span(&self, target: &RefTarget) -> Option<Span> {
        self.documents.get(target.doc)?.span(&target.pointer)
    }

    /// Display name of a document.
    pub fn name(&self, doc: DocId) -> &str {
        &self.documents[doc].name
    }
}

/// Escape one JSON Pointer reference token (RFC 6901).
pub fn escape_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Unescape one JSON Pointer reference token (RFC 6901).
pub fn unescape_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Append a token to a pointer.
pub fn join_pointer(base: &str, token: &str) -> String {
    format!("{base}/{}", escape_token(token))
}

/// The unescaped reference tokens of a pointer (`""` has none).
pub fn split_pointer(pointer: &str) -> Vec<String> {
    match pointer.strip_prefix('/') {
        Some(rest) => rest.split('/').map(unescape_token).collect(),
        None => vec![],
    }
}

/// Internals exposed to the private test harness.
#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod __testing {
    use serde_json::Value;
    use tungsten_core::{Diagnostic, Diagnostics, SourceId, Span};

    /// Maximum number of nodes YAML alias expansion may add to one document.
    pub const MAX_ALIAS_NODES: usize = crate::yaml::MAX_ALIAS_NODES;

    /// Maximum bytes of text YAML alias expansion may add to one document.
    pub const MAX_ALIAS_BYTES: usize = crate::yaml::MAX_ALIAS_BYTES;

    /// Parse JSON or YAML (format chosen from `name`, then content) into a
    /// value and its pointer → span index, as source 0.
    pub fn parse(
        name: &str,
        text: &str,
        max_depth: usize,
    ) -> Result<(Value, Vec<(String, Span)>), Diagnostic> {
        crate::parse::parse(name, text, SourceId(0), max_depth)
            .map(|p| (p.value, p.spans.subtree("")))
            .map_err(|e| e.into_diagnostic(name))
    }

    /// Pointers selected by an overlay JSONPath expression.
    pub fn jsonpath_select(expr: &str, root: &Value) -> Result<Vec<String>, String> {
        crate::jsonpath::JsonPath::parse(expr).and_then(|p| p.select(root))
    }

    /// Apply the 3.0 → 3.1 rewrites to a standalone schema.
    pub fn normalize_schema(schema: &mut Value) {
        normalize_at(schema, crate::walk::Kind::Schema);
    }

    /// Apply the 3.0 → 3.1 rewrites to every schema of a whole document.
    pub fn normalize_document(doc: &mut Value) {
        normalize_at(doc, crate::walk::Kind::Document);
    }

    fn normalize_at(value: &mut Value, kind: crate::walk::Kind) {
        let mut spans = crate::spans::SpanIndex::default();
        let mut moves = vec![];
        crate::normalize::normalize(value, &mut spans, &mut moves, "", kind);
    }

    /// Apply an overlay given as a value (file name `overlay.yaml`).
    pub fn apply_overlay(doc: &mut Value, overlay: &Value) -> Diagnostics {
        let mut diags = Diagnostics::new();
        let mut spans = crate::spans::SpanIndex::default();
        let overlay_spans = crate::spans::SpanIndex::default();
        let overlay = crate::overlay::Overlay {
            name: "overlay.yaml",
            value: overlay,
            spans: &overlay_spans,
        };
        crate::overlay::apply(doc, &mut spans, &overlay, &mut diags);
        diags
    }

    /// Whether a `$ref` string parses, and as what (`local`, `file`,
    /// `remote`) with its decoded pointer; `Err` carries the reason.
    pub fn classify_reference(reference: &str) -> Result<(&'static str, String), String> {
        use crate::refs::Reference;
        crate::refs::parse_reference(reference).map(|r| match r {
            Reference::Local { pointer } => ("local", pointer),
            Reference::File { path, pointer } => ("file", format!("{path}#{pointer}")),
            Reference::Remote { url } => ("remote", url),
        })
    }
}
