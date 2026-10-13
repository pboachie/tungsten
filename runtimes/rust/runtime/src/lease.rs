// SPDX-License-Identifier: Apache-2.0
//! The cross-process lease of a `FileIdempotencyStore` (`<path>.lock`), the
//! same file in every runtime: created exclusively, it records its owner
//! `{"version": 1, "host", "pid", "token"}`. A busy lease is waited for up to
//! [`LEASE_WAIT`]. It is reclaimed only from an owner that is provably dead
//! (same host, another process, and that process no longer runs); any other
//! owner (alive, on another host, or unknown) makes `reserve` refuse after the
//! wait. The holder re-checks its token just before it commits the store
//! file, so a writer whose lease was taken over never overwrites the new
//! owner's key.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::util::random_bytes;

/// How long `reserve` waits for another writer's lease.
const LEASE_WAIT: Duration = Duration::from_secs(2);

struct Owner {
    host: String,
    pid: u64,
    token: String,
}

/// This host's name, where it can be read without platform calls (Linux);
/// elsewhere none, and no lease is ever reclaimed.
fn host() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
    Some(name.trim().to_owned()).filter(|n| !n.is_empty())
}

/// Whether no process with `pid` runs on this host. Only Linux can tell
/// (`/proc`); elsewhere a process is never known to have ended.
fn process_gone(pid: u64) -> bool {
    cfg!(target_os = "linux")
        && Path::new("/proc/self").exists()
        && !Path::new(&format!("/proc/{pid}")).exists()
}

/// The owner of `lease`: `Err` when the lease is gone, `Ok(None)` when it
/// cannot be read or parsed (still being written, or not a lease).
fn read_owner(lease: &Path) -> Result<Option<Owner>, ()> {
    let text = match std::fs::read_to_string(lease) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(()),
        Err(_) => return Ok(None),
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Ok(None);
    };
    let owner = (|| {
        (value.get("version")? == 1).then_some(())?;
        Some(Owner {
            host: value
                .get("host")?
                .as_str()
                .filter(|h| !h.is_empty())?
                .to_owned(),
            pid: value.get("pid")?.as_u64().filter(|p| *p > 0)?,
            token: value
                .get("token")?
                .as_str()
                .filter(|t| !t.is_empty())?
                .to_owned(),
        })
    })();
    Ok(owner)
}

fn provably_dead(owner: &Owner) -> bool {
    host().is_some_and(|h| h == owner.host)
        && owner.pid != u64::from(std::process::id())
        && process_gone(owner.pid)
}

fn sibling(lease: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{suffix}", lease.display()))
}

/// Takes the lease and returns its token.
pub(crate) fn acquire(lease: &Path) -> Result<String, String> {
    let token = random_bytes::<16>()
        .map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>())
        .ok_or("no randomness for the idempotency store lease")?;
    let mine = json!({
        "version": 1,
        "host": host().unwrap_or_default(),
        "pid": std::process::id(),
        "token": token,
    });
    let deadline = Instant::now() + LEASE_WAIT;
    let mut delay = Duration::from_millis(5);
    loop {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(lease) {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{mine}") {
                    let _ = std::fs::remove_file(lease);
                    return Err(e.to_string());
                }
                return Ok(token);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
        let owner = read_owner(lease);
        let mut retry_now = owner.is_err();
        if let Ok(Some(dead)) = &owner
            && provably_dead(dead)
        {
            retry_now = reclaim(lease, dead, &token);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(refusal(lease, owner.ok().flatten().as_ref()));
        }
        if !retry_now {
            std::thread::sleep(delay.min(left));
            delay = (delay * 2).min(Duration::from_millis(100));
        }
    }
}

/// Removes a dead owner's lease. It moves the lease aside first: if what it
/// moved is not the dead owner's (a live writer took the lease in between),
/// it puts it back. Returns whether to retry at once.
fn reclaim(lease: &Path, dead: &Owner, token: &str) -> bool {
    let aside = sibling(lease, &format!("{token}.stale"));
    if let Err(e) = std::fs::rename(lease, &aside) {
        return e.kind() == std::io::ErrorKind::NotFound;
    }
    let reclaimed = matches!(read_owner(&aside), Ok(Some(moved)) if moved.token == dead.token);
    if !reclaimed {
        let _ = std::fs::hard_link(&aside, lease);
    }
    let _ = std::fs::remove_file(&aside);
    reclaimed
}

fn refusal(lease: &Path, owner: Option<&Owner>) -> String {
    match owner {
        None => format!(
            "the idempotency store is locked by another writer ({}); try again",
            lease.display()
        ),
        Some(owner) => format!(
            "the idempotency store is locked by another writer ({}, process {} on {}); try again, or remove the lease if that process no longer runs",
            lease.display(),
            owner.pid,
            owner.host
        ),
    }
}

/// Whether `lease` still holds `token`.
fn owns(lease: &Path, token: &str) -> bool {
    matches!(read_owner(lease), Ok(Some(owner)) if owner.token == token)
}

/// Fails when the lease was taken over: the key is then not stored.
pub(crate) fn fence(lease: &Path, token: &str) -> std::io::Result<()> {
    if owns(lease, token) {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "the idempotency store lease {} was taken over by another writer; the key was not stored",
        lease.display()
    )))
}

/// Removes the lease if it is still this writer's; failures are ignored (the
/// outcome is decided).
pub(crate) fn release(lease: &Path, token: &str) {
    if owns(lease, token) {
        let _ = std::fs::remove_file(lease);
    }
}
