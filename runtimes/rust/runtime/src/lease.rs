// SPDX-License-Identifier: Apache-2.0
//! The cross-process lease of a `FileIdempotencyStore` (`<path>.lock`), the
//! same file in every runtime: created exclusively, it records its owner
//! `{"version": 1, "host", "pid", "token"}`. A busy lease is waited for up to
//! [`LEASE_WAIT`], then `reserve` refuses, naming the owner. Only its owner
//! removes a lease: no writer reclaims another's, because a reclaimer cannot
//! remove a lease and check whose it was in one step (a delayed reclaimer
//! could remove a newer live lease). A lease left by a process that died is
//! removed by hand. The holder re-checks its token just before it commits the
//! store file, so a writer whose lease was removed never overwrites a key.

use std::io::Write as _;
use std::path::Path;
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

/// This host's name, for the refusal message, where it can be read without
/// platform calls (Linux); elsewhere empty.
fn host() -> String {
    if !cfg!(target_os = "linux") {
        return String::new();
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_default()
}

/// The owner of `lease`; None when it cannot be read or parsed (still being
/// written, or not a lease). The host may be empty: it is only reported.
fn read_owner(lease: &Path) -> Option<Owner> {
    let text = std::fs::read_to_string(lease).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    (value.get("version")? == 1).then_some(())?;
    Some(Owner {
        host: value.get("host")?.as_str()?.to_owned(),
        pid: value.get("pid")?.as_u64().filter(|p| *p > 0)?,
        token: value
            .get("token")?
            .as_str()
            .filter(|t| !t.is_empty())?
            .to_owned(),
    })
}

/// Takes the lease and returns its token.
pub(crate) fn acquire(lease: &Path) -> Result<String, String> {
    let token = random_bytes::<16>()
        .map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>())
        .ok_or("no randomness for the idempotency store lease")?;
    let mine = json!({
        "version": 1,
        "host": host(),
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
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(refusal(lease, read_owner(lease).as_ref()));
        }
        std::thread::sleep(delay.min(left));
        delay = (delay * 2).min(Duration::from_millis(100));
    }
}

fn refusal(lease: &Path, owner: Option<&Owner>) -> String {
    match owner {
        None => format!(
            "the idempotency store is locked by another writer ({}); try again",
            lease.display()
        ),
        Some(owner) => format!(
            "the idempotency store is locked by another writer ({}, process {} on {:?}); try again, or remove the lease once that process no longer runs",
            lease.display(),
            owner.pid,
            owner.host
        ),
    }
}

/// Whether `lease` still holds `token`.
fn owns(lease: &Path, token: &str) -> bool {
    read_owner(lease).is_some_and(|owner| owner.token == token)
}

/// Fails when the lease is no longer this writer's: the key is then not stored.
pub(crate) fn fence(lease: &Path, token: &str) -> std::io::Result<()> {
    if owns(lease, token) {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "the idempotency store lease {} is no longer this writer's; the key was not stored",
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
