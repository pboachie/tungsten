// SPDX-License-Identifier: AGPL-3.0-only
//! One compilation of the workspace, reduced to what the server keeps.
//!
//! The compiler's result (documents, IR) is large, so it is not retained:
//! [`analyze`] runs the frontend once and keeps the diagnostics mapped to
//! editor positions and a small index of operations for completion, hover
//! and go-to-definition. Memory stays bounded by the number of operations,
//! whatever the size of the specs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString, Range,
};
use tungsten_build::suggest::{Action, SUGGESTED_CODES, suggest};
use tungsten_build::{CompileOptions, Compiled, compile_project};
use tungsten_core::{Diagnostic as Tg, Severity, SourceId};
use tungsten_ir::{Ir, OperationStatus};
use tungsten_openapi::{CancellationToken, Workspace};

use crate::position::LineIndex;
use crate::uri::{normalize, path_to_uri};

/// Most diagnostics published for one file; the rest are dropped so a spec
/// with thousands of findings cannot flood the client.
const MAX_PER_FILE: usize = 500;
/// Most overlay fixes kept from one compilation.
const MAX_OVERLAY_FIXES: usize = 1000;

/// What the server knows about one operation.
#[derive(Debug, Clone)]
pub struct OpInfo {
    /// The IR id (`public.createWebhookEndpoint`).
    pub id: String,
    /// `<namespace>.<resource path>.<method>`, the other accepted spelling.
    pub alias: String,
    pub method: String,
    pub path: String,
    pub summary: Option<String>,
    /// `implemented`, `planned` or `gated by NAME`.
    pub status: String,
    pub deprecated: bool,
    /// Where the operation is in its OpenAPI document.
    pub location: Option<Location>,
}

/// Names the compiled project offers to completion.
#[derive(Debug, Clone, Default)]
pub struct Index {
    pub ops: Vec<OpInfo>,
    pub macros: Vec<String>,
    pub gates: Vec<String>,
    pub clusters: Vec<String>,
    pub namespaces: Vec<String>,
    pub auth_schemes: Vec<String>,
}

impl Index {
    /// An operation by id or by resource-path spelling.
    pub fn op(&self, name: &str) -> Option<&OpInfo> {
        self.ops
            .iter()
            .find(|o| o.id == name)
            .or_else(|| self.ops.iter().find(|o| o.alias == name))
    }
}

/// One overlay action `tungsten overlay suggest` would write, with the
/// diagnostics it fixes as the editor shows them.
#[derive(Debug, Clone)]
pub struct OverlayFix {
    /// The input's position in `inputs` of the manifest.
    pub input: usize,
    pub namespace: String,
    /// The `spec` of the input, as written in the manifest.
    pub spec: String,
    /// The `overlays` of the input, as written in the manifest.
    pub overlays: Vec<String>,
    pub action: Action,
    /// Where the fixed diagnostics are shown: file, range and code.
    pub fixes: Vec<(PathBuf, Range, String)>,
}

/// The result of one compilation.
#[derive(Debug, Default)]
pub struct Analysis {
    pub manifest: PathBuf,
    pub agent_file: PathBuf,
    pub overlays: Vec<PathBuf>,
    /// Local profile files the manifest selected or included, as editor paths.
    pub profiles: Vec<PathBuf>,
    /// OpenAPI documents the project read.
    pub inputs: BTreeSet<PathBuf>,
    /// Diagnostics by file, sorted by position.
    pub diagnostics: BTreeMap<PathBuf, Vec<Diagnostic>>,
    /// Present only when the project compiled far enough to build the IR.
    pub index: Option<Index>,
    /// Overlay actions that fix TG0401/TG0403 diagnostics, bounded.
    pub overlay_fixes: Vec<OverlayFix>,
    /// What the compilation read, when its result can be reused while
    /// those inputs are unchanged.
    pub inputs_key: Option<InputKey>,
}

/// The identities of every local input of a compilation: the digest of
/// each source text it read, by path, and the optional files it found
/// absent. A compilation is reused only while each of these is the same
/// in the open buffers and on disk; modification times are not consulted.
#[derive(Debug, Clone)]
pub struct InputKey {
    manifest: PathBuf,
    files: Vec<(PathBuf, tungsten_core::Digest)>,
    absent: Vec<PathBuf>,
}

/// Most bytes read from one input to compare it with an [`InputKey`]; a
/// larger file is treated as changed.
const MAX_COMPARED_FILE: u64 = 64 * 1024 * 1024;

impl InputKey {
    /// Whether every input still has the identity it had, with
    /// `overrides` as the text of unsaved buffers. Cancellation counts as
    /// a change.
    pub fn unchanged(
        &self,
        manifest: &Path,
        overrides: &BTreeMap<PathBuf, String>,
        cancellation: &CancellationToken,
    ) -> bool {
        if normalize(manifest) != self.manifest {
            return false;
        }
        for (path, digest) in &self.files {
            if cancellation.is_cancelled() {
                return false;
            }
            let current = match overrides.get(path) {
                Some(text) => tungsten_core::Digest::of(text.as_bytes()),
                None => {
                    let fits = std::fs::metadata(path)
                        .is_ok_and(|m| m.is_file() && m.len() <= MAX_COMPARED_FILE);
                    match std::fs::read(path) {
                        Ok(bytes) if fits => tungsten_core::Digest::of(&bytes),
                        _ => return false,
                    }
                }
            };
            if current != *digest {
                return false;
            }
        }
        self.absent
            .iter()
            .all(|path| !overrides.contains_key(path) && !path.exists())
    }
}

/// The key of `compiled`, or `None` when its result must not be reused:
/// it has errors (a missing or unreadable input is an error, and creating
/// it must be noticed), or it read a document without a local path.
fn inputs_key(
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
    agent_file: &Path,
) -> Option<InputKey> {
    if compiled.has_errors()
        || compiled
            .workspace
            .documents
            .iter()
            .any(|d| d.path().is_none())
    {
        return None;
    }
    let mut files: Vec<(PathBuf, tungsten_core::Digest)> = compiled
        .workspace
        .sources
        .files()
        .iter()
        .map(|f| {
            let path = file_path(&f.name, Some(f.id), compiled, base, manifest_name, manifest);
            (path, tungsten_core::Digest::of(f.text.as_bytes()))
        })
        .collect();
    files.sort();
    files.dedup_by(|a, b| a.0 == b.0);
    let mut absent = vec![];
    if !files.iter().any(|(p, _)| p == agent_file) {
        absent.push(agent_file.to_path_buf());
    }
    Some(InputKey {
        manifest: manifest.to_path_buf(),
        files,
        absent,
    })
}

/// Compile the project whose manifest is `manifest`, with `overrides` as
/// the text of unsaved buffers.
pub fn analyze(
    manifest: &Path,
    overrides: BTreeMap<PathBuf, String>,
    cancellation: CancellationToken,
) -> Option<Analysis> {
    let mut opts = CompileOptions {
        overrides,
        ..CompileOptions::default()
    };
    opts.load.cancellation = cancellation.clone();
    let compiled = compile_project(manifest, &opts);
    if cancellation.is_cancelled() {
        return None;
    }
    let base = manifest.parent().map(normalize).unwrap_or_default();
    let manifest_name = manifest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = Analysis {
        manifest: normalize(manifest),
        ..Analysis::default()
    };
    let agent_name = compiled
        .config
        .as_ref()
        .and_then(|c| c.agent.clone())
        .unwrap_or_else(|| tungsten_agent::DEFAULT_AGENT_MANIFEST.to_string());
    out.agent_file = normalize(&base.join(&agent_name));
    if let Some(config) = &compiled.config {
        let overlays = config
            .overlays
            .iter()
            .chain(config.inputs.iter().flat_map(|i| i.overlays.iter()));
        out.overlays = overlays.map(|o| normalize(&base.join(o))).collect();
    }
    out.profiles = compiled
        .profile_sources
        .values()
        .map(|path| profile_editor_path(path))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    out.inputs = compiled
        .workspace
        .documents
        .iter()
        .filter_map(|d| d.path().map(normalize))
        .collect();
    out.diagnostics = map_diagnostics(&compiled, &base, &manifest_name, &out.manifest);
    out.index = compiled
        .ir
        .as_ref()
        .map(|ir| index(ir, &compiled.workspace, &base));
    if cancellation.is_cancelled() {
        return None;
    }
    out.overlay_fixes = overlay_fixes(&compiled, &base, &manifest_name, &out.manifest);
    out.inputs_key = inputs_key(
        &compiled,
        &base,
        &manifest_name,
        &out.manifest,
        &out.agent_file,
    );
    (!cancellation.is_cancelled()).then_some(out)
}

/// The overlay actions of [`suggest`], each with the editor placement of
/// the diagnostics it fixes.
fn overlay_fixes(
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> Vec<OverlayFix> {
    let Some(config) = &compiled.config else {
        return vec![];
    };
    let ws = &compiled.workspace;
    let mut lines = Lines::new(ws);
    let mut out = vec![];
    for found in suggest(compiled, &[]).inputs {
        let Some(input) = config
            .inputs
            .iter()
            .position(|i| i.namespace == found.namespace && i.spec == found.spec)
        else {
            continue;
        };
        let doc = ws
            .entry_docs
            .get(input)
            .copied()
            .flatten()
            .and_then(|d| ws.documents.get(d));
        let Some(doc) = doc else { continue };
        for action in found.actions {
            if out.len() >= MAX_OVERLAY_FIXES {
                return out;
            }
            let fixes = compiled
                .diagnostics
                .iter()
                .filter(|d| SUGGESTED_CODES.contains(&d.code.as_str()))
                .filter(|d| action.codes.contains(&d.code))
                .filter(|d| {
                    d.labels
                        .first()
                        .is_some_and(|l| l.file == doc.name && l.pointer == action.pointer)
                })
                .map(|d| {
                    let (path, range) =
                        place(d, &mut lines, compiled, base, manifest_name, manifest);
                    (path, range, d.code.clone())
                })
                .collect();
            out.push(OverlayFix {
                input,
                namespace: found.namespace.clone(),
                spec: found.spec.clone(),
                overlays: config.inputs[input].overlays.clone(),
                action,
                fixes,
            });
        }
    }
    out
}

fn severity(s: Severity) -> DiagnosticSeverity {
    match s {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
    }
}

/// Line indexes of the sources of a compilation, built on first use.
struct Lines<'w> {
    ws: &'w Workspace,
    cache: BTreeMap<SourceId, LineIndex>,
}

impl<'w> Lines<'w> {
    fn new(ws: &'w Workspace) -> Self {
        Self {
            ws,
            cache: BTreeMap::new(),
        }
    }

    fn range(&mut self, id: SourceId, start: u32, end: u32) -> Range {
        let text = self.ws.sources.get(id).text.as_str();
        let index = self.cache.entry(id).or_insert_with(|| LineIndex::new(text));
        index.range(start as usize, end as usize, text)
    }

    /// The first line of the file named `name`, as a range.
    fn first_line(&mut self, name: &str) -> Range {
        let found = self.ws.sources.files().iter().find(|f| f.name == name);
        match found {
            Some(f) => {
                let end = f.line_text(1).len() as u32;
                self.range(f.id, 0, end)
            }
            None => Range::default(),
        }
    }
}

pub(crate) fn map_diagnostics(
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> BTreeMap<PathBuf, Vec<Diagnostic>> {
    let ws = &compiled.workspace;
    let mut lines = Lines::new(ws);
    let mut nodes = Nodes::default();
    let mut out: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    for d in &compiled.diagnostics.0 {
        let (path, range) = place(d, &mut lines, compiled, base, manifest_name, manifest);
        let list = out.entry(path).or_default();
        if list.len() < MAX_PER_FILE {
            let mut converted = convert(
                d,
                range,
                &mut lines,
                compiled,
                base,
                manifest_name,
                manifest,
            );
            let sources = effective_sources(
                d,
                &mut lines,
                &mut nodes,
                compiled,
                base,
                manifest_name,
                manifest,
            );
            if !sources.is_empty() {
                converted
                    .related_information
                    .get_or_insert_with(Vec::new)
                    .extend(sources);
            }
            list.push(converted);
        }
    }
    for list in out.values_mut() {
        list.sort_by_key(|d| (d.range.start.line, d.range.start.character));
    }
    out
}

/// Most effective-source locations added to one diagnostic.
const MAX_EFFECTIVE_SOURCES: usize = 8;

/// YAML node offsets of manifest and profile sources, parsed on first use.
#[derive(Default)]
struct Nodes {
    cache: BTreeMap<SourceId, tungsten_config::ManifestSource>,
}

/// Where the configuration value a diagnostic points at comes from: when
/// its first label is at (or under) a declaration of a composed target
/// option, in the manifest or a profile, or at the option's effective path
/// in the manifest, the other declarations of that option. The winner is
/// the effective value; the others are shadowed by it. Spans come from the
/// compiled source text, so unsaved buffers are reflected.
fn effective_sources(
    d: &Tg,
    lines: &mut Lines<'_>,
    nodes: &mut Nodes,
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> Vec<DiagnosticRelatedInformation> {
    let Some(label) = d.labels.first() else {
        return vec![];
    };
    let mut out = selected_by(d, lines, nodes, compiled, base, manifest_name, manifest);
    let Some(effective) = &compiled.effective else {
        return out;
    };
    let under = |pointer: &str, at: &str| {
        pointer == at
            || pointer
                .strip_prefix(at)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    for (path, field) in &effective.provenance {
        let all = std::iter::once(&field.winner).chain(field.shadowed.iter());
        let own = all
            .clone()
            .position(|decl| decl.file == label.file && under(&label.pointer, &decl.pointer));
        let at_effective_path = label.file == manifest_name && under(&label.pointer, path);
        if own.is_none() && !at_effective_path {
            continue;
        }
        for (i, decl) in all.enumerate() {
            if Some(i) == own || out.len() >= MAX_EFFECTIVE_SOURCES {
                continue;
            }
            let Some(location) =
                declaration_location(decl, lines, nodes, compiled, base, manifest_name, manifest)
            else {
                continue;
            };
            let message = if i == 0 {
                format!("effective value of `{path}` is set here ({})", decl.file)
            } else {
                format!(
                    "`{path}` is also set here ({}), shadowed by the effective value",
                    decl.file
                )
            };
            out.push(DiagnosticRelatedInformation { location, message });
        }
    }
    out
}

/// For a diagnostic in a profile, the manifest's `profiles` entry that
/// selected it, or the whole `profiles` list when the profile was reached
/// through an include.
fn selected_by(
    d: &Tg,
    lines: &mut Lines<'_>,
    nodes: &mut Nodes,
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> Vec<DiagnosticRelatedInformation> {
    let ws = lines.ws;
    let Some(config) = &compiled.config else {
        return vec![];
    };
    let in_profile = d
        .labels
        .first()
        .and_then(|l| {
            l.span.map(|s| s.source).or_else(|| {
                ws.sources
                    .files()
                    .iter()
                    .find(|f| f.name == l.file)
                    .map(|f| f.id)
            })
        })
        .and_then(|id| compiled.profile_sources.get(&id));
    let Some(identity) = in_profile else {
        return vec![];
    };
    let selected = config.profiles.iter().position(|selection| {
        let path = base.join(&selection.path);
        std::fs::canonicalize(&path).ok().as_ref() == Some(identity)
            || normalize(&path) == normalize(identity)
    });
    let (pointer, message) = match selected {
        Some(i) => (
            format!("/profiles/{i}"),
            "the profile is selected here".to_string(),
        ),
        None => (
            "/profiles".to_string(),
            "the profile is included by a profile selected here".to_string(),
        ),
    };
    let declaration = tungsten_config::profiles::Declaration {
        kind: tungsten_config::profiles::SourceKind::Project,
        identity: manifest.to_path_buf(),
        file: manifest_name.to_string(),
        pointer,
        digest: None,
    };
    declaration_location(
        &declaration,
        lines,
        nodes,
        compiled,
        base,
        manifest_name,
        manifest,
    )
    .map(|location| vec![DiagnosticRelatedInformation { location, message }])
    .unwrap_or_default()
}

/// The editor location of a declaration: its source in the compilation
/// (a profile by its physical identity, else by name) and the node at its
/// pointer, to the end of that line.
fn declaration_location(
    decl: &tungsten_config::profiles::Declaration,
    lines: &mut Lines<'_>,
    nodes: &mut Nodes,
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> Option<Location> {
    let ws = lines.ws;
    let id = compiled
        .profile_sources
        .iter()
        .find(|(_, path)| **path == decl.identity)
        .map(|(id, _)| *id)
        .or_else(|| {
            ws.sources
                .files()
                .iter()
                .find(|f| f.name == decl.file)
                .map(|f| f.id)
        })?;
    let file = ws.sources.get(id);
    let source = nodes
        .cache
        .entry(id)
        .or_insert_with(|| tungsten_config::parse_with_source(&file.name, &file.text).source);
    let start = source.offset(&decl.pointer)?;
    let end = file
        .text
        .get(start as usize..)
        .and_then(|t| t.find('\n'))
        .map_or(file.text.len(), |i| start as usize + i) as u32;
    let path = file_path(
        &file.name,
        Some(id),
        compiled,
        base,
        manifest_name,
        manifest,
    );
    let uri = path_to_uri(&path)?;
    Some(Location::new(uri, lines.range(id, start, end)))
}

/// The file and range a diagnostic is shown at: its first label with a
/// span, else its first label's file, else the top of the manifest.
fn place(
    d: &Tg,
    lines: &mut Lines<'_>,
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> (PathBuf, Range) {
    if let Some(span) = d.labels.iter().find_map(|l| l.span) {
        let name = lines.ws.sources.get(span.source).name.clone();
        let path = file_path(
            &name,
            Some(span.source),
            compiled,
            base,
            manifest_name,
            manifest,
        );
        return (path, lines.range(span.source, span.start, span.end));
    }
    match d.labels.first() {
        Some(label) => {
            let path = file_path(&label.file, None, compiled, base, manifest_name, manifest);
            let range = lines.first_line(&label.file);
            (path, range)
        }
        None => (manifest.to_path_buf(), lines.first_line(manifest_name)),
    }
}

fn file_path(
    name: &str,
    source: Option<SourceId>,
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> PathBuf {
    // The SourceId binding is authoritative. Do not interpret an admitted
    // profile's portable display name as a path. A span-less label can still
    // resolve through its captured source; duplicate names were refused before
    // admission. Rejected captures publish the manifest collision diagnostic,
    // so existing manifest/spec fallback remains safe and unchanged.
    let source = source.or_else(|| {
        compiled
            .workspace
            .sources
            .files()
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.id)
    });
    if let Some(path) = source.and_then(|source| compiled.profile_sources.get(&source)) {
        return profile_editor_path(path);
    }
    if name.is_empty() || name == manifest_name {
        manifest.to_path_buf()
    } else {
        normalize(&base.join(name))
    }
}

fn profile_editor_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::path::{Component, Prefix};

        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next() {
            let mut ordinary = match prefix.kind() {
                Prefix::VerbatimDisk(drive) => OsString::from(format!("{}:", char::from(drive))),
                Prefix::VerbatimUNC(server, share) => {
                    let mut ordinary = OsString::from(r"\\");
                    ordinary.push(server);
                    ordinary.push(r"\");
                    ordinary.push(share);
                    ordinary
                }
                _ => return normalize(path),
            };
            ordinary.push(components.as_path().as_os_str());
            return normalize(Path::new(&ordinary));
        }
    }
    normalize(path)
}

fn convert(
    d: &Tg,
    range: Range,
    lines: &mut Lines<'_>,
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> Diagnostic {
    let mut message = d.message.clone();
    if let Some(note) = d.labels.first().and_then(|l| l.message.as_deref()) {
        message.push('\n');
        message.push_str(note);
    }
    if let Some(help) = &d.help {
        message.push_str("\n\nhelp: ");
        message.push_str(help);
    }
    let mut related = vec![];
    for label in d.labels.iter().skip(1) {
        let Some(span) = label.span else { continue };
        let name = lines.ws.sources.get(span.source).name.clone();
        let path = file_path(
            &name,
            Some(span.source),
            compiled,
            base,
            manifest_name,
            manifest,
        );
        if let Some(uri) = path_to_uri(&path) {
            related.push(DiagnosticRelatedInformation {
                location: Location::new(uri, lines.range(span.source, span.start, span.end)),
                message: label
                    .message
                    .clone()
                    .unwrap_or_else(|| "related location".to_string()),
            });
        }
    }
    Diagnostic {
        range,
        severity: Some(severity(d.severity)),
        code: Some(NumberOrString::String(d.code.clone())),
        source: Some("tungsten".to_string()),
        message,
        related_information: (!related.is_empty()).then_some(related),
        ..Diagnostic::default()
    }
}

fn index(ir: &Ir, ws: &Workspace, base: &Path) -> Index {
    let mut lines = Lines::new(ws);
    let mut out = Index::default();
    let mut gates: BTreeSet<String> = ir.agent.gates.keys().cloned().collect();
    for ns in &ir.namespaces {
        out.namespaces.push(ns.name.wire.clone());
        for r in &ns.resources {
            resource(r, &ns.name.wire, ws, base, &mut lines, &mut out);
        }
        for op in &ns.planned {
            out.ops.push(op_info(op, &op.id.0, ws, base, &mut lines));
        }
    }
    for op in ir.operations() {
        if let OperationStatus::Gated { gate } = &op.status {
            gates.insert(gate.env_var.clone());
        }
    }
    out.macros = ir.agent.macros.iter().map(|m| m.name.0.clone()).collect();
    out.clusters = ir.agent.clusters.iter().map(|c| c.name.clone()).collect();
    out.gates = gates.into_iter().collect();
    out.auth_schemes = ir.auth.iter().map(|a| a.name().to_string()).collect();
    out.auth_schemes.dedup();
    out
}

fn resource(
    r: &tungsten_ir::Resource,
    prefix: &str,
    ws: &Workspace,
    base: &Path,
    lines: &mut Lines<'_>,
    out: &mut Index,
) {
    let path = format!("{prefix}.{}", r.name.wire);
    for op in &r.operations {
        let alias = format!("{path}.{}", op.name.wire);
        out.ops.push(op_info(op, &alias, ws, base, lines));
    }
    for c in &r.children {
        resource(c, &path, ws, base, lines, out);
    }
}

fn op_info(
    op: &tungsten_ir::Operation,
    alias: &str,
    ws: &Workspace,
    base: &Path,
    lines: &mut Lines<'_>,
) -> OpInfo {
    let status = match &op.status {
        OperationStatus::Implemented => "implemented".to_string(),
        OperationStatus::Planned { reason } => format!("planned ({reason})"),
        OperationStatus::Gated { gate } => format!("gated by {}", gate.env_var),
    };
    let location = ws
        .documents
        .iter()
        .find(|d| d.name == op.source.file)
        .and_then(|doc| {
            let span = doc.span(&op.source.pointer)?;
            let file = ws.sources.get(span.source);
            let path = normalize(&base.join(&file.name));
            let uri = path_to_uri(&path)?;
            let line_end = file
                .text
                .get(span.start as usize..)
                .and_then(|t| t.find('\n'))
                .map_or(file.text.len(), |i| span.start as usize + i);
            let end = (span.end as usize).min(line_end) as u32;
            Some(Location::new(
                uri,
                lines.range(span.source, span.start, end),
            ))
        });
    OpInfo {
        id: op.id.0.clone(),
        alias: alias.to_string(),
        method: format!("{:?}", op.method).to_uppercase(),
        path: op.path.raw.clone(),
        summary: op.doc.as_ref().and_then(|d| d.summary.clone()),
        status,
        deprecated: op.deprecated,
        location,
    }
}
