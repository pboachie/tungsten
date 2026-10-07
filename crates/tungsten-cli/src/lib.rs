// SPDX-License-Identifier: AGPL-3.0-only
//! The `tungsten` command-line interface (planning/07).
//!
//! [`run`] is the whole CLI as a function of its arguments, two output
//! streams and a [`CliEnv`], so it can be driven in-process with
//! deterministic results. The `tungsten` binary calls it with the process
//! streams.
//!
//! Output contract:
//! - Without `--json`, command output goes to stdout and diagnostics and
//!   errors go to stderr.
//! - With `--json`, stdout carries exactly one JSON document matching
//!   [`output::cli_output_schema`]; nothing else is written to stdout.
//! - Exit codes are listed in [`exit`].

mod args;
mod commands;
mod explain_table;
mod input;
pub mod output;
pub mod render;
mod stats;
pub mod targets;

#[cfg(feature = "testing")]
pub mod __testing;

use std::ffi::OsString;
use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use clap::Parser;
use clap::error::ErrorKind as ClapErrorKind;
use tungsten_core::{Diagnostic, SourceMap};

use crate::output::{CliError, CliOutput, CommandName, CommandResult, ErrorKind};

/// Process exit codes. They are part of the CLI contract and are listed in
/// `tungsten --help`.
pub mod exit {
    /// The command succeeded.
    pub const OK: i32 = 0;
    /// The input has errors (or warnings under `--strict`), or the command
    /// refused to act (for example `init` without `--force`, or an
    /// `explain` target that does not exist).
    pub const FAILED: i32 = 1;
    /// The command line could not be parsed.
    pub const USAGE: i32 = 2;
    /// An I/O failure (for example `--out` not writable, or the mock server
    /// cannot start) or an internal error.
    pub const INTERNAL: i32 = 3;
    /// The command refused to act without confirmation (for example `init`
    /// over existing files, or `generate` into a directory tungsten did not
    /// write, without `--force`); it never prompts.
    pub const REFUSED: i32 = 4;
}

/// How long `tungsten mock` serves after printing its address: until this
/// function, called with the base URL, returns. Lets an in-process caller
/// drive the server; the binary uses the default (until stdin closes).
#[derive(Clone)]
pub struct ServeUntil(pub Arc<dyn Fn(&str) + Send + Sync>);

impl std::fmt::Debug for ServeUntil {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServeUntil(..)")
    }
}

/// The environment the CLI runs in. Passing it explicitly keeps [`run`]
/// deterministic under test.
#[derive(Debug, Clone, Default)]
pub struct CliEnv {
    /// Whether stderr is an interactive terminal.
    pub is_tty: bool,
    /// Whether color is disabled (the `NO_COLOR` convention).
    pub no_color: bool,
    /// Search path for external tools (`doctor`). `None` uses the process
    /// `PATH`.
    pub search_path: Option<OsString>,
    /// When `mock` stops serving. `None`: when stdin reaches end of file
    /// (or the process is interrupted).
    pub serve_until: Option<ServeUntil>,
}

impl CliEnv {
    /// ANSI color is used only on a terminal and only when not disabled.
    pub fn color(&self) -> bool {
        self.is_tty && !self.no_color
    }
}

/// What a command produced. Commands never write to the output streams;
/// [`run`] renders the report in human or JSON form.
#[derive(Debug)]
pub(crate) struct Report {
    pub command: CommandName,
    pub exit: i32,
    pub diagnostics: Vec<Diagnostic>,
    /// Sources the diagnostics' spans point into.
    pub sources: SourceMap,
    pub result: Option<CommandResult>,
    pub error: Option<CliError>,
    /// Human-mode stdout text.
    pub human: String,
    /// A mock server started by `mock`, served after the report is printed.
    pub serving: Option<tungsten_mock::MockServer>,
}

impl Report {
    pub fn new(command: CommandName) -> Self {
        Self {
            command,
            exit: exit::OK,
            diagnostics: vec![],
            sources: SourceMap::new(),
            result: None,
            error: None,
            human: String::new(),
            serving: None,
        }
    }

    /// A report that failed with an invocation-level error.
    pub fn failed(mut self, exit: i32, error: CliError) -> Self {
        self.exit = exit;
        self.error = Some(error);
        self.result = None;
        self
    }
}

/// Run the CLI. `args` includes the program name as its first item, as
/// `std::env::args_os()` does. Returns the process exit code (see [`exit`]).
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
) -> i32 {
    let args: Vec<OsString> = args.into_iter().collect();
    let color = env.color();
    let cli = match args::Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => return usage_error(&err, wants_json(&args), color, stdout, stderr),
    };
    let report = panic::catch_unwind(AssertUnwindSafe(|| commands::dispatch(&cli, env)))
        .unwrap_or_else(|payload| {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Report::new(cli.command_name()).failed(
                exit::INTERNAL,
                CliError::new(ErrorKind::Internal, format!("internal error: {message}"))
                    .with_help("this is a bug in tungsten; please report it with the input"),
            )
        });
    let mut report = report;
    let serving = report.serving.take();
    let code = if cli.json {
        emit_json(report, stdout)
    } else {
        emit_human(report, color && !cli.forces_plain(), stdout, stderr)
    };
    if let Some(server) = serving {
        if code == exit::OK {
            serve(&server.base_url(), env);
        }
        server.shutdown();
    }
    code
}

/// Block until the mock should stop: `env.serve_until` returns, or stdin
/// reaches end of file. Read errors end serving too.
fn serve(base_url: &str, env: &CliEnv) {
    match &env.serve_until {
        Some(until) => (until.0)(base_url),
        None => {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        }
    }
}

/// `--json` anywhere before a `--` separator. Used only when parsing failed,
/// so a usage error still produces one JSON document.
fn wants_json(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| a.as_os_str() != "--")
        .any(|a| a.as_os_str() == "--json")
}

fn usage_error(
    err: &clap::Error,
    json: bool,
    color: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    // Help and version are requested output, not errors: plain text on
    // stdout, or with --json one document carrying the text.
    if matches!(
        err.kind(),
        ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion
    ) {
        let text = err.render().to_string();
        if !json {
            return write_or_internal(stdout, &text);
        }
        let output = CliOutput::new(
            None,
            exit::OK,
            vec![],
            None,
            Some(output::CommandResult::Help(output::HelpResult {
                help: text,
            })),
        );
        return match write_json(stdout, &output) {
            Ok(()) => exit::OK,
            Err(_) => exit::INTERNAL,
        };
    }
    if json {
        let message = err.render().to_string();
        let output = CliOutput::new(
            None,
            exit::USAGE,
            vec![],
            Some(CliError::new(ErrorKind::Usage, message.trim_end())),
            None,
        );
        return match write_json(stdout, &output) {
            Ok(()) => exit::USAGE,
            Err(_) => exit::INTERNAL,
        };
    }
    let rendered = if color {
        err.render().ansi().to_string()
    } else {
        err.render().to_string()
    };
    let _ = stderr.write_all(rendered.as_bytes());
    exit::USAGE
}

fn emit_json(report: Report, stdout: &mut dyn Write) -> i32 {
    let diagnostics = report
        .diagnostics
        .iter()
        .map(|d| output::json_diagnostic(d, &report.sources))
        .collect();
    let output = CliOutput::new(
        Some(report.command),
        report.exit,
        diagnostics,
        report.error,
        report.result,
    );
    match write_json(stdout, &output) {
        Ok(()) => report.exit,
        Err(_) => exit::INTERNAL,
    }
}

fn write_json(stdout: &mut dyn Write, output: &CliOutput) -> std::io::Result<()> {
    let mut text = serde_json::to_string(output).map_err(std::io::Error::other)?;
    text.push('\n');
    stdout.write_all(text.as_bytes())?;
    stdout.flush()
}

fn emit_human(report: Report, color: bool, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    // Diagnostics and errors go to stderr; failures to write them are not
    // actionable and must not change the exit code.
    for d in &report.diagnostics {
        let _ = writeln!(
            stderr,
            "{}",
            render::render_diagnostic(d, &report.sources, color)
        );
    }
    let mut code = report.exit;
    if !report.human.is_empty() && write_or_internal(stdout, &report.human) != exit::OK {
        code = exit::INTERNAL;
    }
    if let Some(err) = &report.error {
        let _ = stderr.write_all(render::render_error(err, color).as_bytes());
    }
    code
}

fn write_or_internal(out: &mut dyn Write, text: &str) -> i32 {
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => exit::OK,
        Err(_) => exit::INTERNAL,
    }
}
