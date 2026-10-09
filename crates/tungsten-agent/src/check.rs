// SPDX-License-Identifier: AGPL-3.0-only
//! Rules of the manifest a JSON Schema cannot express. Every violation is a
//! TG0602 error with the pointer of the offending node; none of them needs
//! the compiled API. The per-entry checks return problems relative to the
//! entry so the `x-agent-*` extensions reuse them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tungsten_ir::IdempotencyKind;

use crate::expr::{self, Problem};
use crate::model::*;
use crate::report::{Reporter, child};

/// The only supported manifest format version.
pub const AGENT_MANIFEST_VERSION: u32 = 1;

pub(crate) fn check(config: &AgentConfig, r: &mut Reporter<'_>) {
    if config.agent != AGENT_MANIFEST_VERSION {
        r.error(
            "TG0602",
            "/agent",
            format!(
                "unsupported agent manifest version {}; this tungsten reads version {AGENT_MANIFEST_VERSION}",
                config.agent
            ),
        );
    }
    defaults(&config.defaults, r);
    tools(&config.tools, r);
    errors(&config.errors, r);
    for (name, gate) in &config.gates {
        let at = child("/gates", name);
        if !is_env_name(name) {
            report(
                r,
                &at,
                vec![(
                    String::new(),
                    format!(
                        "gate `{name}` must be an environment variable name ([A-Za-z_][A-Za-z0-9_]*)"
                    ),
                )],
            );
        }
        if let Some(status) = gate.disabled_status {
            report(r, &at, status_problem(status, "/disabled_status"));
        }
    }
    macros(&config.macros, r);
    disclosure(&config.disclosure, r);
}

fn report(r: &mut Reporter<'_>, base: &str, problems: Vec<Problem>) {
    for (suffix, message) in problems {
        r.error("TG0602", &format!("{base}{suffix}"), message);
    }
}

fn defaults(d: &DefaultsConfig, r: &mut Reporter<'_>) {
    if let Some(i) = &d.idempotency {
        report(r, "/defaults/idempotency", idempotency_problems(i));
    }
    if let Some(p) = &d.preview {
        let mut problems = preview_problems(p);
        if p.object().mode == PreviewMode::Endpoint {
            problems.push((
                String::new(),
                "an endpoint preview names one operation; declare it under tools".into(),
            ));
        }
        report(r, "/defaults/preview", problems);
    }
    for (name, policy) in [
        ("read_only", &d.retries.read_only),
        ("mutating", &d.retries.mutating),
    ] {
        let Some(policy) = policy else { continue };
        let at = format!("/defaults/retries/{name}");
        if policy.max > 10 {
            report(
                r,
                &at,
                vec![("/max".into(), format!("max {} is above 10", policy.max))],
            );
        }
        if let Some(b) = &policy.backoff {
            for (key, v) in [("base_ms", b.base_ms), ("max_ms", b.max_ms)] {
                if v == Some(0) {
                    report(
                        r,
                        &at,
                        vec![(
                            format!("/backoff/{key}"),
                            format!("{key} must be at least 1"),
                        )],
                    );
                }
            }
            if let (Some(base), Some(max)) = (b.base_ms, b.max_ms)
                && base > max
            {
                report(
                    r,
                    &at,
                    vec![(
                        "/backoff".into(),
                        format!("base_ms {base} is above max_ms {max}"),
                    )],
                );
            }
        }
    }
    let dd = &d.disclosure;
    for (key, value, min) in [
        ("threshold", dd.threshold, 1),
        ("list_budget_tokens", dd.list_budget_tokens, 100),
        (
            "description_budget_tokens",
            dd.description_budget_tokens,
            10,
        ),
        ("schema_budget_tokens", dd.schema_budget_tokens, 50),
    ] {
        if let Some(v) = value
            && v < min
        {
            report(
                r,
                "/defaults/disclosure",
                vec![(format!("/{key}"), format!("{key} must be at least {min}"))],
            );
        }
    }
}

fn tools(tools: &[ToolConfig], r: &mut Reporter<'_>) {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, tool) in tools.iter().enumerate() {
        let at = format!("/tools/{i}");
        if let Some(problem) = operation_ref_problem(&tool.operation) {
            report(r, &at, vec![("/operation".into(), problem)]);
        } else if let Some(first) = seen.get(tool.operation.as_str()) {
            report(
                r,
                &at,
                vec![(
                    "/operation".into(),
                    format!(
                        "`{}` already has an entry at /tools/{first}; merge them",
                        tool.operation
                    ),
                )],
            );
        } else {
            seen.insert(&tool.operation, i);
        }
        report(r, &at, entry_problems(tool));
        if let Some(cluster) = &tool.cluster
            && !is_machine_name(cluster)
        {
            report(
                r,
                &at,
                vec![(
                    "/cluster".into(),
                    format!("cluster `{cluster}` must match ^[a-z][a-z0-9_]*$"),
                )],
            );
        }
        if tool.gate.as_deref().is_some_and(|g| !is_env_name(g)) {
            report(
                r,
                &at,
                vec![(
                    "/gate".into(),
                    "gate must be an environment variable name".into(),
                )],
            );
        }
    }
}

/// Problems of the parts of a tools entry that `x-agent-*` extensions share.
pub(crate) fn entry_problems(tool: &ToolConfig) -> Vec<Problem> {
    let mut out = vec![];
    let mut nest = |prefix: &str, problems: Vec<Problem>| {
        out.extend(
            problems
                .into_iter()
                .map(|(p, m)| (format!("{prefix}{p}"), m)),
        );
    };
    if let Some(i) = &tool.idempotency {
        nest("/idempotency", idempotency_problems(i));
    }
    if let Some(p) = &tool.preview {
        nest("/preview", preview_problems(p));
    }
    if let Some(v) = &tool.verify {
        nest("/verify", verify_problems(v));
    }
    nest("/remediation", remediation_problems(&tool.remediation));
    if let Some(c) = &tool.confirmation {
        nest("/confirmation", confirmation_problems(c));
    }
    if let Some(resp) = &tool.response {
        nest(
            "/response",
            field_list_problems("sensitive_fields", &resp.sensitive_fields),
        );
    }
    out
}

pub(crate) fn idempotency_problems(config: &IdempotencyConfig) -> Vec<Problem> {
    let p = config.policy();
    let mut out = vec![];
    let keyed = matches!(
        p.policy,
        IdempotencyKind::Auto | IdempotencyKind::CallerOwned | IdempotencyKind::ContentHash
    );
    if p.header.is_some() && !keyed {
        out.push((
            "/header".into(),
            format!(
                "policy `{}` sends no idempotency header",
                kind_name(p.policy)
            ),
        ));
    }
    if p.header.as_deref().is_some_and(|h| h.trim().is_empty()) {
        out.push(("/header".into(), "header must not be empty".into()));
    }
    if p.format.is_some() && p.policy != IdempotencyKind::CallerOwned {
        out.push((
            "/format".into(),
            "format applies to caller_owned keys only".into(),
        ));
    }
    match p.persist {
        Some(Persist::Required) if p.policy != IdempotencyKind::CallerOwned => out.push((
            "/persist".into(),
            "persist: required applies to caller_owned keys only".into(),
        )),
        Some(Persist::Runtime)
            if !matches!(
                p.policy,
                IdempotencyKind::Auto | IdempotencyKind::ContentHash
            ) =>
        {
            out.push((
                "/persist".into(),
                "persist: runtime applies to auto and content_hash keys only".into(),
            ))
        }
        _ => {}
    }
    out
}

pub(crate) fn kind_name(kind: IdempotencyKind) -> &'static str {
    match kind {
        IdempotencyKind::None => "none",
        IdempotencyKind::Auto => "auto",
        IdempotencyKind::CallerOwned => "caller_owned",
        IdempotencyKind::ContentHash => "content_hash",
        IdempotencyKind::ContentIdentity => "content_identity",
    }
}

pub(crate) fn preview_problems(config: &PreviewConfig) -> Vec<Problem> {
    let p = config.object();
    let mut out = vec![];
    let need = |out: &mut Vec<Problem>, key: &str, value: &Option<String>, mode: &str| {
        if value.as_deref().is_none_or(|v| v.trim().is_empty()) {
            out.push((format!("/{key}"), format!("a {mode} preview needs `{key}`")));
        }
    };
    let forbid = |out: &mut Vec<Problem>, key: &str, value: &Option<String>, mode: &str| {
        if value.is_some() {
            out.push((
                format!("/{key}"),
                format!("`{key}` does not apply to a {mode} preview"),
            ));
        }
    };
    match p.mode {
        PreviewMode::Header => {
            need(&mut out, "header", &p.header, "header");
            need(&mut out, "value", &p.value, "header");
            forbid(&mut out, "operation", &p.operation, "header");
        }
        PreviewMode::Endpoint => {
            match &p.operation {
                Some(op) => {
                    if let Some(problem) = operation_ref_problem(op) {
                        out.push(("/operation".into(), problem));
                    }
                }
                None => need(&mut out, "operation", &None, "endpoint"),
            }
            forbid(&mut out, "header", &p.header, "endpoint");
            forbid(&mut out, "value", &p.value, "endpoint");
        }
        PreviewMode::Local | PreviewMode::None => {
            let mode = if p.mode == PreviewMode::Local {
                "local"
            } else {
                "none"
            };
            forbid(&mut out, "header", &p.header, mode);
            forbid(&mut out, "value", &p.value, mode);
            forbid(&mut out, "operation", &p.operation, mode);
        }
    }
    out
}

pub(crate) fn verify_problems(v: &VerifyConfig) -> Vec<Problem> {
    let mut out = vec![];
    if let Some(problem) = operation_ref_problem(&v.operation) {
        out.push(("/operation".into(), problem));
    }
    let args = Value::Object(v.args.clone());
    refs_with_roots(&args, "/args", &["response", "args"], &mut out);
    for (key, predicate) in [("expect", &v.expect), ("terminal", &v.terminal)] {
        let Some(predicate) = predicate else { continue };
        out.extend(
            expr::predicate_problems(predicate)
                .into_iter()
                .map(|(p, m)| (format!("/{key}{p}"), m)),
        );
        refs_with_roots(
            &Value::Object(predicate.clone()),
            &format!("/{key}"),
            &["response", "args"],
            &mut out,
        );
    }
    if let Some(poll) = &v.poll {
        if v.terminal.is_none() {
            out.push((
                "/poll".into(),
                "polling needs `terminal` states to stop at".into(),
            ));
        }
        for (key, value) in [
            ("interval_ms", poll.interval_ms),
            ("budget_ms", poll.budget_ms),
        ] {
            if value.is_some_and(|v| v < 100) {
                out.push((
                    format!("/poll/{key}"),
                    format!("{key} must be at least 100"),
                ));
            }
        }
    }
    out
}

/// References in `value` whose root is not one of `roots`.
fn refs_with_roots(value: &Value, base: &str, roots: &[&str], out: &mut Vec<Problem>) {
    let (refs, problems) = expr::references(value);
    out.extend(problems.into_iter().map(|(p, m)| (format!("{base}{p}"), m)));
    for (at, r) in refs {
        if !roots.contains(&r.root.as_str()) {
            let allowed: Vec<String> = roots.iter().map(|r| format!("${r}")).collect();
            out.push((
                format!("{base}{at}"),
                format!(
                    "`${}` is not available here; use {}",
                    r.root,
                    allowed.join(" or ")
                ),
            ));
        }
    }
}

pub(crate) fn remediation_problems(
    map: &indexmap::IndexMap<String, RemediationConfig>,
) -> Vec<Problem> {
    let mut out = vec![];
    for (code, entry) in map {
        if *entry == RemediationConfig::default() {
            out.push((
                child("", code),
                format!("remediation for `{code}` is empty; give text, retryable, category or next_action"),
            ));
        }
    }
    out
}

pub(crate) fn confirmation_problems(c: &ConfirmationConfig) -> Vec<Problem> {
    let mut out = field_list_problems("summary_fields", &c.summary_fields);
    if let Some(message) = &c.message
        && let Err(problem) = placeholders(message)
    {
        out.push(("/message".into(), problem));
    }
    out
}

fn field_list_problems(key: &str, fields: &[String]) -> Vec<Problem> {
    fields
        .iter()
        .enumerate()
        .filter(|(_, f)| !is_field_path(f))
        .map(|(i, f)| (format!("/{key}/{i}"), format!("`{f}` is not a field path")))
        .collect()
}

/// `{field}` placeholders of a confirmation message, in order.
pub(crate) fn placeholders(message: &str) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut rest = message;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| "unclosed `{` in message".to_string())?;
        let name = &after[..end];
        if !is_field_path(name) {
            return Err(format!("`{{{name}}}` is not a field placeholder"));
        }
        out.push(name.to_string());
        rest = &after[end + 1..];
    }
    Ok(out)
}

fn errors(e: &ErrorsConfig, r: &mut Reporter<'_>) {
    if let Some(env) = &e.envelope {
        for (key, value) in [
            ("schema", Some(&env.schema)),
            ("code_field", env.code_field.as_ref()),
            ("message_field", env.message_field.as_ref()),
        ] {
            if value.is_some_and(|v| !is_field_path(v)) {
                report(
                    r,
                    "/errors/envelope",
                    vec![(
                        format!("/{key}"),
                        format!("{key} must be a name or dotted path"),
                    )],
                );
            }
        }
    }
    for (i, s) in e.ambiguous_statuses.iter().enumerate() {
        report(
            r,
            "/errors/ambiguous_statuses",
            status_problem(*s, &format!("/{i}")),
        );
    }
    let mut seen = BTreeSet::new();
    for (i, n) in e.non_json.iter().enumerate() {
        let at = format!("/errors/non_json/{i}");
        report(r, &at, status_problem(n.status, "/status"));
        if n.media.trim().is_empty() {
            report(
                r,
                &at,
                vec![(
                    "/media".into(),
                    "media must be a media type or `none`".into(),
                )],
            );
        }
        if !seen.insert((n.status, n.media.to_ascii_lowercase())) {
            report(
                r,
                &at,
                vec![(
                    String::new(),
                    format!(
                        "status {} with media `{}` is listed twice",
                        n.status.0, n.media
                    ),
                )],
            );
        }
    }
    report(r, "/errors/codes", remediation_problems(&e.codes));
}

fn status_problem(status: StatusCode, at: &str) -> Vec<Problem> {
    if (100..=599).contains(&status.0) {
        vec![]
    } else {
        vec![(
            at.to_string(),
            format!("{} is not an HTTP status (100 to 599)", status.0),
        )]
    }
}

fn macros(macros: &[MacroConfig], r: &mut Reporter<'_>) {
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, m) in macros.iter().enumerate() {
        let at = format!("/macros/{i}");
        match m.name.split_once('.') {
            Some((ns, name)) if is_machine_name(ns) && expr::is_identifier(name) => {
                if let Some(first) = names.get(m.name.as_str()) {
                    report(
                        r,
                        &at,
                        vec![(
                            "/name".into(),
                            format!("macro `{}` is already defined at /macros/{first}", m.name),
                        )],
                    );
                } else {
                    names.insert(&m.name, i);
                }
            }
            _ => report(
                r,
                &at,
                vec![(
                    "/name".into(),
                    format!("macro name `{}` must be <namespace>.<identifier>", m.name),
                )],
            ),
        }
        if m.summary.trim().is_empty() {
            report(
                r,
                &at,
                vec![("/summary".into(), "summary must not be empty".into())],
            );
        }
        if let Some(input) = &m.input
            && let Some(problem) = input.extends.as_deref().and_then(operation_ref_problem)
        {
            report(r, &at, vec![("/input/extends".into(), problem)]);
        }
        if m.steps.is_empty() {
            report(
                r,
                &at,
                vec![("/steps".into(), "a macro needs at least one step".into())],
            );
        }
        let mut names_so_far: Vec<&str> = vec!["input"];
        for (j, step) in m.steps.iter().enumerate() {
            report(
                r,
                &format!("{at}/steps/{j}"),
                step_problems(step, &names_so_far),
            );
            if let Some(name) = step.as_name.as_deref()
                && expr::is_identifier(name)
                && !names_so_far.contains(&name)
            {
                names_so_far.push(name);
            }
        }
        let mut problems = vec![];
        refs_with_roots(&m.output, "/output", &names_so_far, &mut problems);
        report(r, &at, problems);
        if let Some(resp) = &m.response {
            report(
                r,
                &format!("{at}/response"),
                field_list_problems("sensitive_fields", &resp.sensitive_fields),
            );
        }
    }
}

fn step_problems(step: &MacroStepConfig, earlier: &[&str]) -> Vec<Problem> {
    let mut out = vec![];
    let kinds: Vec<(&str, &String)> = [
        ("call", &step.call),
        ("poll", &step.poll),
        ("paginate", &step.paginate),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.as_ref().map(|v| (k, v)))
    .collect();
    let kind = match kinds.as_slice() {
        [(kind, op)] => {
            if let Some(problem) = operation_ref_problem(op) {
                out.push((format!("/{kind}"), problem));
            }
            *kind
        }
        _ => {
            out.push((
                String::new(),
                "a step needs exactly one of call, poll or paginate".into(),
            ));
            return out;
        }
    };
    if kind == "poll" && step.until.is_none() {
        out.push((String::new(), "a poll step needs `until`".into()));
    }
    if kind != "poll" {
        for (key, present) in [
            ("until", step.until.is_some()),
            ("interval_ms", step.interval_ms.is_some()),
            ("budget_ms", step.budget_ms.is_some()),
        ] {
            if present {
                out.push((
                    format!("/{key}"),
                    format!("`{key}` applies to poll steps only"),
                ));
            }
        }
    }
    if kind != "paginate" && step.max_pages.is_some() {
        out.push((
            "/max_pages".into(),
            "`max_pages` applies to paginate steps only".into(),
        ));
    }
    if step.max_pages == Some(0) {
        out.push(("/max_pages".into(), "max_pages must be at least 1".into()));
    }
    if step.interval_ms.is_some_and(|v| v < 100) {
        out.push((
            "/interval_ms".into(),
            "interval_ms must be at least 100".into(),
        ));
    }
    if let Some(name) = &step.as_name {
        if !expr::is_identifier(name) {
            out.push(("/as".into(), format!("`{name}` must be an identifier")));
        } else if earlier.contains(&name.as_str()) {
            out.push((
                "/as".into(),
                format!("`{name}` is already defined in this macro"),
            ));
        }
    }
    if let Some(args) = &step.args {
        refs_with_roots(args, "/args", earlier, &mut out);
    }
    if let Some(until) = &step.until {
        out.extend(
            expr::predicate_problems(until)
                .into_iter()
                .map(|(p, m)| (format!("/until{p}"), m)),
        );
        refs_with_roots(&Value::Object(until.clone()), "/until", earlier, &mut out);
    }
    match &step.budget_ms {
        None => {}
        Some(Value::Number(n)) if n.as_u64().is_some_and(|v| v >= 100) => {}
        Some(Value::String(s)) if s.starts_with('$') => {
            refs_with_roots(&Value::String(s.clone()), "/budget_ms", earlier, &mut out);
        }
        Some(_) => out.push((
            "/budget_ms".into(),
            "budget_ms must be an integer of at least 100 or a `$` reference".into(),
        )),
    }
    out
}

fn disclosure(d: &DisclosureConfig, r: &mut Reporter<'_>) {
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, c) in d.clusters.iter().enumerate() {
        let at = format!("/disclosure/clusters/{i}");
        if !is_machine_name(&c.name) {
            report(
                r,
                &at,
                vec![(
                    "/name".into(),
                    format!("cluster `{}` must match ^[a-z][a-z0-9_]*$", c.name),
                )],
            );
        } else if let Some(first) = names.get(c.name.as_str()) {
            report(
                r,
                &at,
                vec![(
                    "/name".into(),
                    format!(
                        "cluster `{}` is already declared at /disclosure/clusters/{first}",
                        c.name
                    ),
                )],
            );
        } else {
            names.insert(&c.name, i);
        }
        for (j, op) in c.operations.iter().enumerate() {
            if let Some(problem) = glob_problem(op) {
                report(r, &at, vec![(format!("/operations/{j}"), problem)]);
            }
        }
    }
    if d.prune.descriptions.max_sentences == Some(0) {
        report(
            r,
            "/disclosure/prune/descriptions/max_sentences",
            vec![(String::new(), "max_sentences must be at least 1".into())],
        );
    }
}

/// Why `op` is not `<namespace>.<rest>` with non-empty dot-separated parts.
pub(crate) fn operation_ref_problem(op: &str) -> Option<String> {
    let mut parts = op.split('.');
    let ns = parts.next().unwrap_or_default();
    let rest: Vec<&str> = parts.collect();
    let ok = is_machine_name(ns)
        && !rest.is_empty()
        && rest
            .iter()
            .all(|p| !p.is_empty() && !p.contains('*') && !p.chars().any(char::is_whitespace));
    (!ok).then(|| format!("`{op}` must be an operation reference: <namespace>.<operationId> or <namespace>.<resource path>.<method>"))
}

/// Like [`operation_ref_problem`], also accepting `<namespace>.*` and
/// `<namespace>.<resource path>.*`.
fn glob_problem(op: &str) -> Option<String> {
    match op.strip_suffix(".*") {
        Some(prefix) if is_machine_name(prefix) => None,
        Some(prefix) => operation_ref_problem(prefix).map(|_| {
            format!("`{op}` must be <namespace>.*, <namespace>.<resource path>.* or an operation reference")
        }),
        None => operation_ref_problem(op),
    }
}

pub(crate) fn is_machine_name(name: &str) -> bool {
    tungsten_config::is_machine_name(name)
}

fn is_env_name(name: &str) -> bool {
    expr::is_identifier(name)
}

/// A dotted path of non-empty names without whitespace.
pub(crate) fn is_field_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('.')
            .all(|p| !p.is_empty() && !p.chars().any(|c| c.is_whitespace() || c == '{' || c == '}'))
}
