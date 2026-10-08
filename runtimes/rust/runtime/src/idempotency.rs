// SPDX-License-Identifier: Apache-2.0
//! Idempotency stores.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::types::IdempotencyStore;

/// Keeps keys for the life of the process.
#[derive(Debug, Default)]
pub struct MemoryIdempotencyStore {
    keys: Mutex<HashMap<(String, String), String>>,
}

impl MemoryIdempotencyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdempotencyStore for MemoryIdempotencyStore {
    fn get(&self, _scope: &str, _logical_id: &str) -> Option<String> {
        let _ = &self.keys;
        unimplemented!("PHASE-5 stub")
    }

    fn put(&self, _scope: &str, _logical_id: &str, _key: &str) {
        unimplemented!("PHASE-5 stub")
    }
}

/// Keeps keys in a JSON file, so a retry after a crash reuses the key.
#[derive(Debug)]
pub struct FileIdempotencyStore {
    path: PathBuf,
}

impl FileIdempotencyStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileIdempotencyStore { path: path.into() }
    }
}

impl IdempotencyStore for FileIdempotencyStore {
    fn get(&self, _scope: &str, _logical_id: &str) -> Option<String> {
        let _ = &self.path;
        unimplemented!("PHASE-5 stub")
    }

    fn put(&self, _scope: &str, _logical_id: &str, _key: &str) {
        unimplemented!("PHASE-5 stub")
    }
}
