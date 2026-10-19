// SPDX-License-Identifier: AGPL-3.0-only
//! File-level compilation driver shared by the CLI and the test harness.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tungsten_agent::{DEFAULT_AGENT_MANIFEST, ParsedAgentManifest};
use tungsten_config::profiles::{
    self, CapturedProfile, Declaration, EffectiveConfiguration, ProfileSource, SourceKind,
};
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

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    pub load: LoadOptions,
    /// Unsaved local text keyed by path. Manifests, specs, overlays and local
    /// references share the same source provider and normal loading limits.
    /// Acquisition pins and reference confinement remain independently enforced.
    pub overrides: BTreeMap<PathBuf, String>,
}

impl CompileOptions {
    fn snapshot(&self) -> Self {
        let mut options = self.clone();
        options.load.sources = options
            .load
            .sources
            .with_overrides(std::mem::take(&mut options.overrides));
        options
    }
}

/// Result of a compilation. `ir` is `None` when loading or the manifest
/// failed with errors. `workspace.sources` maps spans for rendering.
#[derive(Debug)]
pub struct Compiled {
    pub config: Option<TungstenConfig>,
    /// Supported declarations and authored provenance, before emitter defaults.
    /// This in-memory result is not an authorization or replayable manifest.
    pub effective: Option<EffectiveConfiguration>,
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
    let opts = opts.snapshot();
    on_compile_stack(|| compile_project_here(manifest, &opts))
}

fn compile_project_here(manifest: &Path, opts: &CompileOptions) -> Compiled {
    if opts.load.cancellation.is_cancelled() {
        return failed(None, Diagnostics::new(), None);
    }
    let ParsedManifest {
        config,
        diagnostics,
        source,
    } = read_project_manifest(manifest, opts);
    let name = manifest_name(manifest);
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

/// Load a project's manifest and bounded local profile defaults without
/// compiling or acquiring inputs, loading agent content, or running tools.
/// Raw project target values retain their legacy emitter fallback behavior.
pub fn load_project_config(manifest: &Path, opts: &CompileOptions) -> ParsedManifest {
    load_project_configuration(manifest, opts).parsed
}

/// Config-only capture including the authored provenance and profile sources.
/// Inspection consumers must redact raw source/configuration content before export.
pub fn load_project_configuration(
    manifest: &Path,
    opts: &CompileOptions,
) -> crate::inspection::ProjectConfiguration {
    let opts = opts.snapshot();
    if opts.load.cancellation.is_cancelled() {
        return crate::inspection::ProjectConfiguration {
            parsed: ParsedManifest {
                config: None,
                diagnostics: Diagnostics::new(),
                source: tungsten_config::parse_with_source(&manifest_name(manifest), "").source,
            },
            effective: None,
            profile_sources: vec![],
        };
    }
    let mut parsed = read_project_manifest(manifest, &opts);
    let mut effective = None;
    let mut profile_sources = vec![];
    if let Some(mut config) = parsed.config.take() {
        let base = manifest.parent().unwrap_or(Path::new(""));
        let manifest_info = Manifest {
            name: manifest_name(manifest),
            source: &parsed.source,
        };
        let agent = agent_manifest(&config, base, true, &opts);
        let (composition, _, _) = compose_profiles(
            &mut config,
            base,
            Some(&manifest_info),
            agent.as_ref(),
            &opts,
        );
        parsed.diagnostics.extend(composition.diagnostics);
        profile_sources = composition.sources;
        if !parsed.diagnostics.has_errors() && !opts.load.cancellation.is_cancelled() {
            parsed.config = Some(config);
            effective = composition.effective;
        }
    }
    parsed.diagnostics.sort();
    crate::inspection::ProjectConfiguration {
        parsed,
        effective,
        profile_sources,
    }
}

fn manifest_name(manifest: &Path) -> String {
    manifest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_MANIFEST_NAME.to_string())
}

fn read_project_manifest(manifest: &Path, opts: &CompileOptions) -> ParsedManifest {
    let name = manifest_name(manifest);
    match opts.load.sources.read(
        manifest,
        &manifest.display().to_string(),
        opts.load.max_bytes,
    ) {
        Ok((text, _)) => tungsten_config::parse_with_source(&name, &text),
        Err(error) => {
            let mut parsed = tungsten_config::parse_with_source(&name, "");
            parsed.config = None;
            parsed.diagnostics = Diagnostics(vec![error.at(name, "", None)]);
            parsed
        }
    }
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
            (path.is_file() || opts.load.sources.text(&path).is_some()).then(|| AgentInput {
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
    let opts = &opts.snapshot();
    let source = spec.to_string_lossy();
    if tungsten_openapi::acquire::is_remote(&source) {
        // Do not derive names from an unvalidated URL: a query or malformed
        // credential-bearing authority could otherwise leak through validation.
        let config = TungstenConfig::for_single_spec("api", &source);
        return on_compile_stack(|| compile_config_here(config, Path::new("."), None, opts));
    }
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
    let opts = opts.snapshot();
    on_compile_stack(|| compile_config_here(config, base_dir, manifest, &opts))
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
        effective: None,
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
    mut config: TungstenConfig,
    base_dir: &Path,
    manifest: Option<Manifest<'_>>,
    agent: Option<AgentInput>,
    mut diagnostics: Diagnostics,
    opts: &CompileOptions,
) -> Compiled {
    let manifest_source = manifest.as_ref().map(|m| m.source);
    let (composition, mut collision, project) = compose_profiles(
        &mut config,
        base_dir,
        manifest.as_ref(),
        agent.as_ref(),
        opts,
    );
    diagnostics.extend(composition.diagnostics);
    let mut compiled = if diagnostics.has_errors() || opts.load.cancellation.is_cancelled() {
        failed(Some(config), diagnostics, manifest.as_ref())
    } else {
        compile_prepared(
            config,
            base_dir,
            manifest,
            agent,
            diagnostics,
            opts,
            &composition.sources,
        )
    };
    // Reference/overlay loaders can introduce additional display names not
    // spelled directly in the manifest. Refuse a collision before returning IR.
    if !collision
        && composition.sources.iter().any(|profile| {
            compiled
                .workspace
                .sources
                .files()
                .iter()
                .any(|source| source.name == profile.source.name())
        })
    {
        collision = true;
        let mut diagnostic = profile_name_collision(&project);
        if let Some(source) = manifest_source
            && let Some(file) = compiled
                .workspace
                .sources
                .files()
                .iter()
                .find(|file| file.name == project.file)
        {
            diagnostic.labels[0].span = source.span(file.id, "/profiles");
        }
        compiled.diagnostics.push(diagnostic);
        compiled.ir = None;
    }
    for profile in composition.sources.iter().filter(|_| !collision) {
        let id = compiled
            .workspace
            .sources
            .add(profile.source.name(), profile.source.text());
        profile.source.attach_spans(id, &mut compiled.diagnostics);
    }
    if !collision && !opts.load.cancellation.is_cancelled() {
        compiled.effective = composition.effective;
    }
    compiled.diagnostics.sort();
    if let Some(ir) = &mut compiled.ir {
        ir.diagnostics = compiled.diagnostics.0.clone();
    }
    compiled
}

fn compose_profiles(
    config: &mut TungstenConfig,
    base_dir: &Path,
    manifest: Option<&Manifest<'_>>,
    agent: Option<&AgentInput>,
    opts: &CompileOptions,
) -> (profiles::Composition, bool, Declaration) {
    let (file, identity, digest, kind) = manifest.as_ref().map_or_else(
        || {
            (
                DEFAULT_MANIFEST_NAME.to_string(),
                base_dir.join(DEFAULT_MANIFEST_NAME),
                None,
                SourceKind::Invocation,
            )
        },
        |manifest| {
            (
                manifest.name.clone(),
                opts.load
                    .sources
                    .canonical_path(&base_dir.join(&manifest.name))
                    .unwrap_or_else(|| {
                        tungsten_openapi::SourceProvider::identity(&base_dir.join(&manifest.name))
                    }),
                Some(Digest::of(manifest.source.text().as_bytes())),
                SourceKind::Project,
            )
        },
    );
    let project = Declaration {
        kind,
        identity,
        file,
        pointer: "/targets/typescript".into(),
        digest,
    };
    let mut occupied = BTreeSet::from([project.file.clone()]);
    if let Some(agent) = &agent {
        occupied.insert(agent.name.clone());
    }
    for input in &config.inputs {
        occupied.insert(input.spec.clone());
    }
    for overlay in config
        .inputs
        .iter()
        .flat_map(|input| &input.overlays)
        .chain(&config.overlays)
    {
        occupied.insert(overlay.clone());
    }
    let mut composition = profiles::compose(config, base_dir, project.clone(), &ProfileReads(opts));
    let collision = composition
        .sources
        .iter()
        .any(|profile| !occupied.insert(profile.source.name().into()));
    if collision {
        composition.diagnostics = Diagnostics(vec![profile_name_collision(&project)]);
    }
    (composition, collision, project)
}

fn profile_name_collision(project: &Declaration) -> Diagnostic {
    Diagnostic::error("TG0602", "portable profile source name conflicts with another compilation input; rename the profile or input")
        .at(&project.file, "/profiles", None)
}

struct ProfileReads<'a>(&'a CompileOptions);

impl ProfileSource for ProfileReads<'_> {
    fn canonical_root(&self, path: &Path) -> Option<PathBuf> {
        self.canonical_path(path).filter(|root| root.is_dir())
    }
    fn canonical_path(&self, path: &Path) -> Option<PathBuf> {
        self.0.load.sources.canonical_path(path)
    }
    fn read(&self, path: &Path, max_bytes: usize) -> Result<(String, Digest), Diagnostic> {
        self.0.load.sources.read(
            path,
            &path.display().to_string(),
            max_bytes.min(self.0.load.max_bytes),
        )
    }
    fn is_cancelled(&self) -> bool {
        self.0.load.cancellation.is_cancelled()
    }
}

fn compile_prepared(
    config: TungstenConfig,
    base_dir: &Path,
    manifest: Option<Manifest<'_>>,
    agent: Option<AgentInput>,
    mut diagnostics: Diagnostics,
    opts: &CompileOptions,
    profiles: &[CapturedProfile],
) -> Compiled {
    let resolve = |p: &str| -> PathBuf { base_dir.join(p) };
    if opts.load.cancellation.is_cancelled() {
        return failed(Some(config), diagnostics, manifest.as_ref());
    }
    let agent = agent.map(|a| {
        match opts
            .load
            .sources
            .read(&a.path, &a.name, opts.load.max_bytes)
        {
            Ok((text, _)) => tungsten_agent::parse_str(&a.name, &text),
            Err(error) => {
                let mut parsed = tungsten_agent::parse_str(&a.name, "");
                parsed.manifest = None;
                parsed.diagnostics = Diagnostics(vec![error.at(&a.name, "", None)]);
                parsed
            }
        }
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
    let mut load = opts.load.clone();
    load.acquisition_root = if base_dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        base_dir.to_path_buf()
    };
    for input in &config.inputs {
        if opts.load.cancellation.is_cancelled() {
            return failed(Some(config), diagnostics, manifest.as_ref());
        }
        if let Some(source) = &input.source_url {
            // A converted artifact must remain tied to the raw bytes from which
            // it was reviewed. Refreshing only the pin would silently retain an
            // old conversion; require re-import into a fresh project instead.
            let mut policy = load.acquisition.clone();
            policy.refresh = false;
            let verified =
                tungsten_openapi::acquire::acquire(source, &load.acquisition_root, &policy)
                    .and_then(|path| tungsten_openapi::acquire::content_hash(&path))
                    .and_then(|hash| {
                        if input.source_sha256.as_deref() == Some(hash.as_str()) {
                            Ok(())
                        } else {
                            Err(Diagnostic::error(
                                "TG0122",
                                "remote source does not match the SHA256 used for this conversion",
                            ))
                        }
                    });
            if let Err(mut error) = verified {
                if error.code == "TG0122" {
                    error = error.with_help("review the changed source and re-import it into a fresh project, then review imported operations again; use --offline to retain the existing conversion");
                }
                diagnostics.push(error);
            }
        }
    }
    let mut workspace = tungsten_openapi::load(&entries, &load);
    diagnostics.extend(std::mem::take(&mut workspace.diagnostics));
    if opts.load.cancellation.is_cancelled() {
        return Compiled {
            config: Some(config),
            effective: None,
            workspace,
            ir: None,
            diagnostics,
        };
    }

    let mut input_digests = BTreeMap::new();
    for profile in profiles {
        input_digests.insert(profile.source.name().into(), profile.digest.clone());
    }
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
        if opts.load.cancellation.is_cancelled() {
            return Compiled {
                config: Some(config),
                effective: None,
                workspace,
                ir: None,
                diagnostics,
            };
        }
        let mut ir = out.ir;
        let (agent_manifest, agent_name) = match &agent {
            Some(a) => (a.manifest.as_ref(), a.name.as_str()),
            None => (None, DEFAULT_AGENT_MANIFEST),
        };
        let mut found = tungsten_agent::apply(&mut ir, agent_manifest, agent_name);
        attach_spec_spans(&workspace, &mut found);
        diagnostics.extend(found);
        if opts.load.cancellation.is_cancelled() {
            return Compiled {
                config: Some(config),
                effective: None,
                workspace,
                ir: None,
                diagnostics,
            };
        }
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
    if opts.load.cancellation.is_cancelled() {
        ir = None;
    }
    if let Some(ir) = &mut ir {
        ir.diagnostics = diagnostics.0.clone();
    }
    Compiled {
        config: Some(config),
        effective: None,
        workspace,
        ir,
        diagnostics,
    }
}
