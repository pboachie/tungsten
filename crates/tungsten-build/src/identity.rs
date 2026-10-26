// SPDX-License-Identifier: AGPL-3.0-only
//! Project identity: what one compilation depended on, so a caller can
//! decide whether its result is still valid without compiling again.
//!
//! The identity of a [`Compiled`] project is the manifest it was compiled
//! from, the content digest of every source text it read (manifest,
//! profiles, agent manifest, specs, local references and overlays, from
//! unsaved buffers or disk), the optional files it looked for and found
//! absent, and one digest of the settings that shape the result: the
//! compiler version, the load limits, the acquisition and remote-reference
//! policy and the invocation policy. Modification times are never used.
//!
//! The identity is in-process only. It is not a persistent cache key, it
//! carries no authority, and a project whose compilation failed or read a
//! document without a local path has none, so callers recompile it.
//! [`ProjectIdentity::reads`] answers which projects a changed file can
//! affect, for callers that serve several projects.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use tungsten_core::{Digest, SourceFile};
use tungsten_openapi::CancellationToken;

use crate::TUNGSTEN_VERSION;
use crate::driver::{CompileOptions, Compiled};

/// Most bytes read from one input to compare it with an identity; a larger
/// file counts as changed.
const MAX_COMPARED_FILE: u64 = 64 * 1024 * 1024;

/// The dependencies of one compilation. See the module documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    manifest: PathBuf,
    settings: Digest,
    /// Sorted by path, one entry per path.
    files: Vec<(PathBuf, Digest)>,
    /// Sorted.
    absent: Vec<PathBuf>,
}

impl ProjectIdentity {
    /// The identity of `compiled`, compiled from `manifest` with `opts`, or
    /// `None` when its result must not be reused: it has errors (a missing
    /// or unreadable input is an error, and creating it must be noticed),
    /// or it read a document without a local path.
    ///
    /// Source files are named by their path relative to the manifest's
    /// directory, profiles by their physical identity. `optional` lists
    /// files the compilation would have read had they existed (such as a
    /// default agent manifest); those not read are recorded as absent.
    pub fn of(
        compiled: &Compiled,
        opts: &CompileOptions,
        manifest: &Path,
        optional: &[PathBuf],
    ) -> Option<Self> {
        let base = manifest.parent().map(normalize).unwrap_or_default();
        Self::with_paths(compiled, opts, manifest, optional, |file| {
            match compiled.profile_sources.get(&file.id) {
                Some(path) => normalize(path),
                None => normalize(&base.join(&file.name)),
            }
        })
    }

    /// [`ProjectIdentity::of`], with `path_of` naming each source file, for
    /// callers that name files differently (an editor's paths).
    pub fn with_paths(
        compiled: &Compiled,
        opts: &CompileOptions,
        manifest: &Path,
        optional: &[PathBuf],
        path_of: impl Fn(&SourceFile) -> PathBuf,
    ) -> Option<Self> {
        if compiled.has_errors()
            || compiled
                .workspace
                .documents
                .iter()
                .any(|d| d.path().is_none())
        {
            return None;
        }
        let mut files: Vec<(PathBuf, Digest)> = compiled
            .workspace
            .sources
            .files()
            .iter()
            .map(|f| (path_of(f), Digest::of(f.text.as_bytes())))
            .collect();
        files.sort();
        files.dedup_by(|a, b| a.0 == b.0);
        let mut absent: Vec<PathBuf> = optional
            .iter()
            .filter(|p| !files.iter().any(|(f, _)| f == *p))
            .cloned()
            .collect();
        absent.sort();
        absent.dedup();
        Some(Self {
            manifest: normalize(manifest),
            settings: settings(opts),
            files,
            absent,
        })
    }

    /// The manifest the project was compiled from.
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }

    /// Every source the compilation read, with the digest of its text.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &Digest)> {
        self.files.iter().map(|(p, d)| (p.as_path(), d))
    }

    /// Whether the compilation depends on `path`: it read it, or looked for
    /// it and found it absent. A change to any other file cannot change the
    /// result.
    pub fn reads(&self, path: &Path) -> bool {
        let path = normalize(path);
        self.files
            .binary_search_by(|(p, _)| p.as_path().cmp(&path))
            .is_ok()
            || self.absent.binary_search(&path).is_ok()
    }

    /// One digest of the whole identity, for comparing or keying results.
    pub fn fingerprint(&self) -> Digest {
        let mut text = format!("{}\n{}\n", self.manifest.display(), self.settings.0);
        for (path, digest) in &self.files {
            text.push_str(&format!("file {} {}\n", path.display(), digest.0));
        }
        for path in &self.absent {
            text.push_str(&format!("absent {}\n", path.display()));
        }
        Digest::of(text.as_bytes())
    }

    /// Whether compiling `manifest` with `opts` now would read the same
    /// inputs: same manifest and settings, every source with the same
    /// content (the unsaved text of `opts.overrides` first, else the file
    /// on disk) and every absent file still absent. Cancellation counts as
    /// a change.
    pub fn unchanged(&self, manifest: &Path, opts: &CompileOptions) -> bool {
        normalize(manifest) == self.manifest
            && settings(opts) == self.settings
            && self.sources_unchanged(&opts.overrides, &opts.load.cancellation)
    }

    /// The source part of [`ProjectIdentity::unchanged`], for a caller that
    /// keeps its settings fixed.
    pub fn sources_unchanged(
        &self,
        overrides: &BTreeMap<PathBuf, String>,
        cancellation: &CancellationToken,
    ) -> bool {
        for (path, digest) in &self.files {
            if cancellation.is_cancelled() {
                return false;
            }
            let current = match overrides.get(path) {
                Some(text) => Digest::of(text.as_bytes()),
                None => {
                    let fits = std::fs::metadata(path)
                        .is_ok_and(|m| m.is_file() && m.len() <= MAX_COMPARED_FILE);
                    match std::fs::read(path) {
                        Ok(bytes) if fits => Digest::of(&bytes),
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

/// The digest of everything in `opts` but the sources that shapes a
/// compilation. Header values are counted, never hashed.
fn settings(opts: &CompileOptions) -> Digest {
    let load = &opts.load;
    let policy = opts.policy.as_ref().map(|p| {
        serde_json::to_string(&p.inspection()).unwrap_or_else(|_| "unserializable".into())
    });
    let text = format!(
        "tungsten {TUNGSTEN_VERSION}\nmax_bytes {}\nmax_depth {}\nswagger2 {}\n\
         offline {}\nrefresh {}\nheaders {}\nheader_sources {:?}\nallow_remote {:?}\n\
         acquisition_root {}\npolicy {policy:?}\n",
        load.max_bytes,
        load.max_depth,
        load.convert_swagger2,
        load.acquisition.offline,
        load.acquisition.refresh,
        load.acquisition.headers.len(),
        load.acquisition.header_sources,
        load.allow_remote,
        load.acquisition_root.display(),
    );
    Digest::of(text.as_bytes())
}

/// `path` without `.` components and with `..` folded lexically.
fn normalize(path: &Path) -> PathBuf {
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
