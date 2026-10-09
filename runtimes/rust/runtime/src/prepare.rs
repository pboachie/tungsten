// SPDX-License-Identifier: Apache-2.0
//! Pre-flight: validation, confirmation, auth, idempotency and the building
//! of the request (the call pipeline is: call, preflight, auth, idempotency,
//! send).

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Map, Value};
use url::Url;

use crate::auth::{AuthPlan, AuthResolution, is_safe_method, resolve_auth};
use crate::client::ClientCore;
use crate::confirm::{CONFIRMATION_TTL_MS, TokenCheck, check_token, token_expiry};
use crate::envelope::Diag;
use crate::expr::contains_placeholder;
use crate::helpers::{
    arg_params, argument_path, descriptor_problem, redact_below, sensitive_arg,
    sensitive_body_paths, sensitive_request_paths, values_at,
};
use crate::idempotency::{check_key_format, key_format_description, key_header};
use crate::serialize::{
    EncodedBody, HeaderBag, Payload, SerializationError, body_value, encode_body, encode_component,
    serialize_cookie_param, serialize_header_param, serialize_path_param, serialize_query_param,
    valid_header_name, valid_header_value,
};
use crate::types::{
    BodyShape, Category, Confirm, Error, HttpMethod, IdempotencyKind, OperationDescriptor,
    ParamDescriptor, ParamLocation, ParamRole, PathSegment, PreviewMode, ResponseKind, Retryable,
    Safety, Validation,
};
use crate::util::{
    MAX_ARG_DEPTH, REDACTED, SecretSet, canonical_json, envelope_value, get_path, integral_numbers,
    looks_sensitive, nests_deeper_than, redact_paths, redact_sensitive_keys, sha256_hex,
    split_path, uuid_v4,
};

/// A fallible step of the pipeline: the failure is the call's result.
pub(crate) type Step<T> = std::result::Result<T, Error>;

/// Why a request is being prepared. `MacroPreview` renders a macro step from
/// a dry evaluation: argument values that are placeholders for earlier
/// results are not validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    Call,
    Preview,
    ServerPreview,
    MacroPreview,
}

/// A macro run's one-time spend of its confirmation token: the first step
/// whose request passed pre-flight and is about to be sent claims it, so a
/// step that fails pre-flight leaves the token unspent.
pub(crate) struct MacroClaim {
    pub name: String,
    pub token: Option<String>,
    pub bind: Option<String>,
    pub claimed: AtomicBool,
}

impl MacroClaim {
    pub fn claim(&self, core: &ClientCore) -> Option<Error> {
        let token = self.token.as_deref()?;
        if self.claimed.swap(true, Ordering::SeqCst) {
            return None;
        }
        core.claim_token(
            &self.name,
            token,
            self.bind.clone(),
            "the macro's preview(...)",
        )
    }
}

/// What a confirmation is for: the id named in remediations, the subject
/// the token is bound to (an operation id, or `macro:<name>`) and the call
/// that issues tokens.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConfirmTarget<'a> {
    pub id: &'a str,
    pub subject: &'a str,
    pub preview_call: &'a str,
}

/// A request ready to send, with its redacted rendering.
pub(crate) struct Prepared<'a> {
    pub op: &'a OperationDescriptor,
    pub args: Map<String, Value>,
    pub method: HttpMethod,
    pub url: String,
    pub display_url: String,
    pub headers: HeaderBag,
    pub payload: Payload,
    /// The body as middleware and previews see it (redacted).
    pub display_body: Value,
    pub key: Option<String>,
    pub key_header: String,
    /// Auth query parameters, re-applied on a followed redirect.
    pub auth_query: Vec<(String, String)>,
    /// Wire names of query parameters whose values are shown redacted.
    pub hidden_query: Vec<String>,
    pub secrets: SecretSet,
}

pub(crate) fn fail(diagnostic: crate::types::Diagnostic) -> Error {
    Error::new(diagnostic)
}

fn segment_string(segment: &PathSegment) -> String {
    match segment {
        PathSegment::Key(key) => key.clone(),
        PathSegment::Index(index) => index.to_string(),
    }
}

fn is_dot_segment(segment: &str) -> bool {
    if segment.is_empty() {
        return true;
    }
    let lower = segment.to_lowercase();
    let mut rest = lower.as_str();
    let mut count = 0;
    while count < 3 {
        if let Some(r) = rest.strip_prefix("%2e") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix('.') {
            rest = r;
        } else {
            break;
        }
        count += 1;
    }
    rest.is_empty() && (1..=2).contains(&count)
}

/// `url` with the query parameter `name` set to `value`, replacing existing
/// ones (`URLSearchParams.set`).
fn set_query_param(url: &mut Url, name: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    match pairs.iter().position(|(k, _)| k == name) {
        Some(index) => {
            pairs[index].1 = value.to_owned();
            let mut seen = false;
            pairs.retain(|(k, _)| {
                if k == name {
                    let keep = !seen;
                    seen = true;
                    keep
                } else {
                    true
                }
            });
        }
        None => pairs.push((name.to_owned(), value.to_owned())),
    }
    url.query_pairs_mut().clear().extend_pairs(pairs);
}

fn has_query_param(url: &Url, name: &str) -> bool {
    url.query_pairs().any(|(k, _)| k == name)
}

/// The URL of a followed redirect of a read, as sent and as shown: auth query
/// parameters are re-applied, hidden ones redacted.
pub(crate) fn with_auth_query(
    mut target: Url,
    auth_query: &[(String, String)],
    hidden: &[String],
) -> (String, String) {
    for (name, value) in auth_query {
        if !has_query_param(&target, name) {
            set_query_param(&mut target, name, value);
        }
    }
    let mut shown = target.clone();
    for name in hidden {
        if has_query_param(&shown, name) {
            set_query_param(&mut shown, name, REDACTED);
        }
    }
    (target.to_string(), shown.to_string())
}

impl ClientCore {
    pub(crate) fn validation(
        &self,
        op: &OperationDescriptor,
        path: &[PathSegment],
        value: Option<&Value>,
        expected: &str,
        remediation: String,
    ) -> Error {
        let received = match value {
            Some(v) => envelope_value(&redact_below(op, path, v), sensitive_arg(op, path)),
            None => envelope_value(&Value::Null, sensitive_arg(op, path)),
        };
        fail(
            Diag::new(op.id.clone(), Category::ValidationFailed)
                .failed_parameter(argument_path(op, path))
                .received_value(received)
                .expected(expected)
                .remediation(remediation)
                .build(),
        )
    }

    fn key_path(name: &str) -> Vec<PathSegment> {
        vec![PathSegment::Key(name.to_owned())]
    }

    /// Pre-flight validation of the args; the validated args on success. With
    /// `placeholders`, an issue at (or below) a value that is a dry
    /// evaluation's placeholder is not a failure: the value is only known once
    /// the earlier step has run.
    fn validate_args(
        &self,
        op: &OperationDescriptor,
        args: &Map<String, Value>,
        placeholders: bool,
    ) -> Step<Map<String, Value>> {
        let params = arg_params(op);
        for p in &params {
            if p.required && args.get(&p.name).is_none_or(Value::is_null) {
                return Err(self.validation(
                    op,
                    &Self::key_path(&p.name),
                    args.get(&p.name),
                    &format!(
                        "a value for the required {} parameter {}",
                        location_name(p.location),
                        p.wire
                    ),
                    format!("Pass {}; {} cannot be called without it.", p.name, op.id),
                ));
            }
        }
        let body = op.body.as_ref();
        if let Some(body) = body
            && body.required
            && let BodyShape::Arg { arg } = &body.shape
            && !args.contains_key(arg)
        {
            return Err(self.validation(
                op,
                &Self::key_path(arg),
                None,
                "a request body",
                format!("Pass the request body as {arg}."),
            ));
        }
        if let Some(validator) = &op.request {
            let mut root = Value::Object(args.clone());
            integral_numbers(&mut root);
            let Ok(outcome) = crate::validate::run(validator.as_ref(), &root) else {
                return Err(fail(
                    Diag::new(op.id.clone(), Category::UnexpectedResponse)
                        .retryable(Retryable::Never)
                        .remediation("The SDK failed while building or sending the request (the request validator panicked). This is a bug in the generated SDK or the runtime; report it. Nothing was sent.")
                        .build(),
                ));
            };
            return match outcome {
                Validation::Valid(Value::Object(normalized)) => Ok(normalized),
                Validation::Valid(_) => Ok(root.as_object().cloned().unwrap_or_default()),
                Validation::Invalid(issues) => {
                    let pending = |path: &[PathSegment]| {
                        placeholders
                            && (0..path.len()).any(|i| {
                                let segments: Vec<String> =
                                    path[..=i].iter().map(segment_string).collect();
                                get_path(Some(&root), &segments).is_some_and(contains_placeholder)
                            })
                    };
                    let issue = if placeholders {
                        match issues.iter().find(|candidate| !pending(&candidate.path)) {
                            Some(issue) => Some(issue),
                            None => return Ok(args.clone()),
                        }
                    } else {
                        issues.first()
                    };
                    let (path, message) = match issue {
                        Some(issue) => (issue.path.clone(), issue.message.clone()),
                        None => (Vec::new(), "a valid value".to_owned()),
                    };
                    let segments: Vec<String> = path.iter().map(segment_string).collect();
                    let shown = argument_path(op, &path);
                    Err(self.validation(
                        op,
                        &path,
                        get_path(Some(&root), &segments),
                        &message,
                        format!("Fix {shown} ({message}) and call again."),
                    ))
                }
            };
        }
        // Without a validator, unknown keys are rejected so a misspelt
        // argument is never silently dropped (rpc methods without a body take
        // free-form args).
        if op.rpc.is_some() && body.is_none() {
            return Ok(args.clone());
        }
        let mut allowed: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        match body.map(|b| &b.shape) {
            Some(BodyShape::Merged { fields }) => {
                allowed.extend(fields.iter().map(|f| f.arg.as_str()));
            }
            Some(BodyShape::Arg { arg }) => allowed.push(arg),
            None => {}
        }
        for (key, value) in args {
            if !allowed.contains(&key.as_str()) {
                let mut names: Vec<&str> = allowed.clone();
                names.sort_unstable();
                names.dedup();
                let list = if names.is_empty() {
                    "no arguments".to_owned()
                } else {
                    names
                        .iter()
                        .take(20)
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                return Err(self.validation(
                    op,
                    &Self::key_path(key),
                    Some(value),
                    &format!("one of: {list}"),
                    format!("Remove {key}; {} does not accept it.", op.id),
                ));
            }
        }
        Ok(args.clone())
    }

    fn check_key(&self, op: &OperationDescriptor, key: Option<&str>, purpose: Purpose) -> Step<()> {
        let policy = &op.agent.idempotency;
        let note = policy
            .note
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(|n| format!(" {n}"))
            .unwrap_or_default();
        let format = policy.format.as_deref();
        let Some(key) = key else {
            if purpose == Purpose::Call
                && policy.policy == IdempotencyKind::CallerOwned
                && policy.persist_required
            {
                let fresh = if format.unwrap_or("").to_lowercase().contains("uuid") {
                    "UUIDv4"
                } else {
                    "unique key"
                };
                return Err(fail(
                    Diag::new(op.id.clone(), Category::ValidationFailed)
                        .failed_parameter("idempotency_key")
                        .expected(key_format_description(format))
                        .remediation(format!(
                            "Generate a random {fresh}, persist it with the intent of this {} call, and pass it as idempotency_key. Reuse the exact same key on every retry; a new key can apply the effect twice.{note}",
                            op.id
                        ))
                        .build(),
                ));
            }
            return Ok(());
        };
        if policy.policy == IdempotencyKind::None
            && !op
                .params
                .iter()
                .any(|p| p.role == ParamRole::IdempotencyKey)
        {
            return Ok(());
        }
        if matches!(
            policy.policy,
            IdempotencyKind::ContentIdentity | IdempotencyKind::ContentHash
        ) {
            return Ok(());
        }
        let checked = check_key_format(
            key,
            if policy.policy == IdempotencyKind::CallerOwned {
                format
            } else {
                None
            },
        );
        if let Some(expected) = checked {
            return Err(fail(
                Diag::new(op.id.clone(), Category::ValidationFailed)
                    .failed_parameter("idempotency_key")
                    .received_value(envelope_value(&Value::String(key.to_owned()), false))
                    .expected(expected)
                    .remediation(format!(
                        "Pass idempotency_key as {expected}. Generate it once, persist it with the intent of this call, and reuse it on every retry.{note}"
                    ))
                    .build(),
            ));
        }
        Ok(())
    }

    /// The confirmation rule for one tier: `destructive`
    /// accepts `Confirm::Yes` (unless `allow_yes` is false) or a token,
    /// `irreversible` only a token issued for `subject` (an operation id, or
    /// `macro:<name>`) and these exact args.
    pub(crate) fn confirmed(
        &self,
        target: &ConfirmTarget<'_>,
        safety: Safety,
        args: &Value,
        opts: &crate::types::CallOptions,
    ) -> Step<()> {
        let ConfirmTarget {
            id,
            subject,
            preview_call,
        } = *target;
        let confirm = opts.confirm.as_ref();
        let allow_yes = opts.allow_confirm_true;
        if !matches!(safety, Safety::Destructive | Safety::Irreversible) {
            return Ok(());
        }
        let tier = if safety == Safety::Destructive {
            "destructive"
        } else {
            "irreversible"
        };
        let how = format!(
            "call {preview_call} with the same arguments and pass its confirmation_token as confirm"
        );
        let required = |remediation: String| {
            fail(
                Diag::new(id, Category::ConfirmationRequired)
                    .failed_parameter("confirm")
                    .expected("a confirmation_token from preview()")
                    .remediation(remediation)
                    .build(),
            )
        };
        match confirm {
            Some(Confirm::Yes) => {
                if safety == Safety::Destructive && allow_yes {
                    Ok(())
                } else {
                    Err(required(format!(
                        "{id} is {tier}, so Confirm::Yes is not accepted: {how}."
                    )))
                }
            }
            Some(Confirm::Token(token)) if !token.is_empty() => {
                match check_token(
                    &self.inner.confirmation_key,
                    token,
                    subject,
                    args,
                    self.now(),
                ) {
                    TokenCheck::Valid => Ok(()),
                    TokenCheck::Expired => Err(required(format!(
                        "The confirmation token expired (tokens last {} minutes): {how}.",
                        CONFIRMATION_TTL_MS / 60_000
                    ))),
                    TokenCheck::Mismatch | TokenCheck::Malformed => Err(required(format!(
                        "The confirmation token was not issued by this client for {id} with exactly these arguments: {how}."
                    ))),
                }
            }
            _ => {
                let or_yes = if safety == Safety::Destructive && allow_yes {
                    " (or pass Confirm::Yes)"
                } else {
                    ""
                };
                Err(required(format!("{id} is {tier}: {how}{or_yes}.")))
            }
        }
    }

    /// Spend a valid confirmation token on the call about to be sent. A token
    /// authorizes one intent: its first use binds it to that call's replay
    /// protection (`bind`: the idempotency key sent, or the identity of an
    /// identity body), and a later use is accepted only as a retry of the same
    /// intent under the same protection. A call without protection (no key)
    /// can use a token once.
    pub(crate) fn claim_token(
        &self,
        id: &str,
        token: &str,
        bind: Option<String>,
        preview_call: &str,
    ) -> Option<Error> {
        let now = self.now();
        let mut used = crate::idempotency::lock(&self.inner.used_tokens);
        used.retain(|_, (_, expiry)| *expiry > now);
        if let Some((previous, _)) = used.get(token) {
            if previous.is_some() && *previous == bind {
                return None;
            }
            let why = match previous {
                None => "This call has no idempotency key, so a repeat can apply the effect twice"
                    .to_owned(),
                Some(_) => format!(
                    "It was used with {}; only a retry with that same key may reuse it",
                    if bind.is_none() {
                        "an idempotency key"
                    } else {
                        "another idempotency key"
                    }
                ),
            };
            return Some(fail(
                Diag::new(id, Category::ConfirmationRequired)
                    .failed_parameter("confirm")
                    .expected("a confirmation_token from preview()")
                    .remediation(format!(
                        "This confirmation token was already used for one {id} call. {why}. Check whether that call took effect; to send again, call {preview_call} and confirm with its new token."
                    ))
                    .build(),
            ));
        }
        let expiry = token_expiry(token).map_or_else(
            || now.saturating_add(CONFIRMATION_TTL_MS),
            |(expiry, _)| expiry,
        );
        used.insert(token.to_owned(), (bind, expiry));
        None
    }

    pub(crate) async fn prepare<'a>(
        &self,
        op: &'a OperationDescriptor,
        raw_args: &Value,
        opts: &crate::types::CallOptions,
        purpose: Purpose,
        url_override: Option<&str>,
        step: Option<&MacroClaim>,
    ) -> Step<Prepared<'a>> {
        if let Some(problem) = descriptor_problem(op) {
            let id = if op.id.is_empty() {
                "<unknown operation>"
            } else {
                &op.id
            };
            return Err(fail(
                Diag::new(id, Category::ValidationFailed)
                    .failed_parameter("operation")
                    .expected("a valid operation descriptor")
                    .remediation(format!(
                        "The operation descriptor is invalid: {problem}. Regenerate the SDK or fix the descriptor; nothing was sent."
                    ))
                    .build(),
            ));
        }
        self.register_one(op);
        let empty = Map::new();
        let supplied = match raw_args {
            Value::Object(map) => map,
            Value::Null => &empty,
            other => {
                let names = arg_params(op)
                    .iter()
                    .take(3)
                    .map(|p| format!("{}: ...", p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(fail(
                    Diag::new(op.id.clone(), Category::ValidationFailed)
                        .failed_parameter("args")
                        .received_value(envelope_value(other, false))
                        .expected("an object of named arguments")
                        .remediation(format!(
                            "Pass the arguments of {} as one object, e.g. {{{names}}}.",
                            op.id
                        ))
                        .build(),
                ));
            }
        };
        if nests_deeper_than(raw_args, MAX_ARG_DEPTH) {
            return Err(self.validation(
                op,
                &[],
                Some(&Value::String("<deeply nested value>".to_owned())),
                &format!("a JSON value nested at most {MAX_ARG_DEPTH} levels"),
                "Pass arguments without such deep nesting.".to_owned(),
            ));
        }
        let args = self.validate_args(op, supplied, purpose == Purpose::MacroPreview)?;
        self.check_key(op, opts.idempotency_key.as_deref(), purpose)?;
        if purpose == Purpose::Call && step.is_none() {
            self.confirmed(
                &ConfirmTarget {
                    id: &op.id,
                    subject: &op.id,
                    preview_call: "preview(...)",
                },
                op.agent.safety,
                &Value::Object(args.clone()),
                opts,
            )?;
        }

        let method = op.method;
        let plan = match resolve_auth(&self.inner.api, op, &self.inner.auth, method, self).await {
            AuthResolution::Ok { plan, secrets } => (plan, secrets),
            AuthResolution::Failed { remediation } => {
                return Err(fail(
                    Diag::new(op.id.clone(), Category::AuthFailed)
                        .remediation(remediation)
                        .build(),
                ));
            }
        };
        let (plan, auth_secrets) = plan;
        let mut secrets = SecretSet::default();
        for secret in auth_secrets {
            secrets.insert(secret);
        }
        for p in &op.params {
            if p.sensitive {
                match args.get(&p.name) {
                    Some(Value::String(text)) => secrets.insert(text.clone()),
                    Some(Value::Number(n)) => secrets.insert(crate::util::number_text(n)),
                    _ => {}
                }
            }
        }
        let root = Value::Object(args.clone());
        for path in sensitive_request_paths(op) {
            let mut found = Vec::new();
            values_at(&root, &split_path(path), &mut found, 0);
            for value in found {
                secrets.insert(value);
            }
        }

        let (url, display_url) = self.build_url(op, &args, &plan, url_override)?;

        let mut headers = HeaderBag::default();
        self.build_headers(op, &args, opts, &plan, &mut headers);

        let params = arg_params(op);
        let param_names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        let body_paths = sensitive_body_paths(op);
        let value = if is_safe_method(method) && method != HttpMethod::Options {
            None
        } else {
            body_value(op, &args, &param_names)
        };
        let redact = |display: &Value| {
            if body_paths.iter().any(String::is_empty) {
                Value::String(REDACTED.to_owned())
            } else {
                redact_sensitive_keys(&redact_paths(display, &body_paths), 0)
            }
        };
        let encoded: EncodedBody = match encode_body(op.body.as_ref(), value.as_ref(), &redact) {
            Ok(encoded) => encoded,
            Err(SerializationError {
                parameter,
                expected,
                value,
            }) => {
                let path = match op.body.as_ref().map(|b| &b.shape) {
                    Some(BodyShape::Arg { arg }) if parameter == "body" => Self::key_path(arg),
                    _ => Vec::new(),
                };
                return Err(self.validation(
                    op,
                    &path,
                    Some(&value),
                    &expected,
                    format!("Pass {parameter} as {expected}."),
                ));
            }
        };
        if let Some(content_type) = &encoded.content_type {
            headers.set("Content-Type", content_type.clone(), false);
        }

        let header = key_header(op);
        let key =
            self.idempotency_key(op, &args, opts, purpose, encoded.hash_material.as_deref())?;
        if let Some(key) = &key {
            headers.set(&header, key.clone(), true);
            secrets.insert(key.clone());
        } else if purpose != Purpose::Call
            && matches!(
                op.agent.idempotency.policy,
                IdempotencyKind::Auto | IdempotencyKind::CallerOwned
            )
        {
            headers.set(&header, "<set at call time>", false);
        }
        for h in &plan.headers {
            headers.set(&h.name, h.value.clone(), h.secret);
        }
        if purpose == Purpose::ServerPreview
            && let PreviewMode::Header { header, value } = &op.agent.preview
        {
            headers.set(header, value.clone(), false);
        }

        for (name, value) in headers.to_map(false) {
            if !valid_header_name(&name) || !valid_header_value(&value) {
                if plan
                    .headers
                    .iter()
                    .any(|h| h.name.eq_ignore_ascii_case(&name))
                {
                    return Err(fail(
                        Diag::new(op.id.clone(), Category::AuthFailed)
                            .remediation(format!(
                                "The credential for header {name} contains characters not allowed in an HTTP header; check ClientOptions.auth."
                            ))
                            .build(),
                    ));
                }
                let param = op
                    .params
                    .iter()
                    .find(|p| p.wire.eq_ignore_ascii_case(&name));
                let path = param.map(|p| Self::key_path(&p.name)).unwrap_or_default();
                let shown = if headers.is_secret(&name) {
                    Value::String(REDACTED.to_owned())
                } else {
                    Value::String(value)
                };
                return Err(self.validation(
                    op,
                    &path,
                    Some(&shown),
                    "a header value of visible ASCII characters (no line breaks)",
                    format!(
                        "Header {name} cannot carry this value; remove line breaks and non-Latin-1 characters."
                    ),
                ));
            }
        }
        if purpose == Purpose::Call {
            if let Some(claim) = step {
                if let Some(spent) = claim.claim(self) {
                    return Err(spent);
                }
            } else if let Some(Confirm::Token(token)) = &opts.confirm
                && matches!(op.agent.safety, Safety::Destructive | Safety::Irreversible)
            {
                let bind = key.clone().or_else(|| {
                    (op.agent.idempotency.policy == IdempotencyKind::ContentIdentity)
                        .then(|| "content-identity".to_owned())
                });
                if let Some(spent) = self.claim_token(&op.id, token, bind, "preview(...)") {
                    return Err(spent);
                }
            }
        }

        for secret in headers.secrets() {
            secrets.insert(secret);
        }
        let mut hidden_query: Vec<String> = plan.query.iter().map(|(n, _)| n.clone()).collect();
        hidden_query.extend(
            op.params
                .iter()
                .filter(|p| p.location == ParamLocation::Query && p.sensitive)
                .map(|p| p.wire.clone()),
        );
        Ok(Prepared {
            op,
            args,
            method,
            url,
            display_url,
            headers,
            payload: encoded.payload,
            display_body: encoded.display,
            key,
            key_header: header,
            auth_query: plan.query.clone(),
            hidden_query,
            secrets,
        })
    }

    fn build_url(
        &self,
        op: &OperationDescriptor,
        args: &Map<String, Value>,
        plan: &AuthPlan,
        url_override: Option<&str>,
    ) -> Step<(String, String)> {
        let base = self.inner.base_url.as_deref().unwrap_or("");
        if base.is_empty() && url_override.is_none() {
            return Err(fail(
                Diag::new(op.id.clone(), Category::TransportFailed)
                    .retryable(Retryable::Never)
                    .remediation(
                        "No base URL is configured: set ClientOptions.base_url. Nothing was sent.",
                    )
                    .build(),
            ));
        }
        let params = arg_params(op);
        let bad_base = || {
            fail(
                Diag::new(op.id.clone(), Category::TransportFailed)
                    .retryable(Retryable::Never)
                    .remediation("The base URL is not a valid absolute http(s) URL without credentials: fix ClientOptions.base_url. Nothing was sent.")
                    .build(),
            )
        };
        let (mut url, mut display) = match url_override {
            Some(target) => (target.to_owned(), target.to_owned()),
            None => {
                let mut unknown: Option<String> = None;
                let mut missing: Option<&ParamDescriptor> = None;
                let mut path = String::with_capacity(op.path.len());
                let mut rest = op.path.as_str();
                while let Some(open) = rest.find('{') {
                    let Some(close) = rest[open..].find('}').map(|c| c + open) else {
                        break;
                    };
                    let inner = &rest[open + 1..close];
                    if inner.is_empty() || inner.contains('{') {
                        path.push_str(&rest[..=open]);
                        rest = &rest[open + 1..];
                        continue;
                    }
                    path.push_str(&rest[..open]);
                    let p = params
                        .iter()
                        .find(|x| x.location == ParamLocation::Path && x.wire == inner)
                        .or_else(|| {
                            params
                                .iter()
                                .find(|x| x.location == ParamLocation::Path && x.name == inner)
                        });
                    match p {
                        None => {
                            unknown = Some(inner.to_owned());
                            path.push_str(&rest[open..=close]);
                        }
                        Some(p) => match args.get(&p.name) {
                            None | Some(Value::Null) => {
                                missing = Some(p);
                                path.push_str(&rest[open..=close]);
                            }
                            Some(value) => path.push_str(&serialize_path_param(p, value)),
                        },
                    }
                    rest = &rest[close + 1..];
                }
                path.push_str(rest);
                if let Some(name) = unknown {
                    return Err(fail(
                        Diag::new(op.id.clone(), Category::ValidationFailed)
                            .failed_parameter("operation")
                            .expected("a valid operation descriptor")
                            .remediation(format!(
                                "The path template has a placeholder {{{name}}} with no path parameter; regenerate the SDK."
                            ))
                            .build(),
                    ));
                }
                if let Some(p) = missing {
                    return Err(self.validation(
                        op,
                        &Self::key_path(&p.name),
                        None,
                        &format!("a value for path parameter {}", p.wire),
                        format!("Pass {}.", p.name),
                    ));
                }
                // A path segment that is empty or a dot segment (`.`, `..`,
                // also percent-encoded) would be removed or resolved by URL
                // parsing, so the request would reach another resource
                // (DELETE /items/.. is DELETE /): refuse it before anything is
                // built or confirmed.
                let template: Vec<&str> = op.path.split('/').collect();
                let built: Vec<&str> = path.split('/').collect();
                if template.len() == built.len() {
                    for (segment, tmpl) in built.iter().zip(&template) {
                        if !tmpl.contains('{') || !is_dot_segment(segment) {
                            continue;
                        }
                        let wire = tmpl
                            .split_once('{')
                            .and_then(|(_, r)| r.split_once('}'))
                            .map(|(w, _)| w);
                        let p = params.iter().find(|x| {
                            x.location == ParamLocation::Path
                                && (Some(x.wire.as_str()) == wire || Some(x.name.as_str()) == wire)
                        });
                        let shown = match p {
                            Some(p) => args.get(&p.name).cloned().unwrap_or(Value::Null),
                            None => Value::String((*segment).to_owned()),
                        };
                        return Err(self.validation(
                            op,
                            &p.map(|p| Self::key_path(&p.name)).unwrap_or_default(),
                            Some(&shown),
                            "a non-empty path segment other than . and ..",
                            format!(
                                "Pass {} as the identifier of one resource; \"{segment}\" would change the request path.",
                                p.map_or("the path parameter", |p| p.name.as_str())
                            ),
                        ));
                    }
                }
                let mut parts: Vec<String> = Vec::new();
                let mut shown: Vec<String> = Vec::new();
                for p in &params {
                    if p.location != ParamLocation::Query {
                        continue;
                    }
                    let Some(value) = args.get(&p.name) else {
                        continue;
                    };
                    let serialized = serialize_query_param(p, value);
                    if p.sensitive {
                        shown.extend(
                            serialized.iter().map(|s| {
                                format!("{}={REDACTED}", s.split('=').next().unwrap_or(""))
                            }),
                        );
                    } else {
                        shown.extend(serialized.iter().cloned());
                    }
                    parts.extend(serialized);
                }
                for (name, value) in &plan.query {
                    parts.push(format!(
                        "{}={}",
                        encode_component(name),
                        encode_component(value)
                    ));
                    shown.push(format!("{}={REDACTED}", encode_component(name)));
                }
                let root = base.trim_end_matches('/');
                let join = |list: &[String]| {
                    if list.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "{}{}",
                            if path.contains('?') { '&' } else { '?' },
                            list.join("&")
                        )
                    }
                };
                (
                    format!("{root}{path}{}", join(&parts)),
                    format!("{root}{path}{}", join(&shown)),
                )
            }
        };
        if url_override.is_some() && !plan.query.is_empty() {
            let mut target = Url::parse(&url).map_err(|_| bad_base())?;
            for (name, value) in &plan.query {
                if !has_query_param(&target, name) {
                    set_query_param(&mut target, name, value);
                }
            }
            url = target.to_string();
            let mut shown = Url::parse(&display).map_err(|_| bad_base())?;
            for (name, _) in &plan.query {
                set_query_param(&mut shown, name, REDACTED);
            }
            display = shown.to_string();
        }
        match Url::parse(&url) {
            Ok(parsed)
                if matches!(parsed.scheme(), "http" | "https")
                    && parsed.username().is_empty()
                    && parsed.password().is_none()
                    && parsed.host_str().is_some() => {}
            _ => return Err(bad_base()),
        }
        Ok((url, display))
    }

    fn build_headers(
        &self,
        op: &OperationDescriptor,
        args: &Map<String, Value>,
        opts: &crate::types::CallOptions,
        plan: &AuthPlan,
        headers: &mut HeaderBag,
    ) {
        let mut accept: Vec<&str> = Vec::new();
        for r in &op.responses {
            if r.kind == ResponseKind::Success
                && let Some(media) = r.media_type.as_deref()
                && !accept.contains(&media)
            {
                accept.push(media);
            }
        }
        if !accept.is_empty() {
            headers.set("Accept", accept.join(", "), false);
        }
        let api = &self.inner.api;
        let api_name = if api.name.is_empty() {
            "api"
        } else {
            &api.name
        };
        let api_version = if api.version.is_empty() {
            "0"
        } else {
            &api.version
        };
        headers.set(
            "X-Tungsten-Runtime",
            format!(
                "tungsten-rs/{} {api_name}-sdk/{api_version}",
                crate::VERSION
            ),
            false,
        );
        headers.set("X-Tungsten-Operation", op.id.clone(), false);
        let tungsten = if api.tungsten_version.is_empty() {
            crate::VERSION
        } else {
            &api.tungsten_version
        };
        headers.set(
            "User-Agent",
            format!("{api_name}-sdk/{api_version} tungsten/{tungsten} (rust)"),
            false,
        );
        for extra in [&self.inner.headers, &opts.headers] {
            for (name, value) in extra {
                headers.set(name, value.clone(), looks_sensitive(name));
            }
        }
        let mut cookies: Vec<String> = Vec::new();
        if let Some(existing) = headers.get("Cookie") {
            cookies.push(existing.to_owned());
        }
        for p in arg_params(op) {
            let Some(value) = args.get(&p.name).filter(|v| !v.is_null()) else {
                continue;
            };
            match p.location {
                ParamLocation::Header => {
                    headers.set(&p.wire, serialize_header_param(p, value), p.sensitive);
                }
                ParamLocation::Cookie => cookies.push(serialize_cookie_param(p, value)),
                _ => {}
            }
        }
        for (name, value) in &plan.cookies {
            cookies.push(format!("{name}={value}"));
        }
        // Cookies are session material: the whole header is always redacted.
        if !cookies.is_empty() {
            headers.set("Cookie", cookies.join("; "), true);
        }
    }

    fn idempotency_key(
        &self,
        op: &OperationDescriptor,
        args: &Map<String, Value>,
        opts: &crate::types::CallOptions,
        purpose: Purpose,
        hash_material: Option<&[u8]>,
    ) -> Step<Option<String>> {
        let supplied = opts.idempotency_key.clone();
        match op.agent.idempotency.policy {
            IdempotencyKind::ContentIdentity => Ok(None),
            IdempotencyKind::ContentHash => Ok(hash_material.map(sha256_hex)),
            IdempotencyKind::CallerOwned => Ok(supplied),
            IdempotencyKind::None => Ok(
                if op
                    .params
                    .iter()
                    .any(|p| p.role == ParamRole::IdempotencyKey)
                {
                    supplied
                } else {
                    None
                },
            ),
            IdempotencyKind::Auto => {
                if supplied.is_some() {
                    return Ok(supplied);
                }
                if purpose != Purpose::Call {
                    return Ok(None);
                }
                let refuse = |why: &str| {
                    fail(
                        Diag::new(op.id.clone(), Category::TransportFailed)
                            .retryable(Retryable::Never)
                            .remediation(format!(
                                "The idempotency store failed ({why}), so the call was not sent rather than risk a second key for the same intent. Fix or replace ClientOptions.idempotency_store."
                            ))
                            .build(),
                    )
                };
                let logical = sha256_hex(canonical_json(&Value::Object(args.clone())).as_bytes());
                let store = &self.inner.store;
                let _atomic = crate::idempotency::lock(&self.inner.key_lock);
                if let Some(existing) = store.get(&op.id, &logical).filter(|k| !k.is_empty()) {
                    return Ok(Some(existing));
                }
                let key = uuid_v4().ok_or_else(|| refuse("no source of randomness"))?;
                store.put(&op.id, &logical, &key);
                if store.get(&op.id, &logical).as_deref() != Some(key.as_str()) {
                    return Err(refuse("the key could not be stored"));
                }
                Ok(Some(key))
            }
        }
    }
}

fn location_name(location: ParamLocation) -> &'static str {
    match location {
        ParamLocation::Path => "path",
        ParamLocation::Query => "query",
        ParamLocation::Header => "header",
        ParamLocation::Cookie => "cookie",
    }
}
