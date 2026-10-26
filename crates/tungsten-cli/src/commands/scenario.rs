// SPDX-License-Identifier: AGPL-3.0-only
//! Explicit saved-scenario validation and Linux integration orchestration.
#[cfg(target_os = "linux")]
pub(crate) mod signals;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::args::ScenarioCommand;
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, ScenarioAction, ScenarioResult,
};
use crate::{Report, exit, input as compile_input};
use tungsten_build::Compiled;
use tungsten_mock::{
    MockOptions, MockServer, RunCancellation, RunLimits, RunOptions, RunReason, RunReport,
};

const POLL: Duration = Duration::from_millis(10);
/// The runner refuses shorter run-phase budgets.
const MIN_RUN: Duration = Duration::from_millis(20);

pub(crate) fn input(command: &ScenarioCommand) -> &Path {
    match command {
        ScenarioCommand::Validate(a) => &a.input.path,
        ScenarioCommand::Run(a) => &a.input.path,
    }
}

struct Prepared {
    compiled: Compiled,
    bytes: std::io::Result<Vec<u8>>,
}

fn prepare(input: &Path, scenario: &Path) -> Prepared {
    let compiled = compile_input::compile(input);
    let bytes = (|| -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(scenario)?
            .take(tungsten_mock::MAX_SCENARIO_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })();
    Prepared { compiled, bytes }
}

/// Acquisition, compilation and file reading run on a worker so the whole
/// command deadline and cancellation also cover them. A worker abandoned at the
/// deadline is detached; it stays within acquisition's own limits and never
/// binds a port or starts the integration. `None` reports a worker failure.
fn prepare_bounded(
    input: &Path,
    scenario: &Path,
    deadline: Instant,
    cancellation: &RunCancellation,
) -> Result<Prepared, Option<RunReason>> {
    let (input, scenario) = (input.to_path_buf(), scenario.to_path_buf());
    let options = tungsten_openapi::acquire::current_options();
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("tungsten-scenario-prepare".into())
        .spawn(move || {
            let prepared =
                tungsten_openapi::acquire::with_options(options, || prepare(&input, &scenario));
            let _ = tx.send(prepared);
        })
        .map_err(|_| None)?;
    loop {
        if (cancellation.0)() {
            return Err(Some(RunReason::Cancelled));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(Some(RunReason::TimedOut));
        }
        match rx.recv_timeout((deadline - now).min(POLL)) {
            Ok(prepared) => return Ok(prepared),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(None),
        }
    }
}

fn interrupted(mut report: Report, reason: RunReason, expected_exit: u8) -> Report {
    report.human = format!("scenario run: {reason:?}; failed before the integration started\n");
    report.exit = exit::FAILED;
    report.error = Some(
        CliError::new(
            ErrorKind::Usage,
            "scenario run did not finish acquisition and compilation within its deadline",
        )
        .with_help("--timeout-ms bounds the whole command, including input acquisition"),
    );
    if reason == RunReason::Cancelled {
        report.error = Some(CliError::new(
            ErrorKind::Usage,
            "scenario run was cancelled before the integration started",
        ));
    }
    report.result = Some(CommandResult::Scenario(ScenarioResult {
        action: ScenarioAction::Run,
        validated: false,
        run: Some(RunReport::interrupted(reason, expected_exit)),
    }));
    report
}

pub(crate) fn run(command: &ScenarioCommand, cancellation: &RunCancellation) -> Report {
    // The whole-command budget starts before acquisition and compilation.
    let started = Instant::now();
    let mut report = Report::new(CommandName::Scenario);
    if matches!(command, ScenarioCommand::Run(_)) && !cfg!(target_os = "linux") {
        return report.failed(exit::USAGE, CliError::new(ErrorKind::Usage,
            "scenario run is currently available on Linux only; use scenario validate on this platform"));
    }
    let (prepared, deadline) = match command {
        ScenarioCommand::Validate(a) => (prepare(&a.input.path, &a.scenario), None),
        ScenarioCommand::Run(a) => {
            let deadline = started + Duration::from_millis(a.timeout_ms);
            match prepare_bounded(&a.input.path, &a.scenario, deadline, cancellation) {
                Ok(prepared) => (prepared, Some(deadline)),
                Err(Some(reason)) => return interrupted(report, reason, a.expect_exit),
                Err(None) => {
                    return report.failed(
                        exit::INTERNAL,
                        CliError::new(ErrorKind::Internal, "internal scenario runner failure"),
                    );
                }
            }
        }
    };
    let Prepared {
        mut compiled,
        bytes,
    } = prepared;
    report.diagnostics = compiled.diagnostics.0.clone();
    report.sources = std::mem::take(&mut compiled.workspace.sources);
    let Some(ir) = compiled.ir.filter(|_| !compiled.diagnostics.has_errors()) else {
        report.exit = exit::FAILED;
        return report;
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(_) => {
            return report.failed(
                exit::USAGE,
                CliError::new(ErrorKind::Io, "cannot read saved scenario file"),
            );
        }
    };
    match command {
        ScenarioCommand::Validate(_) => {
            match MockServer::validate_scenario(ir, MockOptions::default(), &bytes) {
                Err(e) => {
                    report.failed(exit::USAGE, CliError::new(ErrorKind::Usage, e.to_string()))
                }
                Ok(()) => {
                    report.human =
                        "saved scenario is valid; no server or integration was started\n".into();
                    report.result = Some(CommandResult::Scenario(ScenarioResult {
                        action: ScenarioAction::Validate,
                        validated: true,
                        run: None,
                    }));
                    report
                }
            }
        }
        ScenarioCommand::Run(args) => {
            // The runner's own phase receives only what preparation left.
            let remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_default();
            if remaining < MIN_RUN {
                return interrupted(report, RunReason::TimedOut, args.expect_exit);
            }
            if (cancellation.0)() {
                return interrupted(report, RunReason::Cancelled, args.expect_exit);
            }
            match tungsten_mock::run_scenario(
                ir,
                &bytes,
                RunOptions {
                    command: args.command.clone(),
                    expected_exit: args.expect_exit,
                    timeout: remaining,
                    limits: RunLimits {
                        calls: args.max_calls,
                        connections: 32,
                    },
                    cancellation: cancellation.clone(),
                },
            ) {
                Err(_) => report.failed(
                    exit::USAGE,
                    CliError::new(
                        ErrorKind::Usage,
                        "cannot prepare saved scenario run; check its format and run limits",
                    ),
                ),
                Ok(run) => {
                    report.human = format!(
                        "scenario run: {:?}; {}\n",
                        run.reason,
                        if run.passed { "passed" } else { "failed" }
                    );
                    if let Some(scenario) = &run.scenario {
                        report.human.push_str(&failed_assertions(scenario));
                    }
                    if !run.passed {
                        report.exit = exit::FAILED;
                        report.error = Some(CliError::new(ErrorKind::Usage, "scenario run did not satisfy its deadline, cleanup, exit, call and effect assertions").with_help("inspect result.run.reason and the count-only assertion report"));
                    }
                    report.result = Some(CommandResult::Scenario(ScenarioResult {
                        action: ScenarioAction::Run,
                        validated: true,
                        run: Some(run),
                    }));
                    report
                }
            }
        }
    }
}

/// One line per failing count, by file position; counts only, never values.
fn failed_assertions(scenario: &tungsten_mock::ScenarioReport) -> String {
    let mut lines = String::new();
    let mut line = |what: String, count: &tungsten_mock::CountReport| {
        if !count.passed {
            lines.push_str(&format!(
                "  {what}: expected {}, observed {}\n",
                count.expected, count.actual
            ));
        }
    };
    line("expect.total_calls".into(), &scenario.total_calls);
    for (index, count) in scenario.calls.iter().enumerate() {
        line(format!("expect.calls[{index}]"), count);
    }
    for (index, effect) in scenario.effects.iter().enumerate() {
        if let Some(count) = &effect.applied {
            line(format!("expect.effects[{index}].applied"), count);
        }
        if let Some(count) = &effect.replayed {
            line(format!("expect.effects[{index}].replayed"), count);
        }
    }
    if scenario.programs_remaining != 0 {
        lines.push_str(&format!(
            "  programs: {} queued responses were not consumed\n",
            scenario.programs_remaining
        ));
    }
    lines
}

/// Pending binary signals drained during mask restoration still fail the run.
#[cfg(target_os = "linux")]
pub(crate) fn reject_late_cancellation(report: &mut Report) {
    if report.exit != exit::OK {
        return;
    }
    if let Some(CommandResult::Scenario(result)) = &mut report.result
        && let Some(run) = &mut result.run
    {
        run.passed = false;
        run.reason = tungsten_mock::RunReason::Cancelled;
        report.exit = exit::FAILED;
        report.error = Some(CliError::new(
            ErrorKind::Usage,
            "scenario run was cancelled during cleanup",
        ));
        report.human = "scenario run: Cancelled; failed\n".into();
    }
}
