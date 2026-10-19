// SPDX-License-Identifier: AGPL-3.0-only
//! External emitters: code generators that are separate executables.
//!
//! A built-in emitter implements [`crate::Emitter`] inside the compiler. An
//! external emitter is any program that speaks the JSON protocol below, so a
//! Go, Java or Swift generator can be written in its own language and
//! shipped on its own schedule, yet its output goes through the same writer
//! as the built-in targets: `tungsten generate --check`, the
//! `.tungsten/manifest.json` digests, the API surface diff and the report
//! work unchanged.
//!
//! # Protocol version 1
//!
//! ## Finding the emitter
//!
//! A target in `tungsten.yml` whose name is not built in is external when it
//! has an `external` key:
//!
//! ```yaml
//! targets:
//!   go:
//!     out: generated/go
//!     external: true                  # tungsten-emit-go on PATH
//!   swift:
//!     out: generated/swift
//!     external: ./tools/emit-swift    # a path (relative to tungsten.yml)
//!     package: Acme                   # every other key is an emitter option
//!   java:
//!     out: generated/java
//!     external: node tools/emit-java.js --strict   # a command line
//!     timeout_ms: 120000              # tungsten's own limits
//!     max_output_bytes: 134217728
//! ```
//!
//! `external: true` runs the executable `tungsten-emit-<name>` found on
//! `PATH`. A string is a command line (split on spaces; single and double
//! quotes group words); its program is looked up on `PATH` when it has no
//! slash, otherwise relative to the directory of `tungsten.yml`. The process
//! runs with that directory as its working directory.
//!
//! The keys `out`, `external`, `timeout_ms` and `max_output_bytes` belong to
//! tungsten; all other keys of the target are the emitter's `options`.
//!
//! ## One run
//!
//! The compiler starts the emitter with no arguments, writes one
//! [`EmitRequest`] as JSON to its standard input, closes it, and reads one
//! [`EmitResponse`] as JSON from its standard output. Standard error is free
//! text for humans; its last lines are quoted when the emitter fails. Exit
//! status 0 means a response was produced; anything else is TG0803 (the
//! response is ignored). The request:
//!
//! ```json
//! { "protocol": 1,
//!   "target": "go",
//!   "compiler": { "name": "tungsten", "version": "0.1.0" },
//!   "options": { "package": "acme" },
//!   "output": { "policy": "relative-paths-only", "custom_segment": "custom",
//!               "reserved_dir": ".tungsten", "max_files": 10000,
//!               "max_total_bytes": 67108864 },
//!   "ir": { "...": "the document of `tungsten ir dump`, specs/ir.schema.json" } }
//! ```
//!
//! and the response:
//!
//! ```json
//! { "protocol": 1,
//!   "files": [ { "path": "client.go", "content": "package acme\n" },
//!              { "path": "data.bin", "content_base64": "AAEC" } ],
//!   "diagnostics": [ { "code": "GO001", "severity": "warning",
//!                      "message": "operation `x` has no Go mapping",
//!                      "span": { "file": "petstore.yaml", "pointer": "/paths" } } ],
//!   "surface": { "tools": { "get_pet": "pets.getPet" } } }
//! ```
//!
//! - `files[].path` is relative to the target's `out` directory with forward
//!   slashes. The emitter never learns the absolute output path and never
//!   writes files itself; the compiler writes what the response lists. A
//!   path that is absolute, has a `..`, `.` or empty segment, a backslash, a
//!   NUL byte or a drive prefix, or starts with `.tungsten/`, is refused
//!   (TG0805) and nothing of the target is written. A file under a `custom`
//!   segment is created once and never overwritten (as for built-in
//!   targets).
//! - Exactly one of `content` (UTF-8 text) and `content_base64` (standard
//!   alphabet, padded) carries the bytes. `mode` is optional and may only be
//!   `"file"` (a regular file); it is reserved for future versions.
//! - `diagnostics[].severity` is `error`, `warning` or `info`. Each becomes
//!   a TG0806 diagnostic with that severity; the emitter's own code is the
//!   first thing in its message (`external emitter go: GO001: ...`). An
//!   `error` fails the target and nothing is written (`--strict` also fails
//!   on warnings). `span`, when present, is a file name and JSON Pointer
//!   shown with the diagnostic.
//! - `surface` is optional. The compiler derives the API surface (operations,
//!   types) from the IR itself; an emitter that serves agent tools lists
//!   them as `tools` (tool name to operation id or macro name), so that
//!   `tungsten diff --semver` reports a removed or renamed tool.
//! - Unknown response fields are ignored, so a version 1 emitter keeps
//!   working when the protocol gains optional fields. A response whose
//!   `protocol` is not 1 is TG0802.
//!
//! ## Describing the emitter
//!
//! `tungsten-emit-<name> --describe` prints one JSON document
//! ([`EmitterDescription`]) and exits 0:
//!
//! ```json
//! { "protocol": 1, "name": "go", "version": "0.3.1",
//!   "options": { "type": "object", "properties": { "package": { "type": "string" } } },
//!   "import_policy": true }
//! ```
//!
//! `tungsten emitters` lists the emitters found on `PATH` (and, given a
//! project, those its targets configure) with this information. `options` is
//! an optional JSON Schema of the target options, for tooling. `import_policy`
//! is an optional flag: `true` declares that the emitter's runtime enforces
//! the imported-operation policy carried by operations that come from Postman,
//! cURL, `.http` or HAR input. The compiler asks (`--describe`) before it
//! dispatches such a project to the emitter; an absent or `false` flag means
//! the target is refused for that project, so protocol 1 emitters written
//! before the flag keep working for every other project.
//!
//! ## Limits and failures
//!
//! Each invocation owns a Unix process group or Windows Job Object. On leader
//! exit, timeout or output overflow it requests termination of that group/job,
//! reaps its leader and joins completed pipe workers. The process timeout includes
//! spawning, I/O and cleanup, reserving up to one quarter (at most one second)
//! for cleanup. Zero or unrepresentable timeouts refuse before execution.
//! OS calls and scheduling are not hard real-time guarantees.
//!
//! There are 16 process-wide worker slots, four reserved before each launch.
//! Unfinished workers retain their handles and slots after the deadline; later
//! invocations join them when finished. Exhaustion refuses execution (TG0803).
//! Incomplete termination, leader reaping or pipe work also fails with TG0803;
//! it never accepts a partial response. Embeddings must exclusively own child
//! reaping and must not auto-reap children or change SIGCHLD during a run.
//! Linux refuses before launch when `/proc/self/status` is unreadable or reports
//! ignored/caught SIGCHLD. This cannot detect every signal flag or competing reaper.
//! Unix descendants can leave their group or pass pipe handles elsewhere.
//! A successful termination request and joined workers do not prove every
//! descendant settled. These lifecycle controls are not a sandbox, execution
//! approval, inherited-environment filtering or filesystem/network isolation.
//!
//! The run is killed after `timeout_ms` (default 60 s) or when standard
//! output passes `max_output_bytes` (default 64 MiB), TG0803. A response may
//! not list more than 10000 files (TG0804). The compiler checks every path and writes only inside `out`,
//! also through symlinks (TG0704). The process itself is not sandboxed: it
//! runs with the user's privileges like a build script, so only configure
//! emitters you trust. The command starts on every command that emits in
//! memory, not only on a writing `generate`: `generate --dry-run` and
//! `--check`, `check --ci`, `diff` and `report` run it too (plain `check`
//! and `ir dump` do not). `--no-external` (or `TUNGSTEN_NO_EXTERNAL=1`)
//! skips every `external` target with a TG0807 notice, so a CI job can
//! validate a manifest it does not trust.
//!
//! | Code | Meaning |
//! |---|---|
//! | TG0801 | the emitter was not found or cannot be started |
//! | TG0802 | the emitter speaks another protocol version |
//! | TG0803 | non-zero exit, timeout or output over the cap (stderr tail quoted) |
//! | TG0804 | the response is not valid protocol JSON |
//! | TG0805 | a file path leaves the output directory or is reserved |
//! | TG0806 | a diagnostic reported by the emitter |
//! | TG0807 | the target was skipped: external emitters are off (`--no-external`) |

mod base64;
mod discover;
mod process;
mod protocol;
mod run;

pub use discover::{Discovered, command_line, discover, find_executable, locate};
pub use protocol::{
    CompilerInfo, EmitDiagnostic, EmitFile, EmitRequest, EmitResponse, EmitSpan, EmitSurface,
    EmitterDescription, FileMode, OutputPolicy, json_schema,
};
pub use run::{Emitted, describe, run};

/// Private qualification hooks; not part of the extension protocol.
#[cfg(feature = "testing")]
pub mod __testing {
    /// Reserved slots, including unfinished workers retained after a deadline.
    pub fn reserved_workers() -> usize {
        super::process::reserved_workers()
    }

    /// Reserve the finite worker budget without spawning a process or thread.
    pub fn with_capacity_reserved<R>(f: impl FnOnce() -> R) -> Result<R, String> {
        super::process::with_capacity_reserved(f)
    }

    /// Deterministic private control; the first worker supervises/spawns.
    pub fn with_worker_start_failure<R>(nth: usize, f: impl FnOnce() -> R) -> R {
        super::process::with_worker_start_failure(nth, f)
    }

    /// Pause only private cleanup qualification; RAII releases even on panic.
    pub fn with_cleanup_paused<R>(f: impl FnOnce() -> R) -> R {
        super::process::with_cleanup_paused(f)
    }

    /// Delay the admitted owner before its final execution/deadline check.
    pub fn with_launch_paused<R>(f: impl FnOnce() -> R) -> R {
        super::process::with_launch_paused(f)
    }

    /// Finite preflight delay for verifying refusal before admission.
    pub fn with_preflight_delay<R>(f: impl FnOnce() -> R) -> R {
        super::process::with_preflight_delay(f)
    }

    #[cfg(target_os = "linux")]
    pub fn child_reaping_status(status: &[u8]) -> std::io::Result<()> {
        super::process::child_reaping_status(status)
    }
}

use std::path::PathBuf;
use std::time::Duration;

/// The protocol version this compiler speaks.
pub const PROTOCOL: u32 = 1;

/// The executable name prefix: target `go` runs `tungsten-emit-go`.
pub const EXECUTABLE_PREFIX: &str = "tungsten-emit-";

/// Default `timeout_ms`.
pub use tungsten_core::policy::DEFAULT_TIMEOUT;

/// Default `max_output_bytes`.
pub use tungsten_core::policy::DEFAULT_MAX_OUTPUT_BYTES;

/// Most files one run may return.
pub const MAX_FILES: usize = 10_000;

/// Time allowed for `--describe`.
pub use tungsten_core::policy::DESCRIBE_TIMEOUT;

/// A resolved command: the program and its fixed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The program to start.
    pub program: PathBuf,
    /// Arguments that precede any protocol flag.
    pub args: Vec<String>,
    /// The command as configured, for messages.
    pub display: String,
}

/// Limits of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Wall-clock limit for the whole run.
    pub timeout: Duration,
    /// Cap on standard output.
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}
