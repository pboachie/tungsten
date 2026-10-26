// SPDX-License-Identifier: Apache-2.0
//! OAuth2 authorization-code helpers: PKCE (RFC 7636, S256), the
//! authorization URL, the code exchange, refresh with a single flight per
//! scheme, and the token store behind an `authorizationCode` scheme.
//!
//! Credentials: `ClientOptions::auth[scheme]` is a
//! `Credential::Parts` with `flow = "authorizationCode"`, `client_id`, and
//! optionally `client_secret` and `redirect_uri`. Tokens live in
//! `ClientOptions::token_store` (memory by default), never in the options,
//! envelopes or logs.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::auth::part;
use crate::classify::decode_body;
use crate::client::ClientCore;
use crate::envelope::Diag;
use crate::idempotency::lock;
use crate::oauth_attempt::{
    self, AuthorizationAttempt, AuthorizationUrlParams, ExchangeCodeParams, IssuerDefense,
    PendingAuthorization,
};
use crate::serialize::{Payload, base64_text};
use crate::transport::{AttemptOutcome, AttemptRequest, attempt};
use crate::types::{
    AuthSchemeDescriptor, AuthorizationCodeFlow, Category, Credential, Error, HttpMethod,
};
use crate::util::{get_path_str, random_bytes};

/// Tokens expiring within this many milliseconds count as expired.
pub const EXPIRY_SKEW_MS: u64 = 30_000;

const VERIFIER_BYTES: usize = 32;

/// What the store keeps for one scheme. Secrets: `Debug` shows none.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Epoch milliseconds; `None` when the server gave no lifetime.
    pub expires_at: Option<u64>,
    pub token_type: Option<String>,
    pub scope: Option<String>,
}

impl StoredToken {
    pub fn new(access_token: impl Into<String>) -> Self {
        StoredToken {
            access_token: access_token.into(),
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
        }
    }
}

impl fmt::Debug for StoredToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredToken")
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// Holds the tokens of the authorization-code schemes, by scheme name.
/// Methods are synchronous (like [`crate::IdempotencyStore`]): the shipped
/// store is in memory; implement it to persist tokens.
pub trait TokenStore: Send + Sync + fmt::Debug {
    fn get(&self, scheme: &str) -> Option<StoredToken>;
    fn set(&self, scheme: &str, token: StoredToken);
    fn delete(&self, scheme: &str);
    /// Opt in only when both conditional methods invoke the predicate exactly once
    /// against the current token at the final serialized write/removal. The predicate
    /// changes the managed session epoch: no speculative checks, retries or gap before
    /// mutation are permitted. Return true only for a committed mutation; a failed write
    /// must retain the previous token. A predicate followed by a failed write may
    /// conservatively invalidate an old refresh. Direct/low-level writes bypass the
    /// managed session fence and need application-owned coordination.
    fn supports_fencing(&self) -> bool {
        false
    }
    fn set_if_current(
        &self,
        _scheme: &str,
        _token: StoredToken,
        _current: &dyn Fn(Option<&StoredToken>) -> bool,
    ) -> bool {
        false
    }
    fn delete_if_current(
        &self,
        _scheme: &str,
        _current: &dyn Fn(Option<&StoredToken>) -> bool,
    ) -> bool {
        false
    }
}

/// The default store: in memory, per client.
#[derive(Debug, Default)]
pub struct MemoryTokenStore {
    tokens: Mutex<HashMap<String, StoredToken>>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TokenStore for MemoryTokenStore {
    fn supports_fencing(&self) -> bool {
        true
    }
    fn set_if_current(
        &self,
        scheme: &str,
        token: StoredToken,
        current: &dyn Fn(Option<&StoredToken>) -> bool,
    ) -> bool {
        let mut tokens = lock(&self.tokens);
        if !current(tokens.get(scheme)) {
            return false;
        }
        tokens.insert(scheme.to_owned(), token);
        true
    }
    fn delete_if_current(
        &self,
        scheme: &str,
        current: &dyn Fn(Option<&StoredToken>) -> bool,
    ) -> bool {
        let mut tokens = lock(&self.tokens);
        if !current(tokens.get(scheme)) {
            return false;
        }
        tokens.remove(scheme);
        true
    }
    fn get(&self, scheme: &str) -> Option<StoredToken> {
        lock(&self.tokens).get(scheme).cloned()
    }

    fn set(&self, scheme: &str, token: StoredToken) {
        lock(&self.tokens).insert(scheme.to_owned(), token);
    }

    fn delete(&self, scheme: &str) {
        lock(&self.tokens).remove(scheme);
    }
}

/// A PKCE pair (RFC 7636). The verifier is a secret until the exchange.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Always `"S256"`.
    pub const METHOD: &'static str = "S256";
}

impl fmt::Debug for Pkce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

/// The S256 code challenge of `verifier`: base64url(SHA-256(ASCII(verifier))).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A fresh verifier (43 URL-safe characters from 32 random bytes) and its
/// S256 challenge; `None` when the system has no randomness.
pub fn generate_pkce() -> Option<Pkce> {
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes::<VERIFIER_BYTES>()?);
    let challenge = pkce_challenge(&verifier);
    Some(Pkce {
        verifier,
        challenge,
    })
}

/// What `exchange_code` and `refresh` report: no secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    pub scheme: String,
    /// Epoch milliseconds, or `None` when the server gave no lifetime.
    pub expires_at: Option<u64>,
    pub scope: Option<String>,
    /// Whether a refresh token is stored.
    pub refreshable: bool,
}

/// Parameters of [`OAuthFlow::authorization_url`].
#[derive(Debug, Clone, Default)]
pub struct LowLevelAuthorizationUrlParams {
    /// Defaults to the `redirect_uri` of the scheme's auth config.
    pub redirect_uri: Option<String>,
    /// Defaults to the scopes of the flow.
    pub scopes: Option<Vec<String>>,
    pub state: Option<String>,
    /// The `challenge` of a [`Pkce`].
    pub code_challenge: Option<String>,
    /// Further query parameters (`prompt`, `login_hint`, `audience`), added in key order.
    pub extra: BTreeMap<String, String>,
}

/// Parameters of [`OAuthFlow::exchange_code`].
#[derive(Debug, Clone, Default)]
pub struct LowLevelExchangeCodeParams {
    pub code: String,
    /// Defaults to the `redirect_uri` of the scheme's auth config.
    pub redirect_uri: Option<String>,
    /// The `verifier` of the [`Pkce`] whose challenge started the flow.
    pub code_verifier: Option<String>,
}

/// RFC 3986 percent-encoding: everything but `A-Za-z0-9-._~`.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The URL that sends the user to the authorization server. Query order:
/// response_type, client_id, redirect_uri, scope, state, code_challenge,
/// code_challenge_method, then `extra` in key order.
fn build_authorization_url(
    flow: &AuthorizationCodeFlow,
    client_id: &str,
    redirect_uri: &str,
    params: &LowLevelAuthorizationUrlParams,
) -> String {
    let scope = params.scopes.as_deref().unwrap_or(&flow.scopes).join(" ");
    let mut fields: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
    ];
    if !scope.is_empty() {
        fields.push(("scope", &scope));
    }
    if let Some(state) = params.state.as_deref().filter(|s| !s.is_empty()) {
        fields.push(("state", state));
    }
    if let Some(challenge) = params.code_challenge.as_deref().filter(|s| !s.is_empty()) {
        fields.push(("code_challenge", challenge));
        fields.push(("code_challenge_method", Pkce::METHOD));
    }
    let reserved: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
    let extra: Vec<(&str, &str)> = params
        .extra
        .iter()
        .filter(|(k, _)| !reserved.contains(&k.as_str()))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    fields.extend(extra);
    let base = &flow.authorization_url;
    let separator = if base.contains('?') {
        if base.ends_with(['?', '&']) { "" } else { "&" }
    } else {
        "?"
    };
    format!("{base}{separator}{}", form_body(&fields))
}

/// The auth config of an authorization-code scheme.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CodeClient {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
}

/// A token endpoint problem: the text for the envelope, and whether the
/// server refused the credential it was given (HTTP 4xx).
pub(crate) struct TokenFailure {
    pub message: String,
    pub rejected: bool,
}

impl TokenFailure {
    fn new(message: impl Into<String>) -> Self {
        TokenFailure {
            message: message.into(),
            rejected: false,
        }
    }
}

/// Serializes the refreshes of one scheme; `generation` counts the
/// completed ones, so a caller that waited for the lock can tell that
/// another caller refreshed meanwhile and use its token (a single flight).
#[derive(Default)]
pub(crate) struct Gate {
    generation: AtomicU64,
    token_epoch: AtomicU64,
    lock: tokio::sync::Mutex<()>,
}

fn missing_token(scheme: &str) -> TokenFailure {
    TokenFailure::new(format!(
        "No OAuth2 token is stored for {scheme}; begin a managed authorization_url() attempt, then call exchange_code() with its handle, the full callback URL and the owning session binding."
    ))
}

fn text(value: Option<&String>) -> Option<String> {
    value.filter(|v| !v.is_empty()).cloned()
}

fn is_error_code(code: &str) -> bool {
    // Provider-supplied labels can contain secrets even when they look like codes.
    matches!(
        code,
        "invalid_request"
            | "invalid_client"
            | "invalid_grant"
            | "unauthorized_client"
            | "unsupported_grant_type"
            | "invalid_scope"
    )
}

impl ClientCore {
    /// The OAuth2 authorization-code helpers of the scheme `scheme`: PKCE,
    /// the authorization URL, the code exchange and refresh.
    pub fn oauth(&self, scheme: &str) -> OAuthFlow {
        OAuthFlow {
            core: self.clone(),
            scheme: scheme.to_owned(),
        }
    }

    fn code_flow(&self, scheme: &str) -> Result<&AuthorizationCodeFlow, String> {
        let found = self.inner.api.auth.iter().find(|s| s.name() == scheme);
        match found {
            None => Err(format!(
                "the API descriptor defines no scheme named {scheme}"
            )),
            Some(AuthSchemeDescriptor::Oauth2 {
                authorization_code: Some(flow),
                ..
            }) => Ok(flow),
            Some(_) => Err(format!("the scheme {scheme} has no authorizationCode flow")),
        }
    }

    fn code_client(&self, scheme: &str) -> Result<CodeClient, String> {
        let invalid = || {
            format!(
                "auth.{scheme} must be a Credential::Parts with flow = \"authorizationCode\", client_id, and optionally client_secret and redirect_uri"
            )
        };
        let Some(Credential::Parts(parts)) = self.inner.auth.get(scheme) else {
            return Err(invalid());
        };
        let value = |snake: &str, camel: &str| {
            part(parts, snake, camel)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        if value("flow", "flow").as_deref() != Some("authorizationCode") {
            return Err(invalid());
        }
        let client_id = value("client_id", "clientId")
            .ok_or_else(|| format!("auth.{scheme} needs a client_id"))?;
        Ok(CodeClient {
            client_id,
            client_secret: value("client_secret", "clientSecret"),
            redirect_uri: value("redirect_uri", "redirectUri"),
        })
    }

    fn oauth_error(operation: &str, remediation: String) -> Error {
        Error::new(
            Diag::new(operation, Category::AuthFailed)
                .remediation(remediation)
                .build(),
        )
    }

    fn gate(
        &self,
        scheme: &str,
        generation: u64,
        token_epoch: u64,
    ) -> Result<Arc<Gate>, TokenFailure> {
        let mut gates = lock(&self.inner.oauth_gates);
        gates.retain(|_, gate| Arc::strong_count(gate) > 1);
        let key = format!("{generation}:{token_epoch}:{scheme}");
        if let Some(gate) = gates.get(&key) {
            return Ok(gate.clone());
        }
        if gates.len() >= oauth_attempt::MAX_ATTEMPTS {
            return Err(TokenFailure::new(
                "OAuth refresh capacity is exhausted; wait for an owned refresh to settle.",
            ));
        }
        Ok(gates.entry(key).or_default().clone())
    }

    fn read_token(&self, scheme: &str) -> Option<StoredToken> {
        let (generation, token_epoch) = self.authorization_version(scheme);
        if !self.authorization_epoch_current(scheme, generation, token_epoch) {
            return None;
        }
        let stored = self
            .inner
            .token_store
            .get(scheme)
            .filter(|t| !t.access_token.is_empty());
        if !self.authorization_epoch_current(scheme, generation, token_epoch) {
            return None;
        }
        stored
    }

    fn authorization_version(&self, scheme: &str) -> (u64, u64) {
        let state = lock(&self.inner.oauth_authorization);
        (state.generation(scheme), state.token_epoch(scheme))
    }

    fn authorization_epoch_current(&self, scheme: &str, generation: u64, token_epoch: u64) -> bool {
        let state = lock(&self.inner.oauth_authorization);
        state.active(scheme, generation) && state.token_epoch(scheme) == token_epoch
    }

    /// The access token to send: the stored one, refreshed first when it
    /// expires within [`EXPIRY_SKEW_MS`] (or `force`). The error is the text
    /// shown in the `AUTH_FAILED` envelope.
    pub(crate) async fn stored_token(&self, scheme: &str, force: bool) -> Result<String, String> {
        self.stored_token_inner(scheme, force)
            .await
            .map_err(|f| f.message)
    }

    async fn stored_token_inner(&self, scheme: &str, force: bool) -> Result<String, TokenFailure> {
        let (authorization_generation, token_epoch) = self.authorization_version(scheme);
        let missing = || missing_token(scheme);
        let stored = self.read_token(scheme).ok_or_else(missing)?;
        if !self.authorization_epoch_current(scheme, authorization_generation, token_epoch) {
            return Err(TokenFailure::new("OAuth session changed; authorize again."));
        }
        let fresh = stored
            .expires_at
            .is_none_or(|at| at > self.now().saturating_add(EXPIRY_SKEW_MS));
        if fresh && !force {
            return Ok(stored.access_token);
        }
        // The refresh is its own task: a caller whose deadline passes, or
        // whose future is dropped, stops waiting without cancelling a
        // refresh other callers share.
        let core = self.clone();
        let owned = scheme.to_owned();
        let refresh = async move {
            core.refresh_flight(&owned, force, authorization_generation, token_epoch)
                .await
        };
        match crate::verify::detached(refresh).await {
            Ok(result) => result,
            Err(crate::verify::Abandoned::Expired) => Err(TokenFailure::new(format!(
                "The operation deadline expired while refreshing the OAuth2 token for {scheme}."
            ))),
            Err(crate::verify::Abandoned::Stopped) => Err(TokenFailure::new(format!(
                "The OAuth2 token refresh for {scheme} stopped before it finished; call again."
            ))),
        }
    }

    /// One caller's turn at the single flight of `scheme`: wait for the
    /// gate, then use a token another caller refreshed meanwhile or refresh.
    async fn refresh_flight(
        &self,
        scheme: &str,
        force: bool,
        authorization_generation: u64,
        token_epoch: u64,
    ) -> Result<String, TokenFailure> {
        let missing = || missing_token(scheme);
        let gate = self.gate(scheme, authorization_generation, token_epoch)?;
        let seen = gate.generation.load(Ordering::SeqCst);
        let _held = gate.lock.lock().await;
        if !lock(&self.inner.oauth_authorization).active(scheme, authorization_generation) {
            return Err(TokenFailure::new("OAuth session changed; authorize again."));
        }
        if gate.generation.load(Ordering::SeqCst) != seen {
            if !self.authorization_epoch_current(
                scheme,
                authorization_generation,
                gate.token_epoch.load(Ordering::SeqCst),
            ) {
                return Err(TokenFailure::new("OAuth session changed; authorize again."));
            }
            // Another caller refreshed while this one waited.
            return self
                .read_token(scheme)
                .map(|t| t.access_token)
                .ok_or_else(missing);
        }
        if !self.authorization_epoch_current(scheme, authorization_generation, token_epoch) {
            return Err(TokenFailure::new("OAuth session changed; authorize again."));
        }
        let current = self.read_token(scheme).ok_or_else(missing)?;
        let token = self
            .refresh_stored(
                scheme,
                &current,
                force,
                authorization_generation,
                token_epoch,
            )
            .await?;
        gate.token_epoch
            .store(token_epoch.saturating_add(1), Ordering::SeqCst);
        gate.generation.fetch_add(1, Ordering::SeqCst);
        Ok(token)
    }

    /// Force a refresh after a 401, when the stored token has a refresh
    /// token. Returns the new access token, or `None` when nothing changed.
    pub(crate) async fn refresh_after_rejection(
        &self,
        scheme: &str,
        rejected: &str,
    ) -> Option<String> {
        let stored = self.read_token(scheme)?;
        // Another call already replaced the rejected token.
        if stored.access_token != rejected {
            return Some(stored.access_token);
        }
        stored.refresh_token.as_ref()?;
        self.stored_token(scheme, true).await.ok()
    }

    async fn refresh_stored(
        &self,
        scheme: &str,
        stored: &StoredToken,
        force: bool,
        generation: u64,
        token_epoch: u64,
    ) -> Result<String, TokenFailure> {
        if !self.authorization_epoch_current(scheme, generation, token_epoch) {
            return Err(TokenFailure::new("OAuth session changed; authorize again."));
        }
        if !self.inner.token_store.supports_fencing() {
            return Err(TokenFailure::new(
                "OAuth refresh requires an atomic fenced token store.",
            ));
        }
        let flow = self.code_flow(scheme).map_err(|reason| {
            TokenFailure::new(format!(
                "Cannot refresh the OAuth2 token for {scheme}: {reason}."
            ))
        })?;
        let client = self.code_client(scheme).map_err(|reason| {
            TokenFailure::new(format!(
                "Cannot refresh the OAuth2 token for {scheme}: {reason}."
            ))
        })?;
        let Some(refresh) = stored.refresh_token.as_deref() else {
            return Err(TokenFailure::new(format!(
                "The OAuth2 token for {scheme} {}; authorize again with authorization_url() and exchange_code().",
                if force {
                    "has no refresh token"
                } else {
                    "expired and has no refresh token"
                }
            )));
        };
        let url = flow.refresh_url.as_deref().unwrap_or(&flow.token_url);
        oauth_attempt::qualified_endpoint(url, false).map_err(|_| {
            TokenFailure::new(
                "Cannot refresh OAuth2 tokens: provide a valid endpoint without protected query fields.",
            )
        })?;
        let fields = [("grant_type", "refresh_token"), ("refresh_token", refresh)];
        match self
            .token_request(scheme, url, &client, &fields, Some(refresh))
            .await
        {
            Ok(renewed) => {
                let access = renewed.access_token.clone();
                if !self
                    .inner
                    .token_store
                    .set_if_current(scheme, renewed, &|current| {
                        let mut state = lock(&self.inner.oauth_authorization);
                        state.active(scheme, generation)
                            && state.token_epoch(scheme) == token_epoch
                            && oauth_attempt::same_token(current, stored)
                            && state.mutate_token(scheme).is_some()
                    })
                {
                    return Err(TokenFailure::new("OAuth session changed; authorize again."));
                }
                if !self.authorization_epoch_current(
                    scheme,
                    generation,
                    token_epoch.saturating_add(1),
                ) {
                    return Err(TokenFailure::new("OAuth session changed; authorize again."));
                }
                Ok(access)
            }
            Err(failure) => {
                if failure.rejected {
                    // The server no longer accepts the refresh token: forget the tokens.
                    self.inner
                        .token_store
                        .delete_if_current(scheme, &|current| {
                            let mut state = lock(&self.inner.oauth_authorization);
                            state.active(scheme, generation)
                                && state.token_epoch(scheme) == token_epoch
                                && oauth_attempt::same_token(current, stored)
                                && state.mutate_token(scheme).is_some()
                        });
                }
                Err(failure)
            }
        }
    }

    /// One request to the token endpoint. `previous` is the refresh token a
    /// response without one keeps.
    async fn token_request(
        &self,
        scheme: &str,
        url: &str,
        client: &CodeClient,
        fields: &[(&str, &str)],
        previous: Option<&str>,
    ) -> Result<StoredToken, TokenFailure> {
        let limit = self.inner.max_oauth_token_response_bytes;
        if limit == 0 || limit > 9_007_199_254_740_991 {
            return Err(TokenFailure::new(
                "max_oauth_token_response_bytes must be a positive safe integer.",
            ));
        }
        if crate::auth::insecure_http(url, self.inner.allow_insecure_http) {
            return Err(TokenFailure {
                message: crate::auth::INSECURE_HTTP_AUTH.to_owned(),
                rejected: false,
            });
        }
        let mut headers = vec![
            (
                "Content-Type".to_owned(),
                "application/x-www-form-urlencoded".to_owned(),
            ),
            ("Accept".to_owned(), "application/json".to_owned()),
        ];
        let mut all: Vec<(&str, &str)> = fields.to_vec();
        match client.client_secret.as_deref() {
            Some(secret) => headers.push((
                "Authorization".to_owned(),
                format!(
                    "Basic {}",
                    base64_text(&format!(
                        "{}:{}",
                        percent_encode(&client.client_id),
                        percent_encode(secret)
                    ))
                ),
            )),
            None => all.push(("client_id", &client.client_id)),
        }
        let body = Payload::Bytes(form_body(&all).into_bytes());
        let outcome = attempt(
            &self.inner.http,
            &AttemptRequest {
                url,
                method: HttpMethod::Post,
                headers: &headers,
                body: &body,
                timeout: self.inner.timeout.max(Duration::from_millis(1)),
                stream: false,
                download: false,
                idle: None,
                max_response_bytes: Some(limit),
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
            return Err(TokenFailure::new(format!(
                "The OAuth2 token endpoint for {scheme} could not be reached; check network access and the token URL."
            )));
        };
        let decoded = decode_body(&bytes, &headers, Some("application/json"));
        let value = decoded.value.as_ref();
        if !(200..=299).contains(&status) {
            let named = match get_path_str(value, "error") {
                Some(Value::String(code)) if is_error_code(code) => format!(" ({code})"),
                _ => String::new(),
            };
            return Err(TokenFailure {
                message: format!(
                    "The OAuth2 token endpoint for {scheme} answered HTTP {status}{named}; check the code or refresh token, client credentials and redirect URI."
                ),
                rejected: (400..500).contains(&status),
            });
        }
        let access = match get_path_str(value, "access_token") {
            Some(Value::String(token)) if !token.is_empty() => token.clone(),
            _ => {
                return Err(TokenFailure::new(format!(
                    "The OAuth2 token endpoint for {scheme} returned no access_token."
                )));
            }
        };
        let string_at = |path: &str| match get_path_str(value, path) {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        let refresh_token = string_at("refresh_token").or_else(|| previous.map(str::to_owned));
        let expires_at = get_path_str(value, "expires_in")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .map(|n| self.now().saturating_add((n.max(0.0) * 1000.0) as u64));
        Ok(StoredToken {
            access_token: access,
            refresh_token,
            expires_at,
            token_type: string_at("token_type"),
            scope: string_at("scope"),
        })
    }
}

/// The helpers of one authorization-code scheme, as generated clients
/// expose them. Cheap to clone; failures are `AUTH_FAILED` envelopes.
#[derive(Clone)]
pub struct OAuthFlow {
    core: ClientCore,
    scheme: String,
}

impl fmt::Debug for OAuthFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthFlow")
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

impl OAuthFlow {
    /// The scheme these helpers belong to.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The tokens' store.
    pub fn token_store(&self) -> Arc<dyn TokenStore> {
        Arc::clone(&self.core.inner.token_store)
    }

    /// A fresh PKCE pair for explicitly low-level integration. Managed
    /// [`Self::authorization_url`] generates and retains its own pair.
    pub fn pkce(&self) -> Result<Pkce, Error> {
        generate_pkce().ok_or_else(|| {
            ClientCore::oauth_error(
                &self.operation("pkce"),
                "The system has no source of randomness for a PKCE verifier.".to_owned(),
            )
        })
    }

    fn operation(&self, helper: &str) -> String {
        format!("oauth.{}.{helper}", self.scheme)
    }

    /// Begin a managed S256 attempt inside a Tokio runtime with time enabled.
    /// The application must enable time so the owned expiry task can run;
    /// a current runtime handle alone does not establish that capability.
    pub fn authorization_url(
        &self,
        params: &AuthorizationUrlParams,
    ) -> Result<AuthorizationAttempt, Error> {
        let refuse = || {
            ClientCore::oauth_error(&self.operation("authorizationUrl"), "Cannot begin authorization: qualify S256 and issuer defense, supply a session binding, valid bounded endpoints and a fenced token store inside a Tokio runtime with time enabled; protected fields are reserved.".to_owned())
        };
        let flow = self.core.code_flow(&self.scheme).map_err(|_| refuse())?;
        let client = self.core.code_client(&self.scheme).map_err(|_| refuse())?;
        let redirect = params
            .redirect_uri
            .as_deref()
            .or(client.redirect_uri.as_deref())
            .ok_or_else(refuse)?;
        oauth_attempt::qualify(params, flow, &client, redirect, &self.scheme)
            .map_err(|_| refuse())?;
        let limit = self.core.inner.max_oauth_token_response_bytes;
        if !self.core.inner.token_store.supports_fencing()
            || limit == 0
            || limit > 9_007_199_254_740_991
            || [&flow.authorization_url, &flow.token_url]
                .iter()
                .any(|url| crate::auth::insecure_http(url, self.core.inner.allow_insecure_http))
        {
            return Err(refuse());
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| refuse())?;
        let mut state = lock(&self.core.inner.oauth_authorization);
        state.sweep();
        if state.attempts.len() >= oauth_attempt::MAX_ATTEMPTS {
            return Err(ClientCore::oauth_error(&self.operation("authorizationUrl"), "Authorization attempt capacity is exhausted; finish or cancel an existing attempt.".to_owned()));
        }
        let generation = state.generation(&self.scheme);
        if !state.active(&self.scheme, generation) {
            return Err(refuse());
        }
        let pkce = generate_pkce().ok_or_else(refuse)?;
        let handle = URL_SAFE_NO_PAD.encode(random_bytes::<32>().ok_or_else(refuse)?);
        let random_state = URL_SAFE_NO_PAD.encode(random_bytes::<32>().ok_or_else(refuse)?);
        let mut extra = params.extra.clone();
        if let Some(resource) = &params.resource {
            extra.insert("resource".to_owned(), resource.clone());
        }
        if let Some(audience) = &params.audience {
            extra.insert("audience".to_owned(), audience.clone());
        }
        let url = build_authorization_url(
            flow,
            &client.client_id,
            redirect,
            &LowLevelAuthorizationUrlParams {
                scopes: params.scopes.clone(),
                state: Some(random_state.clone()),
                code_challenge: Some(pkce.challenge),
                extra,
                ..Default::default()
            },
        );
        let redirect = redirect.to_owned();
        let weak = Arc::downgrade(&self.core.inner);
        let timer_handle = handle.clone();
        let timer = runtime.spawn(async move {
            tokio::time::sleep(oauth_attempt::ATTEMPT_LIFETIME).await;
            if let Some(inner) = weak.upgrade() {
                lock(&inner.oauth_authorization).cancel(&timer_handle);
            }
        });
        state.attempts.insert(
            handle.clone(),
            PendingAuthorization {
                scheme: self.scheme.clone(),
                generation,
                started: std::time::Instant::now(),
                binding: params.session_binding.clone(),
                redirect,
                client,
                flow: flow.clone(),
                params: params.clone(),
                state: random_state,
                verifier: Some(pkce.verifier),
                claimed: false,
                canceled: false,
                timer: Some(timer.abort_handle()),
            },
        );
        Ok(AuthorizationAttempt {
            attempt_handle: handle,
            url,
            expires_in_ms: oauth_attempt::ATTEMPT_LIFETIME.as_millis() as u64,
        })
    }

    fn authorization_current(&self, handle: &str) -> bool {
        let state = lock(&self.core.inner.oauth_authorization);
        let Some(pending) = state.attempts.get(handle) else {
            return false;
        };
        !pending.canceled
            && pending.started.elapsed() < oauth_attempt::ATTEMPT_LIFETIME
            && state.active(&self.scheme, pending.generation)
            && self
                .core
                .code_flow(&self.scheme)
                .is_ok_and(|f| *f == pending.flow)
            && self
                .core
                .code_client(&self.scheme)
                .is_ok_and(|c| c == pending.client)
    }

    /// Validate the entire callback and claim exactly one code exchange.
    pub async fn exchange_code(&self, params: &ExchangeCodeParams) -> Result<TokenInfo, Error> {
        let refuse = || {
            ClientCore::oauth_error(&self.operation("exchangeCode"), "Authorization callback refused: invalid, expired, consumed or mismatched attempt. Begin a new authorization attempt if needed.".to_owned())
        };
        if !self.authorization_current(&params.attempt_handle) {
            return Err(refuse());
        }
        let (code, verifier, client, url, redirect, generation) = {
            let mut state = lock(&self.core.inner.oauth_authorization);
            state.sweep();
            let pending = state
                .attempts
                .get_mut(&params.attempt_handle)
                .ok_or_else(refuse)?;
            if pending.claimed
                || pending.canceled
                || pending.scheme != self.scheme
                || pending.binding != params.session_binding
                || params.callback_url.len() > oauth_attempt::MAX_CALLBACK_BYTES
                || params.callback_url.contains('#')
            {
                return Err(refuse());
            }
            let (target, query) = params.callback_url.split_once('?').ok_or_else(refuse)?;
            if target != pending.redirect {
                return Err(refuse());
            }
            let fields = oauth_attempt::callback_fields(query).map_err(|_| refuse())?;
            if fields.get("state") != Some(&pending.state) {
                return Err(refuse());
            }
            if let Some(IssuerDefense::IssuerResponse { issuer }) = &pending.params.issuer_defense
                && fields.get("iss") != Some(issuer)
            {
                return Err(refuse());
            }
            let (code, denied) = (fields.get("code"), fields.get("error"));
            if code.is_some() == denied.is_some()
                || code.is_some_and(|c| c.is_empty())
                || denied.is_some_and(|e| e.is_empty())
                || (code.is_some()
                    && (fields.contains_key("error_description")
                        || fields.contains_key("error_uri")))
            {
                return Err(refuse());
            }
            pending.claimed = true;
            if denied.is_some() {
                state.attempts.remove(&params.attempt_handle);
                return Err(ClientCore::oauth_error(
                    &self.operation("exchangeCode"),
                    "Authorization was denied. Begin a new attempt to authorize again.".to_owned(),
                ));
            }
            (
                code.cloned().ok_or_else(refuse)?,
                pending.verifier.take().ok_or_else(refuse)?,
                pending.client.clone(),
                pending.flow.token_url.clone(),
                pending.redirect.clone(),
                pending.generation,
            )
        };
        let _cleanup = AuthorizationClaim {
            flow: self,
            handle: &params.attempt_handle,
        };
        if !self.authorization_current(&params.attempt_handle) {
            return Err(refuse());
        }
        let fields = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier.as_str()),
        ];
        let stored = self.core.token_request(&self.scheme, &url, &client, &fields, None).await
            .map_err(|_| ClientCore::oauth_error(&self.operation("exchangeCode"), "Authorization exchange did not complete. The code may have been consumed; begin a new authorization attempt.".to_owned()))?;
        let result = info(&self.scheme, &stored);
        let committed_epoch = Cell::new(None);
        if !self
            .core
            .inner
            .token_store
            .set_if_current(&self.scheme, stored, &|_| {
                if !self.authorization_current(&params.attempt_handle) {
                    return false;
                }
                let mut state = lock(&self.core.inner.oauth_authorization);
                let Some(pending) = state.attempts.get(&params.attempt_handle) else {
                    return false;
                };
                if pending.canceled
                    || pending.started.elapsed() >= oauth_attempt::ATTEMPT_LIFETIME
                    || !state.active(&self.scheme, pending.generation)
                {
                    return false;
                }
                committed_epoch.set(state.mutate_token(&self.scheme));
                committed_epoch.get().is_some()
            })
        {
            return Err(refuse());
        }
        if !self.authorization_current(&params.attempt_handle)
            || !committed_epoch.get().is_some_and(|epoch| {
                self.core
                    .authorization_epoch_current(&self.scheme, generation, epoch)
            })
        {
            return Err(refuse());
        }
        Ok(result)
    }

    pub fn cancel_authorization(&self, handle: &str, session_binding: &str) -> Result<(), Error> {
        let mut state = lock(&self.core.inner.oauth_authorization);
        if !state
            .attempts
            .get(handle)
            .is_some_and(|a| a.scheme == self.scheme && a.binding == session_binding)
        {
            return Err(ClientCore::oauth_error(
                &self.operation("cancelAuthorization"),
                "Authorization attempt is unavailable or belongs to a different session."
                    .to_owned(),
            ));
        }
        state.cancel(handle);
        Ok(())
    }

    /// Local credential removal, with attempt invalidation before any store work.
    pub fn logout(&self) {
        {
            let mut state = lock(&self.core.inner.oauth_authorization);
            let generation = state.generation(&self.scheme).wrapping_add(1);
            state.generations.insert(self.scheme.clone(), generation);
            *state.logging_out.entry(self.scheme.clone()).or_default() += 1;
            let handles: Vec<String> = state
                .attempts
                .iter()
                .filter(|(_, a)| a.scheme == self.scheme)
                .map(|(h, _)| h.clone())
                .collect();
            for handle in handles {
                state.cancel(&handle);
            }
        }
        self.core.inner.token_store.delete(&self.scheme);
        if let Some(count) = lock(&self.core.inner.oauth_authorization)
            .logging_out
            .get_mut(&self.scheme)
        {
            *count = count.saturating_sub(1);
        }
    }

    /// The URL to send the user to.
    pub fn low_level_authorization_url(
        &self,
        params: &LowLevelAuthorizationUrlParams,
    ) -> Result<String, Error> {
        let operation = self.operation("authorizationUrl");
        let refuse = |reason: String| {
            ClientCore::oauth_error(
                &operation,
                format!("Cannot build an authorization URL: {reason}."),
            )
        };
        let flow = self.core.code_flow(&self.scheme).map_err(refuse)?;
        let client = self.core.code_client(&self.scheme).map_err(refuse)?;
        let redirect = text(params.redirect_uri.as_ref())
            .or(client.redirect_uri)
            .ok_or_else(|| {
                ClientCore::oauth_error(
                    &operation,
                    format!(
                        "Pass redirect_uri, or set redirect_uri in auth.{}; the authorization URL needs one.",
                        self.scheme
                    ),
                )
            })?;
        Ok(build_authorization_url(
            flow,
            &client.client_id,
            &redirect,
            params,
        ))
    }

    /// Trade the code from the redirect for tokens, stored for later calls.
    pub async fn low_level_exchange_code(
        &self,
        params: &LowLevelExchangeCodeParams,
    ) -> Result<TokenInfo, Error> {
        let operation = self.operation("exchangeCode");
        let refuse = |reason: String| {
            ClientCore::oauth_error(&operation, format!("Cannot exchange the code: {reason}."))
        };
        let flow = self.core.code_flow(&self.scheme).map_err(refuse)?;
        let client = self.core.code_client(&self.scheme).map_err(refuse)?;
        if params.code.is_empty() {
            return Err(ClientCore::oauth_error(
                &operation,
                "Pass the authorization code the server redirected back with as code.".to_owned(),
            ));
        }
        let redirect = text(params.redirect_uri.as_ref())
            .or_else(|| client.redirect_uri.clone())
            .ok_or_else(|| {
                ClientCore::oauth_error(
                    &operation,
                    format!(
                        "Pass redirect_uri, or set redirect_uri in auth.{}; it must equal the one used for the authorization URL.",
                        self.scheme
                    ),
                )
            })?;
        let mut fields = vec![
            ("grant_type", "authorization_code"),
            ("code", params.code.as_str()),
            ("redirect_uri", redirect.as_str()),
        ];
        if let Some(verifier) = params.code_verifier.as_deref().filter(|v| !v.is_empty()) {
            fields.push(("code_verifier", verifier));
        }
        let stored = self
            .core
            .token_request(&self.scheme, &flow.token_url, &client, &fields, None)
            .await
            .map_err(|f| ClientCore::oauth_error(&operation, f.message))?;
        let info = info(&self.scheme, &stored);
        self.core.inner.token_store.set(&self.scheme, stored);
        Ok(info)
    }

    /// Refresh the stored token now. Calls refresh on their own when it expires.
    pub async fn refresh(&self) -> Result<TokenInfo, Error> {
        let operation = self.operation("refresh");
        self.core
            .stored_token_inner(&self.scheme, true)
            .await
            .map_err(|f| ClientCore::oauth_error(&operation, f.message))?;
        match self.core.read_token(&self.scheme) {
            Some(stored) => Ok(info(&self.scheme, &stored)),
            None => Err(ClientCore::oauth_error(
                &operation,
                format!("No token is stored for {}.", self.scheme),
            )),
        }
    }
}

struct AuthorizationClaim<'a> {
    flow: &'a OAuthFlow,
    handle: &'a str,
}
impl Drop for AuthorizationClaim<'_> {
    fn drop(&mut self) {
        lock(&self.flow.core.inner.oauth_authorization)
            .attempts
            .remove(self.handle);
    }
}

fn info(scheme: &str, token: &StoredToken) -> TokenInfo {
    TokenInfo {
        scheme: scheme.to_owned(),
        expires_at: token.expires_at,
        scope: token.scope.clone(),
        refreshable: token.refresh_token.is_some(),
    }
}
