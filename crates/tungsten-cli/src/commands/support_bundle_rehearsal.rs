// SPDX-License-Identifier: AGPL-3.0-only
//! The `upgrade.rehearsal` support-bundle entry (schema version 3).
//!
//! Opt-in with `support-bundle --rehearsal DIR`: an allowlisted summary of a
//! rehearsal workspace written by `tungsten upgrade rehearse`, so a failed
//! rehearsal can be reported without sharing generated source. Only states,
//! counts, versions, built-in target ids and finding kinds are copied; paths,
//! digests, file names, finding details, patches and external target names
//! are never read into the bundle.

use std::io::Read as _;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::upgrade::REHEARSAL_FILE;
use super::upgrade_apply::JOURNAL_FILE;

/// The entry id, listed after the six observed entries of schema 1.
pub(super) const ENTRY_ID: &str = "upgrade.rehearsal";
/// Most targets and findings listed; the rest are counted.
const MAX_LISTED: usize = 64;
/// Largest rehearsal record read.
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RehearsalSummary {
    pub state: RehearsalSummaryState,
    /// The compiler recorded in the project's lock.
    pub from_version: Option<String>,
    /// The candidate compiler that rehearsed.
    pub to_version: Option<String>,
    pub verdict: Option<String>,
    pub generated_source: Option<String>,
    pub package_availability: Option<String>,
    pub runtime_acceptance: Option<String>,
    /// `not_applied`, `started` (an interrupted apply), `completed`,
    /// `restored` or `unknown`.
    pub apply_state: String,
    pub targets: Vec<RehearsalTargetSummary>,
    pub targets_omitted: usize,
    /// Identity change kinds, in report order.
    pub identity_changes: Vec<String>,
    pub findings: Vec<RehearsalFindingSummary>,
    pub findings_omitted: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalSummaryState {
    Recorded,
    /// The directory holds no rehearsal record (an interrupted rehearsal).
    Missing,
    /// The record is too large, not JSON or not a rehearsal record.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RehearsalTargetSummary {
    /// A built-in target id, or `external`.
    pub target: String,
    pub status: String,
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub conflicts: usize,
    pub semver: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RehearsalFindingSummary {
    pub kind: String,
    pub blocking: bool,
}

/// Summarize the rehearsal workspace `dir`. Reads only the record and the
/// apply journal.
pub(super) fn summarize(dir: &Path) -> RehearsalSummary {
    let mut summary = RehearsalSummary {
        state: RehearsalSummaryState::Missing,
        from_version: None,
        to_version: None,
        verdict: None,
        generated_source: None,
        package_availability: None,
        runtime_acceptance: None,
        apply_state: apply_state(dir),
        targets: vec![],
        targets_omitted: 0,
        identity_changes: vec![],
        findings: vec![],
        findings_omitted: 0,
    };
    let record = match read_bounded(&dir.join(REHEARSAL_FILE)) {
        Read::Missing => return summary,
        Read::Unreadable => {
            summary.state = RehearsalSummaryState::Unreadable;
            return summary;
        }
        Read::Value(value) => value,
    };
    let report = &record["report"];
    if record["tungsten_rehearsal"].as_u64().is_none() || !report.is_object() {
        summary.state = RehearsalSummaryState::Unreadable;
        return summary;
    }
    summary.state = RehearsalSummaryState::Recorded;
    summary.from_version = version(&report["from"]["version"]);
    summary.to_version = version(&report["to"]["version"]);
    summary.verdict = token(&report["verdict"]);
    summary.generated_source = token(&report["checks"]["generated_source"]);
    summary.package_availability = token(&report["checks"]["package_availability"]);
    summary.runtime_acceptance = token(&report["checks"]["runtime_acceptance"]);
    let targets = report["targets"].as_array().map_or(&[][..], Vec::as_slice);
    summary.targets_omitted = targets.len().saturating_sub(MAX_LISTED);
    summary.targets = targets
        .iter()
        .take(MAX_LISTED)
        .map(|t| {
            let count = |key: &str| t["diff"][key].as_u64().unwrap_or(0) as usize;
            RehearsalTargetSummary {
                target: t["target"]
                    .as_str()
                    .and_then(tungsten_config::capabilities::target)
                    .map_or_else(|| "external".to_string(), |c| c.id.to_string()),
                status: token(&t["status"]).unwrap_or_else(|| "unknown".into()),
                added: count("added"),
                changed: count("changed"),
                removed: count("removed"),
                conflicts: t["conflicts"].as_array().map_or(0, Vec::len),
                semver: token(&t["diff"]["semver"]["level"]),
            }
        })
        .collect();
    summary.identity_changes = report["identity_changes"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_LISTED)
        .filter_map(|c| token(&c["kind"]))
        .collect();
    let findings = report["findings"].as_array().map_or(&[][..], Vec::as_slice);
    summary.findings_omitted = findings.len().saturating_sub(MAX_LISTED);
    summary.findings = findings
        .iter()
        .take(MAX_LISTED)
        .map(|f| RehearsalFindingSummary {
            kind: token(&f["kind"]).unwrap_or_else(|| "unknown".into()),
            blocking: f["blocking"].as_bool().unwrap_or(true),
        })
        .collect();
    summary
}

fn apply_state(dir: &Path) -> String {
    match read_bounded(&dir.join(JOURNAL_FILE)) {
        Read::Missing => "not_applied".into(),
        Read::Unreadable => "unknown".into(),
        Read::Value(journal) => token(&journal["state"])
            .filter(|s| matches!(s.as_str(), "started" | "completed" | "restored"))
            .unwrap_or_else(|| "unknown".into()),
    }
}

enum Read {
    Missing,
    Unreadable,
    Value(Value),
}

fn read_bounded(path: &Path) -> Read {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Read::Missing,
        Err(_) => return Read::Unreadable,
    };
    if !meta.is_file() || meta.len() > MAX_RECORD_BYTES {
        return Read::Unreadable;
    }
    let mut bytes = vec![];
    if std::fs::File::open(path)
        .and_then(|f| f.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes))
        .is_err()
    {
        return Read::Unreadable;
    }
    serde_json::from_slice(&bytes).map_or(Read::Unreadable, Read::Value)
}

/// A snake_case token, as the inspector's `Token` shape accepts.
fn token(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        .map(str::to_string)
}

/// A release version: at most 64 bytes of `[0-9A-Za-z.+-]`.
fn version(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
        })
        .map(str::to_string)
}
