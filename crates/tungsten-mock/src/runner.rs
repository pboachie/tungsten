// SPDX-License-Identifier: AGPL-3.0-only
//! Explicit integration execution against an isolated, immutable saved scenario.
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;
use tungsten_ir::Ir;

use crate::ScenarioReport;
#[cfg(target_os = "linux")]
use crate::{MockOptions, MockServer};

#[derive(Debug, Clone, Copy)]
pub struct RunLimits {
    pub calls: u64,
    pub connections: usize,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            calls: 10_000,
            connections: 32,
        }
    }
}

/// Explicit caller cancellation. Library execution never installs signal handlers.
/// The callback must return promptly; it is cooperative caller code.
#[derive(Clone)]
pub struct RunCancellation(pub Arc<dyn Fn() -> bool + Send + Sync>);
impl Default for RunCancellation {
    fn default() -> Self {
        Self(Arc::new(|| false))
    }
}
impl std::fmt::Debug for RunCancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunCancellation(..)")
    }
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The caller's executable and argument vector; never read from a scenario.
    pub command: Vec<OsString>,
    pub expected_exit: u8,
    /// Run phase only: compilation and saved-file validation precede this budget.
    pub timeout: Duration,
    pub limits: RunLimits,
    pub cancellation: RunCancellation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunReason {
    Completed,
    ExitMismatch,
    AssertionsFailed,
    TimedOut,
    Cancelled,
    CallLimit,
    ConnectionLimit,
    ControlMutation,
    IncompleteCalls,
    SpawnFailed,
    ProcessFailed,
    CleanupFailed,
}

/// Only stable status and count fields; no executable, arguments, output or port.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RunReport {
    pub version: u32,
    pub passed: bool,
    pub reason: RunReason,
    pub expected_exit: u8,
    pub actual_exit: Option<i32>,
    /// Signal delivery and leader reaping do not prove every descendant settled.
    pub group_signalled: bool,
    pub leader_reaped: bool,
    /// Admitted API requests cancelled before handler/body generation completed.
    /// Completion does not prove transport delivery or applied provider effects.
    pub unfinished_calls: u64,
    pub scenario: Option<ScenarioReport>,
}

#[cfg(not(target_os = "linux"))]
pub fn run_scenario(_ir: Ir, _scenario: &[u8], _options: RunOptions) -> std::io::Result<RunReport> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "scenario run is currently available on Linux only; scenario validate is portable",
    ))
}

/// Execute with exclusive child ownership. Embeddings must not auto-reap children,
/// concurrently wait for this child, or change SIGCHLD disposition during the run.
/// Process groups are not a sandbox and do not own descendants that leave them.
#[cfg(target_os = "linux")]
pub fn run_scenario(ir: Ir, scenario: &[u8], options: RunOptions) -> std::io::Result<RunReport> {
    linux::run(ir, scenario, options)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use nix::errno::Errno;
    use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid, waitpid};
    use nix::unistd::Pid;
    use process_wrap::std::{ChildWrapper, CommandWrap};
    use std::io;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    struct OwnedChild {
        child: Box<dyn ChildWrapper>,
        owned: bool,
        signalled: bool,
    }

    impl OwnedChild {
        fn observe(&mut self) -> io::Result<Option<Option<i32>>> {
            let pid = i32::try_from(self.child.id())
                .map_err(|_| io::Error::other("invalid child identity"))?;
            // WNOWAIT retains the leader's PID until group cleanup. Reaping first
            // would allow a subsequent group kill to target a reused identity.
            match waitid(
                Id::Pid(Pid::from_raw(pid)),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
            ) {
                Ok(WaitStatus::Exited(_, code)) => Ok(Some(Some(code))),
                Ok(WaitStatus::Signaled(..)) => Ok(Some(None)),
                Ok(_) | Err(Errno::EINTR) => Ok(None),
                Err(Errno::ECHILD) => {
                    self.owned = false;
                    Err(io::Error::other("child ownership lost"))
                }
                Err(e) => Err(e.into()),
            }
        }

        fn cleanup(&mut self, deadline: Instant) -> bool {
            if !self.owned {
                return false;
            }
            let Ok(pid) = i32::try_from(self.child.id()) else {
                return false;
            };
            self.signalled = self.child.start_kill().is_ok();
            loop {
                // Reap only our retained leader. Group wrappers may conceal
                // ECHILD as an empty status; that must disarm all later kills.
                match waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)) {
                    Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => {
                        self.owned = false;
                        return true;
                    }
                    Err(Errno::ECHILD) => {
                        self.owned = false;
                        return false;
                    }
                    Err(Errno::EINTR) => {}
                    Err(_) => return false,
                    Ok(_) => {}
                }
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.owned {
                // Panic/error cleanup stays bounded; an OS cleanup failure never
                // becomes a passing run. No kill follows loss of PID ownership.
                let _ = self.cleanup(Instant::now() + Duration::from_millis(250));
            }
        }
    }

    pub(super) fn run(ir: Ir, scenario: &[u8], options: RunOptions) -> io::Result<RunReport> {
        if options.command.is_empty()
            || options.command[0].is_empty()
            || options.timeout < Duration::from_millis(20)
            || options.timeout > Duration::from_secs(600)
            || !(1..=10_000).contains(&options.limits.calls)
            || !(1..=64).contains(&options.limits.connections)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid command or scenario run limits",
            ));
        }
        let mock = MockOptions {
            max_body_bytes: 1024 * 1024,
            max_recorded_calls: 0,
            max_idempotent_responses: 64,
            ..MockOptions::default()
        };
        MockServer::validate_scenario(ir.clone(), mock.clone(), scenario)?;
        let start = Instant::now();
        let deadline = start + options.timeout;
        let reserve = (options.timeout / 4).min(Duration::from_secs(1));
        let execution_deadline = deadline - reserve;
        let server = MockServer::start_scenario_run(ir, mock, scenario, options.limits)?;
        let mut report = RunReport {
            version: 1,
            passed: false,
            reason: RunReason::Completed,
            expected_exit: options.expected_exit,
            actual_exit: None,
            group_signalled: false,
            leader_reaped: false,
            unfinished_calls: 0,
            scenario: None,
        };
        if (options.cancellation.0)() {
            report.reason = RunReason::Cancelled;
        } else if Instant::now() >= execution_deadline {
            report.reason = RunReason::TimedOut;
        } else {
            let mut command = Command::new(&options.command[0]);
            command
                .args(&options.command[1..])
                .env("TUNGSTEN_MOCK_BASE_URL", server.base_url())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut command = CommandWrap::from(command);
            command.wrap(process_wrap::std::ProcessGroup::leader());
            command.wrap(process_wrap::std::ResetSigmask);
            match command.spawn() {
                Err(_) => report.reason = RunReason::SpawnFailed,
                Ok(child) => {
                    let mut child = OwnedChild {
                        child,
                        owned: true,
                        signalled: false,
                    };
                    loop {
                        report.reason = match server.state.run_limit_exceeded() {
                            n if n & 4 != 0 => RunReason::ControlMutation,
                            n if n & 2 != 0 => RunReason::ConnectionLimit,
                            n if n & 1 != 0 => RunReason::CallLimit,
                            _ if (options.cancellation.0)() => RunReason::Cancelled,
                            _ if Instant::now() >= execution_deadline => RunReason::TimedOut,
                            _ => match child.observe() {
                                Ok(Some(code)) => {
                                    report.actual_exit = code;
                                    break;
                                }
                                Err(_) => RunReason::ProcessFailed,
                                Ok(None) => {
                                    std::thread::sleep(Duration::from_millis(2));
                                    continue;
                                }
                            },
                        };
                        break;
                    }
                    report.leader_reaped = child.cleanup(deadline);
                    report.group_signalled = child.signalled;
                    if !report.leader_reaped || !report.group_signalled {
                        report.reason = RunReason::CleanupFailed;
                    }
                }
            }
        }
        Ok(finish(server, report, deadline, &options.cancellation))
    }

    pub(crate) fn finish(
        server: MockServer,
        mut report: RunReport,
        deadline: Instant,
        cancellation: &RunCancellation,
    ) -> RunReport {
        // Server shutdown cancels connection tasks and joins their runtime first.
        let state = server.state.clone();
        match server.shutdown_and_report() {
            Ok(scenario) => report.scenario = scenario,
            Err(_) => report.reason = RunReason::CleanupFailed,
        }
        if report.reason == RunReason::Completed {
            report.reason = match state.run_limit_exceeded() {
                n if n & 4 != 0 => RunReason::ControlMutation,
                n if n & 2 != 0 => RunReason::ConnectionLimit,
                n if n & 1 != 0 => RunReason::CallLimit,
                _ => RunReason::Completed,
            };
        }
        report.unfinished_calls = state.unfinished_calls();
        if report.unfinished_calls != 0 && report.reason == RunReason::Completed {
            report.reason = RunReason::IncompleteCalls;
        }
        if report.reason == RunReason::Completed
            && (!report.leader_reaped || !report.group_signalled)
        {
            report.reason = RunReason::CleanupFailed;
        }
        if report.reason == RunReason::Completed {
            if report.actual_exit != Some(i32::from(report.expected_exit)) {
                report.reason = RunReason::ExitMismatch;
            } else if !report.scenario.as_ref().is_some_and(|s| s.passed) {
                report.reason = RunReason::AssertionsFailed;
            } else {
                report.passed = true;
            }
        }
        // Cancellation remains live through cleanup and final assertion work.
        if (cancellation.0)() {
            report.passed = false;
            if report.reason == RunReason::Completed {
                report.reason = RunReason::Cancelled;
            }
        }
        if Instant::now() > deadline {
            report.passed = false;
            report.reason = RunReason::TimedOut;
        }
        report
    }
}

#[cfg(all(feature = "testing", target_os = "linux"))]
pub(crate) use linux::finish;
