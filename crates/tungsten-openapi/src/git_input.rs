// SPDX-License-Identifier: AGPL-3.0-only
//! Pinned Git entry acquisition. Git only writes a disposable bare repository;
//! checkout, hooks, submodules, filters and repository code are never executed.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use process_wrap::std::{ChildWrapper, CommandWrap};
use serde::{Deserialize, Serialize};
use tungsten_core::{Diagnostic, SourceId};
use url::Url;

use crate::acquire::{
    Options, SourceLock, content_hash, directory, hash, pending_path, read, write_atomic,
};

const MAX_FILES: usize = 512;
const MAX_FILE: usize = 8 * 1024 * 1024;
const MAX_TREE: u64 = 64 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(60);
/// How often a running fetch re-measures its object store against `MAX_TREE`.
const STORE_CHECK: Duration = Duration::from_millis(100);

fn error(code: &str, message: &str, help: &str) -> Diagnostic {
    Diagnostic::error(code, message).with_help(help)
}

fn invalid() -> Diagnostic {
    error(
        "TG0130",
        "invalid Git input reference",
        "use git+https://host/owner/repository@ref#path without credentials, query or unsafe path components; Git supplies authentication through its credential helper or environment",
    )
}

fn cache_error() -> Diagnostic {
    error(
        "TG0133",
        "Git input cache is missing, corrupt or unsafe",
        "retain the pinned .tungsten-inputs directory for offline use; inspect cache permissions and unexpected entries before retrying",
    )
}

pub fn is_source(source: &str) -> bool {
    source
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("git+"))
}

#[derive(Debug)]
struct Source {
    url: String,
    reference: String,
    scope: String,
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.eq_ignore_ascii_case(".git")
                && !part.ends_with(['.', ' '])
                && !matches!(
                    part.split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .as_str(),
                    "con"
                        | "prn"
                        | "aux"
                        | "nul"
                        | "com1"
                        | "com2"
                        | "com3"
                        | "com4"
                        | "com5"
                        | "com6"
                        | "com7"
                        | "com8"
                        | "com9"
                        | "lpt1"
                        | "lpt2"
                        | "lpt3"
                        | "lpt4"
                        | "lpt5"
                        | "lpt6"
                        | "lpt7"
                        | "lpt8"
                        | "lpt9"
                )
        })
}

fn commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn parse(source: &str) -> Result<Source, Diagnostic> {
    if source.len() > 8192 || source.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(invalid());
    }
    let source = source
        .get(4..)
        .filter(|_| is_source(source))
        .ok_or_else(invalid)?;
    let (address, scope) = source.split_once('#').unwrap_or((source, ""));
    if !scope.is_empty() && !safe_path(scope) {
        return Err(invalid());
    }
    let authority_end = address
        .find("://")
        .and_then(|at| address[at + 3..].find('/').map(|end| at + 3 + end))
        .ok_or_else(invalid)?;
    let (url, reference) = address[authority_end..]
        .find('@')
        .map(|at| {
            (
                &address[..authority_end + at],
                &address[authority_end + at + 1..],
            )
        })
        .unwrap_or((address, "HEAD"));
    if reference.is_empty()
        || reference.len() > 200
        || reference.starts_with(['-', '/'])
        || reference.ends_with(['/', '.'])
        || reference.contains("..")
        || reference.contains("//")
        || !reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    {
        return Err(invalid());
    }
    let url = Url::parse(url).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "https" | "http" | "file")
        || (url.scheme() != "file" && url.host_str().is_none())
        || (url.scheme() == "file" && url.host_str().is_some_and(|host| host != "localhost"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(Source {
        url: url.into(),
        reference: reference.to_owned(),
        scope: scope.to_owned(),
    })
}

pub(crate) fn display(source: &str) -> String {
    if parse(source).is_ok() {
        source.to_owned()
    } else {
        "<Git input>".to_owned()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    version: u8,
    url: String,
    reference: String,
    scope: String,
    commit: String,
    document: String,
    files: BTreeMap<String, String>,
}

struct Git {
    started: Instant,
    repository: PathBuf,
    hooks: PathBuf,
    #[cfg(windows)]
    owned_scratch: WindowsScratch,
}

impl Drop for Git {
    fn drop(&mut self) {
        // Only this exclusively created, literal scratch directory is removed.
        #[cfg(windows)]
        {
            let ordinary_directory = std::fs::symlink_metadata(&self.repository)
                .is_ok_and(|meta| meta.file_type().is_dir() && !meta.file_type().is_symlink());
            let resolved = std::fs::canonicalize(&self.repository).ok();
            if !ordinary_directory
                || resolved.as_ref() != Some(&self.owned_scratch.child)
                || self.owned_scratch.child.parent() != Some(self.owned_scratch.parent.as_path())
                || self.owned_scratch.child.file_name() != self.repository.file_name()
            {
                return;
            }
            let _ = std::fs::remove_dir_all(&self.owned_scratch.child);
        }
        #[cfg(not(windows))]
        let _ = std::fs::remove_dir_all(&self.repository);
    }
}

#[cfg(windows)]
struct WindowsScratch {
    parent: PathBuf,
    child: PathBuf,
}

#[cfg(windows)]
fn scratch_error() -> Diagnostic {
    error(
        "TG0133",
        "Git acquisition needs a usable local temporary directory with a shorter path",
        "set TMP or TEMP to a writable local disk directory with a shorter path; existing project pins remain available with --offline",
    )
}

// Git for Windows still has traditional path limits even when Rust can access
// the project cache. Reserve the terminating NUL and a SHA-256 pack sidecar
// plus its lock suffix. The empty-hooks cwd and Git's pack directories are
// shorter than this final file path. This is a conservative scratch policy,
// not a promise that every Git version or external helper supports long paths.
#[cfg(windows)]
const WINDOWS_SCRATCH_LIMIT: usize =
    260 - 1 - ("/objects/pack/pack-".len() + 64 + ".promisor.lock".len());

#[cfg(windows)]
fn local_disk_path(path: &Path) -> bool {
    use std::path::Prefix;
    path.is_absolute()
        && matches!(path.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
}

#[cfg(windows)]
fn scratch_supported(repository: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    local_disk_path(repository)
        && repository.to_str().is_some()
        && repository.as_os_str().encode_wide().count() <= WINDOWS_SCRATCH_LIMIT
}

#[cfg(windows)]
fn windows_scratch_root() -> Result<(PathBuf, PathBuf), Diagnostic> {
    use std::ffi::OsString;
    use std::path::Prefix;
    let root = std::env::temp_dir();
    // Reject network/device paths before filesystem access. Resolve aliases to
    // a physical local disk directory before choosing the exclusive child.
    if !local_disk_path(&root) {
        return Err(scratch_error());
    }
    let canonical = std::fs::canonicalize(root).map_err(|_| scratch_error())?;
    let mut parts = canonical.components();
    let ordinary = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(drive) => {
                let mut ordinary = OsString::from(format!("{}:", char::from(drive)));
                ordinary.push(parts.as_path().as_os_str());
                PathBuf::from(ordinary)
            }
            Prefix::Disk(_) => canonical.clone(),
            _ => return Err(scratch_error()),
        },
        _ => return Err(scratch_error()),
    };
    if !std::fs::symlink_metadata(&ordinary)
        .is_ok_and(|meta| meta.file_type().is_dir() && !meta.file_type().is_symlink())
    {
        return Err(scratch_error());
    }
    Ok((ordinary, canonical))
}

#[cfg(windows)]
fn create_windows_scratch(
    repository: PathBuf,
    scratch_parent: PathBuf,
    started: Instant,
) -> Result<Git, Diagnostic> {
    if !local_disk_path(&scratch_parent)
        || !scratch_supported(&repository)
        || repository.file_name().is_none()
        || repository
            .parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            != Some(scratch_parent.clone())
    {
        return Err(scratch_error());
    }
    // A collision is always refused. Never adopt an earlier invocation's
    // directory or install a cleanup guard for a pre-existing file or link.
    std::fs::create_dir(&repository).map_err(|_| scratch_error())?;
    let child = std::fs::canonicalize(&repository).map_err(|_| scratch_error())?;
    if child.parent() != Some(scratch_parent.as_path())
        || child.file_name() != repository.file_name()
        || !std::fs::symlink_metadata(&repository)
            .is_ok_and(|meta| meta.file_type().is_dir() && !meta.file_type().is_symlink())
    {
        return Err(scratch_error());
    }
    let hooks = repository.join("empty-hooks");
    let git = Git {
        started,
        repository,
        hooks,
        owned_scratch: WindowsScratch {
            parent: scratch_parent,
            child,
        },
    };
    // Ownership is established before setup; errors release only this child.
    std::fs::create_dir(&git.hooks).map_err(|_| scratch_error())?;
    Ok(git)
}

#[cfg(all(windows, feature = "testing"))]
#[doc(hidden)]
pub mod __testing {
    /// The production scratch-path policy, without filesystem effects.
    pub fn scratch_supported(path: &std::path::Path) -> bool {
        super::scratch_supported(path)
    }

    /// Exercise the production exclusive creation/cleanup boundary without Git.
    pub fn create_and_release_scratch(
        root: &std::path::Path,
        child: &std::path::Path,
    ) -> Result<(), tungsten_core::Diagnostic> {
        let parent = std::fs::canonicalize(root).map_err(|_| super::scratch_error())?;
        drop(super::create_windows_scratch(
            child.to_path_buf(),
            parent,
            std::time::Instant::now(),
        )?);
        Ok(())
    }
}

/// Own the complete Git process tree, including transports with closed pipes.
struct Child(Box<dyn ChildWrapper>);

impl std::ops::Deref for Child {
    type Target = dyn ChildWrapper;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl std::ops::DerefMut for Child {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.start_kill();
        let _ = self.wait();
    }
}

/// Kill and reap the group (Unix) or Job Object (Windows), never just Git.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn budget_error() -> Diagnostic {
    error(
        "TG0132",
        "Git acquisition exceeded its time or object-store budget",
        "use a smaller repository, retry network access, or reuse a verified pin with --offline",
    )
}

fn incomplete() -> Diagnostic {
    error(
        "TG0132",
        "Git output was incomplete or too large",
        "retry with a smaller repository or narrower #subpath",
    )
}

/// Read one `git cat-file --batch` response: `<oid> blob <size>\n<bytes>\n`.
/// Anything else (a missing object, another type, an oversized blob or a
/// truncated stream) ends the batch.
fn batch_blob(reader: &mut impl BufRead) -> Option<(String, Vec<u8>)> {
    let mut header = Vec::new();
    reader
        .by_ref()
        .take(256)
        .read_until(b'\n', &mut header)
        .ok()?;
    let header = std::str::from_utf8(header.strip_suffix(b"\n")?).ok()?;
    let mut fields = header.split(' ');
    let (oid, kind, size) = (fields.next()?, fields.next()?, fields.next()?);
    let size = size
        .parse::<usize>()
        .ok()
        .filter(|size| *size <= MAX_FILE)?;
    if fields.next().is_some() || kind != "blob" || !commit(oid) {
        return None;
    }
    let mut bytes = vec![0; size + 1];
    reader.read_exact(&mut bytes).ok()?;
    (bytes.pop() == Some(b'\n')).then(|| (oid.to_owned(), bytes))
}

/// Bounded traversal of our disposable bare object store, including pack files
/// while Git downloads them. A remote cannot bypass the byte budget with a pack.
fn store_size(path: &Path) -> Result<u64, Diagnostic> {
    let mut pending = vec![path.to_path_buf()];
    let mut entries = 0;
    let mut bytes = 0;
    while let Some(dir) = pending.pop() {
        let listing = match std::fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(cache_error()),
        };
        for entry in listing {
            let entry = entry.map_err(|_| cache_error())?;
            let meta = match std::fs::symlink_metadata(entry.path()) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(cache_error()),
            };
            entries += 1;
            if entries > 4096 || (!meta.file_type().is_dir() && !meta.file_type().is_file()) {
                return Err(cache_error());
            }
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                bytes += meta.len();
            }
            if bytes > MAX_TREE {
                return Err(error(
                    "TG0132",
                    "Git fetch exceeds the 64 MiB object-store limit",
                    "use a smaller repository or acquire a single document over HTTP(S)",
                ));
            }
        }
    }
    Ok(bytes)
}

impl Git {
    fn spawn(&self, args: &[&str], stdin: bool) -> Result<Child, Diagnostic> {
        if self.started.elapsed() >= DEADLINE {
            return Err(error(
                "TG0131",
                "Git input acquisition exceeded its 60-second deadline",
                "retry with a smaller repository or narrower #subpath, or reuse the verified pin with --offline",
            ));
        }
        let mut command = Command::new("git");
        // Do not discover an editor/project repository or inherit Git command
        // configuration from the parent. User-level credential configuration
        // remains available, but the working tree never supplies Git config.
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        command
            .current_dir(&self.hooks)
            .env("GIT_CEILING_DIRECTORIES", &self.repository)
            .args([
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "protocol.http.allow=always",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "http.followRedirects=false",
                "-c",
                "http.lowSpeedLimit=1",
                "-c",
                "http.lowSpeedTime=15",
                "-c",
                "fetch.fsckObjects=true",
                "-c",
                "transfer.fsckObjects=true",
                "-c",
                "gc.auto=0",
                "-c",
            ])
            .arg(format!("core.hooksPath={}", self.hooks.display()))
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        command.wrap(process_wrap::std::ProcessGroup::leader());
        #[cfg(windows)]
        {
            // JobObject creates the process suspended, assigns it, then resumes
            // it: no helper can escape between spawn and job assignment.
            command.wrap(process_wrap::std::CreationFlags(
                windows::Win32::System::Threading::CREATE_NO_WINDOW,
            ));
            command.wrap(process_wrap::std::JobObject);
        }
        command.spawn().map(Child).map_err(|_| {
            error(
                "TG0131",
                "cannot start Git for input acquisition",
                "install Git and check PATH; offline cache reuse does not require Git",
            )
        })
    }

    /// Block until `receiver` yields a value. The wait is bounded by the
    /// acquisition deadline; only a fetch, whose object store can grow, also
    /// wakes periodically to re-measure the store against its byte budget.
    fn receive<T>(
        &self,
        child: &mut Child,
        receiver: &mpsc::Receiver<T>,
        watch_store: bool,
    ) -> Result<T, Diagnostic> {
        loop {
            let Some(remaining) = DEADLINE
                .checked_sub(self.started.elapsed())
                .filter(|remaining| !remaining.is_zero())
            else {
                stop(child);
                return Err(budget_error());
            };
            let wait = if watch_store {
                remaining.min(STORE_CHECK)
            } else {
                remaining
            };
            match receiver.recv_timeout(wait) {
                Ok(value) => return Ok(value),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if watch_store && store_size(&self.repository).is_err() {
                        stop(child);
                        return Err(budget_error());
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    stop(child);
                    return Err(incomplete());
                }
            }
        }
    }

    /// Reap after output is complete, retaining the acquisition deadline.
    /// Closing stdout does not guarantee that the process has exited.
    fn finish(&self, child: &mut Child) -> Result<(), Diagnostic> {
        let status = loop {
            let Some(remaining) = DEADLINE
                .checked_sub(self.started.elapsed())
                .filter(|remaining| !remaining.is_zero())
            else {
                stop(child);
                return Err(budget_error());
            };
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(remaining.min(Duration::from_millis(10))),
                Err(_) => {
                    stop(child);
                    return Err(cache_error());
                }
            }
        };
        if !status.success() {
            return Err(error(
                "TG0131",
                "Git input acquisition failed (repository, ref, network or authentication)",
                "check the repository URL and ref, network access and Git credential helper; Git stderr and credentials are never included in diagnostics",
            ));
        }
        Ok(())
    }

    fn run(&self, args: &[&str], limit: usize, watch_store: bool) -> Result<Vec<u8>, Diagnostic> {
        let mut child = self.spawn(args, false)?;
        let Some(stdout) = child.stdout().take() else {
            stop(&mut child);
            return Err(cache_error());
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(limit as u64 + 1).read_to_end(&mut bytes);
            let _ = sender.send((result.is_ok() && bytes.len() <= limit).then_some(bytes));
        });
        let Some(bytes) = self.receive(&mut child, &receiver, watch_store)? else {
            stop(&mut child);
            return Err(error(
                "TG0132",
                "Git output exceeds its acquisition budget",
                "narrow #subpath or use a smaller repository",
            ));
        };
        self.finish(&mut child)?;
        if watch_store {
            store_size(&self.repository)?;
        }
        Ok(bytes)
    }

    /// Stream blobs through a single `git cat-file --batch` process instead of
    /// one process per file. Requests are written one at a time and each reply
    /// is consumed before the next request, so neither pipe can fill up while
    /// the other side waits. `each` receives every blob in request order.
    fn blobs(
        &self,
        repo: &str,
        objects: &[(&str, u64)],
        mut each: impl FnMut(usize, Vec<u8>) -> Result<(), Diagnostic>,
    ) -> Result<(), Diagnostic> {
        if objects.is_empty() {
            return Ok(());
        }
        let mut child = self.spawn(&["--git-dir", repo, "cat-file", "--batch"], true)?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin().take(), child.stdout().take()) else {
            stop(&mut child);
            return Err(cache_error());
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let blob = batch_blob(&mut reader);
                let last = blob.is_none();
                if sender.send(blob).is_err() || last {
                    break;
                }
            }
        });
        for (index, (oid, size)) in objects.iter().enumerate() {
            if stdin.write_all(format!("{oid}\n").as_bytes()).is_err() {
                stop(&mut child);
                return Err(incomplete());
            }
            let Some((echoed, bytes)) = self.receive(&mut child, &receiver, false)? else {
                stop(&mut child);
                return Err(incomplete());
            };
            if echoed != *oid || bytes.len() as u64 != *size {
                stop(&mut child);
                return Err(cache_error());
            }
            if let Err(diagnostic) = each(index, bytes) {
                stop(&mut child);
                return Err(diagnostic);
            }
        }
        // End of input: cat-file exits after answering every request.
        drop(stdin);
        self.finish(&mut child)
    }

    fn resolve(&self, source: &Source) -> Result<String, Diagnostic> {
        if commit(&source.reference) {
            return Ok(source.reference.clone());
        }
        let head = format!("refs/heads/{}", source.reference);
        let tag = format!("refs/tags/{}", source.reference);
        let peeled = format!("{tag}^{{}}");
        let exact_peeled = format!("{}^{{}}", source.reference);
        let bytes = self.run(
            &[
                "ls-remote",
                &source.url,
                &source.reference,
                &head,
                &tag,
                &peeled,
                &exact_peeled,
            ],
            256 * 1024,
            false,
        )?;
        let lines = std::str::from_utf8(&bytes).map_err(|_| cache_error())?;
        let refs = lines
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .filter(|(sha, _)| commit(sha))
            .map(|(sha, name)| (name.to_owned(), sha.to_owned()))
            .collect::<BTreeMap<_, _>>();
        let found = [&source.reference, &head, &tag]
            .into_iter()
            .filter_map(|name| refs.get(&format!("{name}^{{}}")).or_else(|| refs.get(name)))
            .cloned()
            .collect::<BTreeSet<_>>();
        if found.len() != 1 {
            return Err(error(
                "TG0131",
                "Git ref does not resolve to exactly one commit",
                "use a full refs/heads/... or refs/tags/... name, or an explicit commit digest",
            ));
        }
        Ok(found.into_iter().next().expect("one resolved commit"))
    }
}

fn candidate(path: &str, bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    if path.ends_with(".http")
        || path.ends_with(".rest")
        || text.trim_start().starts_with("curl ")
        || text.trim_start().starts_with("curl.exe ")
    {
        return true;
    }
    let Ok(parsed) = crate::parse::parse(path, text, SourceId(0), 128) else {
        return false;
    };
    let root = parsed.value;
    root.get("openapi").is_some()
        || root.get("swagger").is_some()
        || root.get("endpoint_manifest").is_some()
        || root.get("log").is_some()
        || (root.get("item").is_some() && root.get("info").is_some())
}

fn verify(dir: &Path, source: &Source, pin: &Pin) -> Result<PathBuf, Diagnostic> {
    if pin.version != 1
        || pin.url != source.url
        || pin.reference != source.reference
        || pin.scope != source.scope
        || !commit(&pin.commit)
        || pin.files.is_empty()
        || pin.files.len() > MAX_FILES
        || !pin.files.contains_key(&pin.document)
    {
        return Err(cache_error());
    }
    let root = dir.join("files");
    let tree = root.join(&pin.commit);
    directory(&root)?;
    directory(&tree)?;
    let mut directories = BTreeSet::new();
    let listed = pin.files.keys().map(Path::new).collect::<BTreeSet<_>>();
    let mut total = 0;
    for (path, sha256) in &pin.files {
        if !safe_path(path)
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(cache_error());
        }
        let mut parent = tree.clone();
        for part in Path::new(path)
            .parent()
            .into_iter()
            .flat_map(Path::components)
        {
            parent.push(part.as_os_str());
            directories.insert(
                parent
                    .strip_prefix(&tree)
                    .map_err(|_| cache_error())?
                    .to_path_buf(),
            );
            if !std::fs::symlink_metadata(&parent).is_ok_and(|meta| meta.file_type().is_dir()) {
                return Err(cache_error());
            }
        }
        let bytes = read(&tree.join(path), MAX_FILE)?;
        total += bytes.len();
        if total as u64 > MAX_TREE || hash(&bytes) != *sha256 {
            return Err(cache_error());
        }
    }
    // A loader may follow any confined relative `$ref`. Verify the complete
    // materialized read set, including extra entries, so an unlisted sibling
    // cannot change offline compilation without changing the source pin.
    let mut pending = vec![tree.clone()];
    let mut seen = BTreeSet::new();
    let mut nodes = 0usize;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).map_err(|_| cache_error())? {
            let entry = entry.map_err(|_| cache_error())?;
            let path = entry.path();
            let relative = path.strip_prefix(&tree).map_err(|_| cache_error())?;
            let meta = std::fs::symlink_metadata(&path).map_err(|_| cache_error())?;
            nodes += 1;
            if nodes > directories.len() + pin.files.len() {
                return Err(cache_error());
            }
            if meta.file_type().is_dir() && directories.contains(relative) {
                pending.push(path);
            } else if meta.file_type().is_file() && listed.contains(relative) {
                seen.insert(relative.to_path_buf());
            } else {
                return Err(cache_error());
            }
        }
    }
    if seen.len() != pin.files.len() {
        return Err(cache_error());
    }
    Ok(tree.join(&pin.document))
}

/// Reuse an existing commit pin, or explicitly resolve/fetch a new one. Online
/// reuse deliberately retains the pinned commit until `--refresh-inputs`.
/// Pin record and pinned tree files of an already verified Git source.
pub(crate) fn pinned_files(text: &str, project: &Path) -> Result<Vec<String>, Diagnostic> {
    let source = parse(text)?;
    let key = hash(format!("{}@{}#{}", source.url, source.reference, source.scope).as_bytes());
    let dir = project.join(".tungsten-inputs").join("git").join(&key);
    let pin: Pin = serde_json::from_slice(&read(&dir.join("source.lock.json"), 1024 * 1024)?)
        .map_err(|_| cache_error())?;
    verify(&dir, &source, &pin)?;
    let mut files = vec![format!(".tungsten-inputs/git/{key}/source.lock.json")];
    files.extend(
        pin.files
            .keys()
            .map(|path| format!(".tungsten-inputs/git/{key}/files/{}/{path}", pin.commit)),
    );
    Ok(files)
}

pub fn acquire(text: &str, project: &Path, options: &Options) -> Result<PathBuf, Diagnostic> {
    let source = parse(text)?;
    if options.offline && options.refresh {
        return Err(invalid());
    }
    std::fs::create_dir_all(project).map_err(|_| cache_error())?;
    let root = project.join(".tungsten-inputs");
    directory(&root)?;
    let git_root = root.join("git");
    directory(&git_root)?;
    let key = format!("{}@{}#{}", source.url, source.reference, source.scope);
    let dir = git_root.join(hash(key.as_bytes()));
    directory(&dir)?;
    // Observe the pin only while holding the online transaction lock. A
    // concurrent acquisition may have committed a pin before this run starts.
    let _lock = (!options.offline)
        .then(|| SourceLock::acquire(&dir, "TG0133"))
        .transpose()?;
    let lock = dir.join("source.lock.json");
    if std::fs::symlink_metadata(&lock).is_ok() {
        let pin: Pin =
            serde_json::from_slice(&read(&lock, 1024 * 1024)?).map_err(|_| cache_error())?;
        let path = verify(&dir, &source, &pin)?;
        if options.offline || !options.refresh {
            return Ok(path);
        }
    } else if options.offline {
        return Err(cache_error());
    }
    let started = Instant::now();
    #[cfg(windows)]
    let (scratch_root, scratch_parent) = windows_scratch_root()?;
    #[cfg(windows)]
    let repository = pending_path(&scratch_root, "tungsten-git");
    #[cfg(windows)]
    let git = create_windows_scratch(repository, scratch_parent, started)?;
    #[cfg(not(windows))]
    let git = {
        let repository = pending_path(&dir, "fetch");
        // Exclusive creation: never run Git through a pre-existing config or symlink.
        std::fs::create_dir(&repository).map_err(|_| {
            error(
                "TG0133",
                &format!(
                    "cannot create the Git scratch repository: {}",
                    repository.display()
                ),
                "use a writable project directory, then retry",
            )
        })?;
        let repository = std::path::absolute(repository).map_err(|_| cache_error())?;
        let hooks = repository.join("empty-hooks");
        std::fs::create_dir(&hooks).map_err(|_| cache_error())?;
        Git {
            started,
            repository,
            hooks,
        }
    };
    let resolved = git.resolve(&source)?;
    let repo = git.repository.to_string_lossy().into_owned();
    let format = if resolved.len() == 64 {
        "--object-format=sha256"
    } else {
        "--object-format=sha1"
    };
    git.run(
        &[
            "init",
            "--bare",
            "--quiet",
            format,
            "--template",
            &git.hooks.to_string_lossy(),
            &repo,
        ],
        1024,
        false,
    )?;
    git.run(
        &[
            "--git-dir",
            &repo,
            "fetch",
            "--depth=1",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-auto-maintenance",
            "--quiet",
            &source.url,
            &resolved,
        ],
        1024,
        true,
    )?;
    let actual = git.run(
        &[
            "--git-dir",
            &repo,
            "rev-parse",
            "--verify",
            "FETCH_HEAD^{commit}",
        ],
        1024,
        false,
    )?;
    if std::str::from_utf8(&actual).map(str::trim).ok() != Some(resolved.as_str()) {
        return Err(cache_error());
    }
    let scope = if source.scope.is_empty() {
        "."
    } else {
        &source.scope
    };
    let listing = git.run(
        &[
            "--git-dir",
            &repo,
            "ls-tree",
            "-r",
            "-l",
            "-z",
            "--full-tree",
            &resolved,
            "--",
            scope,
        ],
        2 * 1024 * 1024,
        false,
    )?;
    let tree_root = dir.join("files");
    directory(&tree_root)?;
    let tree = tree_root.join(&resolved);
    directory(&tree)?;
    // Validate the complete listing before reading any blob.
    let mut entries = Vec::new();
    let mut total = 0u64;
    for line in listing
        .split(|byte| *byte == 0)
        .filter(|line| !line.is_empty())
    {
        let line = std::str::from_utf8(line).map_err(|_| cache_error())?;
        let (metadata, path) = line.split_once('\t').ok_or_else(cache_error)?;
        let parts = metadata.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 4
            || !matches!(parts[0], "100644" | "100755")
            || parts[1] != "blob"
            || !commit(parts[2])
            || !safe_path(path)
            || (!source.scope.is_empty()
                && path != source.scope
                && !path.starts_with(&format!("{}/", source.scope)))
        {
            return Err(error(
                "TG0132",
                "Git input contains a symlink, submodule or unsafe path",
                "choose a subpath containing ordinary input files only; links and submodules are never followed",
            ));
        }
        let size = parts[3].parse::<u64>().map_err(|_| cache_error())?;
        total = total.checked_add(size).ok_or_else(cache_error)?;
        if entries.len() >= MAX_FILES || size > MAX_FILE as u64 || total > MAX_TREE {
            return Err(error(
                "TG0132",
                "Git input exceeds 512 files, 8 MiB per file or 64 MiB total",
                "narrow #subpath to the contract directory or acquire a single document over HTTP(S)",
            ));
        }
        entries.push((path, parts[2], size));
    }
    let objects = entries
        .iter()
        .map(|(_, oid, size)| (*oid, *size))
        .collect::<Vec<_>>();
    let mut files = BTreeMap::new();
    let mut candidates = Vec::new();
    git.blobs(&repo, &objects, |index, bytes| {
        let path = entries[index].0;
        if candidate(path, &bytes) {
            candidates.push(path.to_owned());
        }
        let mut parent = tree.clone();
        for part in Path::new(path)
            .parent()
            .into_iter()
            .flat_map(Path::components)
        {
            parent.push(part.as_os_str());
            directory(&parent)?;
        }
        // The tree is keyed by the verified commit, so a missing or truncated
        // file (an interrupted write) is restored from the fetched blob.
        let digest = hash(&bytes);
        let destination = tree.join(path);
        if !std::fs::symlink_metadata(&destination).is_ok_and(|meta| meta.file_type().is_file())
            || content_hash(&destination)? != digest
        {
            write_atomic(&dir, &destination, &bytes, "TG0133")?;
        }
        files.insert(path.to_owned(), digest);
        Ok(())
    })?;
    if candidates.len() != 1 {
        return Err(Diagnostic::error("TG0134",format!("Git input discovery found {} candidate documents: {}",candidates.len(),candidates.iter().take(16).cloned().collect::<Vec<_>>().join(", "))).with_help("choose exactly one supported document using #path, or narrow the repository subdirectory; nothing is guessed"));
    }
    let document = candidates.remove(0);
    let pin = Pin {
        version: 1,
        url: source.url.clone(),
        reference: source.reference.clone(),
        scope: source.scope.clone(),
        commit: resolved,
        document: document.clone(),
        files,
    };
    let verified = verify(&dir, &source, &pin)?;
    write_atomic(
        &dir,
        &lock,
        &serde_json::to_vec_pretty(&pin).map_err(|_| cache_error())?,
        "TG0133",
    )?;
    Ok(verified)
}
