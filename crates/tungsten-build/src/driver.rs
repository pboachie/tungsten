// SPDX-License-Identifier: AGPL-3.0-only
//! File-level compilation driver shared by the CLI and the test harness.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use tungsten_agent::{DEFAULT_AGENT_MANIFEST, ParsedAgentManifest};
use tungsten_config::{ManifestSource, ParsedManifest, TungstenConfig};
use tungsten_core::{Diagnostic, Diagnostics, Digest};
use tungsten_ir::Ir;
use tungsten_openapi::{LoadEntry, LoadOptions, Workspace};

use crate::{
    BuildInput, DEFAULT_MANIFEST_NAME, NamespaceInput, TUNGSTEN_VERSION, build_with_manifest,
};

/// Stack of the thread a compilation runs on. Building a type recurses
/// once per named type of a `$ref` chain; [`crate::types::MAX_BUILD_DEPTH`]
/// bounds the chain far below what this holds, whatever stack the caller
/// has. Only the pages actually touched are committed.
const COMPILE_STACK_BYTES: usize = 256 * 1024 * 1024;

/// Run `f` on a thread with [`COMPILE_STACK_BYTES`] of stack, or on the
/// current thread when no such thread can be started. A panic in `f`
/// resumes on the caller.
fn on_compile_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    let job = std::sync::Mutex::new(Some(f));
    let run = || {
        let f = job.lock().ok().and_then(|mut j| j.take());
        f.map(|f| f())
    };
    let spawned = std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("tungsten-compile".into())
            .stack_size(COMPILE_STACK_BYTES)
            .spawn_scoped(s, run)
            .ok()
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
    });
    match spawned.flatten() {
        Some(out) => out,
        None => run().expect("the compilation job runs exactly once"),
    }
}

/// The override for `path`: an exact key, else a key naming the same file
/// once `.` and `..` segments are resolved.
fn override_text<'a>(opts: &'a CompileOptions, path: &Path) -> Option<&'a String> {
    fn lexical(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for c in path.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() {
                        out.push("..");
                    }
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
    opts.overrides.get(path).or_else(|| {
        let want = lexical(path);
        opts.overrides
            .iter()
            .find(|(k, _)| lexical(k) == want)
            .map(|(_, v)| v)
    })
}

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    pub load: LoadOptions,
    /// Text that replaces the contents of a file on disk, keyed by the path
    /// the compiler would read: the manifest as given to
    /// [`compile_project`], and the agent manifest as that path's directory
    /// joined with its name. Lets an editor check unsaved buffers. Only the
    /// two manifests are consulted; specs and overlays are read from disk.
    pub overrides: BTreeMap<PathBuf, String>,
}

/// Result of a compilation. `ir` is `None` when loading or the manifest
/// failed with errors. `workspace.sources` maps spans for rendering.
#[derive(Debug)]
pub struct Compiled {
    pub config: Option<TungstenConfig>,
    pub workspace: Workspace,
    pub ir: Option<Ir>,
    /// All diagnostics (manifest, frontend, builder), sorted.
    pub diagnostics: Diagnostics,
}

impl Compiled {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.has_errors()
    }
}

/// Compile a project from its `tungsten.yml`. Paths in the manifest are
/// relative to the manifest's directory. Diagnostics name the manifest by
/// its file name, as specs are named relative to that directory, and carry
/// spans into its text, which is added to `workspace.sources`.
///
/// The agent manifest is the file named by the manifest's `agent` key, or
/// `agent.yml` next to the manifest when that exists. It is named in
/// diagnostics and input digests by that path, its text is added to
/// `workspace.sources`, and it is applied after the build
/// ([`tungsten_agent::apply`]). An agent manifest with errors stops the
/// compilation before the build, like a manifest with errors.
pub fn compile_project(manifest: &Path, opts: &CompileOptions) -> Compiled {
    on_compile_stack(|| compile_project_here(manifest, opts))
}

fn compile_project_here(manifest: &Path, opts: &CompileOptions) -> Compiled {
    let ParsedManifest {
        config,
        mut diagnostics,
        source,
    } = match override_text(opts, manifest) {
        Some(text) => tungsten_config::parse_with_source(&manifest.display().to_string(), text),
        None => tungsten_config::load_with_source(manifest),
    };
    let name = manifest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_MANIFEST_NAME.to_string());
    for label in diagnostics.0.iter_mut().flat_map(|d| d.labels.iter_mut()) {
        if label.file == source.name() {
            label.file = name.clone();
        }
    }
    let manifest_info = Manifest {
        name,
        source: &source,
    };
    let Some(config) = config else {
        return failed(None, diagnostics, Some(&manifest_info));
    };
    let base = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let agent = agent_manifest(&config, &base, true, opts);
    compile_validated(config, &base, Some(manifest_info), agent, diagnostics, opts)
}

/// Where the agent manifest of a project is: the `agent` key, else
/// `agent.yml` in `base` when `implicit` and the file exists (or has an
/// override).
fn agent_manifest(
    config: &TungstenConfig,
    base: &Path,
    implicit: bool,
    opts: &CompileOptions,
) -> Option<AgentInput> {
    match &config.agent {
        Some(path) => Some(AgentInput {
            name: path.clone(),
            path: base.join(path),
        }),
        None if implicit => {
            let path = base.join(DEFAULT_AGENT_MANIFEST);
            (path.is_file() || override_text(opts, &path).is_some()).then(|| AgentInput {
                name: DEFAULT_AGENT_MANIFEST.to_string(),
                path,
            })
        }
        None => None,
    }
}

/// The agent manifest of a compilation: how it is named and where it is.
struct AgentInput {
    name: String,
    path: PathBuf,
}

/// Compile a single OpenAPI file with a default manifest whose namespace is
/// the file stem: lowercased, non-alphanumerics replaced by `_`, and
/// prefixed with `api_` when it does not start with a letter. No agent
/// manifest is read: the spec's `x-agent-*` extensions and the defaults
/// apply.
pub fn compile_spec(spec: &Path, opts: &CompileOptions) -> Compiled {
    let stem = spec
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ns = namespace_for_stem(&stem);
    let base = spec.parent().map(Path::to_path_buf).unwrap_or_default();
    let file = spec
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let config = TungstenConfig::for_single_spec(&ns, &file);
    on_compile_stack(|| compile_config_here(config, &base, None, opts))
}

fn namespace_for_stem(stem: &str) -> String {
    let ns: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    match ns.chars().next() {
        Some(c) if c.is_ascii_lowercase() => ns,
        Some(_) => format!("api_{ns}"),
        None => "api".into(),
    }
}

/// Compile from an already parsed manifest. The configuration is checked
/// with [`tungsten_config::validate`] first. `manifest` is the manifest's
/// name and bytes, when it came from a file: its digest is recorded and
/// its diagnostics get spans. The agent manifest is the `agent` key's file,
/// relative to `base_dir`, or, when `manifest` is given, `agent.yml` in
/// `base_dir` if it exists.
pub fn compile_config(
    config: TungstenConfig,
    base_dir: &Path,
    manifest: Option<(&str, &[u8])>,
    opts: &CompileOptions,
) -> Compiled {
    on_compile_stack(|| compile_config_here(config, base_dir, manifest, opts))
}

fn compile_config_here(
    config: TungstenConfig,
    base_dir: &Path,
    manifest: Option<(&str, &[u8])>,
    opts: &CompileOptions,
) -> Compiled {
    let source = manifest.map(|(name, bytes)| {
        tungsten_config::parse_with_source(name, &String::from_utf8_lossy(bytes)).source
    });
    let name = manifest.map_or(DEFAULT_MANIFEST_NAME, |(name, _)| name);
    let manifest = source.as_ref().map(|source| Manifest {
        name: name.to_string(),
        source,
    });
    let diagnostics = tungsten_config::validate(&config, name, source.as_ref());
    if diagnostics.has_errors() {
        return failed(Some(config), diagnostics, manifest.as_ref());
    }
    let agent = agent_manifest(&config, base_dir, manifest.is_some(), opts);
    compile_validated(config, base_dir, manifest, agent, diagnostics, opts)
}

/// The manifest a compilation came from.
struct Manifest<'m> {
    /// How diagnostics and the input digests name it.
    name: String,
    source: &'m ManifestSource,
}

/// A compilation that stopped at the manifest.
fn failed(
    config: Option<TungstenConfig>,
    mut diagnostics: Diagnostics,
    manifest: Option<&Manifest<'_>>,
) -> Compiled {
    let mut workspace = Workspace::default();
    if let Some(manifest) = manifest {
        attach_manifest(&mut workspace, manifest, &mut diagnostics);
    }
    diagnostics.sort();
    Compiled {
        config,
        workspace,
        ir: None,
        diagnostics,
    }
}

/// Add the manifest text to the source map and give the labels that point
/// into the manifest their spans.
fn attach_manifest(
    workspace: &mut Workspace,
    manifest: &Manifest<'_>,
    diagnostics: &mut Diagnostics,
) {
    let text = manifest.source.text();
    if text.is_empty() {
        return;
    }
    let id = workspace.sources.add(&manifest.name, text);
    for label in diagnostics.0.iter_mut().flat_map(|d| d.labels.iter_mut()) {
        if label.span.is_none() && label.file == manifest.name {
            label.span = manifest.source.span(id, &label.pointer);
        }
    }
}

/// Add the agent manifest text to the source map and give the labels that
/// point into it their spans.
fn attach_agent_manifest(
    workspace: &mut Workspace,
    agent: &ParsedAgentManifest,
    diagnostics: &mut Diagnostics,
) {
    if agent.text.is_empty() {
        return;
    }
    let id = workspace.sources.add(&agent.name, &agent.text);
    for label in diagnostics.0.iter_mut().flat_map(|d| d.labels.iter_mut()) {
        if label.span.is_none() && label.file == agent.name {
            label.span = agent.source.span(id, &label.pointer);
        }
    }
}

/// Give labels that point into a loaded document (the agent transform's
/// diagnostics about `x-agent-*` extensions) the span of that node.
fn attach_spec_spans(workspace: &Workspace, diagnostics: &mut Diagnostics) {
    for label in diagnostics.0.iter_mut().flat_map(|d| d.labels.iter_mut()) {
        if label.span.is_none()
            && let Some(doc) = workspace.documents.iter().find(|d| d.name == label.file)
        {
            label.span = doc.span(&label.pointer);
        }
    }
}

/// Compile a configuration that already passed validation.
fn compile_validated(
    config: TungstenConfig,
    base_dir: &Path,
    manifest: Option<Manifest<'_>>,
    agent: Option<AgentInput>,
    mut diagnostics: Diagnostics,
    opts: &CompileOptions,
) -> Compiled {
    let resolve = |p: &str| -> PathBuf { base_dir.join(p) };
    let agent = agent.map(|a| match override_text(opts, &a.path) {
        Some(text) => tungsten_agent::parse_str(&a.name, text),
        None => tungsten_agent::load_as(&a.path, &a.name),
    });
    if let Some(a) = &agent {
        diagnostics.extend(a.diagnostics.clone());
    }
    let entries: Vec<LoadEntry> = config
        .inputs
        .iter()
        .map(|i| LoadEntry {
            path: resolve(&i.spec),
            display_name: i.spec.clone(),
            overlays: i
                .overlays
                .iter()
                .chain(config.overlays.iter())
                .map(|o| resolve(o))
                .collect(),
        })
        .collect();
    let mut workspace = tungsten_openapi::load(&entries, &opts.load);
    diagnostics.extend(std::mem::take(&mut workspace.diagnostics));

    let mut input_digests = BTreeMap::new();
    if let Some(m) = &manifest {
        input_digests.insert(m.name.clone(), Digest::of(m.source.text().as_bytes()));
    }
    if let Some(a) = agent.as_ref().filter(|a| a.manifest.is_some()) {
        input_digests.insert(a.name.clone(), Digest::of(a.text.as_bytes()));
    }
    for d in &workspace.documents {
        input_digests.insert(d.name.clone(), d.digest.clone());
    }
    // Overlay digests are those of the bytes the loader read and applied.
    for i in config.inputs.iter() {
        for o in i.overlays.iter().chain(config.overlays.iter()) {
            if let Some(digest) = workspace.overlay_digests.get(&resolve(o)) {
                input_digests.insert(o.clone(), digest.clone());
            }
        }
    }

    // Entries come back one per input, in input order (the same spec may
    // be listed by several namespaces, each with its own overlays).
    let mut namespaces = vec![];
    for (idx, input) in config.inputs.iter().enumerate() {
        match workspace.entry_docs.get(idx).copied().flatten() {
            Some(doc) => namespaces.push(NamespaceInput {
                config_index: idx,
                doc,
            }),
            None if !diagnostics.has_errors() => diagnostics.push(
                Diagnostic::error(
                    "TG0101",
                    format!(
                        "input {} for namespace {} was not loaded",
                        input.spec, input.namespace
                    ),
                )
                .at(input.spec.clone(), "", None),
            ),
            None => {}
        }
    }

    let mut ir = if diagnostics.has_errors() {
        None
    } else {
        let manifest_name = manifest
            .as_ref()
            .map_or(DEFAULT_MANIFEST_NAME, |m| m.name.as_str());
        let out = build_with_manifest(
            &BuildInput {
                config: &config,
                workspace: &workspace,
                namespaces,
                tungsten_version: TUNGSTEN_VERSION.to_string(),
                input_digests,
            },
            manifest_name,
        );
        diagnostics.extend(out.diagnostics);
        let mut ir = out.ir;
        let (agent_manifest, agent_name) = match &agent {
            Some(a) => (a.manifest.as_ref(), a.name.as_str()),
            None => (None, DEFAULT_AGENT_MANIFEST),
        };
        let mut found = tungsten_agent::apply(&mut ir, agent_manifest, agent_name);
        attach_spec_spans(&workspace, &mut found);
        diagnostics.extend(found);
        if config.types.prune_unreferenced {
            crate::prune::prune_unreferenced(&mut ir, &mut diagnostics);
        }
        Some(ir)
    };
    if let Some(m) = &manifest {
        attach_manifest(&mut workspace, m, &mut diagnostics);
    }
    if let Some(a) = &agent {
        attach_agent_manifest(&mut workspace, a, &mut diagnostics);
    }
    diagnostics.sort();
    if let Some(ir) = &mut ir {
        ir.diagnostics = diagnostics.0.clone();
    }
    Compiled {
        config: Some(config),
        workspace,
        ir,
        diagnostics,
    }
}
