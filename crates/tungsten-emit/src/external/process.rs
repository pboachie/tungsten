// SPDX-License-Identifier: AGPL-3.0-only
//! Running an emitter process with a timeout and an output cap.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::{Command, Limits};

const POLL: Duration = Duration::from_millis(2);
/// How long to wait for a pipe to close after the process ended (a
/// grandchild may hold it open).
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// How much of standard error is kept: its end.
const STDERR_KEEP: usize = 8 * 1024;
/// Lines of standard error quoted in a failure.
const STDERR_LINES: usize = 12;

/// What a finished run produced.
#[derive(Debug)]
pub(crate) struct Finished {
    pub stdout: Vec<u8>,
    /// The last lines of standard error.
    pub stderr_tail: String,
    pub code: Option<i32>,
}

/// Why a run did not finish normally.
#[derive(Debug)]
pub(crate) enum Failure {
    Spawn(std::io::Error),
    Timeout(Duration),
    TooLarge(usize),
}

enum Keep {
    /// The first `n` bytes; more sets the overflow flag and stops reading.
    Head(usize),
    /// The last `n` bytes; everything is read.
    Tail(usize),
}

struct Captured {
    bytes: Vec<u8>,
}

fn reader(
    mut pipe: impl Read + Send + 'static,
    keep: Keep,
    overflow: Arc<AtomicBool>,
) -> mpsc::Receiver<Captured> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes: Vec<u8> = vec![];
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            bytes.extend_from_slice(&chunk[..n]);
            match keep {
                Keep::Head(max) if bytes.len() > max => {
                    overflow.store(true, Ordering::SeqCst);
                    break;
                }
                Keep::Tail(max) if bytes.len() > 2 * max => {
                    bytes.drain(..bytes.len() - max);
                }
                _ => {}
            }
        }
        if let Keep::Tail(max) = keep
            && bytes.len() > max
        {
            bytes.drain(..bytes.len() - max);
        }
        let _ = tx.send(Captured { bytes });
    });
    rx
}

fn tail_lines(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(STDERR_LINES);
    lines[from..].join("\n")
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Start `command` with `extra` arguments in `cwd`, write `input` to its
/// standard input and collect its output within `limits`.
pub(crate) fn run(
    command: &Command,
    extra: &[&str],
    cwd: &Path,
    input: Vec<u8>,
    limits: Limits,
) -> Result<Finished, Failure> {
    let mut child = std::process::Command::new(&command.program)
        .args(&command.args)
        .args(extra)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Failure::Spawn)?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child
        .stdout
        .take()
        .map(|p| reader(p, Keep::Head(limits.max_output_bytes), overflow.clone()));
    let stderr = child
        .stderr
        .take()
        .map(|p| reader(p, Keep::Tail(STDERR_KEEP), Arc::new(AtomicBool::new(false))));
    // The emitter may exit without reading its input; the write then fails
    // and that is its business.
    if let Some(mut stdin) = child.stdin.take() {
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let deadline = Instant::now() + limits.timeout;
    let status = loop {
        if overflow.load(Ordering::SeqCst) {
            kill(&mut child);
            return Err(Failure::TooLarge(limits.max_output_bytes));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            Ok(None) => {
                kill(&mut child);
                return Err(Failure::Timeout(limits.timeout));
            }
            Err(e) => {
                kill(&mut child);
                return Err(Failure::Spawn(e));
            }
        }
    };
    let collect = |rx: Option<mpsc::Receiver<Captured>>| {
        rx.and_then(|rx| rx.recv_timeout(PIPE_GRACE).ok())
            .map(|c| c.bytes)
            .unwrap_or_default()
    };
    let stdout = collect(stdout);
    if overflow.load(Ordering::SeqCst) {
        return Err(Failure::TooLarge(limits.max_output_bytes));
    }
    Ok(Finished {
        stdout,
        stderr_tail: tail_lines(&collect(stderr)),
        code: status.code(),
    })
}
