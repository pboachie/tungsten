// SPDX-License-Identifier: AGPL-3.0-only
//! Responses, and the error rule: a generated error envelope carrying the
//! best matching code, or text/plain when the namespace has no envelope.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use bytes::Bytes;
use hyper::Response;
use hyper::body::{Body, Frame, SizeHint};
use hyper::header::{HeaderName, HeaderValue};
use serde_json::{Map, Value};
use tungsten_ir::{ErrorModel, TypeRef};

use crate::generate::Generator;
use crate::model::Model;
use crate::validate::Context;

/// Longest `X-Tungsten-Reason` value.
const MAX_REASON_CHARS: usize = 300;

/// Time the body of a cut reply waits after its last bytes before the
/// connection is dropped, so they are on the wire first.
const CUT_DELAY: Duration = Duration::from_millis(25);

#[derive(Debug, Clone)]
pub(crate) struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Send only this many bytes of the body, then drop the connection
    /// (the body then has no length, so the client sees a stream that
    /// breaks).
    pub cut_after: Option<usize>,
}

/// The error that drops a connection in the middle of a body.
#[derive(Debug)]
pub(crate) struct BodyCut;

impl fmt::Display for BodyCut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("response cut by injection")
    }
}

impl std::error::Error for BodyCut {}

/// The body of a [`Reply`]: all of it with its length, or the start of it
/// and then an error that drops the connection.
#[derive(Debug)]
pub(crate) struct ReplyBody {
    data: Option<Bytes>,
    cut: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Body for ReplyBody {
    type Data = Bytes;
    type Error = BodyCut;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyCut>>> {
        if let Some(data) = self.data.take() {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        match self.cut.as_mut() {
            Some(delay) => match delay.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    self.cut = None;
                    Poll::Ready(Some(Err(BodyCut)))
                }
                Poll::Pending => Poll::Pending,
            },
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_none() && self.cut.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        match (&self.cut, &self.data) {
            (None, data) => SizeHint::with_exact(data.as_ref().map_or(0, |d| d.len() as u64)),
            (Some(_), _) => SizeHint::default(),
        }
    }
}

impl Reply {
    pub fn empty(status: u16) -> Reply {
        Reply {
            status,
            headers: vec![],
            body: vec![],
            cut_after: None,
        }
    }

    pub fn text(status: u16, text: &str) -> Reply {
        Reply {
            status,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: text.as_bytes().to_vec(),
            cut_after: None,
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
            cut_after: None,
        }
    }

    /// Set a header, replacing any value of the same name.
    pub fn with_header(mut self, name: &str, value: &str) -> Reply {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.headers
            .push((name.to_ascii_lowercase(), value.to_string()));
        self
    }

    /// Cut the body after `bytes` bytes.
    pub fn cut_after(mut self, bytes: usize) -> Reply {
        self.cut_after = Some(bytes);
        self
    }

    pub fn into_response(self) -> Response<ReplyBody> {
        let body = match self.cut_after {
            Some(n) => ReplyBody {
                data: Some(Bytes::from(self.body[..n.min(self.body.len())].to_vec())),
                cut: Some(Box::pin(tokio::time::sleep(CUT_DELAY))),
            },
            None => ReplyBody {
                data: Some(Bytes::from(self.body)),
                cut: None,
            },
        };
        let mut response = Response::new(body);
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
