// SPDX-License-Identifier: Apache-2.0
//! `ClientCore`: validate, authenticate, apply idempotency and confirmation
//! rules, send with retries, classify the response (the signatures below are
//! stable; changes are additive).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use crate::idempotency::{MemoryIdempotencyStore, lock};
use crate::oauth::{Gate, MemoryTokenStore, TokenStore};
use crate::oauth_attempt::AuthorizationState;
use crate::telemetry::{Pending, PendingKind};
use crate::types::{
    ApiDescriptor, AuthConfig, CallOptions, ClientOptions, DiagnosticSink, IdempotencyStore,
    Jitter, MacroDescriptor, Middleware, OperationDescriptor, Outcome, PartialRetryOptions,
    Predicate, PreviewResult, Response, Result, ValidateResponses,
};
use crate::util::random_bytes;

pub use crate::pages::{Pages, TypedPages};

/// The client could not be built (an unusable base URL, TLS roots, or a
/// credential that is not valid for its scheme's shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// The body a [`ClientCore::poll`] ended with.
#[derive(Debug, Clone, PartialEq)]
pub struct Polled {
    pub body: Option<Value>,
    pub timed_out: bool,
}

/// Retry policy of one operation after merging the layers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Retry {
    pub max: u32,
    pub base_ms: f64,
    pub max_ms: f64,
    pub jitter: Jitter,
    pub honor_retry_after: bool,
}

const DEFAULT_RETRY: Retry = Retry {
    max: 3,
    base_ms: 200.0,
    max_ms: 5000.0,
    jitter: Jitter::Full,
    honor_retry_after: true,
};

#[derive(Debug, Clone)]
pub(crate) struct CachedToken {
    pub token: String,
    pub expires_at: u64,
}

pub(crate) struct Inner {
    pub allow_insecure_http: bool,
    pub redirects_disabled: bool,
    pub api: Arc<ApiDescriptor>,
    pub auth: AuthConfig,
    pub headers: BTreeMap<String, String>,
    pub base_url: Option<String>,
    pub timeout: Duration,
    pub deadline: Option<Duration>,
    pub max_response_bytes: Option<u64>,
    pub max_oauth_token_response_bytes: u64,
    pub max_event_bytes: usize,
    pub max_collect_bytes: usize,
    pub max_collect_time: Duration,
    pub max_reconnects: u32,
    pub reconnect_max: Duration,
    pub idle_timeout: Option<Duration>,
    pub retries: PartialRetryOptions,
    pub store: Arc<dyn IdempotencyStore>,
    pub middleware: Vec<Arc<dyn Middleware>>,
    pub validate_responses: ValidateResponses,
    pub on_diagnostic: Option<DiagnosticSink>,
    pub confirmation_key: Vec<u8>,
    pub http: reqwest::Client,
    pub now: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    pub random: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub scheduler: Option<crate::RequestScheduler>,
    pub tenant: Option<String>,
    pub telemetry: Option<Arc<crate::telemetry::Telemetry>>,
    pub registry: Mutex<BTreeMap<String, Arc<OperationDescriptor>>>,
    /// Makes "find or create the key of a logical call" atomic, so concurrent
    /// identical calls share one key.
    /// Confirmation tokens already used to send, with the replay protection of
    /// their first use, until they expire.
    pub used_tokens: Mutex<HashMap<String, (Option<String>, u64)>>,
    pub oauth_tokens: tokio::sync::Mutex<HashMap<String, CachedToken>>,
    pub oauth_refreshes:
        tokio::sync::Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// The tokens of the `authorizationCode` schemes.
    pub token_store: Arc<dyn TokenStore>,
    /// The refresh gate of each `authorizationCode` scheme.
    pub oauth_gates: Mutex<HashMap<String, Arc<Gate>>>,
    pub oauth_authorization: Mutex<AuthorizationState>,
}

/// The engine behind every generated client. Cheap to clone (shares one
/// connection pool, one confirmation key and one idempotency store).
/// Never panics and never returns an error value for API or transport
/// failures other than through [`Result`]: a failed call is
/// `Err(Error { diagnostic, partial })`.
#[derive(Clone)]
pub struct ClientCore {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for ClientCore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientCore")
            .field("api", &self.inner.api.name)
            .field("base_url", &self.inner.base_url)
            .finish_non_exhaustive()
    }
}

fn config_error(message: impl Into<String>) -> ConfigError {
    ConfigError {
        message: message.into(),
    }
}

impl ClientCore {
    pub fn new(
        api: ApiDescriptor,
        options: ClientOptions,
    ) -> std::result::Result<Self, ConfigError> {
        let base_url = options
            .base_url
            .clone()
            .or_else(|| api.servers.first().cloned());
        if let Some(base) = base_url.as_deref().filter(|b| !b.is_empty()) {
            match Url::parse(base) {
                Ok(url)
                    if matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none() => {}
                _ => {
                    return Err(config_error(
                        "the base URL is not a valid absolute http(s) URL without credentials",
                    ));
                }
            }
        }
        let redirects_disabled = api.redirects == crate::types::RedirectPolicy::None
            || options.redirects == Some(crate::types::RedirectPolicy::None);
        if redirects_disabled && options.http_client.is_some() {
            return Err(config_error(
                "redirects=none requires the runtime-owned HTTP client; omit http_client because a supplied reqwest client has opaque redirect configuration",
            ));
        }
        let http = match options.http_client.clone() {
            Some(client) => client,
            None => reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| config_error(format!("the HTTP client could not be built: {e}")))?,
        };
        let confirmation_key = match options.confirmation_key.clone() {
            Some(key) if key.len() >= 32 => key,
            Some(_) => {
                return Err(config_error(
                    "confirmation_key must contain at least 32 bytes; omit it to generate a random key",
                ));
            }
            None => random_bytes::<32>()
                .ok_or_else(|| config_error("no source of randomness for the confirmation key"))?
                .to_vec(),
        };
        let store: Arc<dyn IdempotencyStore> = options
            .idempotency_store
            .clone()
            .unwrap_or_else(|| Arc::new(MemoryIdempotencyStore::new()));
        let token_store: Arc<dyn TokenStore> = options
            .token_store
            .clone()
            .unwrap_or_else(|| Arc::new(MemoryTokenStore::new()));
        let core = ClientCore {
            inner: Arc::new(Inner {
                allow_insecure_http: options.allow_insecure_http,
                redirects_disabled,
                api: Arc::new(api),
                auth: options.auth,
                headers: options.headers,
                base_url,
                timeout: options.timeout,
                deadline: options.deadline,
                max_response_bytes: options.max_response_bytes.or(Some(4 * 1024 * 1024)),
                max_oauth_token_response_bytes: options.max_oauth_token_response_bytes,
                max_event_bytes: options.max_event_bytes.max(1),
                max_collect_bytes: options.max_collect_bytes.max(1),
                max_collect_time: options.max_collect_time.max(Duration::from_millis(1)),
                max_reconnects: options.max_reconnects.min(1000),
                reconnect_max: options.reconnect_max,
                idle_timeout: options
                    .idle_timeout
                    .map(|idle| idle.max(Duration::from_millis(1))),
                retries: options.retries,
                store,
                middleware: options.middleware,
                validate_responses: options.validate_responses,
                on_diagnostic: options.on_diagnostic,
                confirmation_key,
                http,
                now: options.now,
                random: options.random,
                scheduler: options.scheduler,
                tenant: options.tenant,
                telemetry: options
                    .telemetry
                    .map(|observer| Arc::new(crate::telemetry::Telemetry::new(observer))),
                registry: Mutex::new(BTreeMap::new()),
                used_tokens: Mutex::new(HashMap::new()),
                oauth_tokens: tokio::sync::Mutex::new(HashMap::new()),
                oauth_refreshes: tokio::sync::Mutex::new(HashMap::new()),
                token_store,
                oauth_gates: Mutex::new(HashMap::new()),
                oauth_authorization: Mutex::new(AuthorizationState::default()),
            }),
        };
        core.register(options.operations);
        Ok(core)
    }

    pub fn api(&self) -> &ApiDescriptor {
        &self.inner.api
    }

    /// Make operations resolvable by id (verification, endpoint previews,
    /// macros). Operations passed to `call` are registered too.
    pub fn register(&self, ops: impl IntoIterator<Item = Arc<OperationDescriptor>>) {
        let mut registry = lock(&self.inner.registry);
        for op in ops {
            if !op.id.is_empty() {
                registry.entry(op.id.clone()).or_insert(op);
            }
        }
    }

    pub fn operation(&self, id: &str) -> Option<Arc<OperationDescriptor>> {
        lock(&self.inner.registry).get(id).cloned()
    }

    pub(crate) fn register_one(&self, op: &OperationDescriptor) {
        let mut registry = lock(&self.inner.registry);
        if !op.id.is_empty() && !registry.contains_key(&op.id) {
            registry.insert(op.id.clone(), Arc::new(op.clone()));
        }
    }

    /// Validate, authenticate, apply idempotency and confirmation rules,
    /// send with retries, classify the response. `args` is the arguments
    /// object (a JSON object keyed by argument name).
    pub async fn call(&self, op: &OperationDescriptor, args: Value, opts: &CallOptions) -> Outcome {
        let span = self.start_span(op);
        let pending = Pending::new(span.as_ref(), PendingKind::Operation);
        let outcome = crate::telemetry::scoped(
            span.clone(),
            crate::verify::with_call_deadline(
                self,
                opts,
                self.call_with(op, &args, opts, None, None),
            ),
        )
        .await;
        pending.done();
        if let Some(span) = span {
            match &outcome {
                Ok(response) => span.end(None, false, Some(response.meta.status)),
                Err(error) => {
                    span.end(Some(&error.diagnostic), false, error.diagnostic.http_status)
                }
            }
        }
        outcome
    }

    /// Start the telemetry of one operation; `None` when telemetry is off.
    fn start_span(&self, op: &OperationDescriptor) -> Option<Arc<crate::telemetry::Span>> {
        let telemetry = self.inner.telemetry.as_ref()?;
        Some(telemetry.start(
            &self.inner.api.name,
            &op.id,
            crate::TelemetryEntry::Call,
            crate::classify::is_mutation(op),
        ))
    }

    /// [`ClientCore::call`] for a typed wrapper: the success body is decoded
    /// once, straight from its bytes into `T`, and judged by the operation's
    /// response validator on that value. The JSON form of the body is built
    /// only when it is needed: a body that is not JSON, does not decode as
    /// `T` or does not pass validation, and a call whose verification hook
    /// reads the response; those answer exactly what [`ClientCore::call`]
    /// followed by [`crate::decode`] answers (the same envelopes, with the
    /// body as `partial`).
    pub async fn call_typed<T: DeserializeOwned + Send + 'static>(
        &self,
        op: &OperationDescriptor,
        args: Value,
        opts: &CallOptions,
    ) -> Result<T> {
        let mut sink = crate::dispatch::TypedSink::<T>::new(op.response.clone());
        // The verification hook may read the response, so it needs the body's JSON form.
        let verifies = opts.verify && op.agent.verify.is_some();
        let span = self.start_span(op);
        let pending = Pending::new(span.as_ref(), PendingKind::Operation);
        let outcome = crate::telemetry::scoped(
            span.clone(),
            crate::verify::with_call_deadline(
                self,
                opts,
                self.call_with_tracked(
                    op,
                    &args,
                    opts,
                    None,
                    None,
                    (
                        &mut String::new(),
                        None,
                        (!verifies).then_some(&mut sink as &mut dyn crate::dispatch::BodySink),
                    ),
                ),
            ),
        )
        .await;
        let result = match (outcome, sink.take_value()) {
            (Ok(response), Some(value)) => Ok(Response {
                value,
                meta: response.meta,
                verification: response.verification,
            }),
            (outcome, _) => crate::dispatch::decode(&op.id, outcome),
        };
        pending.done();
        if let Some(span) = span {
            match &result {
                Ok(response) => span.end(None, false, Some(response.meta.status)),
                Err(error) => {
                    span.end(Some(&error.diagnostic), false, error.diagnostic.http_status)
                }
            }
        }
        result
    }

    /// Run the operation's preview mode: local rendering (no network), a
    /// dry-run header, or a preview endpoint. Mutating operations get a
    /// confirmation token bound to these exact arguments for five minutes.
    pub async fn preview(
        &self,
        op: &OperationDescriptor,
        args: Value,
        opts: &CallOptions,
    ) -> Result<PreviewResult> {
        crate::verify::with_call_deadline(self, opts, self.preview_inner(op, &args, opts)).await
    }

    /// Iterate pages; the stream ends after the last page or the first error.
    pub fn pages(&self, op: Arc<OperationDescriptor>, args: Value, opts: CallOptions) -> Pages {
        Pages::new(self.clone(), op, args, opts)
    }

    /// Iterate the pages of a read-only operation like [`ClientCore::pages`],
    /// each with a sealed checkpoint token to persist once the page is
    /// committed to the sink; pass a saved token as
    /// [`CheckpointOptions::resume`] after a restart to continue with the
    /// page after it. A token that is malformed, altered, sealed under
    /// another key, older than `max_age_ms`, stopped, or issued for other
    /// arguments, origin, tenant, scope or contract is refused
    /// (`VALIDATION_FAILED`) before any request. Page budgets, cycle
    /// detection, deadlines and cancellation apply per run.
    pub fn export_pages(
        &self,
        op: Arc<OperationDescriptor>,
        args: Value,
        checkpoint: crate::checkpoint::CheckpointOptions,
        opts: CallOptions,
    ) -> crate::checkpoint::ExportPages {
        crate::checkpoint::ExportPages::new(self.clone(), op, args, checkpoint, opts)
    }

    /// Call a read operation until `until` holds or the budget runs out. On
    /// a timeout the last body is returned with `timed_out` true.
    pub async fn poll(
        &self,
        op: &OperationDescriptor,
        args: Value,
        until: &Predicate,
        interval: Duration,
        budget: Duration,
        opts: &CallOptions,
    ) -> Result<Polled> {
        let spec = crate::verify::PollSpec {
            until,
            interval,
            budget,
        };
        crate::verify::with_call_deadline(self, opts, self.poll_with(op, &args, &spec, opts, None))
            .await
    }

    /// Run a compiled macro; returns its output or the first failing step's
    /// envelope, with the completed steps' results as `partial`. A
    /// `destructive` or `irreversible` macro needs `opts.confirm`.
    pub async fn run_macro(
        &self,
        macro_: &MacroDescriptor,
        input: Value,
        opts: &CallOptions,
    ) -> Outcome {
        crate::verify::with_call_deadline(self, opts, self.run_macro_inner(macro_, &input, opts))
            .await
    }

    /// Preview a macro without sending anything (see `MacroStepPreview`).
    pub async fn preview_macro(
        &self,
        macro_: &MacroDescriptor,
        input: Value,
        opts: &CallOptions,
    ) -> Result<PreviewResult> {
        crate::verify::with_call_deadline(
            self,
            opts,
            self.preview_macro_inner(macro_, &input, opts),
        )
        .await
    }

    // ------------------------------------------------------------ internals

    /// The call pipeline shared by `call`, pages, polls and macro steps:
    /// `url_override` replaces the URL (next-page links), `step` is the macro
    /// run that confirmed this call.
    pub(crate) async fn call_with(
        &self,
        op: &OperationDescriptor,
        args: &Value,
        opts: &CallOptions,
        url_override: Option<&str>,
        step: Option<&crate::prepare::MacroClaim>,
    ) -> Outcome {
        self.call_with_tracked(
            op,
            args,
            opts,
            url_override,
            step,
            (&mut String::new(), None, None),
        )
        .await
    }

    pub(crate) async fn call_with_tracked(
        &self,
        op: &OperationDescriptor,
        args: &Value,
        opts: &CallOptions,
        url_override: Option<&str>,
        step: Option<&crate::prepare::MacroClaim>,
        tracking: (
            &mut String,
            Option<&mut std::collections::HashSet<String>>,
            Option<&mut (dyn crate::dispatch::BodySink + '_)>,
        ),
    ) -> Outcome {
        let (request_url, page_urls, mut sink) = tracking;
        let mut prepared = self
            .prepare(
                op,
                args,
                opts,
                crate::prepare::Purpose::Call,
                url_override,
                step,
            )
            .await?;
        if let Some(page_urls) = page_urls
            && !page_urls.insert(prepared.url.clone())
        {
            return Err(crate::types::Error::new(crate::envelope::Diag::new(op.id.clone(), crate::types::Category::UnexpectedResponse).remediation("Pagination detected a repeated effective page request URL after authentication. Fix the server's next-page Link before continuing; no repeated request was sent.").build()));
        }
        let mut response = match self
            .send_tracked(&prepared, opts, request_url, sink.as_deref_mut())
            .await
        {
            Err(rejected)
                if rejected.diagnostic.http_status == Some(401)
                    && !(rejected.diagnostic.category
                        == crate::types::Category::UnexpectedResponse
                        && rejected.diagnostic.code.as_deref() == Some("RESPONSE_TOO_LARGE"))
                    && prepared.oauth.is_some() =>
            {
                // An authorization-code token the server no longer accepts:
                // refresh it and send once more. Never a second time.
                self.resend_rejected(&mut prepared, rejected, opts, request_url, sink)
                    .await?
            }
            sent => sent?,
        };
        if opts.verify && op.agent.verify.is_some() {
            // Verification calls other operations, which may verify in turn.
            // It is not part of this operation's telemetry.
            let verification = Box::pin(crate::telemetry::scoped(
                None,
                self.verify(op, &prepared.args, response.value.as_ref(), opts),
            ))
            .await;
            response = Response {
                verification: Some(verification),
                ..response
            };
        }
        Ok(response)
    }

    /// The answer to a request whose stored OAuth2 token was rejected (401):
    /// the refreshed token goes on the same request, which is sent once more;
    /// `rejected` stands when nothing could be refreshed.
    async fn resend_rejected(
        &self,
        prepared: &mut crate::prepare::Prepared<'_>,
        rejected: crate::types::Error,
        opts: &CallOptions,
        request_url: &mut String,
        sink: Option<&mut (dyn crate::dispatch::BodySink + '_)>,
    ) -> std::result::Result<Response<Option<Value>>, crate::types::Error> {
        self.check_send_deadline(prepared, rejected.diagnostic.trace.attempts, None)?;
        if let crate::serialize::Payload::Upload(source) = &prepared.payload
            && !source.sendable()
        {
            return Err(crate::upload::one_shot_spent(rejected));
        }
        let Some(scheme) = prepared.oauth.take() else {
            return Err(rejected);
        };
        let Some(current) = prepared
            .headers
            .get("Authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned)
        else {
            return Err(rejected);
        };
        // The refresh is a detached task the call waits for only until its deadline.
        let refreshed = self.refresh_after_rejection(&scheme, &current).await;
        self.check_send_deadline(prepared, rejected.diagnostic.trace.attempts, None)?;
        let Some(token) = refreshed else {
            return Err(rejected);
        };
        prepared
            .headers
            .set("Authorization", format!("Bearer {token}"), true);
        prepared.secrets.insert(token);
        self.send_tracked(prepared, opts, request_url, sink).await
    }

    /// Epoch milliseconds from the configured clock.
    pub(crate) fn now(&self) -> u64 {
        match &self.inner.now {
            Some(now) => now(),
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        }
    }

    /// A random number in [0, 1) for retry jitter.
    pub(crate) fn random(&self) -> f64 {
        let value = match &self.inner.random {
            Some(random) => random(),
            None => random_bytes::<8>().map_or(0.5, |b| {
                (u64::from_le_bytes(b) >> 11) as f64 / (1u64 << 53) as f64
            }),
        };
        if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.5
        }
    }

    /// The retry policy of one operation: the runtime defaults, then the API's
    /// defaults for the operation's tier, then `ClientOptions::retries`; each
    /// layer overrides the fields it sets.
    pub(crate) fn retry_options(&self, op: &OperationDescriptor) -> Retry {
        let tier = self.inner.api.retries.as_ref().map(|tiers| {
            if crate::classify::is_mutation(op) {
                &tiers.mutating
            } else {
                &tiers.read_only
            }
        });
        let mut out = DEFAULT_RETRY;
        for layer in [tier, Some(&self.inner.retries)].into_iter().flatten() {
            if let Some(max) = layer.max {
                out.max = max.min(100);
            }
            if let Some(base) = layer.base {
                out.base_ms = base.as_secs_f64() * 1000.0;
            }
            if let Some(max_delay) = layer.max_delay {
                out.max_ms = max_delay.as_secs_f64() * 1000.0;
            }
            if let Some(jitter) = layer.jitter {
                out.jitter = jitter;
            }
            if let Some(honor) = layer.honor_retry_after {
                out.honor_retry_after = honor;
            }
        }
        out
    }

    /// The per-attempt timeout (at least one millisecond).
    pub(crate) fn timeout(&self, opts: &CallOptions) -> Duration {
        opts.timeout
            .unwrap_or(self.inner.timeout)
            .max(Duration::from_millis(1))
    }

    /// Report a warning to the configured sink; a failing observer never
    /// changes the call.
    pub(crate) fn emit(&self, diagnostic: &crate::types::Diagnostic) {
        if let Some(sink) = &self.inner.on_diagnostic {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(diagnostic)));
        }
    }
}
