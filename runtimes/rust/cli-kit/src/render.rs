// SPDX-License-Identifier: Apache-2.0
//! Rendering: JSON documents, the human forms of values, errors and
//! previews, and the optional ANSI color.

use serde_json::{Map, Value, json};
use tungsten_runtime::{
    Diagnostic, MacroStepKind, MacroStepPreview, PreviewResult, RenderedRequest, Retryable, Safety,
};

/// Whether output is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Style {
    pub on: bool,
}

impl Style {
    fn wrap(self, code: &str, s: &str) -> String {
        if self.on {
            format!("\u{1b}[{code}m{s}\u{1b}[0m")
        } else {
            s.to_string()
        }
    }

    pub(crate) fn bold(self, s: &str) -> String {
        self.wrap("1", s)
    }

    pub(crate) fn red(self, s: &str) -> String {
        self.wrap("1;31", s)
    }

    pub(crate) fn yellow(self, s: &str) -> String {
        self.wrap("33", s)
    }

    fn cyan(self, s: &str) -> String {
        self.wrap("36", s)
    }

    fn green(self, s: &str) -> String {
        self.wrap("32", s)
    }
}

pub(crate) fn safety_name(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "read_only",
        Safety::Mutating => "mutating",
        Safety::Destructive => "destructive",
        Safety::Irreversible => "irreversible",
    }
}

/// One JSON document: compact, or indented when `pretty`.
pub(crate) fn json_doc(v: &Value, pretty: bool) -> String {
    let text = if pretty {
        serde_json::to_string_pretty(v)
    } else {
        serde_json::to_string(v)
    };
    text.unwrap_or_else(|_| "null".into())
}

/// The human form of a success body: a string as is, anything else as
/// indented JSON.
pub(crate) fn human_value(v: &Value, style: Style) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => {
            let mut out = String::new();
            write_value(&mut out, other, 0, style);
            out
        }
    }
}

fn write_value(out: &mut String, v: &Value, depth: usize, style: Style) {
    let pad = |n: usize| "  ".repeat(n);
    match v {
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            for (i, (k, val)) in map.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                out.push_str(&style.cyan(&quote(k)));
                out.push_str(": ");
                write_value(out, val, depth + 1, style);
                out.push_str(if i + 1 < map.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(depth));
            out.push('}');
        }
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, val) in items.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                write_value(out, val, depth + 1, style);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(depth));
            out.push(']');
        }
        Value::String(s) => out.push_str(&style.green(&quote(s))),
        other => out.push_str(&other.to_string()),
    }
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn category_name(d: &Diagnostic) -> String {
    serde_json::to_value(d.category)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "ERROR".into())
}

/// The human form of a failed call, for standard error.
pub(crate) fn error_human(d: &Diagnostic, style: Style) -> String {
    let mut head = format!("{} {}", style.red("error"), style.bold(&category_name(d)));
    head.push_str(&format!(" · {}", d.operation));
    if let Some(status) = d.http_status {
        head.push_str(&format!(" · HTTP {status}"));
    }
    if let Some(code) = &d.code {
        head.push_str(&format!(" · {code}"));
    }
    let mut lines = vec![head, format!("  remediation: {}", d.remediation)];
    if let Some(p) = &d.failed_parameter {
        let mut line = format!("  parameter: {p}");
        if let Some(e) = &d.expected {
            line.push_str(&format!(" (expected {e})"));
        }
        if !d.received_value.is_null() {
            line.push_str(&format!(" (received {})", d.received_value));
        }
        lines.push(line);
    }
    let retry = match d.retryable {
        Retryable::Never => "never".to_string(),
        Retryable::AfterDelay => match d.retry_after_ms {
            Some(ms) => format!("after a delay ({ms} ms)"),
            None => "after a delay".to_string(),
        },
        Retryable::SameKeyOnly => "only with the same idempotency key".to_string(),
        Retryable::AfterRemediation => "after the cause is fixed".to_string(),
    };
    lines.push(format!("  retryable: {retry}"));
    if let Some(n) = &d.next_action {
        lines.push(format!("  next: {n}"));
    }
    if let Some(id) = &d.request_id {
        lines.push(format!("  request id: {id}"));
    }
    lines.join("\n")
}

fn request_json(r: &RenderedRequest) -> Value {
    json!({
        "method": r.method.as_str(),
        "url": r.url,
        "headers": r.headers,
        "body": r.body,
    })
}

fn kind_name(k: MacroStepKind) -> &'static str {
    match k {
        MacroStepKind::Call => "call",
        MacroStepKind::Poll => "poll",
        MacroStepKind::Paginate => "paginate",
    }
}

fn step_json(s: &MacroStepPreview) -> Value {
    json!({
        "step": s.step,
        "kind": kind_name(s.kind),
        "operation": s.operation,
        "as": s.r#as,
        "safety": safety_name(s.safety),
        "request": s.request.as_ref().map(request_json),
        "effects": s.effects,
    })
}

/// The JSON form of a preview.
pub(crate) fn preview_json(p: &PreviewResult) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("operation".into(), json!(p.operation));
    m.insert("safety".into(), json!(safety_name(p.safety)));
    m.insert("request".into(), request_json(&p.request));
    m.insert("effects".into(), json!(p.effects));
    m.insert("confirmation_token".into(), json!(p.confirmation_token));
    m.insert("expires_in_ms".into(), json!(p.expires_in_ms));
    if let Some(s) = &p.server_preview {
        m.insert("server_preview".into(), s.clone());
    }
    if let Some(steps) = &p.steps {
        m.insert(
            "steps".into(),
            Value::Array(steps.iter().map(step_json).collect()),
        );
    }
    m
}

fn request_lines(r: &RenderedRequest, indent: &str, style: Style, lines: &mut Vec<String>) {
    lines.push(format!("{indent}{} {}", r.method.as_str(), r.url));
    if !r.headers.is_empty() {
        lines.push(format!("{indent}headers"));
        for (k, v) in &r.headers {
            lines.push(format!("{indent}  {k}: {v}"));
        }
    }
    if !r.body.is_null() {
        lines.push(format!("{indent}body"));
        for l in human_value(&r.body, style).lines() {
            lines.push(format!("{indent}  {l}"));
        }
    }
}

/// The human form of a preview. `rerun` names the flags that execute it,
/// when it was printed because confirmation is missing.
pub(crate) fn preview_human(p: &PreviewResult, rerun: Option<&str>, style: Style) -> String {
    let mut lines = vec![format!(
        "{} · {} · {}",
        style.bold("Preview"),
        safety_name(p.safety),
        p.operation
    )];
    match &p.steps {
        Some(steps) => {
            for s in steps {
                lines.push(format!(
                    "  {}. {} {} ({})",
                    s.step,
                    kind_name(s.kind),
                    s.operation,
                    safety_name(s.safety)
                ));
                match &s.request {
                    Some(r) => request_lines(r, "     ", style, &mut lines),
                    None => lines.push("     (arguments come from an earlier step)".into()),
                }
                for e in &s.effects {
                    lines.push(format!("     • {e}"));
                }
            }
        }
        None => request_lines(&p.request, "  ", style, &mut lines),
    }
    if p.steps.is_none() && !p.effects.is_empty() {
        lines.push("  effects".into());
        for e in &p.effects {
            lines.push(format!("    • {e}"));
        }
    } else if let Some(steps) = &p.steps
        && !p.effects.is_empty()
        && steps.iter().all(|s| s.effects.is_empty())
    {
        lines.push("  effects".into());
        for e in &p.effects {
            lines.push(format!("    • {e}"));
        }
    }
    if let Some(s) = &p.server_preview {
        lines.push("  server preview".into());
        for l in human_value(s, style).lines() {
            lines.push(format!("    {l}"));
        }
    }
    if let Some(t) = &p.confirmation_token {
        let ttl = p
            .expires_in_ms
            .map(|ms| format!(" (valid for {} s)", ms / 1000))
            .unwrap_or_default();
        lines.push(format!("  confirmation token: {t}{ttl}"));
    }
    if let Some(flags) = rerun {
        lines.push(format!("Re-run with {flags} to execute."));
    }
    lines.join("\n")
}
