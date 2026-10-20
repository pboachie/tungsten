// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded, non-executable saved scenarios over the existing program protocol.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RecordedCall;
use crate::handler::{Injection, parse_program};
use crate::model::Model;
use crate::state::{Action, Program};

/// Largest saved scenario accepted, including its programmed response bodies.
pub const MAX_SCENARIO_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_CALLS: u64 = 10_000;
const MAX_DELAY_MS: u64 = 30_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    programs: Vec<Value>,
    expect: Expectations,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Expectations {
    total_calls: u64,
    calls: Vec<CallExpectation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallExpectation {
    operation: String,
    #[serde(default)]
    status: Option<u16>,
    count: u64,
}

pub(crate) struct Prepared {
    pub programs: Vec<(String, Program)>,
    pub expect: Expectations,
}

pub(crate) fn prepare(model: &Model, bytes: &[u8]) -> Result<Prepared, String> {
    if bytes.len() > MAX_SCENARIO_BYTES {
        return Err("scenario exceeds the 1 MiB file limit".into());
    }
    // Do not echo serde errors: unknown member names and values may be secrets.
    let doc: Document = serde_json::from_slice(bytes).map_err(|e| {
        format!(
            "invalid scenario JSON or structure at line {}, column {}; expected version, programs and expect",
            e.line(), e.column()
        )
    })?;
    if doc.version != 1 {
        return Err("unsupported scenario version; expected 1".into());
    }
    if doc.programs.len() > MAX_ENTRIES || doc.expect.calls.len() > MAX_ENTRIES {
        return Err("scenario allows at most 256 programs and 256 call assertions".into());
    }
    if doc.expect.total_calls > MAX_CALLS {
        return Err("scenario expected total_calls must be at most 10000".into());
    }
    let mut programs = Vec::with_capacity(doc.programs.len());
    let mut uses = 0_u64;
    for (index, item) in doc.programs.iter().enumerate() {
        // The shared parser checks operation identities, unknown controls, header
        // framing and injection combinations. Keep diagnostics free of values.
        let program = parse_program(model, item).map_err(|_| {
            format!("scenario program {index} is invalid; check its operation, response and injection controls")
        })?;
        uses = uses.saturating_add(program.1.remaining);
        if uses > MAX_CALLS {
            return Err("scenario allows at most 10000 programmed calls".into());
        }
        let delay = match &program.1.action {
            Action::Inject(Injection::Timeout(ms)) => *ms,
            Action::Inject(Injection::StallAfterEvents { hold_ms, .. }) => *hold_ms,
            _ => 0,
        };
        if delay > MAX_DELAY_MS {
            return Err(format!(
                "scenario program {index} exceeds the 30000 ms hold limit"
            ));
        }
        programs.push(program);
    }
    for (index, call) in doc.expect.calls.iter().enumerate() {
        if model.op_index(&call.operation).is_none()
            || call.count > doc.expect.total_calls
            || call
                .status
                .is_some_and(|s| s != 0 && !(100..=599).contains(&s))
        {
            return Err(format!(
                "scenario call assertion {index} has an invalid operation, status or count"
            ));
        }
    }
    Ok(Prepared {
        programs,
        expect: doc.expect,
    })
}

/// One count comparison. Assertion indexes refer to `expect.calls` in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CountReport {
    pub expected: u64,
    pub actual: u64,
    pub passed: bool,
}

impl CountReport {
    fn new(expected: u64, actual: u64) -> Self {
        Self {
            expected,
            actual,
            passed: expected == actual,
        }
    }
}

/// A snapshot of scenario assertions, excluding request/response data and IDs.
/// Read after the integration is quiescent: this does not stop in-flight calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ScenarioReport {
    pub version: u32,
    pub passed: bool,
    pub total_calls: CountReport,
    /// Same order as `expect.calls`; includes zero-count assertions.
    pub calls: Vec<CountReport>,
    /// All queued responses must be consumed for the scenario to pass.
    pub programs_remaining: u64,
}

#[derive(Debug)]
pub(crate) struct Progress {
    expect: Expectations,
    total: u64,
    counts: Vec<u64>,
}

impl Progress {
    pub fn new(expect: Expectations) -> Self {
        let counts = vec![0; expect.calls.len()];
        Self {
            expect,
            total: 0,
            counts,
        }
    }

    pub fn record(&mut self, call: &RecordedCall) {
        self.total = self.total.saturating_add(1);
        for (expected, actual) in self.expect.calls.iter().zip(&mut self.counts) {
            if call.operation.as_deref() == Some(expected.operation.as_str())
                && expected
                    .status
                    .is_none_or(|status| status == call.response_status)
            {
                *actual = actual.saturating_add(1);
            }
        }
    }

    pub fn report(&self, programs_remaining: u64) -> ScenarioReport {
        let total_calls = CountReport::new(self.expect.total_calls, self.total);
        let calls: Vec<_> = self
            .expect
            .calls
            .iter()
            .zip(&self.counts)
            .map(|(expected, actual)| CountReport::new(expected.count, *actual))
            .collect();
        let passed =
            total_calls.passed && calls.iter().all(|c| c.passed) && programs_remaining == 0;
        ScenarioReport {
            version: 1,
            passed,
            total_calls,
            calls,
            programs_remaining,
        }
    }
}
