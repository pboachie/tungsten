// SPDX-License-Identifier: Apache-2.0
//! Classification of HTTP responses and lost requests into results and
//! diagnostic envelopes (planning/06 "Diagnostic error envelope", "Unknown
//! outcome", planning/04 "Remediation table resolution").

use std::collections::BTreeMap;

use serde_json::Value;

use crate::envelope::{Diag, category_for_status, generic_remediation};
use crate::idempotency::has_replay_protection;
use crate::types::{
    ApiDescriptor, Category, Diagnostic, IdempotencyKind, OperationDescriptor, OperationStatus,
    RemediationEntry, ResponseDescriptor, ResponseKind, Retryable, Safety, StatusMatch,
    VerifyDescriptor,
};
use crate::util::{get_path_str, integral_numbers, json_text, number_text, take_units};
use crate::value::Binary;

/// Headers that carry a request id, in lookup order.
const REQUEST_ID_HEADERS: [&str; 5] = [
    "x-request-id",
    "request-id",
    "x-correlation-id",
    "x-amzn-requestid",
    "cf-ray",
];

pub fn request_id_of(headers: &BTreeMap<String, String>) -> Option<String> {
    REQUEST_ID_HEADERS
        .iter()
        .filter_map(|name| headers.get(*name))
        .find(|value| !value.is_empty())
        .map(|value| take_units(value, 200))
}

/// The media type without parameters, lowercased; "" when absent.
pub fn media_type_of(headers: &BTreeMap<String, String>) -> String {
    headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(|media| media.trim().to_lowercase())
        .unwrap_or_default()
}

pub fn is_json_media(media: &str) -> bool {
    media == "application/json" || media.ends_with("+json") || media == "text/json"
}

/// The response descriptor matching a status: exact, then `NXX`, then
/// `default`.
pub fn match_response(
    responses: &[ResponseDescriptor],
    status: u16,
) -> Option<&ResponseDescriptor> {
    responses
        .iter()
        .find(|r| r.status == StatusMatch::Exact(status))
        .or_else(|| {
            responses
                .iter()
                .find(|r| r.status == StatusMatch::Class((status / 100) as u8))
        })
        .or_else(|| responses.iter().find(|r| r.status == StatusMatch::Default))
}

/// A decoded response body.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedBody {
    /// Parsed JSON, text as a string, bytes as the `Binary` form; `None` for
    /// an empty body.
    pub value: Option<Value>,
    pub json: bool,
    /// JSON was announced but did not parse.
    pub invalid_json: bool,
    pub empty: bool,
}

fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn decode_body(
    bytes: &[u8],
    headers: &BTreeMap<String, String>,
    declared: Option<&str>,
) -> DecodedBody {
    if bytes.is_empty() {
        return DecodedBody {
            value: None,
            json: false,
            invalid_json: false,
            empty: true,
        };
    }
    let announced = media_type_of(headers);
    let media = if announced.is_empty() {
        declared.unwrap_or("").to_lowercase()
    } else {
        announced
    };
    let plain = |value: Value| DecodedBody {
        value: Some(value),
        json: false,
        invalid_json: false,
        empty: false,
    };
    if is_json_media(&media) {
        let raw = decode_text(bytes);
        return match serde_json::from_str::<Value>(&raw) {
            Ok(mut value) => DecodedBody {
                value: Some({
                    integral_numbers(&mut value);
                    value
                }),
                json: true,
                invalid_json: false,
                empty: false,
            },
            Err(_) => DecodedBody {
                value: Some(Value::String(raw)),
                json: false,
                invalid_json: true,
                empty: false,
            },
        };
    }
    if media.is_empty()
        || media.starts_with("text/")
        || media == "application/problem+xml"
        || media.ends_with("+xml")
        || media == "application/xml"
    {
        let raw = decode_text(bytes);
        if media.is_empty()
            && raw.trim_start().starts_with(['[', '{'])
            && let Ok(mut value) = serde_json::from_str::<Value>(&raw)
        {
            integral_numbers(&mut value);
            return DecodedBody {
                value: Some(value),
                json: true,
                invalid_json: false,
                empty: false,
            };
        }
        return plain(Value::String(raw));
    }
    plain(serde_json::to_value(Binary::new(bytes)).unwrap_or(Value::Null))
}

const MESSAGE_FIELDS: [&str; 6] = [
    "message",
    "error.message",
    "detail",
    "error_description",
    "title",
    "error",
];

fn server_message(body: Option<&Value>) -> Option<String> {
    MESSAGE_FIELDS.iter().find_map(|field| {
        let text = get_path_str(body, field)?.as_str()?.trim();
        (!text.is_empty()).then(|| take_units(text, 200))
    })
}

fn error_code(op: &OperationDescriptor, body: Option<&Value>) -> Option<String> {
    let field = op.error_code_field.as_deref().filter(|f| !f.is_empty())?;
    match get_path_str(body, field)? {
        Value::String(text) if !text.is_empty() => Some(take_units(text, 200)),
        Value::Number(n) => Some(number_text(n)),
        _ => None,
    }
}

/// Context shared by every classification of one call.
#[derive(Debug, Clone, Copy)]
pub struct CallContext<'a> {
    pub api: &'a ApiDescriptor,
    pub op: &'a OperationDescriptor,
    /// Key sent with the request, if any (never shown).
    pub key: Option<&'a str>,
    /// The key's header name, for remediation text.
    pub key_header: &'a str,
    pub attempts: u32,
    /// For a mutation without replay protection: the read that shows whether
    /// it took effect, or `None`.
    pub check: Option<&'a OutcomeCheck>,
}

/// How to find out whether an unprotected mutation took effect, in words:
/// `call` names the read and its arguments, `shows` what to look for in its
/// answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeCheck {
    pub call: String,
    pub shows: String,
}

fn refers_to_response(node: &Value, depth: usize) -> bool {
    if depth > 32 {
        return true;
    }
    match node {
        Value::String(text) => text == "$response" || text.starts_with("$response."),
        Value::Array(items) => items.iter().any(|i| refers_to_response(i, depth + 1)),
        Value::Object(map) => map.values().any(|i| refers_to_response(i, depth + 1)),
        _ => false,
    }
}

/// Whether the verification hook's arguments can be built without the
/// call's response: after a lost or ambiguous answer there is no success
/// body, so a hook whose args reference `$response` cannot be called.
pub fn verify_callable(verify: &VerifyDescriptor) -> bool {
    !refers_to_response(&verify.args, 0)
}

/// What resolves an unknown outcome: the verification hook when it can be
/// called without the lost response, else repeating the call under its
/// replay protection (the same key, or the identical body). `None` when
/// nothing can resolve it safely.
fn next_action_hint(ctx: &CallContext<'_>) -> Option<String> {
    let op = ctx.op;
    if let Some(verify) = &op.agent.verify
        && !verify.operation.is_empty()
        && verify_callable(verify)
    {
        let keys: Vec<&str> = verify
            .args
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let with = if keys.is_empty() {
            String::new()
        } else {
            format!(" with {}", keys.join(", "))
        };
        return Some(format!(
            "Call {}{with} to check whether {} took effect before doing anything else.",
            verify.operation, op.id
        ));
    }
    if op.agent.idempotency.policy == IdempotencyKind::ContentIdentity {
        return Some(format!(
            "Call {} again with the identical body; the body is its own identity, so the server answers the original result instead of applying it twice.",
            op.id
        ));
    }
    ctx.key.map(|_| {
        format!(
            "Call {} again with the same {} value and identical arguments; the server answers the original result instead of applying it twice.",
            op.id, ctx.key_header
        )
    })
}

/// The change an operation makes, in words, for remediation text: its summary
/// when it has one (`the change "Rotate an endpoint's signing secret"`).
pub fn change_of(op: &OperationDescriptor) -> String {
    let summary = op.summary.as_deref().unwrap_or("").trim();
    let summary = summary.strip_suffix('.').unwrap_or(summary);
    if summary.is_empty() {
        format!("the change {} makes", op.id)
    } else {
        format!(
            "the change {}",
            json_text(&Value::String(summary.to_owned()))
        )
    }
}

/// Fields an `OUTCOME_UNKNOWN` envelope may carry from the response.
#[derive(Debug, Clone, Default)]
pub struct UnknownFields {
    pub next_action: Option<String>,
    pub http_status: Option<u16>,
    pub code: Option<String>,
    pub request_id: Option<String>,
    pub retry_after_ms: Option<u64>,
}

/// `OUTCOME_UNKNOWN` for a mutation whose effect cannot be known. Without
/// replay protection (no key, no identity body) a repeat can apply the effect
/// twice, so the envelope never offers one: `retryable` is
/// `after_remediation` (check the state first), not `same_key_only`, and the
/// remediation and `next_action` say what to check before repeating:
/// `ctx.check` when the client found a read that shows it, else the
/// operation's own change in words.
pub fn outcome_unknown(ctx: &CallContext<'_>, cause: &str, fields: UnknownFields) -> Diagnostic {
    let op = ctx.op;
    let rule: String;
    let mut hint: Option<String> = None;
    if op.agent.idempotency.policy == IdempotencyKind::ContentIdentity {
        rule = "If you retry, resend the identical bytes only; the body is its own identity."
            .to_owned();
    } else if ctx.key.is_some() {
        rule = format!(
            "If you retry, reuse the SAME {} value; a new key can apply the effect twice.",
            ctx.key_header
        );
    } else if let Some(check) = ctx.check {
        rule = format!(
            "This operation has no idempotency key, so repeating it can apply the effect twice. Before calling {id} again, call {call} and check {shows}: if it does, {id} took effect and must not be repeated; call it again only if it did not.",
            id = op.id,
            call = check.call,
            shows = check.shows
        );
        hint = Some(format!(
            "Call {} and check {}; call {} again only if it did not take effect.",
            check.call, check.shows, op.id
        ));
    } else {
        let change = change_of(op);
        rule = format!(
            "This operation has no idempotency key, so repeating it can apply the effect twice: do not call it again until you have checked whether it took effect, by reading the resource it changes and looking for {change}."
        );
        hint = Some(format!(
            "Read the resource {id} changes and look for {change}; call {id} again only if it is not there.",
            id = op.id
        ));
    }
    let retryable = if has_replay_protection(op, ctx.key) {
        Retryable::SameKeyOnly
    } else {
        Retryable::AfterRemediation
    };
    Diag::new(op.id.clone(), Category::OutcomeUnknown)
        .remediation(format!(
            "{cause} The server may or may not have applied {}. {rule}",
            op.id
        ))
        .retryable(retryable)
        .next_action(
            fields
                .next_action
                .or(hint)
                .or_else(|| next_action_hint(ctx)),
        )
        .http_status(fields.http_status)
        .code(fields.code)
        .request_id(fields.request_id)
        .retry_after_ms(fields.retry_after_ms)
        .attempts(ctx.attempts)
        .build()
}

/// A manifest entry with one of these categories (or with `retryable`
/// `never` / `after_remediation`) is a definite answer even for a status that
/// is otherwise ambiguous.
fn definite(category: Option<Category>, retryable: Option<Retryable>) -> bool {
    if matches!(
        retryable,
        Some(Retryable::Never) | Some(Retryable::AfterRemediation)
    ) {
        return true;
    }
    matches!(
        category,
        Some(
            Category::ValidationFailed
                | Category::MalformedRequest
                | Category::RequestTooLarge
                | Category::AuthFailed
                | Category::NotFound
                | Category::Conflict
                | Category::PreconditionFailed
                | Category::RateLimited
                | Category::GateDisabled
        )
    )
}

pub fn is_mutation(op: &OperationDescriptor) -> bool {
    op.agent.safety != Safety::ReadOnly
}

fn remediation_entry<'a>(
    table: &'a BTreeMap<String, RemediationEntry>,
    code: Option<&str>,
) -> Option<&'a RemediationEntry> {
    table.get(code?)
}

/// The classification of an HTTP error status.
pub fn classify_error(
    ctx: &CallContext<'_>,
    status: u16,
    headers: &BTreeMap<String, String>,
    decoded: &DecodedBody,
    retry_after_ms: Option<u64>,
) -> Diagnostic {
    let (api, op) = (ctx.api, ctx.op);
    let request_id = request_id_of(headers);
    let base = |d: Diag| {
        d.http_status(Some(status))
            .request_id(request_id.clone())
            .retry_after_ms(retry_after_ms)
            .attempts(ctx.attempts)
    };
    let code = if decoded.json {
        error_code(op, decoded.value.as_ref())
    } else {
        None
    };

    // A disabled gate answers its status without the API's error document
    // (the route is not mounted, so a bare 404). An answer with an error code
    // comes from the mounted route, so it is classified by its code like any
    // other error (an unknown id is NOT_FOUND).
    if let OperationStatus::Gated {
        env_var,
        disabled_status,
    } = &op.status
        && status == *disabled_status
        && code.is_none()
    {
        let text = api.gates.get(env_var).cloned().unwrap_or_else(|| {
            format!(
                "This deployment disables {} because {env_var} is off (HTTP {status}). It is a deployment setting, not a missing resource; do not retry.",
                op.id
            )
        });
        return base(Diag::new(op.id.clone(), Category::GateDisabled))
            .code(code)
            .remediation(text)
            .retryable(Retryable::Never)
            .build();
    }

    let entry = remediation_entry(&op.agent.remediation, code.as_deref())
        .or_else(|| remediation_entry(&api.error_codes, code.as_deref()));
    let media = if decoded.empty {
        "none".to_owned()
    } else {
        let media = media_type_of(headers);
        if media.is_empty() {
            "none".to_owned()
        } else {
            media
        }
    };
    let non_json = if decoded.json {
        None
    } else {
        api.non_json.iter().find(|n| {
            n.status == status && (n.media == media || (n.media == "none" && decoded.empty))
        })
    };
    let declared = match_response(&op.responses, status);
    // On a mutation, a status listed as ambiguous, a response declared
    // ambiguous, and any 5xx leave the outcome unknown (a gateway's 502/504 or
    // an intermediary's 500 says nothing about whether the origin committed),
    // unless the manifest's entry for the answer's code or non-JSON shape says
    // it is a definite rejection (billing_pending, retryable: never).
    let ambiguous = api.ambiguous_statuses.contains(&status)
        || declared.is_some_and(|d| d.kind == ResponseKind::Ambiguous)
        || (500..=599).contains(&status);
    let definite_answer = match (entry, non_json) {
        (Some(e), _) => definite(e.category, e.retryable),
        (None, Some(n)) => definite(Some(n.category), Some(n.retryable)),
        (None, None) => false,
    };
    if ambiguous && is_mutation(op) && !definite_answer {
        // The manifest's text for this answer still explains it.
        let said = entry
            .and_then(|e| e.text.clone())
            .or_else(|| non_json.and_then(|n| n.text.clone()));
        let cause = format!(
            "HTTP {status} leaves the outcome of this call unknown.{}",
            said.map(|s| format!(" {s}")).unwrap_or_default()
        );
        return outcome_unknown(
            ctx,
            &cause,
            UnknownFields {
                next_action: entry.and_then(|e| e.next_action.clone()),
                http_status: Some(status),
                code,
                request_id,
                retry_after_ms,
            },
        );
    }

    let mut category: Category;
    let mut retryable: Option<Retryable>;
    let mut text: Option<String>;
    let mut next_action: Option<String> = None;
    if let Some(entry) = entry {
        category = entry
            .category
            .unwrap_or_else(|| category_for_status(status, decoded.json));
        retryable = entry.retryable;
        text = entry.text.clone();
        next_action = entry.next_action.clone();
    } else if let Some(non_json) = non_json {
        category = non_json.category;
        retryable = Some(non_json.retryable);
        text = non_json.text.clone();
    } else {
        category = category_for_status(status, decoded.json);
        retryable = None;
        text = None;
    }
    if category == Category::OutcomeUnknown && !is_mutation(op) {
        // A read has no effect whose outcome could be unknown: the answer only
        // says the service did not respond in time.
        category = Category::UpstreamUnavailable;
        if matches!(retryable, None | Some(Retryable::SameKeyOnly)) {
            retryable = Some(Retryable::AfterDelay);
        }
    }
    let text = text.take().unwrap_or_else(|| {
        let said = if decoded.json {
            server_message(decoded.value.as_ref())
        } else {
            None
        };
        format!(
            "{}{}",
            generic_remediation(category),
            said.map(|s| format!(" Server message: {}", json_text(&Value::String(s))))
                .unwrap_or_default()
        )
    });
    base(Diag::new(op.id.clone(), category))
        .code(code)
        .remediation(text)
        .retryable_opt(retryable)
        .next_action(next_action)
        .build()
}
