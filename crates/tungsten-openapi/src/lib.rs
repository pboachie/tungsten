// SPDX-License-Identifier: AGPL-3.0-only
//! OpenAPI 3.0/3.1 frontend (planning/02, planning/03).
//!
//! Loads documents (JSON or YAML) with a JSON-Pointer → byte-span index,
//! applies overlays, normalizes 3.0 constructs to 3.1 form, resolves
//! `$ref` across files, and builds the reference graph with cycle
//! information. `$ref`s are NOT inlined: the builder needs them to keep type
//! names.
//!
//! PHASE-1 STUB: the public API in this file is the contract. The current
//! implementation is minimal (JSON only, no spans, local refs only, no
//! overlays, no normalization, empty graph) and is replaced by the frontend
//! work package without changing these signatures.

mod graph;
mod load;

use std::path::PathBuf;

use tungsten_core::{Diagnostics, Digest, SourceId, SourceMap, Span};

pub use graph::RefGraph;
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
    spans: std::collections::HashMap<String, Span>,
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
        }
    }
    /// Span of the value at a JSON Pointer, if known. Pointers changed by
    /// overlays or normalization keep the span of the nearest original node.
    pub fn span(&self, pointer: &str) -> Option<Span> {
        self.spans.get(pointer).copied()
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
    pub graph: RefGraph,
    pub diagnostics: Diagnostics,
}

impl Workspace {
    /// Resolve a `$ref` string found in document `from`. Handles local
    /// pointers (`#/components/schemas/X`), relative files
    /// (`common.yaml#/X`) and allowlisted URLs. Returns `None` when the
    /// target does not exist (the loader has already reported TG0201).
    pub fn resolve(&self, from: DocId, reference: &str) -> Option<RefTarget> {
        load::resolve(self, from, reference)
    }

    /// The value at a target.
    pub fn get(&self, target: &RefTarget) -> Option<&serde_json::Value> {
        self.documents.get(target.doc)?.get(&target.pointer)
    }

    /// Follow `$ref` chains starting at a target until a non-`$ref` value.
    /// Returns the final target, or `None` on a dangling or circular chain.
    pub fn deref(&self, target: &RefTarget) -> Option<RefTarget> {
        let mut cur = target.clone();
        for _ in 0..64 {
            let v = self.get(&cur)?;
            match v.get("$ref").and_then(|r| r.as_str()) {
                Some(r) => cur = self.resolve(cur.doc, r)?,
                None => return Some(cur),
            }
        }
        None
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

/// Append a token to a pointer.
pub fn join_pointer(base: &str, token: &str) -> String {
    format!("{base}/{}", escape_token(token))
}
