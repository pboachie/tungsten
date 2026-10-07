// SPDX-License-Identifier: AGPL-3.0-only
//! Resolving the PATH argument and compiling it.
//!
//! - a directory: its `tungsten.yml` (or `tungsten.yaml`);
//! - a file named `tungsten*.yml` / `tungsten*.yaml`: that manifest;
//! - anything else: a single OpenAPI document with a default manifest.

use std::path::{Component, Path, PathBuf};

use tungsten_build::{CompileOptions, Compiled, compile_project, compile_spec};
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_openapi::Workspace;

const MANIFEST_NAMES: [&str; 2] = ["tungsten.yml", "tungsten.yaml"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Input {
    Project(PathBuf),
    Spec(PathBuf),
}

/// Classify a PATH argument. Fails with TG0101 for a directory without a
/// manifest.
pub(crate) fn resolve(path: &Path) -> Result<Input, Diagnostic> {
    if path.is_dir() {
        return MANIFEST_NAMES
            .iter()
            .map(|name| join(path, name))
            .find(|p| p.is_file())
            .map(Input::Project)
            .ok_or_else(|| {
                let shown = path.display().to_string();
                Diagnostic::error("TG0101", format!("no tungsten.yml in directory {shown}"))
                    .at(shown.clone(), "", None)
                    .with_help(format!(
                        "run `tungsten init --dir {shown}`, or pass an OpenAPI file"
                    ))
            });
    }
    if is_manifest_name(path) {
        Ok(Input::Project(path.to_path_buf()))
    } else {
        Ok(Input::Spec(path.to_path_buf()))
    }
}

fn is_manifest_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    name.starts_with("tungsten") && (name.ends_with(".yml") || name.ends_with(".yaml"))
}

/// `dir/name`, without a leading `./` when `dir` is the current directory,
/// so diagnostics show the path the way the user would type it.
pub(crate) fn join(dir: &Path, name: &str) -> PathBuf {
    if dir.components().all(|c| c == Component::CurDir) {
        PathBuf::from(name)
    } else {
        dir.join(name)
    }
}

/// Compile whatever PATH names.
pub(crate) fn compile(path: &Path) -> Compiled {
    let opts = CompileOptions::default();
    match resolve(path) {
        Ok(Input::Project(manifest)) => compile_project(&manifest, &opts),
        Ok(Input::Spec(spec)) => compile_spec(&spec, &opts),
        Err(diagnostic) => Compiled {
            config: None,
            workspace: Workspace::default(),
            ir: None,
            diagnostics: Diagnostics(vec![diagnostic]),
        },
    }
}
