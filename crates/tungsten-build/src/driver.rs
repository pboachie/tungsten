// SPDX-License-Identifier: AGPL-3.0-only
//! File-level compilation driver shared by the CLI and the test harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
pub fn compile_project(manifest: &Path, opts: &CompileOptions) -> Compiled {
    on_compile_stack(|| compile_project_here(manifest, opts))
}

fn compile_project_here(manifest: &Path, opts: &CompileOptions) -> Compiled {
    let ParsedManifest {
        config,
        mut diagnostics,
        source,
    } = tungsten_config::load_with_source(manifest);
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
    compile_validated(config, &base, Some(manifest_info), diagnostics, opts)
}

/// Compile a single OpenAPI file with a default manifest whose namespace is
/// the file stem: lowercased, non-alphanumerics replaced by `_`, and
/// prefixed with `api_` when it does not start with a letter.
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
/// its diagnostics get spans.
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
    compile_validated(config, base_dir, manifest, diagnostics, opts)
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

/// Compile a configuration that already passed validation.
fn compile_validated(
    config: TungstenConfig,
    base_dir: &Path,
    manifest: Option<Manifest<'_>>,
    mut diagnostics: Diagnostics,
    opts: &CompileOptions,
) -> Compiled {
    let resolve = |p: &str| -> PathBuf { base_dir.join(p) };
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
        Some(out.ir)
    };
    if let Some(m) = &manifest {
        attach_manifest(&mut workspace, m, &mut diagnostics);
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
