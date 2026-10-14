// SPDX-License-Identifier: Apache-2.0
//! Idempotency: the stores, key formats and the header an operation's key
//! travels in.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Map, Value};

use crate::types::{IdempotencyKind, IdempotencyStore, OperationDescriptor, ParamRole};
use crate::util::random_bytes;

/// Locks a mutex, continuing with the data of a poisoned one: the maps
/// behind these locks stay consistent when a holder panics.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    fn reserve(&self, scope: &str, logical_id: &str, candidate: &str) -> Result<String, String> {
        Ok(lock(&self.keys)
            .entry((scope.to_owned(), logical_id.to_owned()))
            .or_insert_with(|| candidate.to_owned())
            .clone())
    }
    fn get(&self, scope: &str, logical_id: &str) -> Option<String> {
        lock(&self.keys)
            .get(&(scope.to_owned(), logical_id.to_owned()))
            .cloned()
    }

    fn put(&self, scope: &str, logical_id: &str, key: &str) {
        lock(&self.keys).insert((scope.to_owned(), logical_id.to_owned()), key.to_owned());
    }
}

type StoreData = BTreeMap<String, BTreeMap<String, String>>;

/// Keeps keys in a JSON file (`{"version": 1, "keys": {scope: {id: key}}}`,
/// the format of the TypeScript and Python runtimes), so a retry after a
/// crash reuses the key. Writes are serialized within the process and atomic
/// (a temporary file in the same directory, then a rename); on Unix the file
/// is created with mode 0600 because keys are never meant to be shown. A file
/// that exists but cannot be parsed makes `get` return `None` and `put`
/// write nothing, and the runtime then refuses the call rather than issue a
/// second key for the same intent. `reserve` also holds the cross-process
/// lease `<path>.lock`: it waits up to 2 s for another writer, then refuses;
/// only its owner removes a lease.
#[derive(Debug)]
pub struct FileIdempotencyStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileIdempotencyStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileIdempotencyStore {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Option<StoreData> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(StoreData::new()),
            Err(_) => return None,
        };
        let parsed: Value = serde_json::from_str(&text).ok()?;
        if parsed.get("version") != Some(&Value::from(1)) {
            return None;
        }
        let mut out = StoreData::new();
        for (scope, entries) in parsed.get("keys")?.as_object()? {
            let mut scoped = BTreeMap::new();
            for (id, key) in entries.as_object()? {
                scoped.insert(id.clone(), key.as_str()?.to_owned());
            }
            out.insert(scope.clone(), scoped);
        }
        Some(out)
    }

    fn write(&self, data: &StoreData) -> std::io::Result<()> {
        self.write_fenced(data, || Ok(()))
    }

    /// Writes `data`; `fence` runs just before the rename that commits the
    /// file, and its error abandons the write.
    fn write_fenced(
        &self,
        data: &StoreData,
        fence: impl FnOnce() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let mut keys = Map::new();
        for (scope, entries) in data {
            let scoped: Map<String, Value> = entries
                .iter()
                .map(|(id, key)| (id.clone(), Value::String(key.clone())))
                .collect();
            keys.insert(scope.clone(), Value::Object(scoped));
        }
        let mut document = Map::new();
        document.insert("version".to_owned(), Value::from(1));
        document.insert("keys".to_owned(), Value::Object(keys));
        let mut text = serde_json::to_string_pretty(&Value::Object(document))
            .map_err(std::io::Error::other)?;
        text.push('\n');
        let directory = match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        std::fs::create_dir_all(&directory)?;
        let suffix = random_bytes::<6>()
            .map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>())
            .unwrap_or_else(|| "tmp".to_owned());
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "store".to_owned());
        let temporary = directory.join(format!(".{name}.{suffix}.tmp"));
        let written = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fence()?;
            std::fs::rename(&temporary, &self.path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written
    }
}

/// The in-process lock of one store file, by absolute path.
fn file_lock(path: &Path) -> Arc<Mutex<()>> {
    static FILES: Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>> = Mutex::new(BTreeMap::new());
    let key = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    lock(&FILES).entry(key).or_default().clone()
}

impl IdempotencyStore for FileIdempotencyStore {
    fn reserve(&self, scope: &str, logical_id: &str, candidate: &str) -> Result<String, String> {
        // Reservations of one file are serialized within the process; the
        // lease serializes them across processes. Other files never wait.
        let file = file_lock(&self.path);
        let _file = lock(&file);
        let _guard = lock(&self.lock);
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let lease = PathBuf::from(format!("{}.lock", self.path.display()));
        let token = crate::lease::acquire(&lease)?;
        let result = (|| {
            let mut data = self.read().ok_or("invalid idempotency store")?;
            if let Some(key) = data
                .get(scope)
                .and_then(|m| m.get(logical_id))
                .filter(|k| !k.is_empty())
            {
                return Ok(key.clone());
            }
            data.entry(scope.to_owned())
                .or_default()
                .insert(logical_id.to_owned(), candidate.to_owned());
            self.write_fenced(&data, || crate::lease::fence(&lease, &token))
                .map_err(|e| e.to_string())?;
            Ok(candidate.to_owned())
        })();
        crate::lease::release(&lease, &token);
        result
    }
    fn get(&self, scope: &str, logical_id: &str) -> Option<String> {
        let _guard = lock(&self.lock);
        self.read()?.get(scope)?.get(logical_id).cloned()
    }

    fn put(&self, scope: &str, logical_id: &str, key: &str) {
        let _guard = lock(&self.lock);
        let Some(mut data) = self.read() else {
            return;
        };
        data.entry(scope.to_owned())
            .or_default()
            .insert(logical_id.to_owned(), key.to_owned());
        // A failed write is noticed by the caller reading the key back.
        let _ = self.write(&data);
    }
}

// ----------------------------------------------------------- key formats

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatKind {
    UuidV4,
    Uuid,
    AsciiToken128,
    Token,
}

fn format_kind(format: Option<&str>) -> FormatKind {
    let normalized: String = format
        .unwrap_or("")
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect();
    match normalized.as_str() {
        "uuidv4" | "uuid4" => FormatKind::UuidV4,
        "uuid" => FormatKind::Uuid,
        "asciitoken128" => FormatKind::AsciiToken128,
        _ => FormatKind::Token,
    }
}

/// Human description of the key format a policy expects.
pub fn key_format_description(format: Option<&str>) -> &'static str {
    match format_kind(format) {
        FormatKind::UuidV4 => "UUIDv4 string",
        FormatKind::Uuid => "UUID string",
        FormatKind::AsciiToken128 => "1-128 ASCII letters, digits, dots, underscores or hyphens",
        FormatKind::Token => "a non-empty printable ASCII string of at most 255 characters",
    }
}

fn is_uuid(key: &str, v4_only: bool) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => *b == b'-',
            14 => {
                if v4_only {
                    *b == b'4'
                } else {
                    (b'1'..=b'8').contains(b)
                }
            }
            19 => matches!(b.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
            _ => b.is_ascii_hexdigit(),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Checks a caller-owned key against the policy's `format`. Returns the
/// description of the expected format when the key does not match.
pub fn check_key_format(key: &str, format: Option<&str>) -> Option<&'static str> {
    let valid = match format_kind(format) {
        FormatKind::UuidV4 => is_uuid(key, true),
        FormatKind::Uuid => is_uuid(key, false),
        FormatKind::AsciiToken128 => {
            !key.is_empty()
                && key.len() <= 128
                && key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        }
        FormatKind::Token => {
            !key.is_empty() && key.len() <= 255 && key.bytes().all(|b| (0x21..=0x7e).contains(&b))
        }
    };
    if valid {
        None
    } else {
        Some(key_format_description(format))
    }
}

/// The wire header of the operation's key: the policy's header, else the
/// parameter with role `IdempotencyKey`, else `Idempotency-Key`.
pub fn key_header(op: &OperationDescriptor) -> String {
    if let Some(header) = op.agent.idempotency.header.as_deref()
        && !header.is_empty()
    {
        return header.to_owned();
    }
    op.params
        .iter()
        .find(|p| p.role == ParamRole::IdempotencyKey)
        .map_or_else(|| "Idempotency-Key".to_owned(), |p| p.wire.clone())
}

/// Whether the operation sends an idempotency key or an identity body, so
/// resending it cannot apply its effect twice.
pub fn has_replay_protection(op: &OperationDescriptor, key: Option<&str>) -> bool {
    op.agent.idempotency.policy == IdempotencyKind::ContentIdentity || key.is_some()
}
