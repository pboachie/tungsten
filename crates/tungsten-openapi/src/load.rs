// SPDX-License-Identifier: AGPL-3.0-only
//! Loading: entry documents, overlays, version checks, normalization,
//! `$ref` resolution across files and the reference graph.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tungsten_core::{Diagnostic, Digest, Label, SourceId};

use crate::overlay::{self, Overlay};
use crate::parse::{self, Parsed};
use crate::refs::{Reference, lexical_normalize, parse_reference};
use crate::walk::{Kind, walk};
use crate::{
    DocId, Document, LoadEntry, LoadOptions, RefTarget, SpecVersion, Workspace, graph, normalize,
    version,
};

/// Load entry documents and everything they reference.
pub fn load(entries: &[LoadEntry], opts: &LoadOptions) -> Workspace {
    let mut loader = Loader::new(opts);
    for entry in entries {
        let doc = loader.load_entry(entry);
        loader.ws.entry_docs.push(doc);
    }
    loader.finish()
}

/// Load one document from memory (used by tests and `--stdin`). The format
/// is chosen from `name`'s extension, then from the content. Relative file
/// references cannot be followed from an in-memory document (TG0201).
pub fn load_str(name: &str, text: &str, opts: &LoadOptions) -> Workspace {
    let mut loader = Loader::new(opts);
    if text.len() > opts.max_bytes {
        loader
            .ws
            .diagnostics
            .push(too_big(name, opts.max_bytes).at(name, "", None));
    } else {
        let source = loader.ws.sources.add(name, text);
        let digest = Digest::of(text.as_bytes());
        let doc = loader
            .parse(name, source, None)
            .and_then(|parsed| loader.add_entry(name, None, parsed, source, digest, &[]));
        loader.ws.entry_docs.push(doc);
    }
    loader.finish()
}

fn too_big(name: &str, max: usize) -> Diagnostic {
    Diagnostic::error(
        "TG0105",
        format!("{name} exceeds the size limit of {max} bytes"),
    )
}

/// Read at most `max` bytes of UTF-8 text, with the digest of the bytes.
/// Only regular files (after symlinks) are opened: a directory, a named
/// pipe or a device is TG0101, so a FIFO never blocks the compiler.
fn read_limited(path: &Path, name: &str, max: usize) -> Result<(String, Digest), Diagnostic> {
    let cannot_read =
        |e: std::io::Error| Diagnostic::error("TG0101", format!("cannot read {name}: {e}"));
    let meta = std::fs::metadata(path).map_err(cannot_read)?;
    if !meta.is_file() {
        return Err(Diagnostic::error(
            "TG0101",
            format!("cannot read {name}: not a regular file"),
        ));
    }
    let file = std::fs::File::open(path).map_err(cannot_read)?;
    let mut bytes = vec![];
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(cannot_read)?;
    if bytes.len() > max {
        return Err(too_big(name, max));
    }
    let digest = Digest::of(&bytes);
    String::from_utf8(bytes)
        .map(|text| (text, digest))
        .map_err(|e| {
            Diagnostic::error(
                "TG0102",
                format!(
                    "{name} is not valid UTF-8 (byte {})",
                    e.utf8_error().valid_up_to()
                ),
            )
        })
}

/// Where a file was referenced from, for diagnostics about that file.
#[derive(Debug, Clone)]
struct Site {
    doc: DocId,
    /// Pointer of the `$ref` member.
    pointer: String,
}

/// A `$ref` found while walking.
#[derive(Debug)]
struct RefSite {
    /// Pointer of the object holding `$ref`.
    pointer: String,
    kind: Kind,
    reference: Value,
}

/// An input root: the directory of an entry document. File references
/// must stay inside one of them.
#[derive(Debug)]
struct Root {
    lexical: PathBuf,
    canonical: PathBuf,
    /// Display-name prefix of the root (`""` or `"specs/"`).
    display: String,
}

struct Loader<'o> {
    opts: &'o LoadOptions,
    ws: Workspace,
    /// The directory of every entry document, in entry order.
    roots: Vec<Root>,
    /// Per document: whether it was declared OpenAPI 3.0 (entries only;
    /// fragments have no dialect of their own).
    dialect30: Vec<bool>,
    /// Subtrees of fragments reached by references from 3.0 documents,
    /// normalized after resolution. A subtree any 3.0 document reaches is
    /// normalized, whatever else reaches it and in whatever order.
    fragment_roots: Vec<(DocId, String, Kind)>,
    /// Every schema `$ref` target.
    schema_targets: BTreeSet<RefTarget>,
    /// Referenced files that could not be read or parsed.
    failed_files: BTreeSet<PathBuf>,
}

impl<'o> Loader<'o> {
    fn new(opts: &'o LoadOptions) -> Self {
        Self {
            opts,
            ws: Workspace::default(),
            roots: vec![],
            dialect30: vec![],
            fragment_roots: vec![],
            schema_targets: BTreeSet::new(),
            failed_files: BTreeSet::new(),
        }
    }

    fn load_entry(&mut self, entry: &LoadEntry) -> Option<DocId> {
        let name = entry.display_name.as_str();
        let (text, digest) = self.read(&entry.path, name, None)?;
        let lexical = absolute(&entry.path);
        if let (Some(dir), Ok(canonical)) = (lexical.parent(), std::fs::canonicalize(&lexical))
            && let Some(canonical_dir) = canonical.parent()
            && !self.roots.iter().any(|r| r.canonical == canonical_dir)
        {
            let display = match name.rfind('/') {
                Some(i) => name[..=i].to_string(),
                None => String::new(),
            };
            self.roots.push(Root {
                lexical: dir.to_path_buf(),
                canonical: canonical_dir.to_path_buf(),
                display,
            });
        }
        let source = self.ws.sources.add(name, text);
        let parsed = self.parse(&entry.path.to_string_lossy(), source, None)?;
        let overlays: Vec<(PathBuf, String)> = entry
            .overlays
            .iter()
            .map(|o| (o.clone(), overlay_display(o, entry)))
            .collect();
        self.add_entry(name, Some(lexical), parsed, source, digest, &overlays)
    }

    /// Overlays, version check, normalization; then register the document.
    fn add_entry(
        &mut self,
        name: &str,
        path: Option<PathBuf>,
        parsed: Parsed,
        source: SourceId,
        digest: Digest,
        overlays: &[(PathBuf, String)],
    ) -> Option<DocId> {
        let Parsed {
            value: mut root,
            mut spans,
        } = parsed;
        for (overlay_path, overlay_name) in overlays {
            let Some((text, overlay_digest)) = self.read(overlay_path, overlay_name, None) else {
                continue;
            };
            self.ws
                .overlay_digests
                .insert(overlay_path.clone(), overlay_digest);
            let overlay_source = self.ws.sources.add(overlay_name.as_str(), text);
            let Some(o) = self.parse(&overlay_path.to_string_lossy(), overlay_source, None) else {
                continue;
            };
            overlay::apply(
                &mut root,
                &mut spans,
                &Overlay {
                    name: overlay_name,
                    value: &o.value,
                    spans: &o.spans,
                },
                &mut self.ws.diagnostics,
            );
        }
        let version = match version::detect(&root, name, &spans, self.opts) {
            Ok((v, warnings)) => {
                for w in warnings {
                    self.ws.diagnostics.push(w);
                }
                v
            }
            Err(errors) => {
                for e in errors {
                    self.ws.diagnostics.push(e);
                }
                return None;
            }
        };
        let v30 = matches!(version, SpecVersion::V30(_));
        let mut moves = vec![];
        if v30 {
            normalize::normalize(&mut root, &mut spans, &mut moves, "", Kind::Document);
        }
        let mut doc = Document::new(source, name.to_string(), version, root, digest);
        doc.spans = spans;
        doc.moves = moves;
        let id = self.push(doc, path, v30);
        self.ws.entries.push(id);
        Some(id)
    }

    fn push(&mut self, mut doc: Document, path: Option<PathBuf>, v30: bool) -> DocId {
        let id = self.ws.documents.len();
        if let Some(p) = &path {
            self.ws.files.entry(p.clone()).or_insert(id);
            if let Ok(c) = std::fs::canonicalize(p) {
                self.ws.files.entry(c).or_insert(id);
            }
        }
        doc.path = path;
        self.ws.documents.push(doc);
        self.dialect30.push(v30);
        id
    }

    /// Read a file under the size limit. Problems are reported at `site`
    /// when the file was referenced, otherwise at the file itself.
    fn read(&mut self, path: &Path, name: &str, site: Option<&Site>) -> Option<(String, Digest)> {
        match read_limited(path, name, self.opts.max_bytes) {
            Ok(read) => Some(read),
            Err(d) => {
                match site {
                    Some(s) => self.report(s, d),
                    None => self.ws.diagnostics.push(d.at(name, "", None)),
                }
                None
            }
        }
    }

    /// Parse the registered source `source`; `format_name` (a path or name)
    /// picks JSON or YAML.
    fn parse(
        &mut self,
        format_name: &str,
        source: SourceId,
        site: Option<&Site>,
    ) -> Option<Parsed> {
        let text = &self.ws.sources.get(source).text;
        let error = match parse::parse(format_name, text, source, self.opts.max_depth) {
            Ok(p) => return Some(p),
            Err(e) => e,
        };
        let mut d = error.into_diagnostic(&self.ws.sources.get(source).name);
        if let Some(s) = site {
            d.labels.push(Label {
                message: Some("referenced here".into()),
                ..site_label(&self.ws, s)
            });
        }
        self.ws.diagnostics.push(d);
        None
    }

    fn finish(mut self) -> Workspace {
        self.resolve_all();
        self.normalize_fragments();
        let mut diags = std::mem::take(&mut self.ws.diagnostics);
        self.ws.graph = graph::build(&self.ws, &self.schema_targets, &mut diags);
        self.ws.diagnostics = diags;
        self.ws
    }

    /// Walk every entry document, follow every `$ref`, load referenced
    /// files, and report the references that cannot be followed. Each walk
    /// carries the dialect of the entry it started from, so a fragment
    /// subtree reached from a 3.0 document is normalized even when a 3.1
    /// document reached it first.
    fn resolve_all(&mut self) {
        let mut queue: VecDeque<(DocId, String, Kind, bool)> = self
            .ws
            .entries
            .iter()
            .map(|&d| (d, String::new(), Kind::Document, self.dialect30[d]))
            .collect();
        let mut seen_roots = HashSet::new();
        // Each `$ref` is followed (and reported) once, whatever walks reach it.
        let mut followed: HashMap<(DocId, String), Option<RefTarget>> = HashMap::new();
        while let Some((doc, pointer, kind, v30)) = queue.pop_front() {
            if !seen_roots.insert((doc, pointer.clone(), kind, v30)) {
                continue;
            }
            if v30 && matches!(self.ws.documents[doc].version, SpecVersion::Fragment) {
                self.fragment_roots.push((doc, pointer.clone(), kind));
            }
            let Some(start) = self.ws.documents[doc].get(&pointer) else {
                continue;
            };
            let mut sites = vec![];
            walk(start, &pointer, kind, |node| {
                if let Some(r) = node.reference() {
                    sites.push(RefSite {
                        pointer: node.pointer.clone(),
                        kind: node.kind,
                        reference: r.clone(),
                    });
                }
                true
            });
            for site in sites {
                let key = (doc, site.pointer.clone());
                let target = match followed.get(&key) {
                    Some(known) => known.clone(),
                    None => {
                        let target = self.follow(doc, &site);
                        if let Some(t) = &target
                            && site.kind == Kind::Schema
                        {
                            self.schema_targets.insert(t.clone());
                        }
                        followed.insert(key, target.clone());
                        target
                    }
                };
                if let Some(target) = target {
                    // A fragment is read in the dialect of whoever reaches
                    // it; an entry document keeps its own.
                    let target_v30 = match self.ws.documents[target.doc].version {
                        SpecVersion::Fragment => v30,
                        _ => self.dialect30[target.doc],
                    };
                    queue.push_back((target.doc, target.pointer, site.kind, target_v30));
                }
            }
        }
    }

    /// Resolve one `$ref`, loading the file it names when needed.
    fn follow(&mut self, doc: DocId, site: &RefSite) -> Option<RefTarget> {
        let at = Site {
            doc,
            pointer: format!("{}/$ref", site.pointer),
        };
        let Value::String(reference) = &site.reference else {
            self.report(&at, Diagnostic::error("TG0201", "`$ref` must be a string"));
            return None;
        };
        let (target_doc, pointer) = match parse_reference(reference) {
            Err(reason) => {
                self.report(
                    &at,
                    Diagnostic::error("TG0201", format!("invalid $ref \"{reference}\": {reason}")),
                );
                return None;
            }
            Ok(Reference::Local { pointer }) => (doc, pointer),
            Ok(Reference::File { path, pointer }) => {
                (self.load_file(doc, &path, reference, &at)?, pointer)
            }
            Ok(Reference::Remote { url }) => {
                self.report(&at, self.remote_diagnostic(&url));
                return None;
            }
        };
        let target = RefTarget {
            doc: target_doc,
            pointer: normalize::translate(&self.ws.documents[target_doc].moves, &pointer),
        };
        if self.ws.get(&target).is_none() {
            let file = self.ws.name(target_doc).to_string();
            self.report(
                &at,
                Diagnostic::error(
                    "TG0201",
                    format!(
                        "$ref \"{reference}\" does not resolve: {file} has no value at `{pointer}`"
                    ),
                ),
            );
            return None;
        }
        Some(target)
    }

    fn remote_diagnostic(&self, url: &str) -> Diagnostic {
        if self
            .opts
            .allow_remote
            .iter()
            .any(|p| url.starts_with(p.as_str()))
        {
            Diagnostic::error("TG0202", format!("remote $ref \"{url}\" cannot be fetched"))
                .with_help("remote fetching not implemented; vendor the file next to the spec and reference it by relative path")
        } else {
            Diagnostic::error("TG0202", format!("remote $ref \"{url}\" is blocked"))
                .with_help("remote $refs are off by default; vendor the file next to the spec and reference it by relative path (remote fetching not implemented)")
        }
    }

    /// The document for a relative file reference from `from`, loading it
    /// on first use. Enforces the input-root traversal guard.
    fn load_file(&mut self, from: DocId, rel: &str, reference: &str, at: &Site) -> Option<DocId> {
        let Some(base) = self.ws.documents[from]
            .path
            .as_deref()
            .and_then(Path::parent)
        else {
            self.report(
                at,
                Diagnostic::error(
                    "TG0201",
                    format!("$ref \"{reference}\" names a file, but this document was not loaded from a file"),
                ),
            );
            return None;
        };
        let lexical = lexical_normalize(&base.join(rel));
        if let Some(&id) = self.ws.files.get(&lexical) {
            return Some(id);
        }
        let outside = || {
            Diagnostic::error(
                "TG0204",
                format!("$ref \"{reference}\" points outside the input root"),
            )
            .with_help("file references must stay inside the directory of an input document")
        };
        if self.roots.is_empty() {
            self.report(
                at,
                Diagnostic::error(
                    "TG0201",
                    format!("$ref \"{reference}\" names a file, but the input root is unknown"),
                ),
            );
            return None;
        }
        if !self.roots.iter().any(|r| lexical.starts_with(&r.lexical)) {
            self.report(at, outside());
            return None;
        }
        let Ok(canonical) = std::fs::canonicalize(&lexical) else {
            self.report(
                at,
                Diagnostic::error(
                    "TG0201",
                    format!("$ref \"{reference}\" does not resolve: file {rel} does not exist"),
                ),
            );
            return None;
        };
        let Some(root) = self
            .roots
            .iter()
            .find(|r| canonical.starts_with(&r.canonical))
        else {
            self.report(at, outside());
            return None;
        };
        if let Some(&id) = self.ws.files.get(&canonical) {
            self.ws.files.insert(lexical, id);
            return Some(id);
        }
        // A file that failed to load was reported at its first reference.
        if !self.failed_files.insert(canonical.clone()) {
            return None;
        }
        let relative = canonical
            .strip_prefix(&root.canonical)
            .expect("checked above")
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let name = format!("{}{relative}", root.display);
        let (text, digest) = self.read(&canonical, &name, Some(at))?;
        let source = self.ws.sources.add(name.as_str(), text);
        let parsed = self.parse(&relative, source, Some(at))?;
        let mut doc = Document::new(source, name, SpecVersion::Fragment, parsed.value, digest);
        doc.spans = parsed.spans;
        let id = self.push(doc, Some(lexical), false);
        self.ws.files.insert(canonical, id);
        Some(id)
    }

    fn report(&mut self, at: &Site, mut d: Diagnostic) {
        d.labels.push(site_label(&self.ws, at));
        self.ws.diagnostics.push(d);
    }

    /// Normalize the parts of 3.0 fragments that references reached, then
    /// move the schema targets recorded against their original layout.
    fn normalize_fragments(&mut self) {
        if self.fragment_roots.is_empty() {
            return;
        }
        for (doc, pointer, kind) in std::mem::take(&mut self.fragment_roots) {
            let d = &mut self.ws.documents[doc];
            let pointer = normalize::translate(&d.moves, &pointer);
            normalize::normalize(&mut d.root, &mut d.spans, &mut d.moves, &pointer, kind);
        }
        let ws = &self.ws;
        self.schema_targets = std::mem::take(&mut self.schema_targets)
            .into_iter()
            .map(|t| match ws.documents[t.doc].version {
                SpecVersion::Fragment => RefTarget {
                    pointer: normalize::translate(&ws.documents[t.doc].moves, &t.pointer),
                    doc: t.doc,
                },
                _ => t,
            })
            .collect();
    }
}

fn site_label(ws: &Workspace, at: &Site) -> Label {
    let doc = &ws.documents[at.doc];
    Label {
        file: doc.name.clone(),
        pointer: at.pointer.clone(),
        span: doc.span(&at.pointer),
        message: None,
    }
}

/// Absolute, lexically normalized form of `path` (no file-system access).
fn absolute(path: &Path) -> PathBuf {
    lexical_normalize(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// Display name of an overlay: its path relative to the directory the entry's
/// display name is relative to, or its file name when it lies elsewhere.
fn overlay_display(overlay: &Path, entry: &LoadEntry) -> String {
    let display = Path::new(&entry.display_name);
    let entry_path = absolute(&entry.path);
    let base = entry_path
        .ends_with(display)
        .then(|| entry_path.ancestors().nth(display.components().count()))
        .flatten();
    let overlay = absolute(overlay);
    let relative = base
        .and_then(|b| overlay.strip_prefix(b).ok())
        .map(Path::to_path_buf)
        .or_else(|| overlay.file_name().map(PathBuf::from))
        .unwrap_or_default();
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
