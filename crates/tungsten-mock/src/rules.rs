// SPDX-License-Identifier: AGPL-3.0-only
//! Server rules: what the real server enforces but its API description
//! leaves out, declared in `agent.yml` (`tools[].server_rules`). The mock
//! refuses a request that breaks one with the rule's status and error code,
//! after the description's own checks (parameters and body) passed.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tungsten_ir::{ServerCheck, ServerRule};

use crate::model::OpEntry;
use crate::params::{RequestView, media_essence};

/// The first rule of the operation that the request breaks, with the reason.
pub(crate) fn violated<'e>(
    entry: &'e OpEntry,
    view: &RequestView<'_>,
    body: &[u8],
) -> Option<(&'e ServerRule, String)> {
    let rules = &entry.op.agent.server_rules;
    if rules.is_empty() {
        return None;
    }
    let json = json_body(view, body);
    for rule in rules {
        let Some(value) = member(rule, json.as_ref(), &view.query) else {
            continue;
        };
        let broken = match &rule.check {
            ServerCheck::HostSuffixes { suffixes } => host_reason(&value, suffixes),
            ServerCheck::MaxAheadMinutes { minutes } => ahead_reason(&value, *minutes),
        };
        if let Some(reason) = broken {
            let reason = rule
                .message
                .clone()
                .unwrap_or_else(|| format!("`{}`: {reason}", rule.field));
            return Some((rule, reason));
        }
    }
    None
}

fn json_body(view: &RequestView<'_>, body: &[u8]) -> Option<Value> {
    let content_type = view.header("content-type")?;
    let essence = media_essence(&content_type);
    if body.is_empty() || !(essence == "application/json" || essence.ends_with("+json")) {
        return None;
    }
    serde_json::from_slice(body).ok()
}

/// The value the rule reads: a dotted path into the JSON body, else the
/// query parameter of that name.
fn member(rule: &ServerRule, json: Option<&Value>, params: &[(String, String)]) -> Option<Value> {
    if let Some(body) = json {
        let mut at = body;
        let mut found = true;
        for part in rule.field.split('.') {
            match at.get(part) {
                Some(next) => at = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            return Some(at.clone());
        }
    }
    params
        .iter()
        .find(|(name, _)| *name == rule.field)
        .map(|(_, value)| Value::String(value.clone()))
}

/// The host of a URL: after the scheme, without credentials and port,
/// lower-cased, without a trailing dot.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn host_reason(value: &Value, suffixes: &[String]) -> Option<String> {
    let host = host_of(value.as_str()?)?;
    suffixes.iter().find_map(|suffix| {
        let bare = suffix.trim_start_matches('.');
        let hit = host == bare || host.ends_with(&format!(".{bare}"));
        hit.then(|| format!("the host `{host}` is under the reserved suffix `{suffix}`"))
    })
}

fn ahead_reason(value: &Value, minutes: u64) -> Option<String> {
    let at = value
        .as_u64()
        .or_else(|| value.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
        .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    let limit = now.saturating_add(u128::from(minutes) * 60_000);
    (u128::from(at) > limit).then(|| format!("the time is more than {minutes} minutes ahead"))
}
