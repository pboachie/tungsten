// SPDX-License-Identifier: AGPL-3.0-only
//! Responses, and the error rule: a generated error envelope carrying the
//! best matching code, or text/plain when the namespace has no envelope.

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use hyper::header::{HeaderName, HeaderValue};
use serde_json::{Map, Value};
use tungsten_ir::{ErrorModel, TypeRef};

use crate::generate::Generator;
use crate::model::Model;
use crate::validate::Context;

/// Longest `X-Tungsten-Reason` value.
const MAX_REASON_CHARS: usize = 300;

#[derive(Debug, Clone)]
pub(crate) struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn empty(status: u16) -> Reply {
        Reply {
            status,
            headers: vec![],
            body: vec![],
        }
    }

    pub fn text(status: u16, text: &str) -> Reply {
        Reply {
            status,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: text.as_bytes().to_vec(),
        }
    }

    pub fn json(status: u16, value: &Value) -> Reply {
        Reply::json_as(status, value, "application/json")
    }

    pub fn json_as(status: u16, value: &Value, media_type: &str) -> Reply {
        Reply {
            status,
            headers: vec![("content-type".into(), media_type.into())],
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    /// Set a header, replacing any value of the same name.
    pub fn with_header(mut self, name: &str, value: &str) -> Reply {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.headers
            .push((name.to_ascii_lowercase(), value.to_string()));
        self
    }

    pub fn into_response(self) -> Response<Full<Bytes>> {
        let mut response = Response::new(Full::new(Bytes::from(self.body)));
        *response.status_mut() = hyper::StatusCode::from_u16(self.status)
            .unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR);
        let headers = response.headers_mut();
        for (name, value) in self.headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                headers.append(name, value);
            }
        }
        response
    }
}

/// An error answer by the error rule, explained in `X-Tungsten-Reason`.
/// `code` is used verbatim; otherwise `preferred` codes, then the
/// conventional codes for the status (plain and `_error` forms), then any
/// code the model lists with the status (the spec's descriptions or an
/// agent.yml `status`) are tried against the model's codes. The code is
/// written at the model's code field path (`error.type` for an error kept
/// in a tagged union).
pub(crate) fn error(
    model: &Model,
    ns: Option<usize>,
    status: u16,
    code: Option<&str>,
    preferred: &[&str],
    reason: &str,
) -> Reply {
    let errors = model.errors(ns);
    let reply = match (&errors.envelope, &errors.code_field) {
        (Some(envelope), Some(code_field)) => {
            let mut body = Generator::new(model, Context::Response).value(
                &TypeRef::Named(envelope.clone()),
                "error",
                &format!("/{status}"),
            );
            let code = code
                .map(str::to_string)
                .or_else(|| best_code(errors, status, preferred));
            if let Some(code) = code {
                set_path(&mut body, code_field, Value::String(code));
            }
            if let Some(message_field) = &errors.message_field {
                set_path(&mut body, message_field, Value::String(reason.to_string()));
            }
            Reply::json(status, &body)
        }
        _ => match code {
            Some(code) => Reply::text(status, &format!("{code}: {reason}")),
            None => Reply::text(status, reason),
        },
    };
    reply.with_header("x-tungsten-reason", &header_text(reason))
}

fn conventional(status: u16) -> &'static [&'static str] {
    match status {
        400 => &[
            "invalid_request",
            "invalid_request_error",
            "bad_request",
            "validation_failed",
        ],
        401 => &["unauthorized", "unauthenticated", "authentication_error"],
        403 => &["forbidden", "permission_denied", "permission_error"],
        404 => &["not_found", "not_found_error"],
        405 => &["method_not_allowed"],
        409 => &["conflict"],
        413 => &["payload_too_large", "request_too_large", "too_large"],
        415 => &["unsupported_media_type"],
        429 => &["rate_limited", "rate_limit_error", "too_many_requests"],
        500 => &["internal_error", "internal", "api_error"],
        503 => &["unavailable", "service_unavailable"],
        529 => &["overloaded_error", "overloaded"],
        _ => &[],
    }
}

fn best_code(errors: &ErrorModel, status: u16, preferred: &[&str]) -> Option<String> {
    let known = |code: &str| errors.codes.iter().any(|c| c.code == code);
    preferred
        .iter()
        .chain(conventional(status))
        .find(|c| known(c))
        .map(|c| c.to_string())
        .or_else(|| {
            errors
                .codes
                .iter()
                .find(|c| c.statuses.contains(&status))
                .map(|c| c.code.clone())
        })
}

/// Set a dotted path (`error.code`), creating objects on the way.
fn set_path(target: &mut Value, path: &str, value: Value) {
    let mut current = target;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        let Value::Object(map) = current else {
            return;
        };
        if parts.peek().is_none() {
            map.insert(part.to_string(), value);
            return;
        }
        current = map
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Visible ASCII only, bounded, for a header value.
pub(crate) fn header_text(text: &str) -> String {
    text.chars()
        .take(MAX_REASON_CHARS)
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .collect()
}

/// A generated value as header text.
pub(crate) fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
