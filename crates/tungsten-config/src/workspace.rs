// SPDX-License-Identifier: AGPL-3.0-only
//! Discovery of the tungsten projects under one workspace directory.
//!
//! A project is a directory holding exactly one manifest (`tungsten.yml` or
//! `tungsten.yaml`). [`discover`] walks the workspace breadth first, in
//! byte order of the entry names, so the same tree always yields the same
//! projects in the same order. The walk is bounded ([`DiscoveryLimits`]):
//! it never follows a symbolic link, skips hidden directories, dependency
//! and build directories and the output directories of the projects found
//! above, and stops with `TG0640` rather than return a partial list when a
//! limit is reached.
//!
//! A project's id is its directory relative to the workspace root with `/`
//! separators (`.` for the root itself), which is unique by construction.
//! The catalog keeps the dependency evidence each manifest declares (local
//! specs, overlays, profiles and the agent manifest) so editors and the
//! command line can route a file to the projects that read it, and it
//! reports output directories that two projects share or nest (`TG0642`)
//! before anything is generated.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use tungsten_core::{Diagnostic, Diagnostics};

use crate::TungstenConfig;

/// File names a project manifest may have, in order of preference.
pub const MANIFEST_NAMES: [&str; 2] = ["tungsten.yml", "tungsten.yaml"];

/// Directory names discovery never enters: dependencies, build output and
/// version-control metadata. Hidden directories (a leading `.`) are skipped
/// too.
pub const SKIPPED_DIRECTORIES: &[&str] = &["node_modules", "target", "vendor", "__pycache__"];

/// Bounds of one discovery walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DiscoveryLimits {
    /// Deepest directory searched, the root being depth 0. Deeper
    /// directories are not searched; this is not an error.
    pub max_depth: usize,
    /// Most directories read; reaching it is `TG0640`.
    pub max_directories: usize,
    /// Most projects; finding more is `TG0640`.
    pub max_projects: usize,
    /// Largest manifest read, in bytes; a larger one is `TG0640`.
    pub max_manifest_bytes: u64,
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        DEFAULT_LIMITS
    }
}

/// The limits used by the command line and the language server.
pub const DEFAULT_LIMITS: DiscoveryLimits = DiscoveryLimits {
    max_depth: 8,
    max_directories: 4096,
    max_projects: 64,
    max_manifest_bytes: 1024 * 1024,
};

/// One output directory of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ProjectOutput {
    /// The target name in `targets`.
    pub target: String,
    /// The `out` option as written (default `generated/<target>`).
    pub out: String,
    /// `out` resolved against the project directory, lexically normalized.
    #[serde(skip)]
    pub dir: PathBuf,
}

/// One discovered project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct Project {
    /// The project directory relative to the workspace root, with `/`
    /// separators; `.` is the root itself.
    pub id: String,
    /// The manifest, relative to the workspace root.
    pub manifest: String,
    /// `api.name`, when the manifest could be read.
    pub name: Option<String>,
    /// Whether the manifest parsed and validated; `check` reports why not.
    pub valid: bool,
    /// Output directories of the configured targets, in manifest order.
    pub outputs: Vec<ProjectOutput>,
    /// Local files the manifest names (specs, overlays, profiles and the
    /// agent manifest), relative to the workspace root, sorted. Files only
    /// reached through `$ref` are known after compiling.
    pub inputs: Vec<String>,
    /// Absolute project directory.
    #[serde(skip)]
    pub dir: PathBuf,
    /// Absolute manifest path.
    #[serde(skip)]
    pub manifest_path: PathBuf,
    /// Absolute, lexically normalized paths of `inputs`.
    #[serde(skip)]
    pub input_paths: BTreeSet<PathBuf>,
}

/// The projects of one workspace directory.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// Absolute workspace root.
    pub root: PathBuf,
    /// In discovery order: breadth first, entries in byte order.
    pub projects: Vec<Project>,
    /// TG0640-TG0643 found while discovering, including every collision.
    pub diagnostics: Diagnostics,
    /// TG0642: output directories shared or nested across two projects.
    pub collisions: Vec<Collision>,
}

/// Two projects whose targets write the same or nested directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// Ids of the two projects, in discovery order.
    pub projects: [String; 2],
    pub diagnostic: Diagnostic,
}

/// Find the projects under `root` (a directory). Paths in diagnostics are
/// relative to `root`.
pub fn discover(root: &Path, limits: DiscoveryLimits) -> Catalog {
    let root = normalize(&absolute(root));
    let mut catalog = Catalog {
        root: root.clone(),
        ..Catalog::default()
    };
    let mut queue = VecDeque::from([(root.clone(), 0usize)]);
    let mut read = 0usize;
    // Output directories of the projects found so far; never searched.
    let mut outputs: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some((dir, depth)) = queue.pop_front() {
        if read == limits.max_directories {
            catalog.diagnostics.push(limit(
                &catalog.root,
                &dir,
                format!(
                    "workspace discovery stopped after reading {} directories; projects below {} were not searched",
                    limits.max_directories,
                    shown(&catalog.root, &dir)
                ),
            ));
            break;
        }
        read += 1;
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<(String, PathBuf)> = entries
            .filter_map(Result::ok)
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        names.sort();
        let mut manifests = vec![];
        let mut children = vec![];
        for (name, path) in names {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let is_manifest = MANIFEST_NAMES.contains(&name.as_str());
            if meta.file_type().is_symlink() {
                if is_manifest {
                    catalog.diagnostics.push(
                        Diagnostic::warning(
                            "TG0643",
                            format!(
                                "{} is a symbolic link; workspace discovery does not follow links, so it is not a project",
                                shown(&catalog.root, &path)
                            ),
                        )
                        .at(shown(&catalog.root, &path), "", None)
                        .with_help("replace the link with the manifest, or run the command in the directory the link points to"),
                    );
                }
                continue;
            }
            if is_manifest && meta.is_file() {
                manifests.push((path, meta.len()));
            } else if meta.is_dir()
                && depth < limits.max_depth
                && !name.starts_with('.')
                && !SKIPPED_DIRECTORIES.contains(&name.as_str())
                && !outputs.contains(&path)
            {
                children.push(path);
            }
        }
        match manifests.as_slice() {
            [] => {}
            [(manifest, size)] => {
                if catalog.projects.len() == limits.max_projects {
                    catalog.diagnostics.push(limit(
                        &catalog.root,
                        manifest,
                        format!(
                            "the workspace has more than {} projects; {} and the projects after it were not loaded",
                            limits.max_projects,
                            shown(&catalog.root, manifest)
                        ),
                    ));
                    break;
                }
                if *size > limits.max_manifest_bytes {
                    catalog.diagnostics.push(limit(
                        &catalog.root,
                        manifest,
                        format!(
                            "{} is larger than {} bytes; it was not loaded as a project",
                            shown(&catalog.root, manifest),
                            limits.max_manifest_bytes
                        ),
                    ));
                } else {
                    let project = load_project(&catalog.root, &dir, manifest);
                    outputs.extend(project.outputs.iter().map(|o| o.dir.clone()));
                    catalog.projects.push(project);
                }
            }
            several => {
                let shown_dir = shown(&catalog.root, &dir);
                let names: Vec<String> = several
                    .iter()
                    .map(|(p, _)| shown(&catalog.root, p))
                    .collect();
                catalog.diagnostics.push(
                    Diagnostic::error(
                        "TG0641",
                        format!(
                            "directory {shown_dir} holds more than one manifest ({}); which one owns the project is ambiguous, so neither is loaded",
                            names.join(", ")
                        ),
                    )
                    .at(names[0].clone(), "", None)
                    .with_help("keep one manifest per directory; move the other project into its own directory"),
                );
            }
        }
        queue.extend(children.into_iter().map(|c| (c, depth + 1)));
    }
    catalog.collisions = output_collisions(&catalog.projects);
    for collision in &catalog.collisions {
        catalog.diagnostics.push(collision.diagnostic.clone());
    }
    catalog
}

fn limit(root: &Path, at: &Path, message: String) -> Diagnostic {
    Diagnostic::error("TG0640", message)
        .at(shown(root, at), "", None)
        .with_help("run the command in the project's own directory, select it with --project, or move unrelated trees out of the workspace")
}

/// TG0642: outputs of two different projects that are the same directory
/// or nested. Collisions inside one project are TG0612 of that manifest.
fn output_collisions(projects: &[Project]) -> Vec<Collision> {
    let mut found = vec![];
    for (i, a) in projects.iter().enumerate() {
        for b in &projects[i + 1..] {
            for oa in &a.outputs {
                for ob in &b.outputs {
                    let relation = if oa.dir == ob.dir {
                        "the same output directory"
                    } else if oa.dir.starts_with(&ob.dir) || ob.dir.starts_with(&oa.dir) {
                        "nested output directories"
                    } else {
                        continue;
                    };
                    let diagnostic = Diagnostic::error(
                            "TG0642",
                            format!(
                                "project `{}` target `{}` (out: {}) and project `{}` target `{}` (out: {}) have {relation}; each target deletes files it did not generate in its own directory",
                                a.id, oa.target, oa.out, b.id, ob.target, ob.out
                            ),
                        )
                        .at(b.manifest.clone(), crate::pointer::from_tokens(["targets", ob.target.as_str()]), None)
                        .with_help("give every target of every project its own directory that is not inside another's");
                    found.push(Collision {
                        projects: [a.id.clone(), b.id.clone()],
                        diagnostic,
                    });
                }
            }
        }
    }
    found
}

fn load_project(root: &Path, dir: &Path, manifest: &Path) -> Project {
    let (config, diagnostics) = crate::load(manifest);
    let mut project = Project {
        id: project_id(root, dir),
        manifest: shown(root, manifest),
        name: None,
        valid: config.is_some() && !diagnostics.has_errors(),
        outputs: vec![],
        inputs: vec![],
        dir: dir.to_path_buf(),
        manifest_path: manifest.to_path_buf(),
        input_paths: BTreeSet::new(),
    };
    if let Some(config) = &config {
        project.name = Some(config.api.name.clone());
        project.outputs = outputs_of(config, dir);
        project.input_paths = declared_inputs(config, dir);
        project.inputs = project.input_paths.iter().map(|p| shown(root, p)).collect();
    }
    project
}

fn outputs_of(config: &TungstenConfig, dir: &Path) -> Vec<ProjectOutput> {
    config
        .targets
        .iter()
        .map(|(target, options)| {
            let out = options
                .get("out")
                .and_then(|v| v.as_str())
                .map_or_else(|| format!("generated/{target}"), str::to_string);
            ProjectOutput {
                target: target.clone(),
                dir: normalize(&dir.join(&out)),
                out,
            }
        })
        .collect()
}

/// The local files `config` names, resolved against its directory.
fn declared_inputs(config: &TungstenConfig, dir: &Path) -> BTreeSet<PathBuf> {
    let mut names: Vec<&str> = vec![];
    for input in &config.inputs {
        names.push(&input.spec);
        names.extend(input.overlays.iter().map(String::as_str));
    }
    names.extend(config.overlays.iter().map(String::as_str));
    names.extend(config.profiles.iter().map(|p| p.path.as_str()));
    let agent = config.agent.as_deref().unwrap_or("agent.yml");
    let mut paths: BTreeSet<PathBuf> = names
        .into_iter()
        .filter(|name| !is_remote(name))
        .map(|name| normalize(&dir.join(name)))
        .collect();
    let agent = normalize(&dir.join(agent));
    if config.agent.is_some() || agent.is_file() {
        paths.insert(agent);
    }
    paths
}

fn is_remote(source: &str) -> bool {
    source.contains("://") || source.starts_with("git+")
}

impl Catalog {
    /// The projects named by `ids`, in discovery order. An id may also be
    /// given as the project directory or manifest path relative to the
    /// workspace root (`./billing`, `billing/tungsten.yml`). An unknown id
    /// is `TG0644`, naming the known ones.
    pub fn select(&self, ids: &[String]) -> Result<Vec<&Project>, Diagnostic> {
        let mut wanted = BTreeSet::new();
        for id in ids {
            let Some(project) = self.find(id) else {
                let known: Vec<&str> = self.projects.iter().map(|p| p.id.as_str()).collect();
                let help = if known.is_empty() {
                    "no project was found under the workspace root; run `tungsten workspace` to see what discovery found".to_string()
                } else {
                    format!("known projects: {}", known.join(", "))
                };
                return Err(Diagnostic::error(
                    "TG0644",
                    format!("no project `{id}` in workspace {}", self.root.display()),
                )
                .at(id.clone(), "", None)
                .with_help(help));
            };
            wanted.insert(project.id.as_str());
        }
        Ok(self
            .projects
            .iter()
            .filter(|p| wanted.contains(p.id.as_str()))
            .collect())
    }

    fn find(&self, id: &str) -> Option<&Project> {
        let trimmed = id.replace('\\', "/");
        let trimmed = trimmed.trim_end_matches('/');
        let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
        let key = if trimmed.is_empty() { "." } else { trimmed };
        self.projects
            .iter()
            .find(|p| p.id == key || p.manifest == key)
    }

    /// The diagnostics that concern `project`: discovery limits and the
    /// output collisions it takes part in.
    pub fn diagnostics_for(&self, project: &Project) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.code == "TG0640")
            .chain(
                self.collisions
                    .iter()
                    .filter(|c| c.projects.contains(&project.id))
                    .map(|c| &c.diagnostic),
            )
            .cloned()
            .collect()
    }

    /// The projects that read `path` (absolute): every project that names
    /// it as an input, else the innermost project whose directory holds it.
    /// Sorted by discovery order; empty when no project owns it.
    pub fn owners(&self, path: &Path) -> Vec<&Project> {
        let path = normalize(path);
        let readers: Vec<&Project> = self
            .projects
            .iter()
            .filter(|p| p.manifest_path == path || p.input_paths.contains(&path))
            .collect();
        if !readers.is_empty() {
            return readers;
        }
        self.projects
            .iter()
            .filter(|p| path.starts_with(&p.dir))
            .max_by_key(|p| p.dir.components().count())
            .into_iter()
            .collect()
    }

    /// Projects by id.
    pub fn by_id(&self) -> BTreeMap<&str, &Project> {
        self.projects.iter().map(|p| (p.id.as_str(), p)).collect()
    }
}

/// The manifest in `dir`, when it holds exactly one.
pub fn manifest_in(dir: &Path) -> Option<PathBuf> {
    let mut found = MANIFEST_NAMES
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

fn project_id(root: &Path, dir: &Path) -> String {
    match dir.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => slashed(rel),
        Err(_) => slashed(dir),
    }
}

fn shown(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => slashed(rel),
        Err(_) => path.display().to_string(),
    }
}

fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// `path` with `.` dropped and `..` applied lexically; links are not
/// resolved, so a path never leaves the tree it was written in.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
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
