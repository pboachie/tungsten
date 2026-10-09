// SPDX-License-Identifier: Apache-2.0
//! Helpers over operation descriptors and arguments: argument paths,
//! sensitive-field handling, reference interpolation and the short texts used
//! in remediations.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::types::{BodyShape, OperationDescriptor, ParamDescriptor, ParamRole, PathSegment};
use crate::util::{
    REDACTED, envelope_value, get_path, json_text, looks_sensitive, redact_paths, split_path,
};

/// Parameters supplied by the caller as arguments (not auth, key or origin).
pub fn arg_params(op: &OperationDescriptor) -> Vec<&ParamDescriptor> {
    op.params
        .iter()
        .filter(|p| matches!(p.role, ParamRole::Plain | ParamRole::DryRun))
        .collect()
}

/// Structural problems that would make the descriptor unusable, or `None`.
pub fn descriptor_problem(op: &OperationDescriptor) -> Option<&'static str> {
    if op.id.is_empty() {
        return Some("`id` must be a non-empty string");
    }
    if op
        .params
        .iter()
        .any(|p| p.name.is_empty() || p.wire.is_empty())
    {
        return Some("every parameter needs a `name` and a `wire`");
    }
    if let Some(rpc) = &op.rpc
        && (rpc.field.is_empty() || rpc.params_field.is_empty())
    {
        return Some("`rpc` needs `field` and `params_field`");
    }
    None
}

fn segment_text(segment: &PathSegment) -> String {
    match segment {
        PathSegment::Key(key) => key.clone(),
        PathSegment::Index(index) => index.to_string(),
    }
}

fn format_path(segments: &[PathSegment]) -> String {
    segments
        .iter()
        .map(|segment| match segment {
            PathSegment::Index(index) => format!("[{index}]"),
            PathSegment::Key(key) if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) => {
                format!("[{key}]")
            }
            PathSegment::Key(key) => format!(".{key}"),
        })
        .collect()
}

/// JSON path of an argument issue: `body.<wire>` for fields of a merged body,
/// `body` for the argument carrying the whole body, else `args.<name>`; the
/// remaining segments are `.key` and `[index]`.
pub fn argument_path(op: &OperationDescriptor, path: &[PathSegment]) -> String {
    if let (Some(body), Some((head, rest))) = (&op.body, path.split_first()) {
        let head = segment_text(head);
        match &body.shape {
            BodyShape::Merged { fields } => {
                if let Some(field) = fields.iter().find(|f| f.arg == head) {
                    return format!("body.{}{}", field.wire, format_path(rest));
                }
            }
            BodyShape::Arg { arg } if *arg == head => {
                return format!("body{}", format_path(rest));
            }
            BodyShape::Arg { .. } => {}
        }
    }
    if path.is_empty() {
        "args".to_owned()
    } else {
        format!("args{}", format_path(path))
    }
}

/// The operation's sensitive argument paths.
pub fn sensitive_request_paths(op: &OperationDescriptor) -> Vec<&str> {
    op.agent
        .sensitive_request_fields
        .iter()
        .map(String::as_str)
        .filter(|p| !p.is_empty())
        .collect()
}

/// An args path without array indices, dotted (`users.0.pin` is `users.pin`).
fn dotted_arg_path(path: &[PathSegment]) -> String {
    path.iter()
        .filter_map(|segment| match segment {
            PathSegment::Key(key) if !key.bytes().all(|b| b.is_ascii_digit()) => Some(key.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(".")
}

pub fn sensitive_arg(op: &OperationDescriptor, path: &[PathSegment]) -> bool {
    if let Some(head) = path.first() {
        let head = segment_text(head);
        if op.params.iter().any(|p| p.sensitive && p.name == head) {
            return true;
        }
    }
    let dotted = dotted_arg_path(path);
    if !dotted.is_empty()
        && sensitive_request_paths(op)
            .iter()
            .any(|f| dotted == *f || dotted.starts_with(&format!("{f}.")))
    {
        return true;
    }
    path.iter().any(|segment| match segment {
        PathSegment::Key(key) => looks_sensitive(key),
        PathSegment::Index(_) => false,
    })
}

/// `value` (the argument at `path`) with the sensitive fields below it redacted.
pub fn redact_below(op: &OperationDescriptor, path: &[PathSegment], value: &Value) -> Value {
    let dotted = dotted_arg_path(path);
    let prefix = if dotted.is_empty() {
        String::new()
    } else {
        format!("{dotted}.")
    };
    let below: Vec<String> = sensitive_request_paths(op)
        .into_iter()
        .filter_map(|f| f.strip_prefix(&prefix).map(str::to_owned))
        .collect();
    if below.is_empty() {
        value.clone()
    } else {
        redact_paths(value, &below)
    }
}

/// Sensitive argument paths relative to the request body as sent (the rpc
/// envelope's params member for rpc operations); `""` is the whole body.
pub fn sensitive_body_paths(op: &OperationDescriptor) -> Vec<String> {
    let paths = sensitive_request_paths(op);
    let relative: Vec<String> = match op.body.as_ref().map(|b| &b.shape) {
        Some(BodyShape::Merged { fields }) => paths
            .iter()
            .filter_map(|p| {
                let segments = split_path(p);
                let head = segments.first()?;
                let field = fields.iter().find(|f| f.arg == *head)?;
                let rest = segments[1..].join(".");
                Some(if rest.is_empty() {
                    field.wire.clone()
                } else {
                    format!("{}.{rest}", field.wire)
                })
            })
            .collect(),
        Some(BodyShape::Arg { arg }) => paths
            .iter()
            .filter_map(|p| {
                if p == arg {
                    Some(String::new())
                } else {
                    p.strip_prefix(&format!("{arg}.")).map(str::to_owned)
                }
            })
            .collect(),
        None if op.rpc.is_some() => paths.iter().map(|p| (*p).to_owned()).collect(),
        None => Vec::new(),
    };
    match &op.rpc {
        Some(rpc) => relative
            .into_iter()
            .map(|p| {
                if p.is_empty() {
                    rpc.params_field.clone()
                } else {
                    format!("{}.{p}", rpc.params_field)
                }
            })
            .collect(),
        None => relative,
    }
}

/// Every string or number found at a path, through arrays.
pub fn values_at(value: &Value, segments: &[String], out: &mut Vec<String>, depth: usize) {
    if depth > 64 {
        return;
    }
    if let Value::Array(items) = value {
        for item in items {
            values_at(item, segments, out, depth + 1);
        }
        return;
    }
    match segments.split_first() {
        None => match value {
            Value::String(text) => out.push(text.clone()),
            Value::Number(n) => out.push(crate::util::number_text(n)),
            Value::Object(map) => {
                for v in map.values() {
                    values_at(v, &[], out, depth + 1);
                }
            }
            _ => {}
        },
        Some((head, rest)) => {
            if let Value::Object(map) = value
                && let Some(next) = map.get(head)
            {
                values_at(next, rest, out, depth + 1);
            }
        }
    }
}

/// `args` plus each parameter's value under its wire name, so references
/// written against the API (`{device_id}`, `$args.endpoint_id`) find
/// arguments whose generated name differs.
pub fn with_wire_names(op: &OperationDescriptor, args: &Map<String, Value>) -> Map<String, Value> {
    let mut out = args.clone();
    for p in &op.params {
        if let Some(value) = args.get(&p.name)
            && !out.contains_key(&p.wire)
        {
            out.insert(p.wire.clone(), value.clone());
        }
    }
    out
}

/// A path whose first segment is a parameter's wire name, rewritten to the
/// parameter's generated name.
pub fn arg_path(op: &OperationDescriptor, path: Vec<String>) -> Vec<String> {
    let Some((head, rest)) = path.split_first() else {
        return path;
    };
    match op
        .params
        .iter()
        .find(|p| p.wire == *head && p.name != *head)
    {
        Some(param) => std::iter::once(param.name.clone())
            .chain(rest.iter().cloned())
            .collect(),
        None => path,
    }
}

/// The argument name for a parameter key given by name or wire name.
pub fn arg_name(op: &OperationDescriptor, key: &str) -> String {
    op.params
        .iter()
        .find(|p| p.name == key)
        .or_else(|| op.params.iter().find(|p| p.wire == key))
        .map_or_else(|| key.to_owned(), |p| p.name.clone())
}

static PLACEHOLDER_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([^{}]+)\}").expect("static regex"));

/// Fill `{field}` references of a template from the arguments; sensitive ones
/// are redacted, unset ones shown as `<unset>`.
pub fn interpolate(template: &str, args: &Map<String, Value>, op: &OperationDescriptor) -> String {
    let root = Value::Object(args.clone());
    PLACEHOLDER_REF
        .replace_all(template, |caps: &regex::Captures<'_>| {
            let segments: Vec<String> = caps[1].trim().split('.').map(str::to_owned).collect();
            let segments = arg_path(op, segments);
            let value = match get_path(Some(&root), &segments) {
                None | Some(Value::Null) => return "<unset>".to_owned(),
                Some(value) => value,
            };
            let path: Vec<PathSegment> = segments.into_iter().map(PathSegment::Key).collect();
            if sensitive_arg(op, &path) {
                return REDACTED.to_owned();
            }
            match envelope_value(value, false) {
                Value::String(text) => text,
                other => json_text(&other),
            }
        })
        .into_owned()
}

/// A preview's display URL with the placeholders of `args` shown as written
/// instead of percent-encoded. Only renderings change; nothing with
/// placeholders is ever sent.
pub fn unescape_placeholders(url: &str, args: &Value) -> String {
    let mut found = Vec::new();
    crate::expr::placeholders_in(args, &mut found, 0);
    let mut out = url.to_owned();
    let mut seen: Vec<&String> = Vec::new();
    for text in &found {
        if seen.contains(&text) {
            continue;
        }
        seen.push(text);
        out = out.replace(&crate::serialize::encode_component(text), text);
    }
    out
}

/// JSON for remediation text, cut to 80 characters.
pub fn short_json(value: &Value) -> String {
    let text = json_text(value);
    if text.chars().count() > 80 {
        format!("{}...", text.chars().take(77).collect::<String>())
    } else {
        text
    }
}

/// ` with name "value", ...` for remediation text naming a call's arguments;
/// "" when there are none.
pub fn with_arguments(args: &Value) -> String {
    let Value::Object(map) = args else {
        return String::new();
    };
    if map.is_empty() {
        return String::new();
    }
    let shown: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{k} {}", short_json(v)))
        .collect();
    format!(" with {}", shown.join(", "))
}

/// ` (name "value", ...)`: the scalar, non-sensitive arguments a call was
/// made with (at most four, by wire name), to recognize what it created.
pub fn sent_arguments(op: &OperationDescriptor, args: &Map<String, Value>) -> String {
    let mut shown = Vec::new();
    for (key, value) in args {
        if shown.len() == 4 {
            break;
        }
        if !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
            continue;
        }
        if sensitive_arg(op, &[PathSegment::Key(key.clone())]) {
            continue;
        }
        let wire = op
            .params
            .iter()
            .find(|p| p.name == *key)
            .map_or(key.as_str(), |p| p.wire.as_str());
        shown.push(format!("{wire} {}", short_json(value)));
    }
    if shown.is_empty() {
        String::new()
    } else {
        format!(" ({})", shown.join(", "))
    }
}

/// The wire spelling of a safety tier.
pub fn safety_name(safety: crate::types::Safety) -> &'static str {
    match safety {
        crate::types::Safety::ReadOnly => "read_only",
        crate::types::Safety::Mutating => "mutating",
        crate::types::Safety::Destructive => "destructive",
        crate::types::Safety::Irreversible => "irreversible",
    }
}

/// A finite number from a JSON value within `[min, max]`, else the fallback.
pub fn bounded(value: Option<&Value>, fallback: f64, min: f64, max: f64) -> f64 {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() => n.clamp(min, max),
        _ => fallback,
    }
}

/// Largest integer a JavaScript number holds exactly.
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
