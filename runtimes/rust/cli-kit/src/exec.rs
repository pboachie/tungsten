// SPDX-License-Identifier: Apache-2.0
//! Running a command: parse, resolve settings, build the client, call
//! through [`Dispatch`], apply the confirmation flow and print.

use std::ops::ControlFlow;

use clap::ArgMatches;
use serde_json::{Value, json};
use tungsten_runtime::{
    CallOptions, Category, ClientOptions, ConfigError, Confirm, Diagnostic, Dispatch, Error,
    MAX_PAGES_REACHED, Page, PreviewResult, ResponseMeta, Retryable, Safety, Status, Trace,
    Verification,
};

use crate::config::{self, Flags, Found, Settings};
use crate::ctx::{Ctx, Fail};
use crate::exit;
use crate::render::{
    error_human, human_array_item, human_value, json_doc, preview_human, preview_json,
};
use crate::spec::CliFlag;
use crate::tree::{self, Leaf, Tree};
use crate::{builtin, check, dotted, explain, login, values};

fn flag(m: &ArgMatches, name: &str) -> bool {
    m.try_get_one::<bool>(name)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

fn text<'a>(m: &'a ArgMatches, name: &str) -> Option<&'a str> {
    m.try_get_one::<String>(name)
        .ok()
        .flatten()
        .map(String::as_str)
}

fn descend(m: &ArgMatches) -> (Vec<String>, &ArgMatches) {
    let mut names = Vec::new();
    let mut cur = m;
    while let Some((name, sub)) = cur.subcommand() {
        names.push(name.to_string());
        cur = sub;
    }
    (names, cur)
}

/// An error envelope for a failure that never reached the SDK.
fn usage_envelope(operation: &str, message: &str, next: String) -> Diagnostic {
    Diagnostic {
        status: Status::Error,
        category: Category::ValidationFailed,
        operation: operation.to_string(),
        http_status: None,
        code: Some("USAGE".into()),
        failed_parameter: None,
        received_value: Value::Null,
        expected: None,
        remediation: message.to_string(),
        retryable: Retryable::Never,
        retry_after_ms: None,
        next_action: Some(next),
        request_id: None,
        trace: Trace { attempts: 0 },
    }
}

fn usage_error(ctx: &mut Ctx<'_>, command: &[String], message: &str) -> u8 {
    let bin = ctx.spec.bin.clone();
    let mut full = vec![bin.as_str()];
    full.extend(command.iter().map(String::as_str));
    let help = format!("{} --help", full.join(" "));
    let mut line = format!("{}: {message}", ctx.err_style.red("error"));
    if !message.contains("--help") {
        line.push_str(&format!("\n  try `{help}`"));
    }
    ctx.err(&line);
    if ctx.json {
        let operation = if command.is_empty() {
            bin
        } else {
            full.join(" ")
        };
        let envelope = usage_envelope(&operation, message, help);
        let doc = serde_json::to_value(&envelope).unwrap_or(Value::Null);
        let out = json_doc(&doc, ctx.pretty);
        ctx.out(&out);
    }
    exit::USAGE
}

fn internal_error(ctx: &mut Ctx<'_>, message: &str) -> u8 {
    let line = format!("{}: {message}", ctx.err_style.red("internal error"));
    ctx.err(&line);
    exit::INTERNAL
}

/// Print a failed call: the envelope on standard output with `--json`, the
/// human form on standard error otherwise; what the call produced before it
/// failed goes to standard error.
fn fail(ctx: &mut Ctx<'_>, e: &Error) -> u8 {
    if ctx.json {
        let doc = serde_json::to_value(&*e.diagnostic).unwrap_or(Value::Null);
        let out = json_doc(&doc, ctx.pretty);
        ctx.out(&out);
    } else {
        let line = error_human(&e.diagnostic, ctx.err_style);
        ctx.err(&line);
    }
    fail_partial(ctx, e)
}

/// A failure after part of a streamed result is already on standard
/// output: the diagnostic goes to standard error only (the envelope as
/// compact JSON with `--json`), so standard output is never a second
/// document.
fn fail_streamed(ctx: &mut Ctx<'_>, e: &Error) -> u8 {
    let line = if ctx.json {
        let doc = serde_json::to_value(&*e.diagnostic).unwrap_or(Value::Null);
        json_doc(&doc, false)
    } else {
        error_human(&e.diagnostic, ctx.err_style)
    };
    ctx.err(&line);
    fail_partial(ctx, e)
}

fn fail_partial(ctx: &mut Ctx<'_>, e: &Error) -> u8 {
    if let Some(p) = &e.partial {
        let line = format!(
            "partial result of the failed call (store any secret in it now): {}",
            json_doc(p, false)
        );
        ctx.err(&line);
    }
    exit::for_diagnostic(&e.diagnostic)
}

fn print_preview(ctx: &mut Ctx<'_>, p: &PreviewResult, rerun: Option<&str>) {
    if ctx.json {
        let mut doc = preview_json(p);
        if let Some(flags) = rerun {
            doc.insert("confirmation_required".into(), json!(true));
            let list: Vec<&str> = flags.split(' ').collect();
            doc.insert("rerun_with".into(), json!(list));
        }
        let out = json_doc(&Value::Object(doc), ctx.pretty);
        ctx.out(&out);
    } else {
        let out = preview_human(p, rerun, ctx.out_style);
        ctx.out(&out);
    }
}

fn verification_json(v: &Verification) -> Value {
    json!({
        "checked": v.checked,
        "passed": v.passed,
        "timed_out": v.timed_out,
        "observed": v.observed,
        "error": v.error.as_ref().map(|d| serde_json::to_value(d).unwrap_or(Value::Null)),
    })
}

/// What a successful call returns besides its body.
struct Done<'a> {
    value: Option<Value>,
    meta: Option<&'a ResponseMeta>,
    verification: Option<&'a Verification>,
}

fn success(ctx: &mut Ctx<'_>, done: Done<'_>, once: &[String], shown_once: bool) -> u8 {
    if ctx.json {
        let out = json_doc(done.value.as_ref().unwrap_or(&Value::Null), ctx.pretty);
        ctx.out(&out);
    } else {
        match &done.value {
            Some(v) => {
                let out = human_value(v, ctx.out_style);
                ctx.out(&out);
            }
            None => {
                let status = done
                    .meta
                    .map_or(String::new(), |m| format!(" (HTTP {})", m.status));
                ctx.err(&format!("ok{status}"));
            }
        }
    }
    if let Some(v) = done.verification {
        let line = if ctx.json {
            json_doc(&json!({ "verification": verification_json(v) }), false)
        } else if !v.checked {
            "verification: could not run".to_string()
        } else if v.passed {
            "verification: the effect is confirmed".to_string()
        } else if v.timed_out {
            "verification: the effect was not seen within the budget".to_string()
        } else {
            "verification: the effect does not match".to_string()
        };
        ctx.err(&line);
    }
    shown_once_note(ctx, once, shown_once);
    exit::OK
}

fn shown_once_note(ctx: &mut Ctx<'_>, once: &[String], shown_once: bool) {
    if shown_once {
        let what = if once.is_empty() {
            "this response holds values".to_string()
        } else {
            once.join(", ")
        };
        let line = format!(
            "{}: store {what} now; the API will not show it again.",
            ctx.err_style.yellow("note")
        );
        ctx.err(&line);
    }
}

/// `--all`: the items of every page as one array, written page by page. The
/// bytes are those of the array printed whole (compact or indented JSON
/// with `--json`, the human form otherwise), but only one page is held at a
/// time. A stop at `--max-pages` closes the array and notes on standard
/// error that more pages exist; a failure before the first page is the
/// ordinary failure output, and one after it ends the output unterminated
/// with the diagnostic on standard error and the failure's exit code.
struct ArrayOut<'c, 'a> {
    ctx: &'c mut Ctx<'a>,
    started: bool,
    pages: u32,
    capped: bool,
    failed: Option<Error>,
    max_pages: Option<usize>,
}

impl ArrayOut<'_, '_> {
    fn indented(&self) -> bool {
        !self.ctx.json || self.ctx.pretty
    }

    fn item_text(&self, item: &Value) -> String {
        if !self.ctx.json {
            human_array_item(item, self.ctx.out_style)
        } else if self.ctx.pretty {
            json_doc(item, true).replace('\n', "\n  ")
        } else {
            json_doc(item, false)
        }
    }

    fn page(&mut self, result: tungsten_runtime::Result<Page<Value>>) -> ControlFlow<()> {
        let page = match result {
            Ok(r) => r.value,
            Err(e) => {
                if self.max_pages.is_some() && e.diagnostic.remediation == MAX_PAGES_REACHED {
                    self.capped = true;
                } else {
                    self.failed = Some(e);
                }
                return ControlFlow::Break(());
            }
        };
        self.pages += 1;
        let mut text = String::new();
        for item in &page.items {
            text.push_str(match (self.started, self.indented()) {
                (false, false) => "[",
                (false, true) => "[\n  ",
                (true, false) => ",",
                (true, true) => ",\n  ",
            });
            self.started = true;
            text.push_str(&self.item_text(item));
        }
        if !text.is_empty() {
            self.ctx.out_part(&text);
        }
        ControlFlow::Continue(())
    }
}

async fn print_all<D: Dispatch>(
    ctx: &mut Ctx<'_>,
    client: &D,
    operation: &str,
    args: Value,
    opts: CallOptions,
    note: (&[String], bool),
) -> u8 {
    let max_pages = opts.max_pages;
    let mut out = ArrayOut {
        ctx,
        started: false,
        pages: 0,
        capped: false,
        failed: None,
        max_pages,
    };
    let mut sink = |r| out.page(r);
    client.pages_each(operation, args, opts, &mut sink).await;
    let ArrayOut {
        ctx,
        started,
        pages,
        capped,
        failed,
        ..
    } = out;
    if let Some(e) = failed {
        return if started {
            fail_streamed(ctx, &e)
        } else {
            fail(ctx, &e)
        };
    }
    let close = match (started, !ctx.json || ctx.pretty) {
        (false, _) => "[]",
        (true, false) => "]",
        (true, true) => "\n]",
    };
    ctx.out_part(&format!("{close}\n"));
    if capped {
        let limit = max_pages.unwrap_or_default();
        let line = format!(
            "{}: stopped after {pages} page{} (--max-pages {limit}); more pages are available. \
             Raise --max-pages to fetch more.",
            ctx.err_style.yellow("note"),
            if pages == 1 { "" } else { "s" },
        );
        ctx.err(&line);
    }
    shown_once_note(ctx, note.0, note.1);
    exit::OK
}

#[derive(Clone, Copy)]
enum Target<'a> {
    Op(&'a str),
    Macro(&'a str),
}

struct Plan<'a> {
    target: Target<'a>,
    tier: Safety,
    args: Value,
    opts: CallOptions,
    dry_run: bool,
    yes: bool,
    understand: bool,
    all: bool,
    shown_once: bool,
    once_fields: Vec<String>,
}

async fn preview<D: Dispatch>(
    client: &D,
    t: Target<'_>,
    args: Value,
    opts: CallOptions,
) -> tungsten_runtime::Result<PreviewResult> {
    match t {
        Target::Op(id) => client.preview(id, args, opts).await,
        Target::Macro(name) => client.preview_macro(name, args, opts).await,
    }
}

async fn execute<D: Dispatch>(ctx: &mut Ctx<'_>, client: &D, plan: Plan<'_>) -> u8 {
    let Plan {
        target,
        tier,
        args,
        mut opts,
        dry_run,
        yes,
        understand,
        all,
        shown_once,
        once_fields,
    } = plan;
    if dry_run {
        return match preview(client, target, args, opts).await {
            Ok(r) => {
                print_preview(ctx, &r.value, None);
                exit::OK
            }
            Err(e) => fail(ctx, &e),
        };
    }
    if matches!(tier, Safety::Destructive | Safety::Irreversible) {
        let p = match preview(client, target, args.clone(), opts.clone()).await {
            Ok(r) => r.value,
            Err(e) => return fail(ctx, &e),
        };
        let irreversible = tier == Safety::Irreversible;
        if !(yes && (understand || !irreversible)) {
            let needed = if irreversible {
                "--yes --i-understand"
            } else {
                "--yes"
            };
            print_preview(ctx, &p, Some(needed));
            return exit::CONFIRMATION_REQUIRED;
        }
        opts.confirm = match p.confirmation_token {
            Some(t) => Some(Confirm::Token(t)),
            None if !irreversible => Some(Confirm::Yes),
            None => None,
        };
    }
    if all && let Target::Op(id) = target {
        return print_all(ctx, client, id, args, opts, (&once_fields, shown_once)).await;
    }
    let outcome = match target {
        Target::Op(id) => client.invoke(id, args, opts).await,
        Target::Macro(name) => client.run_macro(name, args, opts).await,
    };
    match outcome {
        Ok(r) => {
            let done = Done {
                value: r.value,
                meta: Some(&r.meta),
                verification: r.verification.as_ref(),
            };
            success(ctx, done, &once_fields, shown_once)
        }
        Err(e) => fail(ctx, &e),
    }
}

struct Connected<D> {
    client: D,
    settings: Settings,
    found: Vec<Found>,
    /// The stored login sessions the client authenticates with.
    sessions: Option<std::sync::Arc<crate::session::SessionStore>>,
}

/// The settings of the command line and the client options they give,
/// before credentials.
fn base(ctx: &Ctx<'_>, m: &ArgMatches) -> Result<(Settings, ClientOptions), Fail> {
    let settings = config::resolve(
        ctx.spec,
        &ctx.env,
        Flags {
            base_url: text(m, "base-url"),
            profile: text(m, "profile"),
            config: text(m, "config"),
            timeout: text(m, "timeout"),
            deadline: text(m, "deadline"),
        },
    )
    .map_err(Fail::Usage)?;
    let mut options = ClientOptions {
        base_url: settings.base_url.as_ref().map(|(u, _)| u.clone()),
        ..ClientOptions::default()
    };
    if let Some((t, _)) = &settings.timeout {
        options.timeout = *t;
    }
    if let Some((d, _)) = &settings.deadline {
        options.deadline = Some(*d);
    }
    Ok((settings, options))
}

/// `account`: the `--account` of an `auth` command; other commands take the
/// account from the environment and profile only.
fn connect<D, F>(
    ctx: &mut Ctx<'_>,
    m: &ArgMatches,
    account: Option<&str>,
    make: &F,
) -> Result<Connected<D>, Fail>
where
    D: Dispatch,
    F: Fn(ClientOptions) -> Result<D, ConfigError>,
{
    let (settings, mut options) = base(ctx, m)?;
    // The credentials the client needs are named by its own auth schemes,
    // so the first client only tells which they are.
    let probe = make(options.clone()).map_err(|e| Fail::Usage(e.message))?;
    let (auth, found, warnings) = config::credentials(
        &ctx.spec.env_prefix,
        &ctx.spec.credential_env,
        &probe.core().api().auth,
        &ctx.env,
        &settings,
    );
    for w in warnings {
        ctx.err(&w);
    }
    let mut sessions = None;
    let client = if auth.is_empty() {
        probe
    } else {
        options.auth = auth;
        let api = probe.core().api().clone();
        sessions = login::attach(ctx, &settings, account, &api, &mut options)?;
        make(options).map_err(|e| Fail::Usage(e.message))?
    };
    Ok(Connected {
        client,
        settings,
        found,
        sessions,
    })
}

fn call_options(m: &ArgMatches) -> Result<CallOptions, Fail> {
    let key = text(m, "idempotency-key");
    if key.is_some_and(str::is_empty) {
        return Err(Fail::Usage("--idempotency-key must not be empty".into()));
    }
    Ok(CallOptions {
        idempotency_key: key.map(str::to_string),
        max_response_bytes: m
            .try_get_one::<u64>("max-response-bytes")
            .ok()
            .flatten()
            .copied(),
        verify: flag(m, "verify"),
        max_pages: m
            .try_get_one::<u32>("max-pages")
            .ok()
            .flatten()
            .map(|n| *n as usize),
        ..CallOptions::default()
    })
}

/// The `--flag.member value` tokens, applied to the arguments.
fn apply_dotted(
    ctx: &mut Ctx<'_>,
    flags: &[CliFlag],
    schema: &Value,
    args: &mut serde_json::Map<String, Value>,
) -> Result<(), Fail> {
    let found = std::mem::take(&mut ctx.dotted);
    if found.is_empty() {
        return Ok(());
    }
    dotted::apply(ctx, flags, schema, found, args)
}

async fn run_leaf<D, F>(
    ctx: &mut Ctx<'_>,
    tree: &Tree,
    names: &[String],
    m: &ArgMatches,
    make: &F,
) -> Result<u8, Fail>
where
    D: Dispatch,
    F: Fn(ClientOptions) -> Result<D, ConfigError>,
{
    let Some(leaf) = tree.leaves.get(names) else {
        return Err(Fail::Internal(format!(
            "the command `{}` has no table entry",
            names.join(" ")
        )));
    };
    let spec = ctx.spec;
    let conn = connect(ctx, m, None, make)?;
    let client = &conn.client;
    if let Some(store) = &conn.sessions {
        login::refresh_due(client, store).await;
    }
    let plan = match *leaf {
        Leaf::Op(i) => {
            let op = &spec.ops[i];
            let Some(desc) = client.operations().iter().find(|d| d.id == op.id) else {
                return Err(Fail::Internal(format!(
                    "the SDK has no operation {}; regenerate the CLI with the SDK",
                    op.id
                )));
            };
            let mut args = values::collect(ctx, &op.flags, m)?;
            apply_dotted(ctx, &op.flags, &op.schema, &mut args)?;
            if let (Some(arg), Some(raw)) = (&op.body_arg, text(m, "body")) {
                let encoding = desc.body.as_ref().map(|b| b.encoding);
                let v = values::body_value(ctx, raw, encoding)?;
                args.insert(arg.clone(), v);
            }
            Plan {
                target: Target::Op(&op.id),
                tier: desc.agent.safety,
                args: Value::Object(args),
                opts: call_options(m)?,
                dry_run: flag(m, "dry-run"),
                yes: flag(m, "yes"),
                understand: flag(m, "i-understand"),
                all: flag(m, "all"),
                shown_once: desc.agent.shown_once,
                once_fields: desc.agent.sensitive_response_fields.clone(),
            }
        }
        Leaf::Macro(i) => {
            let mac = &spec.macros[i];
            let Some(desc) = client.macros().iter().find(|d| d.name == mac.name) else {
                return Err(Fail::Internal(format!(
                    "the SDK has no macro {}; regenerate the CLI with the SDK",
                    mac.name
                )));
            };
            let mut args = values::collect(ctx, &mac.flags, m)?;
            apply_dotted(ctx, &mac.flags, &mac.schema, &mut args)?;
            Plan {
                target: Target::Macro(&mac.name),
                tier: desc.safety,
                args: Value::Object(args),
                opts: call_options(m)?,
                dry_run: flag(m, "dry-run"),
                yes: flag(m, "yes"),
                understand: flag(m, "i-understand"),
                all: false,
                shown_once: desc.shown_once,
                once_fields: desc.sensitive_response_fields.clone(),
            }
        }
    };
    Ok(execute(ctx, client, plan).await)
}

async fn auth_status<D, F>(ctx: &mut Ctx<'_>, m: &ArgMatches, make: &F) -> Result<u8, Fail>
where
    D: Dispatch,
    F: Fn(ClientOptions) -> Result<D, ConfigError>,
{
    let conn = connect(ctx, m, text(m, "account"), make)?;
    let api = conn.client.core().api();
    let known: Vec<String> =
        config::slots(&ctx.spec.env_prefix, &ctx.spec.credential_env, &api.auth)
            .into_iter()
            .map(|s| s.profile_key)
            .collect();
    let mut known = known;
    known.extend(crate::session::known_keys(&api.auth));
    let unknown = conn.settings.unknown_keys(&known);
    let default_url = api.servers.first().map(String::as_str);
    let (auth, _, _) = config::credentials(
        &ctx.spec.env_prefix,
        &ctx.spec.credential_env,
        &api.auth,
        &ctx.env,
        &conn.settings,
    );
    let sessions =
        login::listed(ctx, &conn.settings, text(m, "account"), api, &auth).map_err(Fail::Usage)?;
    Ok(builtin::auth_status(
        ctx,
        &conn.settings,
        &conn.found,
        default_url,
        &unknown,
        &sessions,
    ))
}

async fn auth_command<D, F>(
    ctx: &mut Ctx<'_>,
    sub: Option<&str>,
    m: &ArgMatches,
    make: &F,
) -> Result<u8, Fail>
where
    D: Dispatch,
    F: Fn(ClientOptions) -> Result<D, ConfigError>,
{
    match sub {
        Some("login") => {
            let (settings, options) = base(ctx, m)?;
            login::login(ctx, m, settings, options, make).await
        }
        Some("switch") => {
            let (settings, options) = base(ctx, m)?;
            login::switch(ctx, m, settings, options, make).await
        }
        Some("logout") => {
            let (settings, options) = base(ctx, m)?;
            login::logout(ctx, m, settings, options, make).await
        }
        _ => auth_status(ctx, m, make).await,
    }
}

pub(crate) async fn drive<D, F>(ctx: &mut Ctx<'_>, argv: Vec<String>, make: &F) -> u8
where
    D: Dispatch,
    F: Fn(ClientOptions) -> Result<D, ConfigError>,
{
    let spec = ctx.spec;
    if let Err(e) = check::validate(spec) {
        return internal_error(ctx, &format!("the command table is inconsistent: {e}"));
    }
    let (argv, dotted) = match dotted::split(spec, argv) {
        Ok(split) => split,
        Err(message) => return usage_error(ctx, &[], &message),
    };
    let tree = tree::build(spec, &dotted::flag_names(&dotted));
    ctx.dotted = dotted;
    let matches = match tree.command.clone().try_get_matches_from(argv) {
        Ok(m) => m,
        Err(e) => {
            use clap::error::ErrorKind;
            let rendered = e.render().to_string();
            return match e.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    ctx.out(rendered.trim_end());
                    exit::OK
                }
                _ => {
                    let message = rendered
                        .trim_end()
                        .trim_start_matches("error: ")
                        .to_string();
                    usage_error(ctx, &[], &message)
                }
            };
        }
    };
    ctx.json = matches.get_flag("json");
    if matches.get_flag("no-color") {
        ctx.disable_color();
    }
    let (names, leaf_m) = descend(&matches);
    let builtin_command = matches!(
        names.first().map(String::as_str),
        Some("schema" | "operations" | "explain-error" | "auth")
    );
    if builtin_command && let Some(d) = ctx.dotted.first() {
        let message = format!(
            "unexpected argument '--{}.{}' found",
            d.flag,
            d.path.join(".")
        );
        return usage_error(ctx, &names, &message);
    }
    let result = match names.first().map(String::as_str) {
        Some("schema") => builtin::schema(ctx, &tree.leaves, leaf_m),
        Some("operations") => Ok(builtin::operations(ctx)),
        Some("explain-error") => explain::run(ctx, leaf_m),
        Some("auth") => auth_command(ctx, names.get(1).map(String::as_str), leaf_m, make).await,
        _ => run_leaf(ctx, &tree, &names, leaf_m, make).await,
    };
    match result {
        Ok(code) => code,
        Err(Fail::Usage(message)) => usage_error(ctx, &names, &message),
        Err(Fail::Internal(message)) => internal_error(ctx, &message),
    }
}
