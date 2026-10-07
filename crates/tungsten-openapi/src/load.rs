// SPDX-License-Identifier: AGPL-3.0-only
//! Loading: entry documents, overlays, version checks, normalization,
//! `$ref` resolution across files and the reference graph.

use std::collections::{BTreeSet, HashSet, VecDeque};
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
        loader.load_entry(entry);
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
        if let Some(parsed) = loader.parse(name, source, None) {
            loader.add_entry(name, None, parsed, source, digest, &[]);
        }
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
fn read_limited(path: &Path, name: &str, max: usize) -> Result<(String, Digest), Diagnostic> {
    let cannot_read =
        |e: std::io::Error| Diagnostic::error("TG0101", format!("cannot read {name}: {e}"));
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

/// The input root: file references must stay inside it.
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
    root: Option<Root>,
    /// Per document: whether it holds OpenAPI 3.0 schemas (entries declared
    /// 3.0, fragments inherit from the document that first referenced them).
    dialect30: Vec<bool>,
    /// Subtrees of 3.0 fragments reached by references, normalized after
    /// resolution.
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
            root: None,
            dialect30: vec![],
            fragment_roots: vec![],
            schema_targets: BTreeSet::new(),
            failed_files: BTreeSet::new(),
        }
    }

    fn load_entry(&mut self, entry: &LoadEntry) {
        let name = entry.display_name.as_str();
        let Some((text, digest)) = self.read(&entry.path, name, None) else {
            return;
        };
        let lexical = absolute(&entry.path);
        if self.root.is_none()
            && let (Some(dir), Ok(canonical)) = (lexical.parent(), std::fs::canonicalize(&lexical))
            && let Some(canonical_dir) = canonical.parent()
        {
            let display = match name.rfind('/') {
                Some(i) => name[..=i].to_string(),
                None => String::new(),
            };
            self.root = Some(Root {
                lexical: dir.to_path_buf(),
                canonical: canonical_dir.to_path_buf(),
                display,
            });
        }
        let source = self.ws.sources.add(name, text);
        let Some(parsed) = self.parse(&entry.path.to_string_lossy(), source, None) else {
            return;
        };
        let overlays: Vec<(PathBuf, String)> = entry
            .overlays
            .iter()
            .map(|o| (o.clone(), overlay_display(o, entry)))
            .collect();
        self.add_entry(name, Some(lexical), parsed, source, digest, &overlays);
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
    ) {
        let Parsed {
            value: mut root,
            mut spans,
        } = parsed;
        for (overlay_path, overlay_name) in overlays {
            let Some((text, _)) = self.read(overlay_path, overlay_name, None) else {
                continue;
            };
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
            Ok(v) => v,
            Err(errors) => {
                for e in errors {
                    self.ws.diagnostics.push(e);
                }
                return;
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
    /// files, and report the references that cannot be followed.
    fn resolve_all(&mut self) {
        let mut queue: VecDeque<(DocId, String, Kind)> = self
            .ws
            .entries
            .iter()
            .map(|&d| (d, String::new(), Kind::Document))
            .collect();
        let mut seen_roots = HashSet::new();
        let mut seen_sites = HashSet::new();
        while let Some((doc, pointer, kind)) = queue.pop_front() {
            if !seen_roots.insert((doc, pointer.clone(), kind)) {
                continue;
            }
            if self.dialect30[doc]
                && matches!(self.ws.documents[doc].version, SpecVersion::Fragment)
            {
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
                if !seen_sites.insert((doc, site.pointer.clone())) {
                    continue;
                }
                if let Some(target) = self.follow(doc, &site) {
                    if site.kind == Kind::Schema {
                        self.schema_targets.insert(target.clone());
                    }
                    queue.push_back((target.doc, target.pointer, site.kind));
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
            .with_help("file references must stay inside the directory of the first input document")
        };
        let Some(root) = self.root.as_ref() else {
            self.report(
                at,
                Diagnostic::error(
                    "TG0201",
                    format!("$ref \"{reference}\" names a file, but the input root is unknown"),
                ),
            );
            return None;
        };
        if !lexical.starts_with(&root.lexical) {
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
        if !canonical.starts_with(&root.canonical) {
            self.report(at, outside());
            return None;
        }
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
        let v30 = self.dialect30[from];
        let id = self.push(doc, Some(lexical), v30);
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
