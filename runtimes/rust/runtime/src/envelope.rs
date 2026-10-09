// SPDX-License-Identifier: Apache-2.0
//! The diagnostic error envelope: construction with every field
//! present, default `retryable` per category, status to category mapping and
//! the generic remediation texts used when neither the operation nor the API
//! has a specific entry.

use serde_json::Value;

use crate::types::{Category, Diagnostic, Retryable, Status, Trace};
use crate::util::{SecretSet, scrub_text, scrub_value};

/// Default `retryable` per category.
pub fn default_retryable(category: Category) -> Retryable {
    match category {
        Category::RateLimited | Category::UpstreamUnavailable | Category::TransportFailed => {
            Retryable::AfterDelay
        }
        Category::OutcomeUnknown => Retryable::SameKeyOnly,
        _ => Retryable::Never,
    }
}

/// Remediation used when no manifest entry applies.
pub fn generic_remediation(category: Category) -> &'static str {
    match category {
        Category::ValidationFailed => {
            "The server rejected the arguments. Fix the parameter the API error names and call again; the same request fails the same way."
        }
        Category::MalformedRequest => {
            "The server could not parse the request. Check the format of path and query parameters (for example canonical UUIDs); do not retry unchanged."
        }
        Category::RequestTooLarge => {
            "The request body is larger than the server accepts. Send a smaller body; do not retry unchanged."
        }
        Category::AuthFailed => {
            "The credential is missing, expired, revoked or lacks permission. Do not retry; fix the credential configured in ClientOptions.auth."
        }
        Category::NotFound => {
            "The resource does not exist or is not visible to this credential. Check the identifiers; do not retry unchanged."
        }
        Category::Conflict => {
            "The request conflicts with the resource's current state. Read the current state before deciding what to do; do not retry unchanged."
        }
        Category::PreconditionFailed => {
            "A precondition of this operation is not met (account, billing or resource state). Resolve it first; do not retry unchanged."
        }
        Category::RateLimited => {
            "Rate limited. Wait retry_after_ms (or a few seconds when it is null) before calling again."
        }
        Category::UpstreamUnavailable => {
            "The service is temporarily unavailable. Wait, then call again."
        }
        Category::OutcomeUnknown => {
            "The server may or may not have applied this call; do not retry it blindly. Check whether it took effect before doing anything else."
        }
        Category::TransportFailed => {
            "The request could not be delivered (DNS, TLS or connection failure), so the server did not receive it. Check base_url and network access, then call again."
        }
        Category::ConfirmationRequired => {
            "This operation needs confirmation: call preview(...) and pass its confirmation_token."
        }
        Category::GateDisabled => {
            "This operation is disabled on this deployment. It is a deployment setting; do not retry."
        }
        Category::UnexpectedResponse => {
            "The server's response did not match the API description. Do not retry blindly; report it."
        }
    }
}

/// Category of an HTTP error status when no manifest entry says otherwise.
pub fn category_for_status(status: u16, json_body: bool) -> Category {
    match status {
        401 | 403 | 407 => Category::AuthFailed,
        404 | 410 => Category::NotFound,
        409 => Category::Conflict,
        402 | 412 | 428 => Category::PreconditionFailed,
        413 => Category::RequestTooLarge,
        422 => Category::ValidationFailed,
        429 => Category::RateLimited,
        408 | 502 | 503 | 504 => Category::UpstreamUnavailable,
        500..=599 => Category::UpstreamUnavailable,
        400..=499 => {
            if json_body {
                Category::ValidationFailed
            } else {
                Category::MalformedRequest
            }
        }
        _ => Category::UnexpectedResponse,
    }
}

/// Builder for an envelope with every field present, in the documented order.
#[derive(Debug, Clone)]
pub struct Diag {
    operation: String,
    category: Category,
    http_status: Option<u16>,
    code: Option<String>,
    failed_parameter: Option<String>,
    received_value: Value,
    expected: Option<String>,
    remediation: Option<String>,
    retryable: Option<Retryable>,
    retry_after_ms: Option<u64>,
    next_action: Option<String>,
    request_id: Option<String>,
    attempts: u32,
}

impl Diag {
    pub fn new(operation: impl Into<String>, category: Category) -> Self {
        Diag {
            operation: operation.into(),
            category,
            http_status: None,
            code: None,
            failed_parameter: None,
            received_value: Value::Null,
            expected: None,
            remediation: None,
            retryable: None,
            retry_after_ms: None,
            next_action: None,
            request_id: None,
            attempts: 0,
        }
    }

    pub fn http_status(mut self, status: Option<u16>) -> Self {
        self.http_status = status;
        self
    }

    pub fn code(mut self, code: Option<String>) -> Self {
        self.code = code;
        self
    }

    pub fn failed_parameter(mut self, path: impl Into<String>) -> Self {
        self.failed_parameter = Some(path.into());
        self
    }

    pub fn received_value(mut self, value: Value) -> Self {
        self.received_value = value;
        self
    }

    pub fn expected(mut self, expected: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self
    }

    pub fn remediation(mut self, text: impl Into<String>) -> Self {
        self.remediation = Some(text.into());
        self
    }

    pub fn retryable(mut self, retryable: Retryable) -> Self {
        self.retryable = Some(retryable);
        self
    }

    pub fn retryable_opt(mut self, retryable: Option<Retryable>) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn retry_after_ms(mut self, ms: Option<u64>) -> Self {
        self.retry_after_ms = ms;
        self
    }

    pub fn next_action(mut self, text: Option<String>) -> Self {
        self.next_action = text;
        self
    }

    pub fn request_id(mut self, id: Option<String>) -> Self {
        self.request_id = id;
        self
    }

    pub fn attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts;
        self
    }

    pub fn build(self) -> Diagnostic {
        Diagnostic {
            status: Status::Error,
            category: self.category,
            operation: self.operation,
            http_status: self.http_status,
            code: self.code,
            failed_parameter: self.failed_parameter,
            received_value: self.received_value,
            expected: self.expected,
            remediation: self
                .remediation
                .unwrap_or_else(|| generic_remediation(self.category).to_owned()),
            retryable: self
                .retryable
                .unwrap_or_else(|| default_retryable(self.category)),
            retry_after_ms: self.retry_after_ms,
            next_action: self.next_action,
            request_id: self.request_id,
            trace: Trace {
                attempts: self.attempts,
            },
        }
    }
}

/// Replace every occurrence of the secrets in the envelope's string fields. A
/// last line of defence: no field is built from a secret.
pub fn scrub_diagnostic(mut d: Diagnostic, secrets: &SecretSet) -> Diagnostic {
    if secrets.is_empty() {
        return d;
    }
    let scrub = |text: String| scrub_text(&text, secrets);
    d.code = d.code.map(scrub);
    d.failed_parameter = d.failed_parameter.map(scrub);
    d.received_value = scrub_value(&d.received_value, secrets);
    d.expected = d.expected.map(scrub);
    d.remediation = scrub(d.remediation);
    d.next_action = d.next_action.map(scrub);
    d.request_id = d.request_id.map(scrub);
    d
}
