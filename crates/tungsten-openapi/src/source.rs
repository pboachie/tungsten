// SPDX-License-Identifier: AGPL-3.0-only
//! Local source snapshots shared by the frontend and compilation driver.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tungsten_core::{Diagnostic, Digest};

/// Cooperative cancellation of one compilation. Clones share the same flag.
/// Reads/parsers already in progress finish before the next checkpoint.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Immutable unsaved local text over a bounded regular-file reader. Keys are
/// normalized local identities; existing symlink aliases refer to one buffer.
/// This is a source snapshot, not a persistent acquisition or analysis cache.
#[derive(Debug, Clone, Default)]
pub struct SourceProvider {
    overrides: BTreeMap<PathBuf, String>,
}

impl SourceProvider {
    pub fn with_overrides(mut self, overrides: BTreeMap<PathBuf, String>) -> Self {
        self.overrides
            .extend(overrides.into_iter().map(|(p, text)| (Self::key(&p), text)));
        self
    }
    pub fn new(overrides: BTreeMap<PathBuf, String>) -> Self {
        Self::default().with_overrides(overrides)
    }

    pub fn identity(path: &Path) -> PathBuf {
        crate::refs::lexical_normalize(
            &std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
        )
    }

    pub fn text(&self, path: &Path) -> Option<&str> {
        self.overrides.get(&Self::key(path)).map(String::as_str)
    }

    pub(crate) fn key(path: &Path) -> PathBuf {
        let path = Self::identity(path);
        std::fs::canonicalize(&path)
            .ok()
            .or_else(|| {
                Some(
                    std::fs::canonicalize(path.parent()?)
                        .ok()?
                        .join(path.file_name()?),
                )
            })
            .unwrap_or(path)
    }

    /// Physical identity for confinement checks. A new unsaved file may use
    /// its existing parent's canonical directory; dangling symlinks may not.
    pub fn canonical_path(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok().or_else(|| {
            if self.text(path).is_none() || std::fs::symlink_metadata(path).is_ok() {
                return None;
            }
            Some(
                std::fs::canonicalize(path.parent()?)
                    .ok()?
                    .join(path.file_name()?),
            )
        })
    }

    /// Read UTF-8 and hash the same bytes used for parsing and source spans.
    pub fn read(
        &self,
        path: &Path,
        name: &str,
        max: usize,
    ) -> Result<(String, Digest), Diagnostic> {
        let cannot_read =
            |e: std::io::Error| Diagnostic::error("TG0101", format!("cannot read {name}: {e}"));
        let bytes = if let Some(text) = self.text(path) {
            if text.len() > max {
                return Err(too_big(name, max));
            }
            text.as_bytes().to_vec()
        } else {
            let meta = std::fs::metadata(path).map_err(cannot_read)?;
            if !meta.is_file() {
                return Err(Diagnostic::error(
                    "TG0101",
                    format!("cannot read {name}: not a regular file"),
                ));
            }
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(cannot_read)?
                .take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(cannot_read)?;
            bytes
        };
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
}

pub(crate) fn too_big(name: &str, max: usize) -> Diagnostic {
    Diagnostic::error(
        "TG0105",
        format!("{name} exceeds the size limit of {max} bytes"),
    )
}
