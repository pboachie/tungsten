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
//!   "options": { "type": "object", "properties": { "package": { "type": "string" } } } }
//! ```
//!
//! `tungsten emitters` lists the emitters found on `PATH` (and, given a
//! project, those its targets configure) with this information. `options` is
//! an optional JSON Schema of the target options, for tooling.
//!
//! ## Limits and failures
//!
//! The run is killed after `timeout_ms` (default 60 s) or when standard
//! output passes `max_output_bytes` (default 64 MiB), TG0803. A response may
//! not list more than 10000 files (TG0804). The compiler checks every path and writes only inside `out`,
//! also through symlinks (TG0704). The process itself is not sandboxed: it
//! runs with the user's privileges like a build script, so only configure
//! emitters you trust.
//!
//! | Code | Meaning |
//! |---|---|
//! | TG0801 | the emitter was not found or cannot be started |
//! | TG0802 | the emitter speaks another protocol version |
//! | TG0803 | non-zero exit, timeout or output over the cap (stderr tail quoted) |
//! | TG0804 | the response is not valid protocol JSON |
//! | TG0805 | a file path leaves the output directory or is reserved |
//! | TG0806 | a diagnostic reported by the emitter |

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

use std::path::PathBuf;
use std::time::Duration;

/// The protocol version this compiler speaks.
pub const PROTOCOL: u32 = 1;

/// The executable name prefix: target `go` runs `tungsten-emit-go`.
pub const EXECUTABLE_PREFIX: &str = "tungsten-emit-";

/// Default `timeout_ms`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Default `max_output_bytes`.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Most files one run may return.
pub const MAX_FILES: usize = 10_000;

/// Time allowed for `--describe`.
pub const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(5);

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
