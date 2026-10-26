// SPDX-License-Identifier: AGPL-3.0-only
//! The `tungsten` command-line interface.
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
mod html;
mod input;
pub mod output;
mod policy;
pub mod render;
mod stats;
pub mod targets;
mod textdiff;

#[cfg(feature = "testing")]
pub mod __testing;

use std::ffi::OsString;
use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use clap::Parser;
use clap::error::ErrorKind as ClapErrorKind;
use tungsten_core::{Diagnostic, Severity, SourceMap};

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
    /// `update check` found a newer release with this platform's archive.
    pub const UPDATE_AVAILABLE: i32 = 5;
    /// `update check` could not confirm a usable release: offline, opted out,
    /// network or metadata failure, or the platform archive is missing.
    pub const UPDATE_UNCONFIRMED: i32 = 6;
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
    /// Explicitly selected immutable context for this invocation. The CLI
    /// intersects its optional selection; projects/editor overrides cannot
    /// replace it. Recapture/revalidate selections for each new invocation.
    pub policy: Option<Arc<tungsten_core::policy::InvocationPolicy>>,
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
    /// Human mode: show a repeated warning or info code only a few times.
    pub collapse: Option<Collapse>,
    /// A mock server started by `mock`, served after the report is printed.
    pub serving: Option<tungsten_mock::MockServer>,
}

/// Which diagnostics the human output may collapse.
#[derive(Debug)]
pub(crate) struct Collapse {
    /// Diagnostics of one code and severity shown in full before the rest
    /// are summarized.
    pub limit: usize,
    /// Parallel to `Report::diagnostics`: false for a diagnostic that must
    /// always be shown (a real error, as opposed to a promoted warning).
    pub eligible: Vec<bool>,
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
            collapse: None,
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

/// `tungsten lsp`: serve the language server protocol on the process's stdin
/// and stdout and return the exit code. `None` when `args` is any other
/// command (or does not parse): [`run`] handles those.
///
/// The server owns the process streams, so the binary calls this before it
/// locks them for [`run`].
pub fn lsp(args: &[OsString]) -> Option<i32> {
    let cli = args::Cli::try_parse_from(args).ok()?;
    if cli.policy.is_some() || cli.policy_digest.is_some() {
        return None;
    }
    match cli.command {
        args::Command::Lsp(lsp) => Some(commands::lsp::serve(&lsp)),
        _ => None,
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
    run_inner(
        args.into_iter().collect(),
        stdout,
        stderr,
        env,
        tungsten_mock::RunCancellation::default(),
        false,
    )
}

/// Embed scenario execution or Linux support-bundle export with cooperative
/// cancellation; never changes signal masks. Export commits at publication.
pub fn run_with_cancellation(
    args: impl IntoIterator<Item = OsString>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
    cancellation: tungsten_mock::RunCancellation,
) -> i32 {
    run_inner(
        args.into_iter().collect(),
        stdout,
        stderr,
        env,
        cancellation,
        false,
    )
}

/// Process entry point. Linux scenario runs own scoped SIGINT/SIGTERM capture;
/// library callers use `run` or `run_with_cancellation` instead.
pub fn run_binary(
    args: impl IntoIterator<Item = OsString>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
) -> i32 {
    run_inner(
        args.into_iter().collect(),
        stdout,
        stderr,
        env,
        tungsten_mock::RunCancellation::default(),
        true,
    )
}

fn run_inner(
    args: Vec<OsString>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
    cancellation: tungsten_mock::RunCancellation,
    _binary_signals: bool,
) -> i32 {
    let color = env.color();
    let cli = match args::Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => return usage_error(&err, wants_json(&args), color, stdout, stderr),
    };
    let selected_env = match policy::select(&cli, env) {
        Ok(selected) => selected,
        Err(report) => {
            return if cli.json {
                emit_json(*report, stdout)
            } else {
                emit_human(*report, color, stdout, stderr)
            };
        }
    };
    let env = &selected_env;
    #[cfg(target_os = "linux")]
    let signals = if _binary_signals
        && matches!(
            &cli.command,
            args::Command::Scenario {
                command: args::ScenarioCommand::Run(_)
            }
        ) {
        match commands::scenario::signals::Signals::capture() {
            Ok(signals) => Some(signals),
            Err(_) => {
                let report = Report::new(CommandName::Scenario).failed(
                    exit::INTERNAL,
                    CliError::new(
                        ErrorKind::Io,
                        "cannot establish scenario cancellation and child ownership",
                    ).with_help("Linux scenario run requires readable /proc/self/status and default SIGCHLD disposition"),
                );
                return if cli.json {
                    emit_json(report, stdout)
                } else {
                    emit_human(report, color, stdout, stderr)
                };
            }
        }
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let cancellation = signals.as_ref().map_or(cancellation, |s| s.cancellation());
    let report = panic::catch_unwind(AssertUnwindSafe(|| {
        commands::dispatch(&cli, env, &cancellation)
    }))
    .unwrap_or_else(|payload| {
        if matches!(&cli.command, args::Command::Scenario { .. }) {
            return Report::new(CommandName::Scenario).failed(
                exit::INTERNAL,
                CliError::new(ErrorKind::Internal, "internal scenario runner failure"),
            );
        }
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        Report::new(cli.command_name().unwrap_or(CommandName::Check)).failed(
            exit::INTERNAL,
            CliError::new(ErrorKind::Internal, format!("internal error: {message}"))
                .with_help("this is a bug in tungsten; please report it with the input"),
        )
    });
    let mut report = report;
    #[cfg(target_os = "linux")]
    if let Some(signals) = signals {
        match signals.restore() {
            Ok(true) => commands::scenario::reject_late_cancellation(&mut report),
            Ok(false) => {}
            Err(_) => {
                report = report.failed(
                    exit::INTERNAL,
                    CliError::new(ErrorKind::Io, "cannot restore scenario signal mask"),
                );
            }
        }
    }
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
    let mut shown: Vec<(&str, Severity, usize)> = vec![];
    for (i, d) in report.diagnostics.iter().enumerate() {
        if let Some(c) = &report.collapse
            && c.eligible.get(i).copied().unwrap_or(false)
        {
            let at = match shown
                .iter()
                .position(|(code, sev, _)| *code == d.code && *sev == d.severity)
            {
                Some(at) => at,
                None => {
                    shown.push((&d.code, d.severity, 0));
                    shown.len() - 1
                }
            };
            shown[at].2 += 1;
            if shown[at].2 > c.limit {
                continue;
            }
        }
        let _ = writeln!(
            stderr,
            "{}",
            render::render_diagnostic(d, &report.sources, color)
        );
    }
    if let Some(c) = &report.collapse {
        for (code, severity, n) in shown.iter().filter(|(_, _, n)| *n > c.limit) {
            let _ = writeln!(
                stderr,
                "{}\n",
                render::collapsed_line(code, *severity, n - c.limit, color)
            );
        }
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
