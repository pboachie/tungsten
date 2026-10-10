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
use tungsten_build::{CompileOptions, Compiled, compile_project};
use tungsten_core::{Diagnostic as Tg, Severity, SourceId};
use tungsten_ir::{Ir, OperationStatus};
use tungsten_openapi::Workspace;

use crate::position::LineIndex;
use crate::uri::{normalize, path_to_uri};

/// Most diagnostics published for one file; the rest are dropped so a spec
/// with thousands of findings cannot flood the client.
const MAX_PER_FILE: usize = 500;

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

/// The result of one compilation.
#[derive(Debug, Default)]
pub struct Analysis {
    pub manifest: PathBuf,
    pub agent_file: PathBuf,
    pub overlays: Vec<PathBuf>,
    /// OpenAPI documents the project read.
    pub inputs: BTreeSet<PathBuf>,
    /// Diagnostics by file, sorted by position.
    pub diagnostics: BTreeMap<PathBuf, Vec<Diagnostic>>,
    /// Present only when the project compiled far enough to build the IR.
    pub index: Option<Index>,
}

/// Compile the project whose manifest is `manifest`, with `overrides` as
/// the text of unsaved buffers.
pub fn analyze(manifest: &Path, overrides: BTreeMap<PathBuf, String>) -> Analysis {
    let opts = CompileOptions {
        overrides,
        ..CompileOptions::default()
    };
    let compiled = compile_project(manifest, &opts);
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
    out.inputs = compiled
        .workspace
        .documents
        .iter()
        .map(|d| normalize(&base.join(&d.name)))
        .collect();
    out.diagnostics = map_diagnostics(&compiled, &base, &manifest_name, &out.manifest);
    out.index = compiled
        .ir
        .as_ref()
        .map(|ir| index(ir, &compiled.workspace, &base));
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

fn map_diagnostics(
    compiled: &Compiled,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> BTreeMap<PathBuf, Vec<Diagnostic>> {
    let ws = &compiled.workspace;
    let mut lines = Lines::new(ws);
    let mut out: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    for d in &compiled.diagnostics.0 {
        let (path, range) = place(d, &mut lines, base, manifest_name, manifest);
        let list = out.entry(path).or_default();
        if list.len() < MAX_PER_FILE {
            list.push(convert(d, range, &mut lines, base));
        }
    }
    for list in out.values_mut() {
        list.sort_by_key(|d| (d.range.start.line, d.range.start.character));
    }
    out
}

/// The file and range a diagnostic is shown at: its first label with a
/// span, else its first label's file, else the top of the manifest.
fn place(
    d: &Tg,
    lines: &mut Lines<'_>,
    base: &Path,
    manifest_name: &str,
    manifest: &Path,
) -> (PathBuf, Range) {
    if let Some(span) = d.labels.iter().find_map(|l| l.span) {
        let name = lines.ws.sources.get(span.source).name.clone();
        let path = file_path(&name, base, manifest_name, manifest);
        return (path, lines.range(span.source, span.start, span.end));
    }
    match d.labels.first() {
        Some(label) => {
            let path = file_path(&label.file, base, manifest_name, manifest);
            let range = lines.first_line(&label.file);
            (path, range)
        }
        None => (manifest.to_path_buf(), lines.first_line(manifest_name)),
    }
}

fn file_path(name: &str, base: &Path, manifest_name: &str, manifest: &Path) -> PathBuf {
    if name.is_empty() || name == manifest_name {
        manifest.to_path_buf()
    } else {
        normalize(&base.join(name))
    }
}

fn convert(d: &Tg, range: Range, lines: &mut Lines<'_>, base: &Path) -> Diagnostic {
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
        let path = normalize(&base.join(&name));
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
            let path = normalize(&base.join(&doc.name));
            let uri = path_to_uri(&path)?;
            let file = ws.sources.get(span.source);
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
