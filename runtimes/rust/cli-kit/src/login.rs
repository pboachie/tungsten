// SPDX-License-Identifier: Apache-2.0
//! `auth login`, `auth switch` and `auth logout`, and the stored sessions a
//! command uses.
//!
//! `auth login` runs the runtime's managed authorization-code flow (S256
//! PKCE, a single-use attempt bound to this process) with a loopback
//! redirect: it listens on `127.0.0.1` (or the loopback address of the
//! configured `<PREFIX>_<SCHEME>_REDIRECT_URI`), prints the authorization
//! URL, opens the browser unless `--no-browser` is given, and waits for the
//! one callback until the attempt expires (10 minutes) or `--deadline`
//! passes. It never runs in CI (`CI` set) or without a terminal, and no
//! other command starts a login: a command without a stored session uses
//! only the credentials of the environment and profile.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use clap::ArgMatches;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tungsten_runtime::{
    AuthConfig, AuthorizationUrlParams, Category, ClientOptions, ConfigError, Credential,
    Diagnostic, Dispatch, ExchangeCodeParams, IssuerDefense, Retryable, Status, Trace,
    generate_pkce,
};

use crate::config::{self, Settings};
use crate::ctx::{Ctx, Fail};
use crate::exit;
use crate::render::json_doc;
use crate::session::{
    self, CodeScheme, DEFAULT_ACCOUNT, Entry, Index, Partition, SessionStore, StoreKind,
};

/// The bytes of a callback request head the listener reads.
const MAX_REQUEST_BYTES: usize = 16_384;

/// An `AUTH_FAILED` envelope for a session failure outside the SDK (exit 7).
pub(crate) fn auth_fail(ctx: &mut Ctx<'_>, operation: &str, remediation: &str) -> u8 {
    let d = Diagnostic {
        status: Status::Error,
        category: Category::AuthFailed,
        operation: operation.to_string(),
        http_status: None,
        code: None,
        failed_parameter: None,
        received_value: Value::Null,
        expected: None,
        remediation: remediation.to_string(),
        retryable: Retryable::Never,
        retry_after_ms: None,
        next_action: None,
        request_id: None,
        trace: Trace { attempts: 0 },
    };
    let line = format!("{}: {remediation}", ctx.err_style.red("error"));
    ctx.err(&line);
    if ctx.json {
        let doc = serde_json::to_value(&d).unwrap_or(Value::Null);
        let out = json_doc(&doc, ctx.pretty);
        ctx.out(&out);
    }
    exit::AUTH_MISSING
}

fn text<'a>(m: &'a ArgMatches, name: &str) -> Option<&'a str> {
    m.try_get_one::<String>(name)
        .ok()
        .flatten()
        .map(String::as_str)
}

fn flag(m: &ArgMatches, name: &str) -> bool {
    m.try_get_one::<bool>(name)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

/// The base URL origin a session is bound to.
fn base_origin(settings: &Settings, servers: &[String]) -> Option<String> {
    settings
        .base_url
        .as_ref()
        .map(|(u, _)| u.as_str())
        .or(servers.first().map(String::as_str))
        .map(session::origin)
}

fn client_parts<'c>(auth: &'c AuthConfig, scheme: &str) -> Option<&'c BTreeMap<String, String>> {
    match auth.get(scheme) {
        Some(Credential::Parts(p)) => Some(p),
        _ => None,
    }
}

fn client_id(auth: &AuthConfig, scheme: &str) -> Option<String> {
    client_parts(auth, scheme)?
        .get("clientId")
        .filter(|v| !v.is_empty())
        .cloned()
}

/// The `authorizationCode` schemes a session command acts on: the one named
/// by `--scheme`, or every one.
fn chosen<'s>(
    schemes: &'s [CodeScheme],
    named: Option<&str>,
) -> Result<Vec<&'s CodeScheme>, String> {
    if schemes.is_empty() {
        return Err(
            "this API declares no OAuth2 authorizationCode scheme, so there is nothing to log in to; use the credentials `auth status` lists".to_string(),
        );
    }
    match named {
        Some(n) => schemes
            .iter()
            .find(|s| s.name == n)
            .map(|s| vec![s])
            .ok_or_else(|| {
                let names: Vec<&str> = schemes.iter().map(|s| s.name.as_str()).collect();
                format!("--scheme {n}: choose one of {}", names.join(", "))
            }),
        None => Ok(schemes.iter().collect()),
    }
}

/// Attach the stored sessions of the effective accounts to a command's
/// client options: each bound scheme authenticates with its session's
/// tokens through a [`SessionStore`]. Problems are warnings; the command
/// then runs with the other credentials it has.
pub(crate) fn attach(
    ctx: &mut Ctx<'_>,
    settings: &Settings,
    account_flag: Option<&str>,
    api: &tungsten_runtime::ApiDescriptor,
    options: &mut ClientOptions,
) -> Result<(), Fail> {
    let schemes = session::code_schemes(&api.auth);
    if schemes.is_empty() {
        return Ok(());
    }
    let ss = session::session_settings(ctx.spec, &ctx.env, settings, account_flag, None)
        .map_err(Fail::Usage)?;
    if ss.store.0 == StoreKind::Ephemeral {
        return Ok(());
    }
    let Some(dir) = session::state_dir(ctx.spec, &ctx.env) else {
        return Ok(());
    };
    let index = match Index::load(&dir) {
        Ok(i) => i,
        Err(e) => {
            ctx.err(&format!("warning: stored sessions are not used: {e}"));
            return Ok(());
        }
    };
    if index.entries.is_empty() {
        return Ok(());
    }
    let Some(origin) = base_origin(settings, &api.servers) else {
        return Ok(());
    };
    let store = Arc::new(SessionStore::new(Some(dir.clone())));
    let mut any = false;
    let named = ss.account.as_ref().map(|(a, _)| a.as_str());
    for scheme in &schemes {
        let Some(client) = client_id(&options.auth, &scheme.name) else {
            continue;
        };
        let account =
            session::effective_account(named, &index, &settings.profile, scheme, &client, &origin);
        let wanted = Partition {
            scheme: scheme.name.clone(),
            account: account.clone(),
            issuer: scheme.token_url.clone(),
            client_id: client,
            origin: origin.clone(),
            scopes: Vec::new(),
        };
        let Some(entry) = index.find(&wanted) else {
            continue;
        };
        let bin = ctx.spec.bin.clone();
        let relogin = format!("`{bin} auth login --account {account}`");
        let vault = match session::vault(entry.store, ctx.spec, Some(&dir)) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(e) => {
                ctx.err(&format!(
                    "warning: the session of account `{account}` ({}) is not used: {e}",
                    scheme.name
                ));
                continue;
            }
        };
        match store.bind(entry.partition.clone(), vault, true) {
            Ok(true) => {
                any = true;
                if let Some(Credential::Parts(parts)) = options.auth.get_mut(&scheme.name) {
                    parts.insert("flow".into(), "authorizationCode".into());
                }
            }
            Ok(false) => ctx.err(&format!(
                "warning: the {} store holds no tokens for account `{account}` ({}): it may be locked or the session was removed; run {relogin}",
                entry.store.name(),
                scheme.name
            )),
            Err(e) => ctx.err(&format!(
                "warning: the session of account `{account}` ({}) cannot be read: {e}; run {relogin}",
                scheme.name
            )),
        }
    }
    if any {
        options.token_store = Some(store);
    }
    Ok(())
}

/// Why login refuses to run here, if it does.
fn noninteractive(ctx: &Ctx<'_>) -> Option<String> {
    let bin = &ctx.spec.bin;
    if ctx
        .env
        .get("CI")
        .is_some_and(|v| !v.is_empty() && v != "0" && v != "false")
    {
        return Some(format!(
            "`{bin} auth login` is interactive and does not run in CI (CI is set); give the job its credentials through the variables `{bin} auth status` lists"
        ));
    }
    if !ctx.stdin_tty && !ctx.stderr_tty {
        return Some(format!(
            "`{bin} auth login` needs a terminal; without one, supply credentials through the variables `{bin} auth status` lists"
        ));
    }
    None
}

/// A loopback redirect: host as written, port (0 for any) and path.
fn loopback(redirect: &str) -> Result<(String, u16, String), String> {
    let bad = || {
        format!(
            "redirect URI `{redirect}`: login needs a loopback http redirect such as http://127.0.0.1:8765/callback (RFC 8252)"
        )
    };
    let rest = redirect.strip_prefix("http://").ok_or_else(bad)?;
    if rest.contains(['?', '#', '@']) {
        return Err(bad());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or_else(bad)?;
        (format!("[{h}]"), after.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
        return Err(bad());
    }
    let port = match port {
        Some(p) => p.parse::<u16>().map_err(|_| bad())?,
        None => 0,
    };
    Ok((host, port, path.to_string()))
}

fn open_browser(url: &str) -> bool {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("rundll32", vec!["url.dll,FileProtocolHandler", url])
    } else {
        ("xdg-open", vec![url])
    };
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Wait for the request on `path`; other requests (a favicon) get 404.
async fn accept_callback(listener: &TcpListener, path: &str) -> (String, TcpStream) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            continue;
        };
        let mut head = Vec::new();
        let mut buf = [0u8; 2048];
        let complete = loop {
            match tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break false,
                Ok(Ok(n)) => {
                    head.extend_from_slice(&buf[..n]);
                    if head.windows(4).any(|w| w == b"\r\n\r\n") {
                        break true;
                    }
                    if head.len() > MAX_REQUEST_BYTES {
                        break false;
                    }
                }
            }
        };
        let line = String::from_utf8_lossy(&head);
        let target = line
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .and_then(|l| l.split(' ').next())
            .map(str::to_string);
        match target {
            Some(t) if complete && t.split('?').next() == Some(path) => return (t, stream),
            _ => {
                let _ = respond(&mut stream, 404, "Not found.").await;
            }
        }
    }
}

async fn respond(stream: &mut TcpStream, status: u16, message: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!("<!doctype html><meta charset=utf-8><title>Sign-in</title><p>{message}</p>");
    let reply = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await?;
    stream.shutdown().await
}

/// What `auth login` needs before the browser opens.
struct Prepared {
    scheme: CodeScheme,
    partition: Partition,
    store: StoreKind,
    vault: Arc<dyn session::Vault>,
    dir: std::path::PathBuf,
    auth: AuthConfig,
    index: Index,
}

fn prepare(
    ctx: &mut Ctx<'_>,
    settings: &Settings,
    m: &ArgMatches,
    api: &tungsten_runtime::ApiDescriptor,
) -> Result<Prepared, String> {
    let spec = ctx.spec;
    let ss = session::session_settings(
        spec,
        &ctx.env,
        settings,
        text(m, "account"),
        text(m, "store"),
    )?;
    if ss.store.0 == StoreKind::Ephemeral {
        return Err("the ephemeral session store keeps no login; choose `--store keychain` or, explicitly, `--store file`".to_string());
    }
    let schemes = session::code_schemes(&api.auth);
    let picked = chosen(&schemes, text(m, "scheme"))?;
    let scheme = match picked.as_slice() {
        [one] => (*one).clone(),
        _ => {
            let names: Vec<&str> = picked.iter().map(|s| s.name.as_str()).collect();
            return Err(format!(
                "this API has several authorizationCode schemes; choose one with --scheme ({})",
                names.join(", ")
            ));
        }
    };
    let (auth, found, warnings) = config::credentials(
        &spec.env_prefix,
        &spec.credential_env,
        &api.auth,
        &ctx.env,
        settings,
    );
    for w in warnings {
        ctx.err(&w);
    }
    let client = client_id(&auth, &scheme.name).ok_or_else(|| {
        let slot = found
            .iter()
            .find(|f| f.slot.scheme == scheme.name && f.slot.part.as_deref() == Some("clientId"));
        match slot {
            Some(f) => format!(
                "login needs the OAuth client id of {}: set {} or the profile key {}",
                scheme.name, f.slot.env, f.slot.profile_key
            ),
            None => format!("login needs the OAuth client id of {}", scheme.name),
        }
    })?;
    let origin = base_origin(settings, &api.servers).ok_or_else(|| {
        "login needs a base URL: pass --base-url or set it in the profile".to_string()
    })?;
    let mut scopes: Vec<String> = match m.get_many::<String>("scope") {
        Some(v) => v.cloned().collect(),
        None => scheme.scopes.clone(),
    };
    scopes.sort();
    scopes.dedup();
    let dir = session::state_dir(spec, &ctx.env).ok_or_else(|| {
        "login needs a home directory for its session index (HOME, XDG_CONFIG_HOME, APPDATA or USERPROFILE)".to_string()
    })?;
    let index = Index::load(&dir)?;
    let account = session::effective_account(
        ss.account.as_ref().map(|(a, _)| a.as_str()),
        &index,
        &settings.profile,
        &scheme,
        &client,
        &origin,
    );
    let vault = session::vault(ss.store.0, spec, Some(&dir))?
        .ok_or_else(|| "the ephemeral session store keeps no login".to_string())?;
    session::probe(vault.as_ref()).map_err(|e| {
        let advice = if ss.store.0 == StoreKind::Keychain {
            "; unlock the keychain and retry, or choose the owner-only file store explicitly with `--store file` (or session_store = \"file\" in the profile)"
        } else {
            ""
        };
        format!("the {} session store cannot keep a login: {e}{advice}", ss.store.0.name())
    })?;
    Ok(Prepared {
        partition: Partition {
            scheme: scheme.name.clone(),
            account,
            issuer: scheme.token_url.clone(),
            client_id: client,
            origin,
            scopes,
        },
        scheme,
        store: ss.store.0,
        vault,
        dir,
        auth,
        index,
    })
}

/// `auth login`.
pub(crate) async fn login<D, F>(
    ctx: &mut Ctx<'_>,
    m: &ArgMatches,
    settings: Settings,
    base: ClientOptions,
    make: &F,
) -> Result<u8, Fail>
where
    D: Dispatch,
    F: Fn(ClientOptions) -> Result<D, ConfigError>,
{
    const OP: &str = "auth login";
    if let Some(why) = noninteractive(ctx) {
        return Ok(auth_fail(ctx, OP, &why));
    }
    let probe = make(base.clone()).map_err(|e| Fail::Usage(e.message))?;
    let api = probe.core().api().clone();
    let prepared = match prepare(ctx, &settings, m, &api) {
        Ok(p) => p,
        Err(e) => return Ok(auth_fail(ctx, OP, &e)),
    };
    let Prepared {
        scheme,
        partition,
        store,
        vault,
        dir,
        mut auth,
        mut index,
    } = prepared;
    let configured =
        session::scheme_setting(ctx.spec, &ctx.env, &settings, &scheme.name, "REDIRECT_URI");
    let (host, port, path) =
        match loopback(configured.as_deref().unwrap_or("http://127.0.0.1/callback")) {
            Ok(r) => r,
            Err(e) => return Ok(auth_fail(ctx, OP, &e)),
        };
    let bind_host = if host == "[::1]" {
        "::1"
    } else {
        host.as_str()
    };
    let listener = match TcpListener::bind((bind_host, port)).await {
        Ok(l) => l,
        Err(e) => {
            return Ok(auth_fail(
                ctx,
                OP,
                &format!("cannot listen on {host}:{port} for the sign-in callback: {e}"),
            ));
        }
    };
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let redirect = format!("http://{host}:{bound}{path}");
    if let Some(Credential::Parts(parts)) = auth.get_mut(&scheme.name) {
        parts.insert("flow".into(), "authorizationCode".into());
        parts.insert("redirectUri".into(), redirect.clone());
    }
    let sessions = Arc::new(SessionStore::new(Some(dir.clone())));
    sessions
        .bind(partition.clone(), Arc::clone(&vault), false)
        .map_err(Fail::Internal)?;
    let mut options = base;
    options.auth = auth;
    options.token_store = Some(sessions);
    let deadline = options.deadline;
    let client = make(options).map_err(|e| Fail::Usage(e.message))?;
    let flow = client.core().oauth(&scheme.name);
    let Some(binding) = generate_pkce().map(|p| p.verifier) else {
        return Ok(auth_fail(ctx, OP, "the system has no source of randomness"));
    };
    let issuer = session::scheme_setting(ctx.spec, &ctx.env, &settings, &scheme.name, "ISSUER");
    let params = AuthorizationUrlParams {
        session_binding: binding.clone(),
        issuer_defense: Some(match issuer {
            Some(issuer) => IssuerDefense::IssuerResponse { issuer },
            None => IssuerDefense::DistinctRedirect {
                qualification: "loopback redirect owned by this login process".into(),
            },
        }),
        s256_supported: true,
        redirect_uri: Some(redirect.clone()),
        scopes: (!partition.scopes.is_empty()).then(|| partition.scopes.clone()),
        account_context: Some(partition.account.clone()),
        ..AuthorizationUrlParams::default()
    };
    let attempt = match flow.authorization_url(&params) {
        Ok(a) => a,
        Err(e) => return Ok(auth_fail(ctx, OP, &e.diagnostic.remediation)),
    };
    let mut wait = Duration::from_millis(attempt.expires_in_ms);
    if let Some(d) = deadline {
        wait = wait.min(d);
    }
    let bin = ctx.spec.bin.clone();
    ctx.err(&format!(
        "Sign in as account `{}` ({}) in the browser:\n\n  {}\n\nWaiting up to {} s for the browser to return to {redirect}; press Ctrl-C to cancel.",
        partition.account,
        scheme.name,
        attempt.url,
        wait.as_secs()
    ));
    if !flag(m, "no-browser") && !open_browser(&attempt.url) {
        ctx.err("The browser could not be opened; open the URL above yourself.");
    }
    let (target, mut stream) = match tokio::time::timeout(wait, accept_callback(&listener, &path))
        .await
    {
        Ok(got) => got,
        Err(_) => {
            let _ = flow.cancel_authorization(&attempt.attempt_handle, &binding);
            return Ok(auth_fail(
                ctx,
                OP,
                &format!(
                    "sign-in did not complete within {} s and was canceled; run `{bin} auth login` again",
                    wait.as_secs()
                ),
            ));
        }
    };
    drop(listener);
    let callback_url = format!("http://{host}:{bound}{target}");
    let exchanged = flow
        .exchange_code(&ExchangeCodeParams {
            attempt_handle: attempt.attempt_handle.clone(),
            callback_url,
            session_binding: binding,
        })
        .await;
    let info = match exchanged {
        Ok(info) => {
            let _ = respond(
                &mut stream,
                200,
                "Signed in. You can close this window and return to the terminal.",
            )
            .await;
            info
        }
        Err(e) => {
            let _ = respond(
                &mut stream,
                400,
                "Sign-in did not complete. Return to the terminal for details.",
            )
            .await;
            return Ok(auth_fail(ctx, OP, &e.diagnostic.remediation));
        }
    };
    index.upsert(Entry {
        partition: partition.clone(),
        store,
        created_at: session::now_ms(),
        expires_at: info.expires_at,
        refreshable: info.refreshable,
        granted_scope: info.scope.clone(),
    });
    index.set_active(&settings.profile, &scheme.name, &partition.account);
    if let Err(e) = index.save(&dir) {
        let _ = vault.remove(&partition.id());
        return Ok(auth_fail(
            ctx,
            OP,
            &format!(
                "signed in, but the session could not be recorded, so its tokens were discarded: {e}"
            ),
        ));
    }
    if ctx.json {
        let doc = json!({
            "scheme": scheme.name,
            "account": partition.account,
            "origin": partition.origin,
            "store": store.name(),
            "expires_at": info.expires_at,
            "refreshable": info.refreshable,
            "granted_scope": info.scope,
        });
        let out = json_doc(&doc, ctx.pretty);
        ctx.out(&out);
    } else {
        ctx.out(&format!(
            "Signed in as account `{}` ({}) for {}; tokens are kept in the {} store.",
            partition.account,
            scheme.name,
            partition.origin,
            store.name()
        ));
    }
    Ok(exit::OK)
}

/// The stored sessions of this run's profile, each with whether a command
/// would use it.
pub(crate) fn listed(
    ctx: &Ctx<'_>,
    settings: &Settings,
    account_flag: Option<&str>,
    api: &tungsten_runtime::ApiDescriptor,
    auth: &AuthConfig,
) -> Result<(Vec<(Entry, bool)>, session::SessionSettings), String> {
    let ss = session::session_settings(ctx.spec, &ctx.env, settings, account_flag, None)?;
    let Some(dir) = session::state_dir(ctx.spec, &ctx.env) else {
        return Ok((Vec::new(), ss));
    };
    let index = Index::load(&dir)?;
    let origin = base_origin(settings, &api.servers);
    let named = ss.account.as_ref().map(|(a, _)| a.as_str());
    let mut selected: Vec<Partition> = Vec::new();
    if ss.store.0 != StoreKind::Ephemeral
        && let Some(origin) = &origin
    {
        for scheme in session::code_schemes(&api.auth) {
            let Some(client) = client_id(auth, &scheme.name) else {
                continue;
            };
            let account = session::effective_account(
                named,
                &index,
                &settings.profile,
                &scheme,
                &client,
                origin,
            );
            let wanted = Partition {
                scheme: scheme.name.clone(),
                account,
                issuer: scheme.token_url.clone(),
                client_id: client,
                origin: origin.clone(),
                scopes: Vec::new(),
            };
            if let Some(e) = index.find(&wanted) {
                selected.push(e.partition.clone());
            }
        }
    }
    let rows = index
        .entries
        .iter()
        .map(|e| (e.clone(), selected.contains(&e.partition)))
        .collect();
    Ok((rows, ss))
}

/// `auth status`: the session part of the JSON document.
pub(crate) fn status_json(rows: &[(Entry, bool)], ss: &session::SessionSettings) -> Value {
    let now = session::now_ms();
    json!({
        "account": ss.account.as_ref().map(|(a, s)| json!({ "value": a, "source": s.name() })),
        "session_store": { "value": ss.store.0.name(), "source": ss.store.1.name() },
        "sessions": rows.iter().map(|(e, sel)| e.status_json(*sel, now)).collect::<Vec<_>>(),
    })
}

/// `auth status`: the session lines of the human output.
pub(crate) fn status_lines(rows: &[(Entry, bool)], ss: &session::SessionSettings) -> Vec<String> {
    let now = session::now_ms();
    let mut lines = vec![format!(
        "session store {} ({})",
        ss.store.0.name(),
        ss.store.1.name()
    )];
    if let Some((a, s)) = &ss.account {
        lines.push(format!("account    {a} ({})", s.name()));
    }
    for (e, selected) in rows {
        let p = &e.partition;
        let expiry = match e.expires_at {
            Some(t) if t <= now => "expired".to_string(),
            Some(t) => format!("expires in {} min", (t - now) / 60_000),
            None => "no expiry given".to_string(),
        };
        let refresh = if e.refreshable { ", refreshable" } else { "" };
        let mark = if *selected { " (selected)" } else { "" };
        lines.push(format!(
            "session    {} account {}{mark}: {} store, {}, {expiry}{refresh}",
            p.scheme,
            p.account,
            e.store.name(),
            p.origin
        ));
    }
    lines
}

/// `auth switch <ACCOUNT>`: select a stored account for this profile.
pub(crate) async fn switch<D, F>(
    ctx: &mut Ctx<'_>,
    m: &ArgMatches,
    settings: Settings,
    base: ClientOptions,
    make: &F,
) -> Result<u8, Fail>
where
    D: Dispatch,
    F: Fn(ClientOptions) -> Result<D, ConfigError>,
{
    const OP: &str = "auth switch";
    let account = text(m, "account-name")
        .unwrap_or(DEFAULT_ACCOUNT)
        .to_string();
    session::check_account(&account).map_err(Fail::Usage)?;
    let probe = make(base).map_err(|e| Fail::Usage(e.message))?;
    let api = probe.core().api().clone();
    let schemes = session::code_schemes(&api.auth);
    let picked = chosen(&schemes, text(m, "scheme")).map_err(Fail::Usage)?;
    let Some(dir) = session::state_dir(ctx.spec, &ctx.env) else {
        return Ok(auth_fail(
            ctx,
            OP,
            "there is no home directory, so no session is stored",
        ));
    };
    let mut index = Index::load(&dir).map_err(Fail::Usage)?;
    let origin = base_origin(&settings, &api.servers);
    let mut switched = Vec::new();
    for scheme in picked {
        let has = index.entries.iter().any(|e| {
            e.partition.scheme == scheme.name
                && e.partition.account == account
                && Some(&e.partition.origin) == origin.as_ref()
        });
        if has {
            index.set_active(&settings.profile, &scheme.name, &account);
            switched.push(scheme.name.clone());
        }
    }
    if switched.is_empty() {
        let bin = ctx.spec.bin.clone();
        return Ok(auth_fail(
            ctx,
            OP,
            &format!(
                "no session of account `{account}` is stored for {}; run `{bin} auth login --account {account}` first",
                origin.as_deref().unwrap_or("this base URL")
            ),
        ));
    }
    index.save(&dir).map_err(Fail::Usage)?;
    if ctx.json {
        let doc = json!({ "profile": settings.profile, "account": account, "schemes": switched });
        let out = json_doc(&doc, ctx.pretty);
        ctx.out(&out);
    } else {
        ctx.out(&format!(
            "Profile `{}` now uses account `{account}` for {}.",
            settings.profile,
            switched.join(", ")
        ));
    }
    Ok(exit::OK)
}

/// `auth logout`: remove the selected account's stored sessions (or every
/// stored session with `--all`). Tokens are not revoked at the provider.
pub(crate) async fn logout<D, F>(
    ctx: &mut Ctx<'_>,
    m: &ArgMatches,
    settings: Settings,
    base: ClientOptions,
    make: &F,
) -> Result<u8, Fail>
where
    D: Dispatch,
    F: Fn(ClientOptions) -> Result<D, ConfigError>,
{
    const OP: &str = "auth logout";
    let probe = make(base).map_err(|e| Fail::Usage(e.message))?;
    let api = probe.core().api().clone();
    let ss = session::session_settings(ctx.spec, &ctx.env, &settings, text(m, "account"), None)
        .map_err(Fail::Usage)?;
    let Some(dir) = session::state_dir(ctx.spec, &ctx.env) else {
        return Ok(auth_fail(
            ctx,
            OP,
            "there is no home directory, so no session is stored",
        ));
    };
    let mut index = Index::load(&dir).map_err(Fail::Usage)?;
    let origin = base_origin(&settings, &api.servers);
    let targets: Vec<Entry> = if flag(m, "all") {
        index.entries.clone()
    } else {
        let schemes = session::code_schemes(&api.auth);
        let picked = chosen(&schemes, text(m, "scheme")).map_err(Fail::Usage)?;
        let named = ss.account.as_ref().map(|(a, _)| a.as_str());
        let mut out = Vec::new();
        for scheme in picked {
            let account = named
                .map(str::to_string)
                .or_else(|| {
                    index
                        .active_account(&settings.profile, &scheme.name)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| {
                    let accounts: Vec<&str> = index
                        .entries
                        .iter()
                        .filter(|e| {
                            e.partition.scheme == scheme.name
                                && Some(&e.partition.origin) == origin.as_ref()
                        })
                        .map(|e| e.partition.account.as_str())
                        .collect();
                    match accounts.as_slice() {
                        [only] => (*only).to_string(),
                        _ => DEFAULT_ACCOUNT.to_string(),
                    }
                });
            out.extend(
                index
                    .entries
                    .iter()
                    .filter(|e| {
                        e.partition.scheme == scheme.name
                            && e.partition.account == account
                            && Some(&e.partition.origin) == origin.as_ref()
                    })
                    .cloned(),
            );
        }
        out
    };
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for entry in targets {
        let p = &entry.partition;
        let result = session::vault(entry.store, ctx.spec, Some(&dir))
            .and_then(|v| v.map_or(Ok(()), |v| v.remove(&p.id())));
        let row = json!({
            "scheme": p.scheme,
            "account": p.account,
            "origin": p.origin,
            "store": entry.store.name(),
        });
        match result {
            Ok(()) => {
                index.remove(p);
                index.clear_active(&p.scheme, &p.account);
                removed.push(row);
            }
            Err(e) => failed.push((row, e)),
        }
    }
    index.save(&dir).map_err(Fail::Usage)?;
    let revocation = "not_performed";
    if ctx.json {
        let doc = json!({
            "removed": removed,
            "failed": failed.iter().map(|(r, e)| {
                let mut r = r.clone();
                if let Value::Object(map) = &mut r {
                    map.insert("error".into(), json!(e));
                }
                r
            }).collect::<Vec<_>>(),
            "remote_revocation": revocation,
        });
        let out = json_doc(&doc, ctx.pretty);
        ctx.out(&out);
    } else {
        for r in &removed {
            ctx.out(&format!(
                "Logged out account `{}` ({}) for {}; its tokens were removed from the {} store.",
                r["account"].as_str().unwrap_or(""),
                r["scheme"].as_str().unwrap_or(""),
                r["origin"].as_str().unwrap_or(""),
                r["store"].as_str().unwrap_or("")
            ));
        }
        if removed.is_empty() && failed.is_empty() {
            ctx.out("No stored session matched; nothing was removed.");
        }
        if !removed.is_empty() {
            ctx.err("note: the tokens were not revoked at the provider (the API declares no revocation endpoint); revoke them there if they may have been exposed.");
        }
    }
    if failed.is_empty() {
        return Ok(exit::OK);
    }
    let detail: Vec<String> = failed
        .iter()
        .map(|(r, e)| {
            format!(
                "account `{}` ({}): {e}",
                r["account"].as_str().unwrap_or(""),
                r["scheme"].as_str().unwrap_or("")
            )
        })
        .collect();
    if ctx.json {
        // The document above already reports the failures.
        return Ok(exit::AUTH_MISSING);
    }
    Ok(auth_fail(
        ctx,
        OP,
        &format!(
            "some sessions could not be removed and are still recorded: {}; unlock the store and run logout again",
            detail.join("; ")
        ),
    ))
}
