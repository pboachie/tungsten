// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded transport for entry documents. Remote references remain a separate
//! loader policy: acquiring an entry never grants access to its neighbours.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tungsten_core::Diagnostic;
use url::{Host, Url};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const LOCK_LIMIT: usize = 32 * 1024;

/// Acquisition policy. Header values are never formatted in diagnostics or Debug.
#[derive(Clone, Default)]
pub struct Options {
    pub offline: bool,
    pub refresh: bool,
    /// `Name: value` for explicit sources, or `[https://host] Name: value`.
    pub headers: Vec<String>,
    /// URLs explicitly selected by the caller, never URLs read from a manifest.
    pub header_sources: Vec<String>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("offline", &self.offline)
            .field("refresh", &self.refresh)
            .field("headers", &self.headers.len())
            .field("header_sources", &self.header_sources.len())
            .finish()
    }
}

thread_local! {
    static OPTIONS: RefCell<Options> = RefCell::new(Options::default());
}

/// Current command policy. The build driver snapshots it before starting its
/// compilation thread, so concurrent CLI invocations do not share credentials.
pub fn current_options() -> Options {
    OPTIONS.with(|options| options.borrow().clone())
}

/// Bind policy to one command without mutating process environment variables.
pub fn with_options<T>(options: Options, run: impl FnOnce() -> T) -> T {
    struct Restore(Options);
    impl Drop for Restore {
        fn drop(&mut self) {
            OPTIONS.with(|slot| *slot.borrow_mut() = self.0.clone());
        }
    }
    let _restore = Restore(OPTIONS.with(|slot| slot.replace(options)));
    run()
}

/// Recognize source URLs before a platform's path normalization can alter them.
pub fn is_remote(source: &str) -> bool {
    let scheme = |prefix: &str| {
        source
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    };
    scheme("http://") || scheme("https://") || crate::git_input::is_source(source)
}

/// Whether the URL names this machine, where cleartext HTTP never leaves it.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
        }
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.');
            name.eq_ignore_ascii_case("localhost")
                || name
                    .len()
                    .checked_sub(".localhost".len())
                    .and_then(|start| name.get(start..))
                    .is_some_and(|end| end.eq_ignore_ascii_case(".localhost"))
        }
        None => false,
    }
}

/// Input headers usually carry credentials. Over plain HTTP to another host
/// they can be read or altered on the network, so warn (once per origin and
/// process) without ever printing a header name or value. This stays a warning
/// so existing plain-HTTP setups on trusted networks keep working.
fn warn_cleartext_headers(url: &Url, count: usize) {
    static WARNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    if count == 0 || url.scheme() != "http" || is_loopback(url) {
        return;
    }
    let origin = url.origin().ascii_serialization();
    let first = WARNED
        .lock()
        .map(|mut warned| warned.insert(origin.clone()))
        .unwrap_or(true);
    if first {
        eprintln!(
            "warning: sending {count} --input-header/TUNGSTEN_INPUT_HEADERS value(s) to {origin} over unencrypted HTTP; \
             anyone on the network path can read them. Use an https:// URL for authenticated inputs."
        );
    }
}

/// Display a source without reflecting forbidden URL credentials or queries.
pub fn display_source(source: &str) -> String {
    if crate::git_input::is_source(source) {
        return crate::git_input::display(source);
    }
    if is_remote(source)
        && !Url::parse(source).is_ok_and(|url| {
            url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && source.len() <= 8192
        })
    {
        "<remote input>".to_owned()
    } else {
        source.to_owned()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    version: u8,
    url: String,
    sha256: String,
}

fn diagnostic(code: &str, message: impl Into<String>, help: &str) -> Diagnostic {
    Diagnostic::error(code, message).with_help(help)
}

fn cache_error() -> Diagnostic {
    diagnostic(
        "TG0123",
        "cannot safely read or write the remote input cache",
        "use a writable project directory with regular cache files; inspect .tungsten-inputs before retrying",
    )
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// SHA256 of a bounded, regular acquired file, for conversion provenance.
pub fn content_hash(path: &Path) -> Result<String, Diagnostic> {
    read(path, MAX_BYTES).map(|bytes| hash(&bytes))
}

pub(crate) fn directory(path: &Path) -> Result<(), Diagnostic> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        Ok(_) => Err(cache_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|_| cache_error())
        }
        Err(_) => Err(cache_error()),
    }
}

pub(crate) fn read(path: &Path, limit: usize) -> Result<Vec<u8>, Diagnostic> {
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file()) {
        return Err(cache_error());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| cache_error())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| cache_error())?;
    if bytes.len() > limit {
        return Err(cache_error());
    }
    Ok(bytes)
}

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Diagnostic> {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| cache_error())?;
    file.write_all(bytes).map_err(|_| cache_error())?;
    file.sync_all().map_err(|_| cache_error())
}

fn path_error(code: &str, message: &str, path: &Path, help: &str) -> Diagnostic {
    Diagnostic::error(code, format!("{message}: {}", path.display())).with_help(help)
}

/// Exclusive acquisition of one source cache directory. The advisory lock is
/// released by the operating system when its owner exits, including on Ctrl-C,
/// so a held lock always names a live fetch and a free one proves that every
/// `*.pending` entry beside it is an abandoned partial write.
pub(crate) struct SourceLock {
    _file: std::fs::File,
}

impl SourceLock {
    /// Take the lock on `dir/acquire.lock`, then discard stale partial writes.
    pub(crate) fn acquire(dir: &Path, code: &str) -> Result<Self, Diagnostic> {
        let path = dir.join("acquire.lock");
        let unsafe_entry = || {
            path_error(
                code,
                "input cache lock is not a regular file",
                &path,
                "remove this entry and retry; it is recreated as an empty regular file",
            )
        };
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.file_type().is_file() => return Err(unsafe_entry()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unsafe_entry()),
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|_| {
                path_error(
                    code,
                    "cannot open the input cache lock",
                    &path,
                    "use a writable project directory, then retry",
                )
            })?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(path_error(
                    code,
                    "another tungsten process is fetching this input (lock held)",
                    &path,
                    "wait for the other run to finish, then retry; the lock is released automatically when that process exits",
                ));
            }
            Err(std::fs::TryLockError::Error(_)) => {
                return Err(path_error(
                    code,
                    "cannot lock the input cache",
                    &path,
                    "use a local project directory whose filesystem supports file locks, then retry",
                ));
            }
        }
        for entry in std::fs::read_dir(dir).map_err(|_| cache_error())? {
            let entry = entry.map_err(|_| cache_error())?;
            if !entry.file_name().to_string_lossy().ends_with(".pending") {
                continue;
            }
            let stale = entry.path();
            // Never follow a link: only the entry itself is removed.
            let removed = match std::fs::symlink_metadata(&stale) {
                Ok(meta) if meta.file_type().is_dir() => std::fs::remove_dir_all(&stale),
                Ok(_) => std::fs::remove_file(&stale),
                Err(e) => Err(e),
            };
            if let Err(e) = removed
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(path_error(
                    code,
                    "cannot remove a partial download left by an interrupted fetch",
                    &stale,
                    "delete this entry manually (it is never used as input), then retry",
                ));
            }
        }
        Ok(Self { _file: file })
    }
}

/// A fresh scratch path in `dir` that no earlier or concurrent run can own.
pub(crate) fn pending_path(dir: &Path, stem: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    dir.join(format!(
        "{stem}.{}-{nanos}-{}.pending",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Write `bytes` to a unique scratch file in `scratch` and atomically rename it
/// over `destination`, so an interrupted write never leaves a partial file
/// under its final name. `scratch` must be on the destination's filesystem.
pub(crate) fn write_atomic(
    scratch: &Path,
    destination: &Path,
    bytes: &[u8],
    code: &str,
) -> Result<(), Diagnostic> {
    if std::fs::symlink_metadata(destination).is_ok_and(|meta| meta.file_type().is_dir()) {
        return Err(path_error(
            code,
            "input cache entry is a directory where a file is expected",
            destination,
            "remove this entry and retry the fetch",
        ));
    }
    let temporary = pending_path(scratch, "write");
    write_new(&temporary, bytes).map_err(|_| {
        let _ = std::fs::remove_file(&temporary);
        path_error(
            code,
            "cannot write the input cache",
            &temporary,
            "use a writable project directory with free space, then retry",
        )
    })?;
    std::fs::rename(&temporary, destination).map_err(|_| {
        let _ = std::fs::remove_file(&temporary);
        path_error(
            code,
            "cannot replace input cache entry",
            destination,
            "check permissions on this path, remove it if it is not a regular file, then retry",
        )
    })
}

/// Preserve only recognized format suffixes, never arbitrary remote filenames.
fn content_extension(url: &Url) -> &str {
    let extension = url.path().rsplit('.').next().unwrap_or("");
    match extension {
        "json" | "yaml" | "yml" | "http" | "rest" | "curl" | "har" => extension,
        _ => "input",
    }
}

/// Project-relative paths (`/`-separated) of the verified offline pin of a
/// remote source: its pin record and exactly the pinned content files, so a
/// copy of them reproduces the input offline. Verifies through offline
/// acquisition first; never fetches, refreshes or writes content.
pub fn pinned_files(source: &str, project: &Path) -> Result<Vec<String>, Diagnostic> {
    let offline = Options {
        offline: true,
        ..Options::default()
    };
    acquire(source, project, &offline)?;
    if crate::git_input::is_source(source) {
        return crate::git_input::pinned_files(source, project);
    }
    let url = Url::parse(source).map_err(|_| cache_error())?;
    let key = hash(url.as_str().as_bytes());
    let pin: Pin = serde_json::from_slice(&read(
        &project
            .join(".tungsten-inputs")
            .join(&key)
            .join("source.lock.json"),
        LOCK_LIMIT,
    )?)
    .map_err(|_| cache_error())?;
    Ok(vec![
        format!(".tungsten-inputs/{key}/source.lock.json"),
        format!(
            ".tungsten-inputs/{key}/{}.{}",
            pin.sha256,
            content_extension(&url)
        ),
    ])
}

/// Fetch or reuse a pinned input. The per-source lock and immutable content live
/// in `project/.tungsten-inputs/<sha256-of-url>/`. Retain this directory securely
/// when offline reproducibility is required. Ordinary online loads verify
/// upstream bytes against the pin; only explicit refresh replaces a pin.
pub fn acquire(source: &str, project: &Path, options: &Options) -> Result<PathBuf, Diagnostic> {
    if crate::git_input::is_source(source) {
        return crate::git_input::acquire(source, project, options);
    }
    let invalid = || {
        diagnostic(
            "TG0120",
            "remote input must be an HTTP(S) URL without credentials, query or fragment",
            "use a stable document URL and --input-header (or TUNGSTEN_INPUT_HEADERS) for authentication",
        )
    };
    let url = Url::parse(source).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || source.len() > 8192
    {
        return Err(invalid());
    }
    if options.offline && options.refresh {
        return Err(diagnostic(
            "TG0120",
            "offline input acquisition cannot refresh pins",
            "choose --offline to reuse verified bytes or --refresh-inputs to accept new bytes online",
        ));
    }
    // Validate headers without ever echoing a supplied value. Restrict framing
    // headers so credentials cannot change the request destination or message.
    let mut headers = Vec::new();
    for header in &options.headers {
        let (header, allowed) = if let Some(scoped) = header.strip_prefix('[') {
            let (origin, header) = scoped
                .split_once("] ")
                .or_else(|| scoped.split_once("]\t"))
                .ok_or_else(invalid)?;
            let origin = Url::parse(origin).map_err(|_| invalid())?;
            if !matches!(origin.scheme(), "http" | "https")
                || origin.host_str().is_none()
                || !origin.username().is_empty()
                || origin.password().is_some()
                || origin.path() != "/"
                || origin.query().is_some()
                || origin.fragment().is_some()
            {
                return Err(invalid());
            }
            (header.trim_start(), origin.origin() == url.origin())
        } else {
            (
                header.as_str(),
                options
                    .header_sources
                    .iter()
                    .any(|explicit| explicit == source),
            )
        };
        let (name, value) = header.split_once(':').ok_or_else(invalid)?;
        let name =
            ureq::http::HeaderName::from_bytes(name.trim().as_bytes()).map_err(|_| invalid())?;
        let value = ureq::http::HeaderValue::from_str(value.trim()).map_err(|_| invalid())?;
        if matches!(
            name.as_str(),
            "host" | "content-length" | "transfer-encoding" | "connection" | "accept-encoding"
        ) {
            return Err(diagnostic(
                "TG0120",
                "remote input header changes HTTP framing or routing",
                "supply authentication or content-negotiation headers only",
            ));
        }
        if allowed {
            headers.push((name, value));
        }
    }
    std::fs::create_dir_all(project).map_err(|_| cache_error())?;
    let root = project.join(".tungsten-inputs");
    directory(&root)?;
    let dir = root.join(hash(url.as_str().as_bytes()));
    directory(&dir)?;
    // Pin inspection and its possible replacement belong to the same online
    // transaction. A process paused before taking the lock must not reuse an
    // earlier observation that no pin existed. Offline readers never mutate.
    let _lock = (!options.offline)
        .then(|| SourceLock::acquire(&dir, "TG0123"))
        .transpose()?;
    let lock = dir.join("source.lock.json");
    let pin = match std::fs::symlink_metadata(&lock) {
        Ok(_) => {
            let pin: Pin =
                serde_json::from_slice(&read(&lock, LOCK_LIMIT)?).map_err(|_| cache_error())?;
            if pin.version != 1
                || pin.url != url.as_str()
                || pin.sha256.len() != 64
                || !pin
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(cache_error());
            }
            Some(pin)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(cache_error()),
    };
    let extension = content_extension(&url);
    let content_path = |digest: &str| dir.join(format!("{digest}.{extension}"));
    if options.offline {
        let pin = pin.ok_or_else(|| diagnostic("TG0123", "remote input has no offline pin", "run once online to fetch and pin the source, then retain .tungsten-inputs with the project"))?;
        let path = content_path(&pin.sha256);
        if hash(&read(&path, MAX_BYTES)?) != pin.sha256 {
            return Err(diagnostic(
                "TG0122",
                "cached remote input does not match its SHA256 pin",
                "restore the pinned cache from a trusted copy or fetch online; never edit a cached source in place",
            ));
        }
        return Ok(path);
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .http_status_as_error(false)
        .max_response_header_size(32 * 1024)
        .build()
        .new_agent();
    // Redirects are disabled above, so headers never follow a Location to
    // another origin; they reach only the URL named here.
    warn_cleartext_headers(&url, headers.len());
    let mut request = agent.get(url.as_str());
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let mut response = request.call().map_err(|_| diagnostic("TG0121", "remote input fetch failed (DNS, connection, TLS or timeout)", "verify network access and trusted TLS certificates, then retry; use --offline to reuse an existing verified pin"))?;
    if !response.status().is_success() {
        return Err(diagnostic(
            "TG0121",
            format!("remote input returned HTTP {}", response.status().as_u16()),
            "check access and --input-header authentication; redirects are refused, so pass the final document URL explicitly",
        ));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            diagnostic(
                "TG0121",
                "remote input body could not be read within the fetch deadline",
                "retry the fetch or use --offline with a previously verified pin",
            )
        })?;
    if bytes.len() > MAX_BYTES {
        return Err(diagnostic(
            "TG0121",
            "remote input exceeds the 8 MiB acquisition limit",
            "provide a smaller document or download and review it through the explicit local compiler flow",
        ));
    }
    let sha256 = hash(&bytes);
    if let Some(pin) = &pin
        && pin.sha256 != sha256
        && !options.refresh
    {
        return Err(diagnostic(
            "TG0122",
            "remote input changed: fetched SHA256 differs from the recorded pin",
            "review the upstream change, then pass --refresh-inputs explicitly to accept it; use --offline to regenerate from the existing pin",
        ));
    }
    // Content files are addressed by the SHA256 of these verified bytes, so a
    // missing or truncated one (an interrupted write) is restored in place.
    let path = content_path(&sha256);
    if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_file())
        || read(&path, MAX_BYTES)? != bytes
    {
        write_atomic(&dir, &path, &bytes, "TG0123")?;
    }
    if pin.as_ref().is_none_or(|pin| pin.sha256 != sha256) {
        let pin = Pin {
            version: 1,
            url: url.into(),
            sha256,
        };
        write_atomic(
            &dir,
            &lock,
            &serde_json::to_vec_pretty(&pin).map_err(|_| cache_error())?,
            "TG0123",
        )?;
    }
    Ok(path)
}
