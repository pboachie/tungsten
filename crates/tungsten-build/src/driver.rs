// SPDX-License-Identifier: AGPL-3.0-only
//! File-level compilation driver shared by the CLI and the test harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tungsten_config::TungstenConfig;
use tungsten_core::{Diagnostic, Diagnostics, Digest};
use tungsten_ir::Ir;
use tungsten_openapi::{LoadEntry, LoadOptions, Workspace};

use crate::{BuildInput, NamespaceInput, TUNGSTEN_VERSION, build};

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
/// relative to the manifest's directory.
pub fn compile_project(manifest: &Path, opts: &CompileOptions) -> Compiled {
    let (config, mut diags) = tungsten_config::load(manifest);
    let Some(config) = config else {
        diags.sort();
        return Compiled {
            config: None,
            workspace: Workspace::default(),
            ir: None,
            diagnostics: diags,
        };
    };
    let base = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = manifest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = std::fs::read(manifest).unwrap_or_default();
    let mut c = compile_config(config, &base, Some((&name, &bytes)), opts);
    c.diagnostics.extend(diags);
    c.diagnostics.sort();
    c
}

/// Compile a single OpenAPI file with a default manifest whose namespace is
/// the file stem (lowercased, non-alphanumerics replaced by `_`).
pub fn compile_spec(spec: &Path, opts: &CompileOptions) -> Compiled {
    let stem = spec
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "api".into());
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
    let base = spec.parent().map(Path::to_path_buf).unwrap_or_default();
    let file = spec
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let config = TungstenConfig::for_single_spec(&ns, &file);
    compile_config(config, &base, None, opts)
}

/// Compile from an already parsed manifest.
pub fn compile_config(
    config: TungstenConfig,
    base_dir: &Path,
    manifest: Option<(&str, &[u8])>,
    opts: &CompileOptions,
) -> Compiled {
    let mut diagnostics = Diagnostics::new();
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
    if let Some((name, bytes)) = manifest {
        input_digests.insert(name.to_string(), Digest::of(bytes));
    }
    for d in &workspace.documents {
        input_digests.insert(d.name.clone(), d.digest.clone());
    }
    for i in config.inputs.iter() {
        for o in i.overlays.iter().chain(config.overlays.iter()) {
            if let Ok(bytes) = std::fs::read(resolve(o)) {
                input_digests.insert(o.clone(), Digest::of(&bytes));
            }
        }
    }

    // Match entries back to their config inputs by display name.
    let mut namespaces = vec![];
    for (idx, input) in config.inputs.iter().enumerate() {
        match workspace
            .entries
            .iter()
            .find(|&&d| workspace.documents[d].name == input.spec)
        {
            Some(&doc) => namespaces.push(NamespaceInput {
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

    let ir = if diagnostics.has_errors() {
        None
    } else {
        let out = build(&BuildInput {
            config: &config,
            workspace: &workspace,
            namespaces,
            tungsten_version: TUNGSTEN_VERSION.to_string(),
            input_digests,
        });
        diagnostics.extend(out.diagnostics);
        let mut ir = out.ir;
        diagnostics.sort();
        ir.diagnostics = diagnostics.0.clone();
        Some(ir)
    };
    diagnostics.sort();
    Compiled {
        config: Some(config),
        workspace,
        ir,
        diagnostics,
    }
}
