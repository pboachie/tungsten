// SPDX-License-Identifier: AGPL-3.0-only
//! `llms.txt` and `llms-full.txt` (planning/05 "Docs", NFR-8).
//!
//! Both follow the llms.txt layout: an H1 title, a blockquote summary, then
//! sections. Callable operations are listed per namespace and resource as
//! `METHOD path — summary [tier] [idempotency]`; gated ones carry their
//! environment variable; planned operations appear only under "Planned (not
//! available)" with the reason they are not callable. The generated-file
//! header is an HTML comment at the end, so the title stays the first line.

use std::fmt::Write as _;

use tungsten_emit::args::{BodyArg, args_layout};
use tungsten_emit::header_text;
use tungsten_emit::schema::{collapse_whitespace, prune_sentences};
use tungsten_ir::{
    ErrorModel, Ir, Operation, OperationStatus, PreviewMode, Remediation, Response, ResponseKind,
    Retryable, StatusMatch,
};

use crate::compact::{Compact, key};
use crate::model::{
    CallableOp, Model, NamespaceDocs, api_summary, api_title, auth_lines, endpoint, gate,
    macro_summary, markers, overview, planned_reason, safety, security, summary,
};

/// `llms.txt`.
pub(crate) fn llms(model: &Model<'_>) -> String {
    let mut out = preamble(model, &api_title(model.ir));
    for n in &model.namespaces {
        if n.callable.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\n{}", namespace_heading(n));
        let mut current: Option<String> = None;
        for c in &n.callable {
            let resource = c.resources.join(".");
            if current.as_deref() != Some(resource.as_str()) {
                let _ = writeln!(out, "\n### {resource}\n");
                current = Some(resource);
            }
            let _ = writeln!(
                out,
                "- {} — {} {}",
                endpoint(c.op),
                summary(c.op),
                markers(c.op)
            );
        }
    }
    macros_section(model, &mut out);
    planned_section(model, &mut out, false);
    out.push_str("\n## Optional\n\n");
    out.push_str("- [Full reference](llms-full.txt): arguments, responses, errors, remediation and types of every operation.\n");
    out.push_str(
        "- [Tool manifest](tools.json): one function-calling tool per callable operation.\n",
    );
    footer(model.ir, &mut out);
    out
}

/// `llms-full.txt`.
pub(crate) fn llms_full(model: &Model<'_>) -> String {
    let ir = model.ir;
    let mut out = preamble(model, &format!("{} — full reference", api_title(ir)));
    out.push_str(CONVENTIONS);
    let mut compact = Compact::default();
    for n in &model.namespaces {
        if n.callable.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\n{}", namespace_heading(n));
        for c in &n.callable {
            operation(model, n, c, &mut compact, &mut out);
        }
    }
    macros_section(model, &mut out);
    errors_section(ir, &mut out);
    planned_section(model, &mut out, true);
    let types = compact.definitions(ir);
    if !types.is_empty() {
        out.push_str("\n## Types\n\n");
        for (id, text, doc) in types {
            let _ = writeln!(out, "- {} = {text}", id.0);
            if let Some(doc) = doc {
                let _ = writeln!(out, "  {doc}");
            }
        }
    }
    footer(ir, &mut out);
    out
}

const CONVENTIONS: &str = "
## Conventions

- Arguments: every operation takes one object. Parameters are keyed by camelCase name; the fields of a JSON object body are merged in under their wire names, any other body is the `body` argument. Idempotency keys, the Origin header and credentials are call options or auth configuration, never arguments.
- Results: success bodies, or an error envelope with `category`, `retryable` and `remediation`. `retryable: never` means do not retry; `same_key_only` means retry only with the same idempotency key.
- Safety tiers: `read_only` has no side effects; `mutating` changes state; `destructive` needs a confirmation; `irreversible` needs a confirmation token from a preview of the same arguments.
- Types: `T?` optional, `T | null` nullable, `T[]` array, `{[key: string]: T}` map, `...` more properties allowed, `string{1..64}` length, `integer[1..20]` range, `/re/` pattern. Named types are listed under Types.
";

/// Title, summary, overview and the two auth lines.
fn preamble(model: &Model<'_>, title: &str) -> String {
    let ir = model.ir;
    let mut out = format!("# {}\n\n> {}\n\n", title, api_summary(ir));
    let _ = writeln!(out, "{}\n", overview(model));
    let [first, second] = auth_lines(ir);
    let _ = writeln!(out, "{first}\n{second}");
    out
}

fn namespace_heading(n: &NamespaceDocs<'_>) -> String {
    let title = collapse_whitespace(&n.ns.title);
    if title.is_empty() {
        format!("## {}", n.ns.name.wire)
    } else {
        format!("## {}: {title}", n.ns.name.wire)
    }
}

fn macros_section(model: &Model<'_>, out: &mut String) {
    if model.macros.is_empty() {
        return;
    }
    out.push_str("\n## Macros\n\nMulti-step workflows the SDK runs as one call.\n\n");
    for m in &model.macros {
        let steps: Vec<String> = m
            .mac
            .steps
            .as_array()
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|s| {
                        let kind = s.get("kind")?.as_str()?;
                        let op = s.get("operation")?.as_str()?;
                        Some(format!("{kind} {op}"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let _ = write!(
            out,
            "- {} — {} [{}]",
            m.mac.name.0,
            macro_summary(m.mac),
            safety(m.mac.safety)
        );
        if !steps.is_empty() {
            let _ = write!(out, " (steps: {})", steps.join(", "));
        }
        out.push('\n');
    }
}

/// Planned operations, never presented as callable.
fn planned_section(model: &Model<'_>, out: &mut String, full: bool) {
    if model.planned_count() == 0 {
        return;
    }
    out.push_str("\n## Planned (not available)\n\n");
    out.push_str(
        "Documented by the API but not callable: no SDK method or tool exists for these operations, and calling their paths will fail.\n\n",
    );
    for n in &model.namespaces {
        for op in &n.planned {
            let text = summary(op);
            let _ = write!(out, "- {} — {text}", endpoint(op));
            let mut notes = vec![];
            if text != op.id.0 {
                notes.push(op.id.0.clone());
            }
            if full {
                notes.push(format!("not callable: {}", planned_reason(op)));
            }
            if !notes.is_empty() {
                let _ = write!(out, " ({})", notes.join("; "));
            }
            out.push('\n');
        }
    }
}

fn footer(ir: &Ir, out: &mut String) {
    let header = header_text(ir).replace("--", "- -");
    let _ = write!(out, "\n<!-- {} -->\n", header.replace('\n', " "));
}

/// One operation's entry in `llms-full.txt`.
fn operation(
    model: &Model<'_>,
    n: &NamespaceDocs<'_>,
    c: &CallableOp<'_>,
    compact: &mut Compact,
    out: &mut String,
) {
    let op = c.op;
    let _ = writeln!(out, "\n### {}\n", c.sdk_path(&n.ns.name.wire));
    let _ = writeln!(out, "{} — {} {}\n", endpoint(op), summary(op), markers(op));
    let _ = writeln!(out, "- Operation: {} · tool `{}`", op.id.0, c.tool);
    let _ = writeln!(out, "- Auth: {}", security(op));
    let _ = writeln!(out, "- Args: {}", args(model.ir, op, compact));
    let _ = writeln!(out, "- Returns: {}", responses(op, compact, false));
    let errors = responses(op, compact, true);
    if !errors.is_empty() {
        let _ = writeln!(out, "- Errors: {errors}");
    }
    if let Some(p) = &op.pagination {
        let _ = writeln!(out, "- Pagination: {}", pagination(p));
    }
    if let Some(g) = gate(op) {
        let text = match &op.status {
            OperationStatus::Gated { gate } => model.ir.agent.gates.get(&gate.env_var),
            _ => None,
        };
        match text {
            Some(t) => {
                let _ = writeln!(out, "- Gate: {g}. {}", collapse_whitespace(t));
            }
            None => {
                let _ = writeln!(out, "- Gate: {g}.");
            }
        }
    }
    agent_lines(model, op, out);
    if let Some(text) = description(op) {
        let _ = writeln!(out, "\n{text}");
    }
    remediation_table(&op.agent.remediation, out);
}

/// The arguments object in compact notation (see `tungsten_emit::args`).
fn args(ir: &Ir, op: &Operation, compact: &mut Compact) -> String {
    let layout = args_layout(ir, op);
    let mut parts = vec![];
    for p in &layout.params {
        parts.push(format!(
            "{}{}: {}",
            key(&p.key),
            if p.param.required { "" } else { "?" },
            compact.type_ref(&p.param.ty)
        ));
    }
    match &layout.body {
        Some(BodyArg::Merged { fields, .. }) => {
            for f in fields.iter().filter(|f| !f.read_only) {
                parts.push(compact.field(f, !layout.body_required));
            }
        }
        Some(BodyArg::Arg { key: k, content }) => parts.push(format!(
            "{}{}: {} ({})",
            key(k),
            if layout.body_required { "" } else { "?" },
            compact.type_ref(&content.ty),
            content.media_type
        )),
        None => {}
    }
    if parts.is_empty() {
        "{}".into()
    } else {
        format!("{{ {} }}", parts.join(", "))
    }
}

/// Success (or error) responses: `200 public.Pet`, `204 (no body)`,
/// `413 text/plain`.
fn responses(op: &Operation, compact: &mut Compact, errors: bool) -> String {
    let list: Vec<String> = op
        .responses
        .iter()
        .filter(|r| matches!(r.kind, ResponseKind::Success) != errors)
        .map(|r| response(r, compact))
        .collect();
    if list.is_empty() && !errors {
        return "nothing documented".into();
    }
    list.join(", ")
}

fn response(r: &Response, compact: &mut Compact) -> String {
    let status = match r.status {
        StatusMatch::Exact(code) => code.to_string(),
        StatusMatch::Range(n) => format!("{n}XX"),
        StatusMatch::Default => "default".into(),
    };
    let body = match r.content.first() {
        None => "(no body)".to_string(),
        Some(c) if c.media_type.contains("json") => compact.type_ref(&c.ty),
        Some(c) => c.media_type.clone(),
    };
    let ambiguous = if matches!(r.kind, ResponseKind::Ambiguous) {
        " (outcome unknown)"
    } else {
        ""
    };
    format!("{status} {body}{ambiguous}")
}

fn pagination(p: &tungsten_ir::Pagination) -> String {
    use tungsten_ir::PaginationStyle as S;
    let style = match &p.style {
        S::Cursor {
            request_param,
            response_field,
        } => format!("cursor: pass `{response_field}` from the response as `{request_param}`"),
        S::Offset {
            offset_param,
            limit_param,
        } => format!("offset: `{offset_param}` and `{limit_param}`"),
        S::Page {
            page_param,
            size_param,
        } => format!("page: `{page_param}` and `{size_param}`"),
        S::LinkHeader => "Link header: follow rel=\"next\"".into(),
    };
    let items = if p.items_field.is_empty() {
        "the response array".to_string()
    } else {
        format!("`{}`", p.items_field)
    };
    let mut text = format!("{style}; items in {items}");
    if let Some(size) = &p.page_size_param {
        text.push_str(&format!("; page size `{size}`"));
    }
    text
}

/// Preview, confirmation, verification, sensitive fields and the
/// remediation note of an operation, when set.
fn agent_lines(model: &Model<'_>, op: &Operation, out: &mut String) {
    let a = &op.agent;
    let preview = match &a.preview {
        PreviewMode::Local => {
            Some("local (validates and renders the request, no network)".to_string())
        }
        PreviewMode::Header { header, value } => {
            Some(format!("server dry run (header {header}: {value})"))
        }
        PreviewMode::Endpoint { operation } => Some(format!("via {}", operation.0)),
        PreviewMode::None => None,
    };
    if let Some(p) = preview {
        let _ = writeln!(out, "- Preview: {p}");
    }
    if let Some(c) = &a.confirmation {
        let mut text = String::new();
        if !c.summary_fields.is_empty() {
            text.push_str(&format!("shows {}", c.summary_fields.join(", ")));
        }
        if let Some(m) = &c.message {
            if !text.is_empty() {
                text.push_str("; ");
            }
            text.push_str(&format!("\"{}\"", collapse_whitespace(m)));
        }
        let _ = writeln!(out, "- Confirmation: {text}");
    }
    if let Some(v) = &a.verify {
        let mut text = format!("call {}", v.operation.0);
        if model.find(&v.operation.0).is_none() {
            text.push_str(" (not callable)");
        }
        if !v.expect.is_null() {
            text.push_str(&format!(" and expect {}", v.expect));
        }
        if !v.terminal.is_null() {
            text.push_str(&format!("; terminal when {}", v.terminal));
        }
        let _ = writeln!(out, "- Verify: {text}");
    }
    if !a.sensitive_response_fields.is_empty() {
        let once = if a.shown_once {
            " (shown once: store it now)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "- Sensitive response fields: {}{once}",
            a.sensitive_response_fields.join(", ")
        );
    } else if a.shown_once {
        let _ = writeln!(out, "- The response is shown once: store it now.");
    }
    if let Some(note) = &a.remediation_note {
        let _ = writeln!(out, "- Note: {}", collapse_whitespace(note));
    }
}

/// The operation's compacted description: the agent `compact_doc`, else
/// the spec description cut to three sentences. `None` when it would only
/// repeat the summary.
fn description(op: &Operation) -> Option<String> {
    let text = if op.agent.compact_doc.trim().is_empty() {
        prune_sentences(op.doc.as_ref()?.description.as_deref()?, 3)
    } else {
        collapse_whitespace(&op.agent.compact_doc)
    };
    (!text.is_empty() && text != summary(op)).then_some(text)
}

fn retryable(r: Retryable) -> &'static str {
    match r {
        Retryable::Never => "never",
        Retryable::AfterDelay => "after_delay",
        Retryable::SameKeyOnly => "same_key_only",
        Retryable::AfterRemediation => "after_remediation",
    }
}

fn cell(text: &str) -> String {
    collapse_whitespace(text).replace('|', "\\|")
}

fn remediation_table(entries: &std::collections::BTreeMap<String, Remediation>, out: &mut String) {
    if entries.is_empty() {
        return;
    }
    out.push_str("\n| code | category | retryable | remediation |\n|---|---|---|---|\n");
    for (code, r) in entries {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(code),
            r.category.as_deref().map(cell).unwrap_or_default(),
            r.retryable.map(retryable).unwrap_or_default(),
            remediation_text(r)
        );
    }
}

fn remediation_text(r: &Remediation) -> String {
    let mut text = r.text.as_deref().map(cell).unwrap_or_default();
    if let Some(next) = &r.next_action {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&format!("Next: {}", cell(next)));
    }
    text
}

/// Error models per namespace with the global remediation, the ambiguous
/// statuses and the gates.
fn errors_section(ir: &Ir, out: &mut String) {
    let any_codes = ir.namespaces.iter().any(|n| !n.errors.codes.is_empty())
        || !ir.agent.error_codes.is_empty();
    if !any_codes && ir.agent.ambiguous_statuses.is_empty() && ir.agent.gates.is_empty() {
        return;
    }
    out.push_str("\n## Errors\n");
    for n in &ir.namespaces {
        error_model(&n.name.wire, &n.errors, ir, out);
    }
    let known: std::collections::BTreeSet<&str> = ir
        .namespaces
        .iter()
        .flat_map(|n| n.errors.codes.iter().map(|c| c.code.as_str()))
        .collect();
    let extra: std::collections::BTreeMap<String, Remediation> = ir
        .agent
        .error_codes
        .iter()
        .filter(|(code, _)| !known.contains(code.as_str()))
        .map(|(c, r)| (c.clone(), r.clone()))
        .collect();
    if !extra.is_empty() {
        out.push_str("\n### Other codes\n");
        remediation_table(&extra, out);
    }
    if !ir.agent.ambiguous_statuses.is_empty() {
        let statuses: Vec<String> = ir
            .agent
            .ambiguous_statuses
            .iter()
            .map(u16::to_string)
            .collect();
        let _ = writeln!(
            out,
            "\nAmbiguous statuses: {}. On a mutation they mean the outcome is unknown: do not retry with a new idempotency key; verify first.",
            statuses.join(", ")
        );
    }
    if !ir.agent.gates.is_empty() {
        out.push_str("\nRuntime gates:\n\n");
        for (env, text) in &ir.agent.gates {
            let _ = writeln!(out, "- {env}: {}", collapse_whitespace(text));
        }
    }
}

fn error_model(ns: &str, model: &ErrorModel, ir: &Ir, out: &mut String) {
    if model.codes.is_empty() && model.envelope.is_none() {
        return;
    }
    let _ = write!(out, "\n### {ns}");
    if let Some(env) = &model.envelope {
        let _ = write!(out, " (envelope {}", env.0);
        if let Some(field) = &model.code_field {
            let _ = write!(out, ", code at `{field}`");
        }
        out.push(')');
    }
    out.push('\n');
    if model.codes.is_empty() {
        return;
    }
    let remediated = model
        .codes
        .iter()
        .any(|c| ir.agent.error_codes.contains_key(&c.code));
    if remediated {
        out.push_str(
            "\n| code | statuses | category | retryable | remediation |\n|---|---|---|---|---|\n",
        );
    } else {
        out.push_str("\n| code | statuses |\n|---|---|\n");
    }
    for code in &model.codes {
        let statuses: Vec<String> = code.statuses.iter().map(u16::to_string).collect();
        let _ = write!(out, "| {} | {} |", cell(&code.code), statuses.join(", "));
        if remediated {
            let r = ir.agent.error_codes.get(&code.code);
            let _ = write!(
                out,
                " {} | {} | {} |",
                r.and_then(|r| r.category.as_deref())
                    .map(cell)
                    .unwrap_or_default(),
                r.and_then(|r| r.retryable)
                    .map(retryable)
                    .unwrap_or_default(),
                r.map(remediation_text).unwrap_or_default()
            );
        }
        out.push('\n');
    }
}
