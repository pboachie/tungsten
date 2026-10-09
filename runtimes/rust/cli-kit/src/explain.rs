// SPDX-License-Identifier: Apache-2.0
//! `explain-error <CODE|-|@file>`: what an error category, an API error code
//! or a whole error envelope means for the person or agent running the CLI.

use clap::ArgMatches;
use serde_json::{Map, Value, json};
use tungsten_runtime::internals::{default_retryable, generic_remediation};
use tungsten_runtime::{Category, Retryable};

use crate::ctx::{Ctx, Fail};
use crate::exit;
use crate::render::json_doc;
use crate::spec::{CliOp, CliRemediation};

const CATEGORIES: [Category; 14] = [
    Category::ValidationFailed,
    Category::MalformedRequest,
    Category::RequestTooLarge,
    Category::AuthFailed,
    Category::NotFound,
    Category::Conflict,
    Category::PreconditionFailed,
    Category::RateLimited,
    Category::UpstreamUnavailable,
    Category::OutcomeUnknown,
    Category::TransportFailed,
    Category::ConfirmationRequired,
    Category::GateDisabled,
    Category::UnexpectedResponse,
];

fn category_name(c: Category) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn parse_category(text: &str) -> Option<Category> {
    let wanted = text.trim().to_ascii_uppercase().replace('-', "_");
    CATEGORIES.into_iter().find(|c| category_name(*c) == wanted)
}

fn retryable_name(r: Retryable) -> &'static str {
    match r {
        Retryable::Never => "never",
        Retryable::AfterDelay => "after_delay",
        Retryable::SameKeyOnly => "same_key_only",
        Retryable::AfterRemediation => "after_remediation",
    }
}

fn parse_retryable(text: &str) -> Option<Retryable> {
    [
        Retryable::Never,
        Retryable::AfterDelay,
        Retryable::SameKeyOnly,
        Retryable::AfterRemediation,
    ]
    .into_iter()
    .find(|r| retryable_name(*r) == text)
}

fn retryable_meaning(r: Retryable) -> &'static str {
    match r {
        Retryable::Never => "do not repeat the call unchanged",
        Retryable::AfterDelay => {
            "repeat the same call after waiting (retry_after_ms of the envelope, when set)"
        }
        Retryable::SameKeyOnly => {
            "repeat it only with the same idempotency key, so a second attempt cannot apply the effect twice"
        }
        Retryable::AfterRemediation => {
            "repeat it only after the cause is fixed or the state has been checked"
        }
    }
}

/// What the CLI's flags mean for a category.
fn flag_notes(c: Category, bin: &str, env_prefix: &str) -> Vec<String> {
    let key = "--idempotency-key KEY: a UUIDv4 generated once, kept with the intent of the call and passed again, unchanged, on every retry";
    match c {
        Category::ValidationFailed => vec![
            format!("`{bin} schema <command>` prints the arguments the command takes; fix the flag or body field the remediation names"),
            "--idempotency-key KEY: required by operations that need a caller-owned key; generate a UUIDv4 once and reuse it on every retry".into(),
            "--dry-run: print the request without sending it, to check arguments".into(),
        ],
        Category::MalformedRequest => vec![
            "check the format of identifiers and values (for example canonical UUIDs)".into(),
            "--dry-run: print the request without sending it".into(),
        ],
        Category::RequestTooLarge => vec!["send a smaller body (--body @file.json)".into()],
        Category::AuthFailed => vec![
            format!("`{bin} auth status` shows which credentials are present, never their values"),
            format!("credentials come from {env_prefix}_<SCHEME> environment variables or the config file; a secret flag takes `-` or `env:NAME`"),
        ],
        Category::NotFound => vec!["check the identifier flags; --base-url and --profile pick another deployment".into()],
        Category::Conflict => vec![
            "read the current state with a read command before deciding; a different --idempotency-key does not resolve a conflict".into(),
        ],
        Category::PreconditionFailed => vec!["resolve the account, billing or resource state first; repeating the command does not help".into()],
        Category::RateLimited => vec![
            "wait retry_after_ms (a few seconds when it is null), then run the same command".into(),
            format!("on a mutation pass the same {key}"),
        ],
        Category::UpstreamUnavailable => vec![
            "wait, then run the same command".into(),
            format!("on a mutation pass the same {key}"),
        ],
        Category::TransportFailed => vec![
            "--base-url and --timeout: check the address and how long a request attempt may take".into(),
            "nothing reached the server, so the same command can be run again".into(),
        ],
        Category::OutcomeUnknown => vec![
            format!("{key}; the server returns the first result instead of applying the effect again"),
            "--verify: after a success, check the effect with the operation's verification hook".into(),
            "without a key, check the state with a read command before repeating the command".into(),
        ],
        Category::ConfirmationRequired => vec![
            "--dry-run: print the preview (request and effects) without sending anything".into(),
            "--yes: confirm a destructive operation; an irreversible one also needs --i-understand".into(),
            "without them the command prints the preview and exits 6; nothing was sent".into(),
        ],
        Category::GateDisabled => vec![
            "no flag changes this: the operation is switched off on the deployment (a deployment setting)".into(),
        ],
        Category::UnexpectedResponse => vec![
            "--json prints the envelope; the server's answer did not match the API description, so report it".into(),
        ],
    }
}

/// The exit codes a category can produce, ascending.
fn exit_codes(c: Category, retryable: Retryable) -> Vec<u8> {
    let mut codes = vec![
        exit::for_parts(c, true, retryable),
        exit::for_parts(c, false, retryable),
    ];
    codes.sort_unstable();
    codes.dedup();
    codes
}

fn exit_text(codes: &[u8]) -> String {
    codes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

fn entry_json(op: Option<&CliOp>, bin: &str, e: &CliRemediation) -> Value {
    json!({
        "operation": op.map(|o| o.id.as_str()),
        "command": op.map(|o| format!("{bin} {}", o.path.join(" "))),
        "category": e.category,
        "text": e.text,
        "retryable": e.retryable,
        "next_action": e.next_action,
    })
}

struct Explained {
    kind: &'static str,
    category: Option<Category>,
    code: Option<String>,
    operation: Option<String>,
    command: Option<String>,
    http_status: Option<u64>,
    retryable: Option<Retryable>,
    exit_codes: Vec<u8>,
    /// What the category means (the runtime's generic remediation).
    meaning: Option<&'static str>,
    /// The envelope's own remediation text.
    remediation: Option<String>,
    next_action: Option<String>,
    manifest: Vec<Value>,
    note: Option<String>,
    flags: Vec<String>,
}

fn text_of<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn entries_for<'a>(
    ctx: &'a Ctx<'_>,
    code: &str,
    op: Option<&'a CliOp>,
) -> Vec<(Option<&'a CliOp>, &'a CliRemediation)> {
    let spec = ctx.spec;
    let same = |a: &str, b: &str| a == b || a.eq_ignore_ascii_case(b);
    let mut found = Vec::new();
    match op {
        Some(o) => {
            found.extend(
                o.remediation
                    .iter()
                    .filter(|e| same(&e.code, code))
                    .map(|e| (Some(o), e)),
            );
        }
        None => {
            for o in &spec.ops {
                found.extend(
                    o.remediation
                        .iter()
                        .filter(|e| same(&e.code, code))
                        .map(|e| (Some(o), e)),
                );
            }
        }
    }
    found.extend(
        spec.error_codes
            .iter()
            .filter(|e| same(&e.code, code))
            .map(|e| (None, e)),
    );
    found
}

fn explain_code(ctx: &Ctx<'_>, input: &str) -> Result<Explained, Fail> {
    let spec = ctx.spec;
    let category = parse_category(input);
    let entries = entries_for(ctx, input, None);
    if category.is_none() && entries.is_empty() {
        let mut known: Vec<&str> = spec
            .error_codes
            .iter()
            .chain(spec.ops.iter().flat_map(|o| o.remediation.iter()))
            .map(|e| e.code.as_str())
            .collect();
        known.sort_unstable();
        known.dedup();
        let names: Vec<String> = CATEGORIES.into_iter().map(category_name).collect();
        let mut message = format!(
            "`{input}` is neither an error category nor an error code of this API; categories: {}",
            names.join(", ")
        );
        if !known.is_empty() {
            message.push_str(&format!("; API codes: {}", known.join(", ")));
        }
        return Err(Fail::Usage(message));
    }
    let entry_category = entries
        .iter()
        .find_map(|(_, e)| e.category.as_deref().and_then(parse_category));
    let category = category.or(entry_category);
    let entry_retry = entries
        .iter()
        .find_map(|(_, e)| e.retryable.as_deref().and_then(parse_retryable));
    let retryable = entry_retry.or_else(|| category.map(default_retryable));
    let codes = match category {
        Some(c) if parse_category(input).is_some() => {
            exit_codes(c, retryable.unwrap_or(Retryable::Never))
        }
        Some(c) => vec![exit::for_parts(
            c,
            true,
            retryable.unwrap_or(Retryable::Never),
        )],
        None => vec![exit::for_parts(
            Category::UnexpectedResponse,
            true,
            retryable.unwrap_or(Retryable::Never),
        )],
    };
    let is_category = parse_category(input).is_some();
    Ok(Explained {
        kind: if is_category { "category" } else { "code" },
        category,
        code: (!is_category).then(|| input.to_string()),
        operation: None,
        command: None,
        http_status: None,
        retryable,
        exit_codes: codes,
        meaning: category.map(generic_remediation),
        remediation: None,
        next_action: None,
        manifest: entries
            .iter()
            .map(|(o, e)| entry_json(*o, &spec.bin, e))
            .collect(),
        note: None,
        flags: category
            .map(|c| flag_notes(c, &spec.bin, &spec.env_prefix))
            .unwrap_or_default(),
    })
}

fn explain_envelope(ctx: &Ctx<'_>, m: &Map<String, Value>) -> Result<Explained, Fail> {
    let spec = ctx.spec;
    let category = text_of(m, "category")
        .and_then(parse_category)
        .ok_or_else(|| {
            Fail::Usage(
                "the JSON is not an error envelope: `category` is missing or not one of the error categories"
                    .into(),
            )
        })?;
    let code = text_of(m, "code").map(str::to_string);
    let op_id = text_of(m, "operation");
    let op = op_id.and_then(|id| spec.ops.iter().find(|o| o.id == id));
    let http_status = m.get("http_status").and_then(Value::as_u64);
    let retryable = text_of(m, "retryable")
        .and_then(parse_retryable)
        .unwrap_or_else(|| default_retryable(category));
    let entries = match (&code, op) {
        (Some(c), Some(o)) => entries_for(ctx, c, Some(o)),
        (Some(c), None) => spec
            .error_codes
            .iter()
            .filter(|e| e.code == *c)
            .map(|e| (None, e))
            .collect(),
        (None, _) => vec![],
    };
    Ok(Explained {
        kind: "envelope",
        category: Some(category),
        code,
        operation: op_id.map(str::to_string),
        command: op.map(|o| format!("{} {}", spec.bin, o.path.join(" "))),
        http_status,
        retryable: Some(retryable),
        exit_codes: vec![exit::for_parts(category, http_status.is_some(), retryable)],
        meaning: Some(generic_remediation(category)),
        remediation: text_of(m, "remediation").map(str::to_string),
        next_action: text_of(m, "next_action").map(str::to_string),
        manifest: entries
            .iter()
            .map(|(o, e)| entry_json(*o, &spec.bin, e))
            .collect(),
        note: op.and_then(|o| o.remediation_note.clone()),
        flags: flag_notes(category, &spec.bin, &spec.env_prefix),
    })
}

fn to_json(x: &Explained, input: &str) -> Value {
    let codes: Vec<u64> = x.exit_codes.iter().map(|c| u64::from(*c)).collect();
    let mut doc = json!({
        "kind": x.kind,
        "input": if x.kind == "envelope" { Value::Null } else { json!(input) },
        "category": x.category.map(category_name),
        "code": x.code,
        "operation": x.operation,
        "command": x.command,
        "http_status": x.http_status,
        "retryable": x.retryable.map(retryable_name),
        "retryable_meaning": x.retryable.map(retryable_meaning),
        "meaning": x.meaning,
        "remediation": x.remediation,
        "next_action": x.next_action,
        "operation_remediation": x.manifest,
        "remediation_note": x.note,
        "flags": x.flags,
    });
    if let Value::Object(o) = &mut doc {
        if x.kind == "envelope" || codes.len() == 1 {
            o.insert("exit_code".into(), json!(codes[0]));
        } else {
            o.insert("exit_codes".into(), json!(codes));
        }
    }
    doc
}

fn human(ctx: &mut Ctx<'_>, x: &Explained, input: &str) {
    let style = ctx.out_style;
    let cat = x.category.map(category_name);
    let mut title = match (x.kind, &cat) {
        ("envelope", Some(c)) => format!("{} {c}", style.red("error")),
        ("category", Some(c)) => style.bold(c),
        _ => style.bold(input),
    };
    if x.kind == "envelope" {
        if let Some(code) = &x.code {
            title.push_str(&format!(" · {code}"));
        }
        if let Some(op) = &x.operation {
            title.push_str(&format!(" in {op}"));
        }
        if let Some(s) = x.http_status {
            title.push_str(&format!(" (HTTP {s})"));
        }
    } else if x.kind == "category" {
        title.push_str(" · error category");
    } else {
        title.push_str(" · error code of this API");
    }
    ctx.out(&title);
    let row = |ctx: &mut Ctx<'_>, label: &str, text: &str| {
        let mut lines = text.lines();
        ctx.out(&format!("  {label:<12}{}", lines.next().unwrap_or("")));
        for l in lines {
            ctx.out(&format!("  {:<12}{l}", ""));
        }
    };
    if x.kind == "code"
        && let Some(c) = &cat
    {
        row(ctx, "category", c);
    }
    if let Some(r) = x.retryable {
        row(
            ctx,
            "retryable",
            &format!("{}: {}", retryable_name(r), retryable_meaning(r)),
        );
    }
    let exits = exit_text(&x.exit_codes);
    row(ctx, "exit code", &exits);
    if let Some(c) = &x.command {
        row(ctx, "command", c);
    }
    if let Some(t) = &x.remediation {
        row(ctx, "remediation", t);
    }
    if let Some(t) = &x.next_action {
        row(ctx, "next action", t);
    }
    if let Some(t) = &x.meaning {
        row(ctx, "meaning", t);
    }
    if let Some(t) = &x.note {
        row(ctx, "note", t);
    }
    for e in &x.manifest {
        let scope = e["operation"]
            .as_str()
            .map_or(String::new(), |o| format!("[{o}] "));
        let mut parts: Vec<String> = Vec::new();
        for (k, label) in [
            ("text", ""),
            ("next_action", "next action: "),
            ("category", "category: "),
            ("retryable", "retryable: "),
        ] {
            if let Some(s) = e[k].as_str() {
                parts.push(format!("{label}{s}"));
            }
        }
        if !parts.is_empty() {
            row(ctx, "manifest", &format!("{scope}{}", parts.join("\n")));
        }
    }
    ctx.out("  flags");
    for f in &x.flags {
        ctx.out(&format!("    {f}"));
    }
}

/// `explain-error <CODE|-|@file>`.
pub(crate) fn run(ctx: &mut Ctx<'_>, m: &ArgMatches) -> Result<u8, Fail> {
    let input = m.get_one::<String>("input").cloned().unwrap_or_default();
    let raw = if input == "-" {
        read_text(ctx.stdin_all()?)?
    } else if let Some(path) = input.strip_prefix('@') {
        let bytes =
            std::fs::read(path).map_err(|e| Fail::Usage(format!("cannot read {path}: {e}")))?;
        read_text(bytes)?
    } else {
        input.clone()
    };
    let text = raw.trim();
    if text.is_empty() {
        return Err(Fail::Usage("nothing to explain: the input is empty".into()));
    }
    let explained = if text.starts_with('{') {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(o)) => explain_envelope(ctx, &o)?,
            Ok(_) => {
                return Err(Fail::Usage("the JSON is not an error envelope".into()));
            }
            Err(e) => return Err(Fail::Usage(format!("invalid JSON: {e}"))),
        }
    } else {
        explain_code(ctx, text)?
    };
    if ctx.json {
        let doc = to_json(&explained, text);
        let out = json_doc(&doc, ctx.pretty);
        ctx.out(&out);
    } else {
        human(ctx, &explained, text);
    }
    Ok(exit::OK)
}

fn read_text(bytes: Vec<u8>) -> Result<String, Fail> {
    String::from_utf8(bytes).map_err(|_| Fail::Usage("the input is not valid UTF-8".into()))
}
