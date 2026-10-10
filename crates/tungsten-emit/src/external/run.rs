// SPDX-License-Identifier: AGPL-3.0-only
//! One emitter run: request out, response in, every check in between.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_ir::Ir;

use super::process::{self, Failure, Finished};
use super::protocol::{EmitDiagnostic, EmitRequest, EmitResponse, EmitterDescription};
use super::{Command, DESCRIBE_TIMEOUT, Limits, MAX_FILES, PROTOCOL, base64};
use crate::fileset::valid_path;
use crate::{FileSet, FileSetError};

/// Most path problems reported for one response.
const MAX_PATH_PROBLEMS: usize = 10;
/// Cap on the output of `--describe`.
const DESCRIBE_MAX_BYTES: usize = 1024 * 1024;

/// What an external emitter produced.
#[derive(Debug, Default)]
pub struct Emitted {
    /// The files to write. Empty when the run failed.
    pub files: FileSet,
    /// The agent tools the target serves (tool name to operation id or
    /// macro name).
    pub tools: BTreeMap<String, String>,
    /// TG0801 to TG0806. An error means the target failed.
    pub diagnostics: Diagnostics,
}

fn failure(name: &str, command: &Command, failure: Failure) -> Diagnostic {
    match failure {
        Failure::Spawn(e) => Diagnostic::error(
            "TG0801",
            format!(
                "external emitter for target `{name}` could not be started (`{}`): {e}",
                command.display
            ),
        )
        .with_help("check that the file exists and is executable"),
        Failure::Timeout(after) => Diagnostic::error(
            "TG0803",
            format!(
                "external emitter `{name}` (`{}`) did not finish within {}",
                command.display,
                human_duration(after)
            ),
        )
        .with_help("raise `timeout_ms` on the target in tungsten.yml, or fix the emitter"),
        Failure::TooLarge(cap) => Diagnostic::error(
            "TG0803",
            format!(
                "external emitter `{name}` (`{}`) wrote more than {cap} bytes to standard output",
                command.display
            ),
        )
        .with_help("raise `max_output_bytes` on the target in tungsten.yml, or fix the emitter"),
    }
}

fn human_duration(d: Duration) -> String {
    if d.as_millis().is_multiple_of(1000) {
        format!("{} s", d.as_secs())
    } else {
        format!("{} ms", d.as_millis())
    }
}

/// TG0803 for a non-zero exit, quoting the end of standard error.
fn exited(name: &str, command: &Command, done: &Finished) -> Option<Diagnostic> {
    if done.code == Some(0) {
        return None;
    }
    let status = done.code.map_or_else(
        || "was killed by a signal".to_string(),
        |c| format!("exited with status {c}"),
    );
    let mut message = format!("external emitter `{name}` (`{}`) {status}", command.display);
    if done.stderr_tail.is_empty() {
        message.push_str(" and printed nothing on standard error");
    } else {
        message.push_str("; standard error ended with:\n");
        message.push_str(&indent(&done.stderr_tail));
    }
    Some(Diagnostic::error("TG0803", message))
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a protocol document, checking the version before the shape.
fn parse<T: serde::de::DeserializeOwned>(
    name: &str,
    what: &str,
    bytes: &[u8],
) -> Result<T, Diagnostic> {
    let invalid = |why: String| {
        Diagnostic::error(
            "TG0804",
            format!("external emitter `{name}` returned an invalid {what}: {why}"),
        )
        .with_help("see `tungsten schema external-emitter` for the documents of protocol 1")
    };
    let value: Value = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
    match value.get("protocol").and_then(Value::as_u64) {
        None => return Err(invalid("missing integer field `protocol`".into())),
        Some(p) if p != u64::from(PROTOCOL) => {
            return Err(Diagnostic::error(
                "TG0802",
                format!(
                    "external emitter `{name}` speaks protocol {p}; this tungsten speaks protocol {PROTOCOL}"
                ),
            )
            .with_help("use an emitter built for protocol 1, or a tungsten that speaks the emitter's protocol"));
        }
        Some(_) => {}
    }
    serde_json::from_value(value).map_err(|e| invalid(e.to_string()))
}

/// Run `command` for target `name` over `ir` with the target's `options`,
/// in directory `cwd`. Never panics; every problem is a diagnostic and a
/// failed run returns no files.
pub fn run(
    name: &str,
    command: &Command,
    options: Value,
    ir: &Ir,
    cwd: &Path,
    limits: Limits,
) -> Emitted {
    let mut out = Emitted::default();
    let request = EmitRequest::new(name, options, ir, limits.max_output_bytes);
    let input = match serde_json::to_vec(&request) {
        Ok(bytes) => bytes,
        Err(e) => {
            out.diagnostics.push(Diagnostic::error(
                "TG0804",
                format!("the request for external emitter `{name}` could not be serialized: {e}"),
            ));
            return out;
        }
    };
    let done = match process::run(command, &[], cwd, input, limits) {
        Ok(done) => done,
        Err(f) => {
            out.diagnostics.push(failure(name, command, f));
            return out;
        }
    };
    if let Some(d) = exited(name, command, &done) {
        out.diagnostics.push(d);
        return out;
    }
    let response: EmitResponse = match parse(name, "response", &done.stdout) {
        Ok(r) => r,
        Err(d) => {
            out.diagnostics.push(d);
            return out;
        }
    };
    for d in &response.diagnostics {
        out.diagnostics.push(wrap(name, d));
    }
    let mut problems = Diagnostics::new();
    let files = collect_files(name, &response, &mut problems);
    if problems.has_errors() {
        out.diagnostics.extend(problems);
        return out;
    }
    out.files = files;
    out.tools = response.surface.map(|s| s.tools).unwrap_or_default();
    out
}

/// TG0806: an emitter diagnostic under the compiler's code, carrying the
/// emitter's own.
fn wrap(name: &str, d: &EmitDiagnostic) -> Diagnostic {
    let code = d.code.trim();
    let code = if code.is_empty() { "(no code)" } else { code };
    let mut wrapped = Diagnostic::new(
        "TG0806",
        d.severity,
        format!("external emitter {name}: {code}: {}", d.message),
    );
    if let Some(span) = &d.span {
        wrapped = wrapped.at(span.file.clone(), span.pointer.clone(), None);
    }
    if let Some(help) = &d.help {
        wrapped = wrapped.with_help(help.clone());
    }
    wrapped
}

/// Validate and decode the files of `response`.
fn collect_files(name: &str, response: &EmitResponse, problems: &mut Diagnostics) -> FileSet {
    let mut set = FileSet::new();
    if response.files.len() > MAX_FILES {
        problems.push(Diagnostic::error(
            "TG0804",
            format!(
                "external emitter `{name}` returned {} files; the limit is {MAX_FILES}",
                response.files.len()
            ),
        ));
        return set;
    }
    let mut reported = 0usize;
    let mut bad = |problems: &mut Diagnostics, d: Diagnostic| {
        reported += 1;
        if reported <= MAX_PATH_PROBLEMS {
            problems.push(d);
        }
    };
    for file in &response.files {
        if !valid_path(&file.path) || file.path.split('/').next() == Some(".tungsten") {
            bad(
                problems,
                Diagnostic::error(
                    "TG0805",
                    format!(
                        "external emitter `{name}` returned the path {:?}, which leaves the output directory or is reserved; nothing was written",
                        file.path
                    ),
                )
                .with_help("paths must be relative, use `/`, contain no `.` or `..` segment and not start with `.tungsten/`"),
            );
            continue;
        }
        let bytes = match (&file.content, &file.content_base64) {
            (Some(text), None) => text.clone().into_bytes(),
            (None, Some(encoded)) => match base64::decode(encoded) {
                Ok(bytes) => bytes,
                Err(why) => {
                    bad(
                        problems,
                        invalid_file(
                            name,
                            &file.path,
                            &format!("`content_base64` is not valid base64 ({why})"),
                        ),
                    );
                    continue;
                }
            },
            _ => {
                bad(
                    problems,
                    invalid_file(
                        name,
                        &file.path,
                        "exactly one of `content` and `content_base64` is required",
                    ),
                );
                continue;
            }
        };
        match set.add(file.path.clone(), bytes) {
            Ok(()) => {}
            Err(FileSetError::Duplicate(path)) => {
                bad(
                    problems,
                    invalid_file(name, &path, "the path is listed twice"),
                );
            }
            Err(FileSetError::InvalidPath(path)) => {
                bad(problems, invalid_file(name, &path, "invalid path"));
            }
        }
    }
    if reported > MAX_PATH_PROBLEMS {
        problems.push(Diagnostic::error(
            "TG0804",
            format!(
                "external emitter `{name}`: {} more file problems not shown",
                reported - MAX_PATH_PROBLEMS
            ),
        ));
    }
    set
}

fn invalid_file(name: &str, path: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        "TG0804",
        format!("external emitter `{name}` returned an invalid file {path:?}: {why}"),
    )
}

/// Ask `command` to describe itself (`--describe`). The error is the
/// diagnostic that explains why it could not (TG0801 to TG0804).
pub fn describe(
    name: &str,
    command: &Command,
    cwd: &Path,
) -> Result<EmitterDescription, Diagnostic> {
    let limits = Limits {
        timeout: DESCRIBE_TIMEOUT,
        max_output_bytes: DESCRIBE_MAX_BYTES,
    };
    let done = process::run(command, &["--describe"], cwd, vec![], limits)
        .map_err(|f| failure(name, command, f))?;
    if let Some(d) = exited(name, command, &done) {
        return Err(d);
    }
    let description: EmitterDescription = parse(name, "description", &done.stdout)?;
    if description.name.trim().is_empty() || description.version.trim().is_empty() {
        return Err(Diagnostic::error(
            "TG0804",
            format!("external emitter `{name}` returned a description without a name or version"),
        ));
    }
    Ok(description)
}
