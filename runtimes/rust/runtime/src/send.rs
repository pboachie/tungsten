// SPDX-License-Identifier: Apache-2.0
//! Sending with tier-aware retries, classification of the answer into a
//! result or an envelope, and OAuth2 client-credentials tokens.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Map, Value};
use url::Url;

use crate::auth::{BoxFuture, OAuthClient, TokenSource};
use crate::classify::{
    CallContext, OutcomeCheck, UnknownFields, change_of, classify_error, decode_body, is_mutation,
    match_response, outcome_unknown, request_id_of, verify_callable,
};
use crate::client::{CachedToken, ClientCore};
use crate::envelope::{Diag, scrub_diagnostic};
use crate::expr::{describe_predicate, evaluate_expr, resolve_ref};
use crate::helpers::{sent_arguments, with_arguments, with_wire_names};
use crate::idempotency::has_replay_protection;
use crate::prepare::{Prepared, Step, fail, with_auth_query};
use crate::serialize::Payload;
use crate::serialize::encode_component;
use crate::stream::{Sent, StreamStart};
use crate::transport::{AttemptOutcome, AttemptRequest, BodyFailure, attempt, parse_retry_after};
use crate::types::{
    CallOptions, Category, Diagnostic, Error, HttpMethod, Jitter, OperationDescriptor,
    ParamLocation, RequestContext, Response, ResponseContext, ResponseKind, ResponseMeta,
    Retryable, Safety, ValidateResponses,
};
use crate::util::{envelope_value, get_path, get_path_str, looks_sensitive, redact_paths};

/// Same-origin redirects followed for one read attempt.
const MAX_REDIRECTS: usize = 5;

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn retryable_category(category: Category) -> bool {
    matches!(
        category,
        Category::RateLimited
            | Category::UpstreamUnavailable
            | Category::TransportFailed
            | Category::OutcomeUnknown
    )
}

const EMPTY_PAYLOAD: &Payload = &Payload::Empty;

impl ClientCore {
    pub(crate) async fn send(
        &self,
        prepared: &Prepared<'_>,
        opts: &CallOptions,
    ) -> Step<Response<Option<Value>>> {
        match self.send_with(prepared, opts, false).await? {
            Sent::Response(response) => Ok(response),
            Sent::Stream(_) => Err(fail(
                Diag::new(prepared.op.id.clone(), Category::UnexpectedResponse)
                    .remediation(
                        "The SDK received an event stream it did not ask for. This is a bug in the runtime; report it. If the call may have been sent, check its effect before repeating it.",
                    )
                    .retryable(Retryable::Never)
                    .build(),
            )),
        }
    }

    /// Send with retries. With `stream`, a 2xx `text/event-stream` answer is
    /// returned unread as [`Sent::Stream`]; every other answer is classified
    /// as usual.
    pub(crate) async fn send_with(
        &self,
        prepared: &Prepared<'_>,
        opts: &CallOptions,
        stream: bool,
    ) -> Step<Sent> {
        let op = prepared.op;
        let retries = self.retry_options(op);
        let mutation = is_mutation(op);
        let protected = has_replay_protection(op, prepared.key.as_deref());
        let check = if mutation && !protected {
            self.outcome_check(op, &prepared.args)
        } else {
            None
        };
        let timeout = self.timeout(opts);
        let real_headers: Vec<(String, String)> = prepared
            .headers
            .pairs()
            .map(|(n, v)| (n.to_owned(), v.to_owned()))
            .collect();
        let mut attempts: u32 = 0;
        loop {
            attempts += 1;
            let mut ctx = RequestContext {
                operation: op.id.clone(),
                attempt: attempts,
                method: prepared.method,
                url: prepared.display_url.clone(),
                headers: prepared.headers.to_map(true),
                body: match &prepared.payload {
                    Payload::Empty => None,
                    _ => Some(prepared.display_body.clone()),
                },
            };
            for m in &self.inner.middleware {
                m.on_request(&ctx);
            }
            // Redirects are never followed by the HTTP client: it would
            // forward custom credential headers (API keys, CSRF headers) to
            // another origin. A read follows same-origin redirects here; a
            // mutation follows none.
            let mut url = prepared.url.clone();
            let mut method = prepared.method;
            let mut body: &Payload = &prepared.payload;
            let mut headers = real_headers.clone();
            let mut outcome = attempt(
                &self.inner.http,
                &AttemptRequest {
                    url: &url,
                    method,
                    headers: &headers,
                    body,
                    timeout,
                    stream,
                },
            )
            .await;
            let mut hops = 0;
            while !mutation && hops < MAX_REDIRECTS {
                let AttemptOutcome::Response {
                    status,
                    headers: answer,
                    ..
                } = &outcome
                else {
                    break;
                };
                if !is_redirect(*status) {
                    break;
                }
                let status = *status;
                let Some((next, shown)) = self.redirect_target(
                    prepared,
                    &url,
                    answer.get("location").map(String::as_str),
                ) else {
                    break;
                };
                let rewrite = !matches!(status, 307 | 308)
                    && !matches!(method, HttpMethod::Get | HttpMethod::Head);
                url = next;
                if rewrite {
                    method = HttpMethod::Get;
                    body = EMPTY_PAYLOAD;
                    headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-type"));
                }
                ctx.url = shown;
                ctx.method = method;
                outcome = attempt(
                    &self.inner.http,
                    &AttemptRequest {
                        url: &url,
                        method,
                        headers: &headers,
                        body,
                        timeout,
                        stream,
                    },
                )
                .await;
                hops += 1;
            }
            let call_ctx = CallContext {
                api: &self.inner.api,
                op,
                key: prepared.key.as_deref(),
                key_header: &prepared.key_header,
                attempts,
                check: check.as_ref(),
            };
            let error = match self.classify_response(&call_ctx, prepared, outcome, &ctx, timeout) {
                Ok(sent) => return Ok(sent),
                Err(error) => error,
            };
            let Error {
                diagnostic,
                partial,
            } = error;
            let diagnostic = scrub_diagnostic(*diagnostic, &prepared.secrets);
            let done = |d: Diagnostic, p: Option<Box<Value>>| Error {
                diagnostic: Box::new(d),
                partial: p,
            };
            if attempts > retries.max || !retryable_category(diagnostic.category) {
                return Err(done(diagnostic, partial));
            }
            if matches!(
                diagnostic.retryable,
                Retryable::Never | Retryable::AfterRemediation
            ) || (mutation && !protected)
            {
                return Err(done(diagnostic, partial));
            }
            let delay_ms = match diagnostic.retry_after_ms {
                Some(after) if retries.honor_retry_after => {
                    if after as f64 > retries.max_ms {
                        return Err(done(diagnostic, partial));
                    }
                    after as f64
                }
                _ => {
                    let exponent = i32::try_from(attempts - 1).unwrap_or(i32::MAX);
                    let ceiling = retries.max_ms.min(retries.base_ms * 2f64.powi(exponent));
                    let random = self.random();
                    match retries.jitter {
                        Jitter::None => ceiling,
                        Jitter::Equal => ceiling / 2.0 + random * ceiling / 2.0,
                        Jitter::Full => random * ceiling,
                    }
                }
            };
            for m in &self.inner.middleware {
                m.on_retry(&ctx, &diagnostic);
            }
            tokio::time::sleep(crate::util::duration_from_ms(delay_ms)).await;
        }
    }

    /// The URL a read's redirect points to, when it stays on the request's
    /// origin (scheme, host and port), with the auth query re-applied; `None`
    /// for any other origin, which is reported instead of followed.
    fn redirect_target(
        &self,
        prepared: &Prepared<'_>,
        from: &str,
        location: Option<&str>,
    ) -> Option<(String, String)> {
        let location = location.filter(|l| !l.is_empty())?;
        let base = Url::parse(from).ok()?;
        let target = base.join(location).ok()?;
        if target.origin() != base.origin()
            || !target.username().is_empty()
            || target.password().is_some()
        {
            return None;
        }
        Some(with_auth_query(
            target,
            &prepared.auth_query,
            &prepared.hidden_query,
        ))
    }

    fn classify_response(
        &self,
        call_ctx: &CallContext<'_>,
        prepared: &Prepared<'_>,
        outcome: AttemptOutcome,
        ctx: &RequestContext,
        timeout: Duration,
    ) -> Step<Sent> {
        let op = prepared.op;
        let mutation = is_mutation(op);
        let attempts = call_ctx.attempts;
        let (status, headers, body, body_failure) = match outcome {
            AttemptOutcome::NotSent(detail) => {
                return Err(fail(
                    Diag::new(op.id.clone(), Category::TransportFailed)
                        .remediation(format!(
                            "The request could not be delivered ({detail}), so the server did not receive it. Check base_url and network access, then call again."
                        ))
                        .attempts(attempts)
                        .build(),
                ));
            }
            AttemptOutcome::Lost(detail) => {
                let cause = format!("The connection failed before a response arrived ({detail}).");
                return Err(fail(if mutation {
                    outcome_unknown(call_ctx, &cause, UnknownFields::default())
                } else {
                    Diag::new(op.id.clone(), Category::TransportFailed)
                        .remediation(format!(
                            "{cause} This read has no side effects; call again."
                        ))
                        .attempts(attempts)
                        .build()
                }));
            }
            AttemptOutcome::Timeout => {
                let ms = timeout.as_millis();
                return Err(fail(if mutation {
                    outcome_unknown(
                        call_ctx,
                        &format!("No response arrived within {ms} ms."),
                        UnknownFields::default(),
                    )
                } else {
                    Diag::new(op.id.clone(), Category::UpstreamUnavailable)
                        .remediation(format!(
                            "No response arrived within {ms} ms. This read has no side effects; call again later or with a larger `timeout`."
                        ))
                        .attempts(attempts)
                        .build()
                }));
            }
            AttemptOutcome::Response {
                status,
                headers,
                body,
                body_failure,
            } => (status, headers, body, body_failure),
            AttemptOutcome::Streaming {
                status,
                headers,
                body,
            } => {
                for m in &self.inner.middleware {
                    m.on_response(
                        ctx,
                        &ResponseContext {
                            status,
                            headers: headers.clone(),
                        },
                    );
                }
                let request_id = request_id_of(&headers);
                return Ok(Sent::Stream(StreamStart {
                    meta: ResponseMeta {
                        status,
                        headers,
                        request_id,
                        attempts,
                    },
                    body,
                }));
            }
        };
        for m in &self.inner.middleware {
            m.on_response(
                ctx,
                &ResponseContext {
                    status,
                    headers: headers.clone(),
                },
            );
        }
        let request_id = request_id_of(&headers);
        let retry_after =
            parse_retry_after(headers.get("retry-after").map(String::as_str), self.now());
        let declared = match_response(&op.responses, status);
        let success = (200..=299).contains(&status)
            || (declared.is_some_and(|d| d.kind == ResponseKind::Success)
                && (100..=399).contains(&status));
        let hint = op
            .agent
            .verify
            .as_ref()
            .map(|v| format!("Call {} to read the current state.", v.operation));

        if success && body.is_none() {
            let failure = match body_failure {
                Some(BodyFailure::Timeout) => "timeout",
                _ => "broken",
            };
            if !mutation {
                return Err(fail(
                    Diag::new(op.id.clone(), Category::UpstreamUnavailable)
                        .http_status(Some(status))
                        .request_id(request_id)
                        .retryable(Retryable::AfterDelay)
                        .remediation(format!(
                            "The response body was cut off ({failure}). This read has no side effects; call again."
                        ))
                        .attempts(attempts)
                        .build(),
                ));
            }
            return Err(fail(
                Diag::new(op.id.clone(), Category::UnexpectedResponse)
                    .http_status(Some(status))
                    .request_id(request_id)
                    .retryable(Retryable::Never)
                    .remediation(format!(
                        "The server accepted {} (HTTP {status}) but the response body was cut off ({failure}). The call took effect; do not repeat it.",
                        op.id
                    ))
                    .next_action(hint)
                    .attempts(attempts)
                    .build(),
            ));
        }
        let bytes = body.unwrap_or_default();
        if !success {
            if (300..=399).contains(&status) {
                let to = headers
                    .get("location")
                    .map(|l| format!(" to {}", crate::util::take_units(l, 200)))
                    .unwrap_or_default();
                let remediation = if mutation {
                    format!(
                        "The server answered with a redirect{to}. Redirects are never followed on mutations; check base_url (scheme, host, trailing slash). Whether the call took effect is unknown only if the server applied it before redirecting."
                    )
                } else {
                    format!(
                        "The server answered with a redirect{to} that leaves the API's origin (or too many redirects). It is not followed, so credentials never leave the API's host; check base_url."
                    )
                };
                return Err(fail(
                    Diag::new(op.id.clone(), Category::UnexpectedResponse)
                        .http_status(Some(status))
                        .request_id(request_id)
                        .remediation(remediation)
                        .next_action(if mutation { hint } else { None })
                        .attempts(attempts)
                        .build(),
                ));
            }
            let decoded = decode_body(
                &bytes,
                &headers,
                declared.and_then(|d| d.media_type.as_deref()),
            );
            return Err(fail(classify_error(
                call_ctx,
                status,
                &headers,
                &decoded,
                retry_after,
            )));
        }

        let decoded = decode_body(
            &bytes,
            &headers,
            declared.and_then(|d| d.media_type.as_deref()),
        );
        let mode = self.inner.validate_responses;
        let sensitive = &op.agent.sensitive_response_fields;
        let after_effect = if mutation {
            " The call took effect; do not repeat it."
        } else {
            ""
        };
        let mut problem: Option<Diagnostic> = None;
        if decoded.invalid_json {
            problem = Some(
                Diag::new(op.id.clone(), Category::UnexpectedResponse)
                    .http_status(Some(status))
                    .request_id(request_id.clone())
                    .failed_parameter("response")
                    .expected("a JSON body")
                    .remediation(format!(
                        "The success response announced JSON but did not parse.{after_effect}"
                    ))
                    .attempts(attempts)
                    .build(),
            );
        } else if mode != ValidateResponses::Off
            && let (Some(validator), Some(value)) = (&op.response, decoded.value.as_ref())
            && let crate::types::Validation::Invalid(issues) =
                crate::validate::judge(validator.as_ref(), value)
        {
            let issue = issues.first();
            let path: Vec<String> = issue
                .map(|i| {
                    i.path
                        .iter()
                        .map(|s| match s {
                            crate::types::PathSegment::Key(k) => k.clone(),
                            crate::types::PathSegment::Index(n) => n.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let message = issue.map_or("a valid response", |i| i.message.as_str());
            let path_text: String = path
                .iter()
                .map(|s| {
                    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
                        format!("[{s}]")
                    } else {
                        format!(".{s}")
                    }
                })
                .collect();
            let kept = if mutation && mode == ValidateResponses::Strict {
                " The decoded body is in the result's partial; store any value shown only once from it before anything else."
            } else {
                ""
            };
            let redacted = redact_paths(value, sensitive);
            let received = get_path(Some(&redacted), &path)
                .cloned()
                .unwrap_or(Value::Null);
            problem = Some(
                    Diag::new(op.id.clone(), Category::UnexpectedResponse)
                        .http_status(Some(status))
                        .request_id(request_id.clone())
                        .failed_parameter(format!("response{path_text}"))
                        .received_value(envelope_value(
                            &received,
                            path.iter().any(|s| looks_sensitive(s)),
                        ))
                        .expected(message)
                        .remediation(format!(
                            "The success response does not match the API description at response{path_text} ({message}).{after_effect}{kept}"
                        ))
                        .attempts(attempts)
                        .build(),
                );
        }
        if let Some(problem) = problem {
            let scrubbed = scrub_diagnostic(problem, &prepared.secrets);
            // The effect of a mutation happened: its body is never dropped,
            // since it can hold the only copy of a value (a one-time signing
            // secret).
            if mode == ValidateResponses::Strict {
                return Err(if mutation && !decoded.invalid_json {
                    Error {
                        diagnostic: Box::new(scrubbed),
                        partial: decoded.value.map(Box::new),
                    }
                } else {
                    fail(scrubbed)
                });
            }
            self.emit(&scrubbed);
        }
        Ok(Sent::Response(Response {
            value: decoded.value,
            meta: ResponseMeta {
                status,
                headers,
                request_id,
                attempts,
            },
            verification: None,
        }))
    }

    /// How to find out whether `op` (a mutation without replay protection)
    /// took effect after its answer was lost: its verification hook when it
    /// can be called without the lost response, else a registered read of the
    /// same resource. `None` when neither exists.
    pub(crate) fn outcome_check(
        &self,
        op: &OperationDescriptor,
        args: &Map<String, Value>,
    ) -> Option<OutcomeCheck> {
        let sent = sent_arguments(op, args);
        if let Some(hook) = &op.agent.verify
            && !hook.operation.is_empty()
            && verify_callable(hook)
        {
            let mut scope = Map::new();
            scope.insert("args".to_owned(), Value::Object(with_wire_names(op, args)));
            let hook_args = if hook.args.is_null() {
                Value::Object(Map::new())
            } else {
                hook.args.clone()
            };
            let evaluated = evaluate_expr(&hook_args, &scope, 0).unwrap_or(Value::Null);
            let call = format!("{}{}", hook.operation, with_arguments(&evaluated));
            // `expect` is what a successful call leaves behind; references to
            // the call's arguments are known, references to its response are
            // not.
            let mut lost: Vec<String> = Vec::new();
            let expect = match &hook.expect {
                Value::Object(_) => resolve_lost(&hook.expect, &scope, &mut lost, 0),
                _ => Value::Object(Map::new()),
            };
            let fields: Vec<&str> = expect
                .as_object()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            let shows = if fields.is_empty() {
                format!("whether it shows {}", change_of(op))
            } else if !lost.is_empty() {
                let mut unique: Vec<&str> = Vec::new();
                for item in &lost {
                    if !unique.contains(&item.as_str()) {
                        unique.push(item);
                    }
                }
                format!(
                    "whether {} holds what {} creates, matching the arguments you sent{sent} (its {} was in the lost response)",
                    fields.join(" and "),
                    op.id,
                    unique.join(", ")
                )
            } else {
                format!("whether {}", describe_predicate(&expect))
            };
            return Some(OutcomeCheck { call, shows });
        }
        let read = self.resource_read(op, args)?;
        Some(OutcomeCheck {
            call: format!("{}{}", read.0, with_arguments(&Value::Object(read.1))),
            shows: format!("whether it shows {}", change_of(op)),
        })
    }

    /// A registered read of the resource `op` changes: a `GET` whose path is
    /// the longest prefix of `op`'s path (`/v1/items/{id}/cancel` to
    /// `/v1/items/{id}`, then `/v1/items`), never the API root, whose
    /// required parameters are all path parameters `op` was called with.
    /// Candidates on one path are taken in id order. rpc methods have no
    /// resource paths.
    fn resource_read(
        &self,
        op: &OperationDescriptor,
        args: &Map<String, Value>,
    ) -> Option<(String, Map<String, Value>)> {
        if op.rpc.is_some() {
            return None;
        }
        let mut known: BTreeMap<&str, &Value> = BTreeMap::new();
        for p in &op.params {
            if p.location == ParamLocation::Path
                && let Some(value) = args.get(&p.name).filter(|v| !v.is_null())
            {
                known.insert(&p.wire, value);
            }
        }
        let segments: Vec<&str> = op.path.split('/').collect();
        let is_root = |s: &str| {
            s.is_empty()
                || s == "api"
                || (s.len() > 1
                    && s[..1].eq_ignore_ascii_case("v")
                    && s[1..]
                        .split('.')
                        .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
        };
        let reads: Vec<std::sync::Arc<OperationDescriptor>> =
            crate::idempotency::lock(&self.inner.registry)
                .values()
                .filter(|r| {
                    r.id != op.id
                        && r.method == HttpMethod::Get
                        && r.agent.safety == Safety::ReadOnly
                        && r.rpc.is_none()
                })
                .cloned()
                .collect();
        for end in (1..=segments.len()).rev() {
            let prefix = &segments[..end];
            if prefix.iter().all(|s| is_root(s)) {
                break;
            }
            let path = prefix.join("/");
            for read in &reads {
                if read.path != path {
                    continue;
                }
                let params = crate::helpers::arg_params(read);
                let required_ok = params.iter().filter(|p| p.required).all(|p| {
                    p.location == ParamLocation::Path && known.contains_key(p.wire.as_str())
                });
                if !required_ok {
                    continue;
                }
                let mut read_args = Map::new();
                for p in params {
                    if p.location == ParamLocation::Path
                        && let Some(value) = known.get(p.wire.as_str())
                    {
                        read_args.insert(p.wire.clone(), (*value).clone());
                    }
                }
                return Some((read.id.clone(), read_args));
            }
        }
        None
    }

    /// Fetch (and cache) an OAuth2 client-credentials token.
    async fn oauth_token(&self, client: OAuthClient<'_>) -> std::result::Result<String, String> {
        let mut cache = self.inner.oauth_tokens.lock().await;
        let real_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        if let Some(cached) = cache.get(client.name)
            && cached.expires_at > real_now + 30_000
        {
            return Ok(cached.token.clone());
        }
        let name = client.name;
        let mut form = vec!["grant_type=client_credentials".to_owned()];
        if !client.scopes.is_empty() {
            form.push(format!(
                "scope={}",
                crate::serialize::encode_form_component(&client.scopes.join(" "))
            ));
        }
        let basic = crate::serialize::base64_text(&format!(
            "{}:{}",
            encode_component(client.client_id),
            encode_component(client.client_secret)
        ));
        let headers = vec![
            (
                "Content-Type".to_owned(),
                "application/x-www-form-urlencoded".to_owned(),
            ),
            ("Accept".to_owned(), "application/json".to_owned()),
            ("Authorization".to_owned(), format!("Basic {basic}")),
        ];
        let body = Payload::Bytes(form.join("&").into_bytes());
        let outcome = attempt(
            &self.inner.http,
            &AttemptRequest {
                url: client.token_url,
                method: HttpMethod::Post,
                headers: &headers,
                body: &body,
                timeout: self.inner.timeout.max(Duration::from_millis(1)),
                stream: false,
            },
        )
        .await;
        let AttemptOutcome::Response {
            status,
            headers,
            body: Some(bytes),
            ..
        } = outcome
        else {
            return Err(format!(
                "The OAuth2 token endpoint for {name} could not be reached; check network access and the token URL."
            ));
        };
        if !(200..=299).contains(&status) {
            return Err(format!(
                "The OAuth2 token endpoint for {name} answered HTTP {status}; check client_id, client_secret and scopes."
            ));
        }
        let decoded = decode_body(&bytes, &headers, Some("application/json"));
        let token = match get_path_str(decoded.value.as_ref(), "access_token") {
            Some(Value::String(token)) if !token.is_empty() => token.clone(),
            _ => {
                return Err(format!(
                    "The OAuth2 token endpoint for {name} returned no access_token."
                ));
            }
        };
        let lifetime = get_path_str(decoded.value.as_ref(), "expires_in")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .map_or(3_600_000, |n| (n * 1000.0).max(0.0) as u64);
        cache.insert(
            name.to_owned(),
            CachedToken {
                token: token.clone(),
                expires_at: real_now.saturating_add(lifetime),
            },
        );
        Ok(token)
    }
}

impl TokenSource for ClientCore {
    fn token<'a>(
        &'a self,
        client: OAuthClient<'a>,
    ) -> BoxFuture<'a, std::result::Result<String, String>> {
        Box::pin(self.oauth_token(client))
    }
}

/// `node` with references resolved against the call's arguments; references
/// to the lost response are shown as `<response...>` and recorded in `lost`.
fn resolve_lost(
    node: &Value,
    scope: &Map<String, Value>,
    lost: &mut Vec<String>,
    depth: usize,
) -> Value {
    if depth > 32 {
        return Value::Null;
    }
    match node {
        Value::String(text) if text.starts_with("$response") => {
            let rest = &text["$response".len()..];
            let rest = rest.strip_prefix('.').unwrap_or(rest);
            lost.push(if rest.is_empty() {
                "body".to_owned()
            } else {
                rest.to_owned()
            });
            Value::String(format!("<{}>", &text[1..]))
        }
        Value::String(text) if text.starts_with('$') => {
            resolve_ref(text, scope).unwrap_or_else(|| Value::String(format!("<{}>", &text[1..])))
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| resolve_lost(i, scope, lost, depth + 1))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), resolve_lost(v, scope, lost, depth + 1)))
                .collect(),
        ),
        other => other.clone(),
    }
}
