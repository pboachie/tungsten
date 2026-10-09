// SPDX-License-Identifier: AGPL-3.0-only
//! The request pipeline documented in the crate root: control endpoints,
//! injections, routing, programs, gates, auth, validation, idempotency and
//! generated success responses.

use std::sync::Arc;
use std::time::Duration;

use http_body_util::BodyExt;
use hyper::Request;
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue};
use serde_json::{Map, Value};
use tungsten_ir::{BodyEncoding, OperationStatus, Response, ResponseKind, StatusMatch};

use crate::RecordedCall;
use crate::auth::{self, AuthFailure};
use crate::generate::Generator;
use crate::model::{Model, OpEntry};
use crate::params::{self, RequestView, media_essence};
use crate::reply::{self, Reply, header_text, value_text};
use crate::route::{Routed, route};
use crate::state::{Action, Answer, Idempotent, Program, State};
use crate::validate::{Context, Validator, empty_object, json_eq};

/// Hold of `timeout` without a duration.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Bytes read and discarded past the body limit, so the `413` reaches the
/// client instead of a reset.
const DRAIN_LIMIT: usize = 4 * 1024 * 1024;
const CONTROL_PREFIX: &str = "/__tungsten/";
const CONTROL_PATHS: [(&str, &str); 3] = [
    ("/__tungsten/calls", "GET"),
    ("/__tungsten/reset", "POST"),
    ("/__tungsten/program", "POST"),
];
/// Response headers the mock never copies from the IR.
const RESERVED_HEADERS: [&str; 4] = [
    "content-type",
    "content-length",
    "transfer-encoding",
    "connection",
];

/// Response headers a program may not set: they frame the message, so a
/// wrong value corrupts the response instead of shaping it.
const FRAMING_HEADERS: [&str; 6] = [
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "trailer",
];

/// What the connection does with a request.
#[derive(Debug)]
pub(crate) enum Outcome {
    Reply(Reply),
    /// Close the connection without a response.
    Drop,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Injection {
    Timeout(u64),
    DropAfterWrite,
    Reset,
    Status {
        status: u16,
        retry_after: Option<String>,
        code: Option<String>,
        /// Process the request first (validation, auth, idempotent
        /// storage), then answer the injected status instead.
        apply: bool,
    },
}

impl Injection {
    fn parse(value: &str) -> Result<Injection, String> {
        let value = value.trim();
        let bad = || format!("unknown X-Tungsten-Inject value `{value}`");
        match value {
            "timeout" => return Ok(Injection::Timeout(DEFAULT_TIMEOUT_MS)),
            "drop-after-write" => return Ok(Injection::DropAfterWrite),
            "reset" => return Ok(Injection::Reset),
            _ => {}
        }
        if let Some(ms) = value.strip_prefix("timeout=") {
            return ms.trim().parse().map(Injection::Timeout).map_err(|_| bad());
        }
        let Some(rest) = value.strip_prefix("status=") else {
            return Err(bad());
        };
        let mut parts = rest.split(';').map(str::trim);
        let status: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|s| (200..=599).contains(s))
            .ok_or_else(bad)?;
        let (mut retry_after, mut code, mut apply) = (None, None, false);
        for part in parts {
            if part == "apply" {
                apply = true;
                continue;
            }
            match part.split_once('=') {
                Some(("retry-after", v)) if !v.is_empty() && HeaderValue::from_str(v).is_ok() => {
                    retry_after = Some(v.to_string());
                }
                Some(("code", v)) if !v.is_empty() => code = Some(v.to_string()),
                _ => return Err(bad()),
            }
        }
        Ok(Injection::Status {
            status,
            retry_after,
            code,
            apply,
        })
    }
}

/// What happens after the reply is computed and the call recorded.
#[derive(Debug, PartialEq)]
enum After {
    Send,
    Hold(u64),
    Drop,
}

#[derive(Debug)]
enum BodyError {
    TooLarge,
    Broken,
}

pub(crate) async fn handle(state: Arc<State>, req: Request<Incoming>) -> Outcome {
    let sequence = state.next_sequence();
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(n, v)| {
            (
                n.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let declared_length = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());

    if path.starts_with(CONTROL_PREFIX) {
        let body = read_body(req.into_body(), declared_length, state.model.max_body_bytes).await;
        return control(&state, &method, &path, body);
    }

    let injection = headers
        .iter()
        .find(|(n, _)| n == "x-tungsten-inject")
        .map(|(_, v)| (v.clone(), Injection::parse(v)));
    let mut sorted = headers.clone();
    sorted.sort();
    let mut call = RecordedCall {
        method: method.clone(),
        path: path.clone(),
        query: query.clone(),
        operation: None,
        headers: sorted,
        body: vec![],
        response_status: 0,
        injected: injection.as_ref().map(|(raw, _)| raw.trim().to_string()),
    };
    let model = &state.model;

    if let Some((_, Ok(Injection::Reset))) = &injection {
        call.operation = operation_id(model, &route(model, &method, &path, None));
        state.record(sequence, call);
        return Outcome::Drop;
    }

    let body = match read_body(req.into_body(), declared_length, model.max_body_bytes).await {
        Ok(body) => body,
        Err(BodyError::TooLarge) => {
            let routed = route(model, &method, &path, None);
            call.operation = operation_id(model, &routed);
            let reason = format!("request body exceeds {} bytes", model.max_body_bytes);
            let reply = reply::error(model, routed_ns(model, &routed), 413, None, &[], &reason)
                .with_header("connection", "close");
            call.response_status = reply.status;
            state.record(sequence, call);
            return Outcome::Reply(reply);
        }
        Err(BodyError::Broken) => {
            state.record(sequence, call);
            return Outcome::Drop;
        }
    };
    let routed = route(model, &method, &path, Some(&body));
    call.operation = operation_id(model, &routed);
    let view = RequestView::new(&method, &headers, &query, routed_params(&routed));
    let request = Exchange {
        path: &path,
        query: &query,
        body: &body,
        view: &view,
    };

    // A header injection wins and leaves programs queued; otherwise the
    // operation's next program either injects or answers.
    let mut applied = injection.map(|(_, parsed)| parsed);
    let program = match (&applied, &routed) {
        (None, Routed::Op { index, .. }) => state.take_program(model.ops[*index].id()),
        _ => None,
    };
    let answer = match program.map(|p| p.action) {
        Some(Action::Inject(injection)) => {
            call.injected = Some("program".into());
            applied = Some(Ok(injection));
            None
        }
        Some(Action::Answer(answer)) => Some(answer),
        None => None,
    };
    let (reply, after) = match applied {
        Some(Err(reason)) => (
            Reply::text(400, &reason).with_header("x-tungsten-reason", &header_text(&reason)),
            After::Send,
        ),
        Some(Ok(Injection::Status {
            status,
            retry_after,
            code,
            apply,
        })) => {
            if apply {
                // The effect happens (an idempotency key stores its
                // response); only the answer is replaced.
                let _ = process(&state, &routed, &request);
            }
            let mut reply = injected_status(model, &routed, status, code.as_deref());
            if let Some(seconds) = retry_after {
                reply = reply.with_header("retry-after", &seconds);
            }
            (reply, After::Send)
        }
        Some(Ok(Injection::Timeout(ms))) => (process(&state, &routed, &request), After::Hold(ms)),
        // A reset never gets here: it closed the connection before the body.
        Some(Ok(Injection::DropAfterWrite | Injection::Reset)) => {
            (process(&state, &routed, &request), After::Drop)
        }
        None => match (answer, &routed) {
            (Some(answer), Routed::Op { index, .. }) => {
                call.injected = Some("program".into());
                (programmed(model, &model.ops[*index], &answer), After::Send)
            }
            _ => (process(&state, &routed, &request), After::Send),
        },
    };
    call.body = body;
    call.response_status = if after == After::Drop {
        0
    } else {
        reply.status
    };
    state.record(sequence, call);
    match after {
        After::Send => Outcome::Reply(reply),
        After::Hold(ms) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Outcome::Reply(reply)
        }
        After::Drop => Outcome::Drop,
    }
}

/// Read the whole body within the limit. A body over the limit is drained
/// up to [`DRAIN_LIMIT`] more bytes before answering.
async fn read_body(
    mut body: Incoming,
    declared: Option<u64>,
    max: usize,
) -> Result<Vec<u8>, BodyError> {
    let drain_to = max.saturating_add(DRAIN_LIMIT);
    if declared.is_some_and(|n| n > drain_to as u64) {
        return Err(BodyError::TooLarge);
    }
    let mut out = Vec::new();
    let mut total = 0usize;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| BodyError::Broken)?;
        if let Ok(data) = frame.into_data() {
            total = total.saturating_add(data.len());
            if total > max {
                out = Vec::new();
                if total > drain_to {
                    break;
                }
                continue;
            }
            out.extend_from_slice(&data);
        }
    }
    if total > max {
        Err(BodyError::TooLarge)
    } else {
        Ok(out)
    }
}

fn operation_id(model: &Model, routed: &Routed) -> Option<String> {
    match routed {
        Routed::Op { index, .. } => Some(model.ops[*index].id().to_string()),
        _ => None,
    }
}

fn routed_ns(model: &Model, routed: &Routed) -> Option<usize> {
    match routed {
        Routed::Op { index, .. } => Some(model.ops[*index].ns),
        Routed::RpcUnknown { ns } => Some(*ns),
        Routed::NotAllowed(_) | Routed::NotFound => None,
    }
}

fn routed_params(routed: &Routed) -> Vec<(String, String)> {
    match routed {
        Routed::Op { params, .. } => params.clone(),
        _ => vec![],
    }
}

/// The request as the operation checks need it.
#[derive(Debug)]
struct Exchange<'a> {
    path: &'a str,
    query: &'a str,
    body: &'a [u8],
    view: &'a RequestView<'a>,
}

fn process(state: &State, routed: &Routed, request: &Exchange<'_>) -> Reply {
    let model = &state.model;
    match routed {
        Routed::NotFound => reply::error(
            model,
            None,
            404,
            None,
            &[],
            &format!(
                "no operation matches {} {}",
                request.view.method, request.path
            ),
        ),
        Routed::NotAllowed(methods) => reply::error(
            model,
            None,
            405,
            None,
            &[],
            &format!("{} is served only for {}", request.path, methods.join(", ")),
        )
        .with_header("allow", &methods.join(", ")),
        Routed::RpcUnknown { ns } => reply::error(
            model,
            Some(*ns),
            400,
            None,
            &[],
            "the body names no rpc method served at this path",
        ),
        Routed::Op { index, .. } => operation(state, &model.ops[*index], request),
    }
}

/// An error answer of a routed operation. A status the operation declares
/// without content is answered without a body (as the API does); otherwise,
/// or when an error `code` is asked for, the error rule's body is sent.
fn op_error(
    model: &Model,
    entry: &OpEntry,
    status: u16,
    code: Option<&str>,
    preferred: &[&str],
    reason: &str,
) -> Reply {
    let bare = code.is_none()
        && entry
            .op
            .responses
            .iter()
            .any(|r| r.status == StatusMatch::Exact(status) && r.content.is_empty());
    if bare {
        return Reply::empty(status).with_header("x-tungsten-reason", &header_text(reason));
    }
    reply::error(model, Some(entry.ns), status, code, preferred, reason)
}

fn operation(state: &State, entry: &OpEntry, request: &Exchange<'_>) -> Reply {
    let model = &state.model;
    if let OperationStatus::Gated { gate } = &entry.op.status
        && !model.gate_on(gate)
    {
        // The gate's route is not mounted while it is off (ZROtext's
        // conditional axum routes), so the answer is the framework's bare
        // status, without the API's error document: that is how clients
        // tell a disabled gate from the mounted route's own errors.
        let reason = format!("runtime gate {} is off", gate.env_var);
        return Reply::empty(gate.disabled_status)
            .with_header("x-tungsten-reason", &header_text(&reason));
    }
    match auth::check(model, &entry.op, request.view) {
        Ok(()) => {}
        Err(AuthFailure::Unauthenticated(reason)) => {
            return op_error(model, entry, 401, None, &[], &reason);
        }
        Err(AuthFailure::Forbidden(reason)) => {
            return op_error(model, entry, 403, None, &[], &reason);
        }
    }
    if let Err(reason) = params::check(model, &entry.op, request.view) {
        return op_error(model, entry, 400, None, &[], &reason);
    }
    if let Err(reason) = check_body(model, entry, request) {
        return op_error(model, entry, 400, None, &[], &reason);
    }
    let Some((header, key)) = entry
        .idempotency_header()
        .and_then(|h| request.view.header(h).map(|k| (h, k)))
    else {
        return success(model, entry, None);
    };
    let mut fingerprint = format!("{}?{}\n", request.path, request.query).into_bytes();
    fingerprint.extend_from_slice(request.body);
    match state.idempotent(entry.id(), &key, &fingerprint) {
        Idempotent::Replay(stored) => replayed(stored),
        Idempotent::Conflict => op_error(
            model,
            entry,
            409,
            None,
            &["idempotency_conflict", "conflict"],
            &format!("{header} was used with a different request"),
        ),
        Idempotent::Fresh => {
            let reply = success(model, entry, None);
            state.store(entry.id(), &key, fingerprint, reply.clone());
            reply
        }
    }
}

fn check_body(model: &Model, entry: &OpEntry, request: &Exchange<'_>) -> Result<(), String> {
    let Some(declared) = &entry.op.body else {
        return Ok(());
    };
    let content_type = request.view.header("content-type");
    let missing = || {
        if declared.required {
            Err("missing required request body".to_string())
        } else {
            Ok(())
        }
    };
    let Some(content_type) = content_type else {
        return if request.body.is_empty() {
            missing()
        } else {
            Err("missing Content-Type for the request body".into())
        };
    };
    let essence = media_essence(&content_type);
    let Some(content) = declared
        .content
        .iter()
        .find(|c| media_matches(&media_essence(&c.media_type), &essence))
    else {
        let accepted: Vec<&str> = declared
            .content
            .iter()
            .map(|c| c.media_type.as_str())
            .collect();
        return Err(format!(
            "Content-Type `{essence}` is not accepted (expected {})",
            accepted.join(", ")
        ));
    };
    if request.body.is_empty() {
        return missing();
    }
    if content.encoding != BodyEncoding::Json {
        return Ok(());
    }
    let value: Value = serde_json::from_slice(request.body)
        .map_err(|e| format!("request body is not valid JSON: {e}"))?;
    let validator = Validator::new(model, Context::Request);
    let Some(rpc) = &entry.op.rpc else {
        return validator
            .check(&content.ty, &value)
            .map_err(|e| format!("request body: {e}"));
    };
    let Some(envelope) = value.as_object() else {
        return Err("request body: expected an object".into());
    };
    for (member, expected) in &rpc.constants {
        if !envelope.get(member).is_some_and(|v| json_eq(v, expected)) {
            return Err(format!("request body: /{member} must equal {expected}"));
        }
    }
    let params = envelope
        .get(&rpc.params_field)
        .cloned()
        .unwrap_or_else(empty_object);
    validator
        .check(&content.ty, &params)
        .map_err(|e| format!("request body /{}: {e}", rpc.params_field))
}

/// `*/*` and `type/*` declarations match their family.
fn media_matches(declared: &str, actual: &str) -> bool {
    declared == actual
        || declared == "*/*"
        || declared
            .strip_suffix('*')
            .is_some_and(|family| family.ends_with('/') && actual.starts_with(family))
}

/// The response answering a status: the exact one, its range, or
/// `default`. Without a status, the lowest 2xx (`2XX` and a successful
/// `default` count as 200).
fn select_response(responses: &[Response], status: Option<u16>) -> Option<(u16, &Response)> {
    if let Some(status) = status {
        let class = (status / 100) as u8;
        return responses
            .iter()
            .find(|r| r.status == StatusMatch::Exact(status))
            .or_else(|| {
                responses
                    .iter()
                    .find(|r| r.status == StatusMatch::Range(class))
            })
            .or_else(|| responses.iter().find(|r| r.status == StatusMatch::Default))
            .map(|r| (status, r));
    }
    let exact = responses.iter().find_map(|r| match r.status {
        StatusMatch::Exact(code) if (200..300).contains(&code) => Some((code, r)),
        _ => None,
    });
    exact
        .or_else(|| {
            responses
                .iter()
                .find(|r| r.status == StatusMatch::Range(2))
                .map(|r| (200, r))
        })
        .or_else(|| {
            responses
                .iter()
                .find(|r| r.status == StatusMatch::Default && r.kind == ResponseKind::Success)
                .map(|r| (200, r))
        })
}

/// The generated success answer of an operation (for `status`, or its
/// lowest 2xx).
fn success(model: &Model, entry: &OpEntry, status: Option<u16>) -> Reply {
    let op = &entry.op;
    let Some((code, response)) = select_response(&op.responses, status) else {
        return Reply::empty(status.unwrap_or(200));
    };
    let generator = Generator::new(model, Context::Response);
    let base = format!("/response/{code}");
    let content = response
        .content
        .iter()
        .find(|c| c.encoding == BodyEncoding::Json)
        .or(response.content.first());
    let mut reply = match content {
        None => Reply::empty(code),
        Some(content) => match content.encoding {
            BodyEncoding::Json => Reply::json_as(
                code,
                &generator.value(&content.ty, entry.id(), &base),
                &content.media_type,
            ),
            BodyEncoding::Text => {
                let text = value_text(&generator.value(&content.ty, entry.id(), &base));
                Reply {
                    status: code,
                    headers: vec![("content-type".into(), content.media_type.clone())],
                    body: text.into_bytes(),
                }
            }
            BodyEncoding::Bytes | BodyEncoding::Form | BodyEncoding::Multipart => {
                Reply::empty(code).with_header("content-type", &content.media_type)
            }
        },
    };
    for header in &response.headers {
        let name = header.wire_name.to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&name.as_str())
            || HeaderName::from_bytes(name.as_bytes()).is_err()
        {
            continue;
        }
        let value =
            value_text(&generator.value(&header.ty, entry.id(), &format!("{base}/headers/{name}")));
        if HeaderValue::from_str(&value).is_ok() {
            reply = reply.with_header(&name, &value);
        }
    }
    reply
}

/// A stored idempotent answer, with a top-level boolean `created` of a JSON
/// object body set to `false`.
fn replayed(mut stored: Reply) -> Reply {
    if let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(&stored.body)
        && body.get("created").is_some_and(Value::is_boolean)
    {
        body.insert("created".into(), Value::Bool(false));
        stored.body = serde_json::to_vec(&Value::Object(body)).unwrap_or_default();
    }
    stored
}

fn injected_status(model: &Model, routed: &Routed, status: u16, code: Option<&str>) -> Reply {
    match routed {
        Routed::Op { index, .. } if (200..300).contains(&status) => {
            success(model, &model.ops[*index], Some(status))
        }
        Routed::Op { index, .. } => op_error(
            model,
            &model.ops[*index],
            status,
            code,
            &[],
            &format!("injected status {status}"),
        ),
        _ if (200..300).contains(&status) => Reply::empty(status),
        _ => reply::error(
            model,
            routed_ns(model, routed),
            status,
            code,
            &[],
            &format!("injected status {status}"),
        ),
    }
}

fn programmed(model: &Model, entry: &OpEntry, program: &Answer) -> Reply {
    let mut reply = match &program.body {
        Some(Value::String(text)) => Reply::text(program.status, text),
        Some(value) => Reply::json(program.status, value),
        None if (200..300).contains(&program.status) => success(model, entry, Some(program.status)),
        None => op_error(
            model,
            entry,
            program.status,
            program.code.as_deref(),
            &[],
            "programmed response",
        ),
    };
    for (name, value) in &program.headers {
        reply = reply.with_header(name, value);
    }
    reply
}

fn control(state: &State, method: &str, path: &str, body: Result<Vec<u8>, BodyError>) -> Outcome {
    let Some(&(_, allowed)) = CONTROL_PATHS.iter().find(|(p, _)| *p == path) else {
        return Outcome::Reply(Reply::text(
            404,
            &format!("unknown control endpoint {path}"),
        ));
    };
    if method != allowed {
        return Outcome::Reply(
            Reply::text(405, &format!("{path} accepts {allowed} only"))
                .with_header("allow", allowed),
        );
    }
    let reply = match path {
        "/__tungsten/calls" => {
            let calls: Vec<Value> = state
                .calls()
                .into_iter()
                .map(|call| {
                    let text = String::from_utf8(call.body.clone()).ok();
                    let mut value =
                        serde_json::to_value(&call).unwrap_or_else(|_| Value::Object(Map::new()));
                    if let Value::Object(map) = &mut value {
                        map.insert("body_text".into(), text.map_or(Value::Null, Value::String));
                    }
                    value
                })
                .collect();
            Reply::json(200, &Value::Array(calls))
        }
        "/__tungsten/reset" => {
            state.reset();
            Reply::empty(204)
        }
        _ => match body {
            Err(BodyError::TooLarge) => Reply::text(413, "program body is too large"),
            Err(BodyError::Broken) => return Outcome::Drop,
            Ok(bytes) => match parse_programs(&state.model, &bytes) {
                Ok(programs) => {
                    let queued = programs.len();
                    state.queue(programs);
                    Reply::json(200, &serde_json::json!({ "queued": queued }))
                }
                Err(reason) => Reply::text(400, &reason),
            },
        },
    };
    Outcome::Reply(reply)
}

fn parse_programs(model: &Model, bytes: &[u8]) -> Result<Vec<(String, Program)>, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("program body is not valid JSON: {e}"))?;
    let items = match value {
        Value::Array(items) => items,
        other => vec![other],
    };
    items
        .iter()
        .map(|item| parse_program(model, item))
        .collect()
}

fn parse_program(model: &Model, item: &Value) -> Result<(String, Program), String> {
    let Some(object) = item.as_object() else {
        return Err("a program must be a JSON object".into());
    };
    const MEMBERS: [&str; 7] = [
        "operation",
        "status",
        "body",
        "headers",
        "times",
        "code",
        "inject",
    ];
    if let Some(unknown) = object.keys().find(|k| !MEMBERS.contains(&k.as_str())) {
        return Err(format!("unknown program member `{unknown}`"));
    }
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .ok_or("a program needs `operation` (an operation id)")?;
    if model.op_index(operation).is_none() {
        return Err(format!("`{operation}` is not a callable operation"));
    }
    let times = match object.get("times") {
        None => 1,
        Some(v) => v
            .as_u64()
            .filter(|t| *t >= 1)
            .ok_or("`times` must be a positive integer")?,
    };
    if let Some(inject) = object.get("inject") {
        if ["status", "body", "headers", "code"]
            .iter()
            .any(|m| object.contains_key(*m))
        {
            return Err(
                "a program with `inject` takes no `status`, `body`, `headers` or `code`".into(),
            );
        }
        let value = inject
            .as_str()
            .ok_or("`inject` must be an X-Tungsten-Inject value")?;
        let injection = Injection::parse(value).map_err(|reason| format!("`inject`: {reason}"))?;
        if injection == Injection::Reset {
            return Err(
                "`inject: reset` closes the connection before the body is read, \
                 before a program is chosen; send `X-Tungsten-Inject: reset` instead"
                    .into(),
            );
        }
        return Ok((
            operation.to_string(),
            Program {
                action: Action::Inject(injection),
                remaining: times,
            },
        ));
    }
    let status = object
        .get("status")
        .and_then(Value::as_u64)
        .filter(|s| (200..=599).contains(s))
        .ok_or("a program needs `status` between 200 and 599")? as u16;
    let mut headers = vec![];
    match object.get("headers") {
        None => {}
        Some(Value::Object(map)) => {
            for (name, value) in map {
                let value = value
                    .as_str()
                    .ok_or_else(|| format!("header `{name}` must be a string"))?;
                if HeaderName::from_bytes(name.as_bytes()).is_err()
                    || HeaderValue::from_str(value).is_err()
                {
                    return Err(format!("header `{name}` is not a valid HTTP header"));
                }
                // Framing is the server's: a programmed Content-Length that
                // does not match the body leaves the client waiting.
                if FRAMING_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h)) {
                    return Err(format!(
                        "header `{name}` frames the response and is set by the mock; remove it"
                    ));
                }
                headers.push((name.clone(), value.to_string()));
            }
        }
        Some(_) => return Err("`headers` must be an object of strings".into()),
    }
    let code = match object.get("code") {
        None => None,
        Some(Value::String(code)) => Some(code.clone()),
        Some(_) => return Err("`code` must be a string".into()),
    };
    Ok((
        operation.to_string(),
        Program {
            action: Action::Answer(Answer {
                status,
                body: object.get("body").cloned(),
                headers,
                code,
            }),
            remaining: times,
        },
    ))
}
