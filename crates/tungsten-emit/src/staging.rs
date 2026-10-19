// SPDX-License-Identifier: AGPL-3.0-only
//! Owned temporary output staging, separate from persistent acquisition caches.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tungsten_core::{Diagnostic, Diagnostics, Digest};

use crate::FileSet;

/// Aggregate metadata budget for one generation, across every staged target.
/// Output bytes live in the temporary spool; emission/loading retains at most
/// one target's FileSet. Limits are explicit rather than silently truncating.
#[derive(Debug, Clone, Copy)]
pub struct StagingLimits {
    pub max_files: usize,
    /// Sum of path bytes, digest text bytes and StagedFile structure sizes.
    /// Allocator overhead, per-target diagnostics/configuration and IR/plans
    /// are separate; this is not a total process-memory ceiling.
    pub max_metadata_bytes: usize,
}

impl Default for StagingLimits {
    fn default() -> Self {
        Self {
            max_files: 262_144,
            max_metadata_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
struct Directory(PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        // This path was exclusively created by new(), never supplied by a
        // caller. Drop also runs on diagnostics, cancellation and unwinding.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One generated path's bounded comparison identity, without output bytes.
#[derive(Debug, Clone)]
pub struct StagedFile {
    pub path: String,
    pub digest: Digest,
    pub bytes: u64,
}

/// A generation's owned spool and aggregate metadata accounting.
#[derive(Debug)]
pub struct OutputStaging {
    directory: Arc<Directory>,
    limits: StagingLimits,
    files: usize,
    metadata_bytes: usize,
    targets: usize,
}

impl OutputStaging {
    pub fn new(limits: StagingLimits) -> Result<Self, Diagnostics> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp = std::env::temp_dir();
        for _ in 0..100 {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = temp.join(format!("tungsten-output-{}-{unique}", std::process::id()));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        directory: Arc::new(Directory(path)),
                        limits,
                        files: 0,
                        metadata_bytes: 0,
                        targets: 0,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(failure(format!("cannot create output staging: {e}"))),
            }
        }
        Err(failure("cannot create a unique output staging directory"))
    }

    /// Stage one target and release its in-memory bytes on return. A caller
    /// must finish all strict diagnostic preflight before applying any target.
    /// Cancellation is checked between files; individual I/O is not preempted.
    pub fn stage(
        &mut self,
        files: FileSet,
        cancelled: impl Fn() -> bool,
    ) -> Result<StagedOutput, Diagnostics> {
        if cancelled() {
            return Err(failure("generation cancelled before staging target output"));
        }
        let digest_bytes = Digest::of(&[]).0.len();
        let metadata = files.iter().try_fold(0usize, |total, (path, _)| {
            total.checked_add(
                path.len()
                    .saturating_add(digest_bytes)
                    .saturating_add(std::mem::size_of::<StagedFile>()),
            )
        });
        let count = self.files.checked_add(files.len());
        let metadata = metadata.and_then(|n| self.metadata_bytes.checked_add(n));
        if count.is_none_or(|n| n > self.limits.max_files)
            || metadata.is_none_or(|n| n > self.limits.max_metadata_bytes)
        {
            return Err(failure(
                "output staging metadata limit exceeded; select fewer targets or reduce generated paths",
            ));
        }
        self.files = count.unwrap_or_default();
        self.metadata_bytes = metadata.unwrap_or_default();
        let target = self.targets;
        self.targets += 1;
        let dir = self.directory.0.join(target.to_string());
        std::fs::create_dir(&dir)
            .map_err(|e| failure(format!("cannot create target staging: {e}")))?;
        let mut manifest = Vec::with_capacity(files.len());
        for (index, (path, bytes)) in files.iter().enumerate() {
            if cancelled() {
                return Err(failure(
                    "generation cancelled while staging output; target output has not been applied",
                ));
            }
            std::fs::write(dir.join(index.to_string()), bytes)
                .map_err(|e| failure(format!("cannot stage generated file {path}: {e}")))?;
            manifest.push(StagedFile {
                path: path.to_string(),
                digest: Digest::of(bytes),
                bytes: bytes.len() as u64,
            });
        }
        Ok(StagedOutput {
            directory: self.directory.clone(),
            target,
            manifest,
        })
    }

    /// Accounted metadata across this generation; excludes output file bytes.
    pub fn metadata_bytes(&self) -> usize {
        self.metadata_bytes
    }
}

/// Immutable prepared output identity. Dropping the last spool owner removes
/// every temporary target, including when a run fails before output writes.
#[derive(Debug)]
pub struct StagedOutput {
    directory: Arc<Directory>,
    target: usize,
    manifest: Vec<StagedFile>,
}

impl StagedOutput {
    /// Owned spool location for private corruption and cleanup controls.
    #[cfg(feature = "testing")]
    pub fn staging_directory(&self) -> &std::path::Path {
        &self.directory.0
    }
    pub fn manifest(&self) -> &[StagedFile] {
        &self.manifest
    }

    /// Load and verify one target before applying it through write_output.
    /// No other target's output bytes need to be retained at the same time.
    pub fn load(&self, cancelled: impl Fn() -> bool) -> Result<FileSet, Diagnostics> {
        use std::io::Read;
        let mut files = FileSet::new();
        for (index, entry) in self.manifest.iter().enumerate() {
            if cancelled() {
                return Err(failure(
                    "generation cancelled before applying staged target output",
                ));
            }
            let path = self
                .directory
                .0
                .join(self.target.to_string())
                .join(index.to_string());
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .and_then(|file| {
                    file.take(entry.bytes.saturating_add(1))
                        .read_to_end(&mut bytes)
                })
                .map_err(|e| failure(format!("cannot read staged file {}: {e}", entry.path)))?;
            if bytes.len() as u64 != entry.bytes || Digest::of(&bytes) != entry.digest {
                return Err(failure(format!(
                    "staged file {} changed; regenerate from the source inputs",
                    entry.path
                )));
            }
            files
                .add(entry.path.clone(), bytes)
                .map_err(|e| failure(format!("invalid staged file: {e}")))?;
        }
        Ok(files)
    }
}

fn failure(message: impl Into<String>) -> Diagnostics {
    Diagnostics(vec![Diagnostic::error("TG0701", message)])
}
