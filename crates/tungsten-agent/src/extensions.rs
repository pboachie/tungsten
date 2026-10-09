// SPDX-License-Identifier: AGPL-3.0-only
//! `x-agent-*` operation extensions (planning/04 "Vendor extensions").
//!
//! Each extension has the shape of the matching `tools` key:
//! `x-agent-safety`, `x-agent-idempotency`, `x-agent-preview`,
//! `x-agent-confirmation`, `x-agent-remediation`, `x-agent-verify` and
//! `x-agent-cluster`. `x-agent-sensitive` belongs on schema properties,
//! where the type builder reads it. Any other `x-agent-*` key is TG0610; a
//! value of the wrong shape is TG0611. Both are ignored.

use serde::de::DeserializeOwned;
use tungsten_core::Severity;
use tungsten_ir::Operation;

use crate::check;
use crate::expr::Problem;
use crate::model::ToolConfig;
use crate::report::{Reporter, child};

const KNOWN: &[&str] = &[
    "x-agent-safety",
    "x-agent-idempotency",
    "x-agent-preview",
    "x-agent-confirmation",
    "x-agent-remediation",
    "x-agent-verify",
    "x-agent-cluster",
];

/// The operation's extensions as a tools entry (operation = its id).
pub(crate) fn parse(op: &Operation, r: &mut Reporter<'_>) -> ToolConfig {
    let mut entry = ToolConfig {
        operation: op.id.0.clone(),
        safety: None,
        idempotency: None,
        preview: None,
        confirmation: None,
        verify: None,
        remediation: Default::default(),
        remediation_note: None,
        response: None,
        gate: None,
        cluster: None,
        hidden: None,
    };
    for (key, value) in &op.extensions {
        if !key.starts_with("x-agent-") {
            continue;
        }
        let at = child(&op.source.pointer, key);
        let ext = Ext {
            op,
            key,
            at: &at,
            value,
        };
        match key.as_str() {
            "x-agent-safety" => entry.safety = ext.read(r, |_| vec![]),
            "x-agent-idempotency" => entry.idempotency = ext.read(r, check::idempotency_problems),
            "x-agent-preview" => entry.preview = ext.read(r, check::preview_problems),
            "x-agent-confirmation" => entry.confirmation = ext.read(r, check::confirmation_problems),
            "x-agent-remediation" => {
                entry.remediation = ext.read(r, check::remediation_problems).unwrap_or_default()
            }
            "x-agent-verify" => entry.verify = ext.read(r, check::verify_problems),
            "x-agent-cluster" => {
                entry.cluster = ext.read(r, |c: &String| {
                    if check::is_machine_name(c) {
                        vec![]
                    } else {
                        vec![(String::new(), format!("cluster `{c}` must match ^[a-z][a-z0-9_]*$"))]
                    }
                })
            }
            "x-agent-sensitive" => r.spec(
                Severity::Warning,
                "TG0610",
                &op.source.file,
                &at,
                format!(
                    "x-agent-sensitive on operation `{}` is ignored; it belongs on schema properties",
                    op.id.0
                ),
            ),
            _ => r.spec(
                Severity::Warning,
                "TG0610",
                &op.source.file,
                &at,
                format!(
                    "unknown extension {key} on `{}` ignored; known: {}, x-agent-sensitive (on schema properties)",
                    op.id.0,
                    KNOWN.join(", ")
                ),
            ),
        }
    }
    entry
}

/// One extension of one operation.
struct Ext<'e> {
    op: &'e Operation,
    key: &'e str,
    at: &'e str,
    value: &'e serde_json::Value,
}

impl Ext<'_> {
    /// The value as `T` when it has the right shape and passes `problems`;
    /// otherwise TG0611 and `None`.
    fn read<T: DeserializeOwned>(
        &self,
        r: &mut Reporter<'_>,
        problems: impl Fn(&T) -> Vec<Problem>,
    ) -> Option<T> {
        let found = match tungsten_config::deserialize_value::<T>(self.value) {
            Ok(v) => {
                let found = problems(&v);
                if found.is_empty() {
                    return Some(v);
                }
                found
            }
            Err(problem) => vec![problem],
        };
        for (suffix, message) in found {
            r.spec(
                Severity::Warning,
                "TG0611",
                &self.op.source.file,
                &format!("{}{suffix}", self.at),
                format!("{} on `{}` ignored: {message}", self.key, self.op.id.0),
            );
        }
        None
    }
}
