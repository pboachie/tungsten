// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic in-memory set of generated files.

use std::collections::BTreeMap;

/// Error adding a file to a [`FileSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSetError {
    /// The path is absolute, contains `..`, a backslash, an empty segment or
    /// a NUL byte.
    InvalidPath(String),
    /// A file with this path was already added.
    Duplicate(String),
}

impl std::fmt::Display for FileSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileSetError::InvalidPath(p) => write!(f, "invalid output path {p:?}"),
            FileSetError::Duplicate(p) => write!(f, "duplicate output path {p:?}"),
        }
    }
}

impl std::error::Error for FileSetError {}

/// Generated files keyed by relative path (forward slashes), iterated in
/// sorted order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSet {
    files: BTreeMap<String, Vec<u8>>,
}

impl FileSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a file. The path must stay inside the output directory.
    pub fn add(
        &mut self,
        path: impl Into<String>,
        contents: impl Into<Vec<u8>>,
    ) -> Result<(), FileSetError> {
        let path = path.into();
        let bad = path.is_empty()
            || path.starts_with('/')
            || path.contains('\\')
            || path.contains('\0')
            || path
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
            || path.chars().nth(1) == Some(':');
        if bad {
            return Err(FileSetError::InvalidPath(path));
        }
        if self.files.contains_key(&path) {
            return Err(FileSetError::Duplicate(path));
        }
        self.files.insert(path, contents.into());
        Ok(())
    }

    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.files.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Move every file of `other` under `prefix/`.
    pub fn merge_under(&mut self, prefix: &str, other: FileSet) -> Result<(), FileSetError> {
        for (k, v) in other.files {
            self.add(format!("{prefix}/{k}"), v)?;
        }
        Ok(())
    }
}
