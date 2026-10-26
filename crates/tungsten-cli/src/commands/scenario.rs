// SPDX-License-Identifier: AGPL-3.0-only
//! Explicit saved-scenario validation and integration orchestration.
#[cfg(any(target_vendor = "apple", windows))]
pub(crate) mod interrupt;
#[cfg(target_os = "linux")]
pub(crate) mod signals;

/// The binary's scoped cancellation capture for `scenario run`.
#[cfg(any(target_vendor = "apple", windows))]
pub(crate) use interrupt::Signals as BinarySignals;
#[cfg(target_os = "linux")]
pub(crate) use signals::Signals as BinarySignals;

/// Where `scenario run` can own an integration process.
pub(crate) const RUN_SUPPORTED: bool =
    cfg!(any(target_os = "linux", target_vendor = "apple", windows));
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use crate::args::ScenarioCommand;
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, ScenarioAction, ScenarioResult,
};
use crate::{Report, exit, input as compile_input};
use tungsten_mock::{MockOptions, MockServer, RunCancellation, RunLimits, RunOptions};

pub(crate) fn input(command: &ScenarioCommand) -> &Path {
    match command {
        ScenarioCommand::Validate(a) => &a.input.path,
        ScenarioCommand::Run(a) => &a.input.path,
    }
}

pub(crate) fn run(command: &ScenarioCommand, cancellation: &RunCancellation) -> Report {
    let mut report = Report::new(CommandName::Scenario);
    if matches!(command, ScenarioCommand::Run(_)) && !RUN_SUPPORTED {
        return report.failed(exit::USAGE, CliError::new(ErrorKind::Usage,
            "scenario run is available on Linux, macOS and Windows only; use scenario validate on this platform"));
    }
    let mut compiled = compile_input::compile(input(command));
    report.diagnostics = compiled.diagnostics.0.clone();
    report.sources = std::mem::take(&mut compiled.workspace.sources);
    let Some(ir) = compiled.ir.filter(|_| !compiled.diagnostics.has_errors()) else {
        report.exit = exit::FAILED;
        return report;
    };
    let path = match command {
        ScenarioCommand::Validate(a) => &a.scenario,
        ScenarioCommand::Run(a) => &a.scenario,
    };
    let bytes = (|| -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(tungsten_mock::MAX_SCENARIO_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })();
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
            match tungsten_mock::run_scenario(
                ir,
                &bytes,
                RunOptions {
                    command: args.command.clone(),
                    expected_exit: args.expect_exit,
                    timeout: Duration::from_millis(args.timeout_ms),
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

/// Binary cancellation observed while ending the capture still fails the run.
#[cfg(any(target_os = "linux", target_vendor = "apple", windows))]
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
