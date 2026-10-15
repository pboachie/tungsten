// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded transport for entry documents. Remote references remain a separate
//! loader policy: acquiring an entry never grants access to its neighbours.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tungsten_core::Diagnostic;
use url::Url;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const LOCK_LIMIT: usize = 32 * 1024;

/// Acquisition policy. Header values are never formatted in diagnostics or Debug.
#[derive(Clone, Default)]
pub struct Options {
    pub offline: bool,
    pub refresh: bool,
    /// Repeatable `Name: value` headers; supplied only to the requested origin.
    pub headers: Vec<String>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("offline", &self.offline)
            .field("refresh", &self.refresh)
            .field("headers", &self.headers.len())
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
    source.to_ascii_lowercase().starts_with("http://")
        || source.to_ascii_lowercase().starts_with("https://")
}

/// Display a source without reflecting forbidden URL credentials or queries.
pub fn display_source(source: &str) -> String {
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

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// SHA256 of a bounded, regular acquired file, for conversion provenance.
pub fn content_hash(path: &Path) -> Result<String, Diagnostic> {
    read(path, MAX_BYTES).map(|bytes| hash(&bytes))
}

fn directory(path: &Path) -> Result<(), Diagnostic> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        Ok(_) => Err(cache_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|_| cache_error())
        }
        Err(_) => Err(cache_error()),
    }
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, Diagnostic> {
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

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Diagnostic> {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| cache_error())?;
    file.write_all(bytes).map_err(|_| cache_error())?;
    file.sync_all().map_err(|_| cache_error())
}

/// Fetch or reuse a pinned input. The per-source lock and immutable content live
/// in `project/.tungsten-inputs/<sha256-of-url>/`. Retain this directory securely
/// when offline reproducibility is required. Ordinary online loads verify
/// upstream bytes against the pin; only explicit refresh replaces a pin.
pub fn acquire(source: &str, project: &Path, options: &Options) -> Result<PathBuf, Diagnostic> {
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
        headers.push((name, value));
    }
    std::fs::create_dir_all(project).map_err(|_| cache_error())?;
    let root = project.join(".tungsten-inputs");
    directory(&root)?;
    let dir = root.join(hash(url.as_str().as_bytes()));
    directory(&dir)?;
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
    // Preserve only recognized format suffixes, never arbitrary remote filenames.
    let extension = url.path().rsplit('.').next().unwrap_or("");
    let extension = match extension {
        "json" | "yaml" | "yml" | "http" | "rest" | "curl" | "har" => extension,
        _ => "input",
    };
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
    let path = content_path(&sha256);
    if std::fs::symlink_metadata(&path).is_ok() {
        if read(&path, MAX_BYTES)? != bytes {
            return Err(cache_error());
        }
    } else {
        write_new(&path, &bytes)?;
    }
    if pin.as_ref().is_none_or(|pin| pin.sha256 != sha256) {
        let pin = Pin {
            version: 1,
            url: url.into(),
            sha256,
        };
        let temporary = dir.join("source.lock.pending");
        write_new(
            &temporary,
            &serde_json::to_vec_pretty(&pin).map_err(|_| cache_error())?,
        )?;
        std::fs::rename(&temporary, &lock).map_err(|_| cache_error())?;
    }
    Ok(path)
}
