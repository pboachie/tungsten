// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten doctor`: which optional external tools are installed.
//!
//! Every tool is optional, so doctor always succeeds. Tool paths are not
//! reported (they are machine-specific); versions are.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::output::{CommandName, CommandResult, DoctorResult, ToolReport};
use crate::{CliEnv, Report};

/// Tools in report order, with what tungsten uses them for.
const TOOLS: [(&str, &str); 8] = [
    ("cargo", "Rust SDK and CLI targets"),
    ("node", "TypeScript SDK and MCP server"),
    ("python3", "Python SDK"),
    ("deno", "sandboxed run_script in the MCP server (opt-in)"),
    ("uv", "Python SDK packaging"),
    ("prettier", "formatting TypeScript output (--format)"),
    ("ruff", "formatting Python output (--format)"),
    ("rustfmt", "formatting Rust output (--format)"),
];
/// How long a tool may take to print its version.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) fn run(env: &CliEnv) -> Report {
    let search_path = env
        .search_path
        .clone()
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    let tools: Vec<ToolReport> = TOOLS
        .iter()
        .map(|&(name, purpose)| {
            let exe = find_executable(name, &search_path);
            ToolReport {
                name: name.to_string(),
                found: exe.is_some(),
                version: exe.as_deref().and_then(tool_version),
                purpose: purpose.to_string(),
            }
        })
        .collect();
    let mut report = Report::new(CommandName::Doctor);
    report.human = human(&tools);
    report.result = Some(CommandResult::Doctor(DoctorResult { tools }));
    report
}

fn human(tools: &[ToolReport]) -> String {
    let mut out = format!("tungsten {} · doctor\n", tungsten_build::TUNGSTEN_VERSION);
    for t in tools {
        let status = match (&t.version, t.found) {
            (Some(v), _) => v.clone(),
            (None, true) => "found".to_string(),
            (None, false) => "missing".to_string(),
        };
        let _ = writeln!(out, "  {:<10} {:<14} {}", t.name, status, t.purpose);
    }
    out.push_str("all tools are optional; a missing tool only disables what it is listed for\n");
    out
}

/// The first executable file named `name` on `search_path`.
fn find_executable(name: &str, search_path: &OsString) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates(&dir, name))
        .find(|p| is_executable(p))
}

#[cfg(windows)]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .map(|e| dir.join(format!("{name}{e}")))
        .collect()
}

#[cfg(not(windows))]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![dir.join(name)]
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Run `<exe> --version` with a timeout and parse its output.
fn tool_version(exe: &Path) -> Option<String> {
    let mut child = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let read = |rx: mpsc::Receiver<String>| rx.recv_timeout(Duration::from_secs(1)).ok();
    let out = read(stdout).unwrap_or_default();
    let err = read(stderr).unwrap_or_default();
    // Some tools (older Python) print their version on stderr.
    parse_version(&out).or_else(|| parse_version(&err))
}

/// Read a pipe to the end on a thread so a chatty child cannot block on a
/// full pipe while we wait for it.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        });
    }
    rx
}

/// The first version-like token on the first non-empty line:
/// `cargo 1.97.0 (abc)` → `1.97.0`, `v20.1.0` → `20.1.0`.
pub(crate) fn parse_version(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    line.split_whitespace().find_map(|token| {
        let token = token.strip_prefix('v').unwrap_or(token);
        let token = token.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        let version_like = token.starts_with(|c: char| c.is_ascii_digit())
            && token.contains('.')
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
        version_like.then(|| token.to_string())
    })
}
