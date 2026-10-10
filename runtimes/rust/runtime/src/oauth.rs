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
pub struct AuthorizationUrlParams {
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
pub struct ExchangeCodeParams {
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
    params: &AuthorizationUrlParams,
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
struct CodeClient {
    client_id: String,
    client_secret: Option<String>,
    redirect_uri: Option<String>,
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
    lock: tokio::sync::Mutex<()>,
}

fn text(value: Option<&String>) -> Option<String> {
    value.filter(|v| !v.is_empty()).cloned()
}

fn is_error_code(code: &str) -> bool {
    (1..=40).contains(&code.len()) && code.bytes().all(|b| b == b'_' || b.is_ascii_lowercase())
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

    fn gate(&self, scheme: &str) -> Arc<Gate> {
        lock(&self.inner.oauth_gates)
            .entry(scheme.to_owned())
            .or_default()
            .clone()
    }

    fn read_token(&self, scheme: &str) -> Option<StoredToken> {
        self.inner
            .token_store
            .get(scheme)
            .filter(|t| !t.access_token.is_empty())
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
        let missing = || {
            TokenFailure::new(format!(
                "No OAuth2 token is stored for {scheme}; send the user to authorization_url(), then call exchange_code() with the code, or put a token in ClientOptions.token_store."
            ))
        };
        let stored = self.read_token(scheme).ok_or_else(missing)?;
        let fresh = stored
            .expires_at
            .is_none_or(|at| at > self.now().saturating_add(EXPIRY_SKEW_MS));
        if fresh && !force {
            return Ok(stored.access_token);
        }
        let gate = self.gate(scheme);
        let seen = gate.generation.load(Ordering::SeqCst);
        let _held = gate.lock.lock().await;
        if gate.generation.load(Ordering::SeqCst) != seen {
            // Another caller refreshed while this one waited.
            return self
                .read_token(scheme)
                .map(|t| t.access_token)
                .ok_or_else(missing);
        }
        let current = self.read_token(scheme).ok_or_else(missing)?;
        let token = self.refresh_stored(scheme, &current, force).await?;
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
    ) -> Result<String, TokenFailure> {
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
        let fields = [("grant_type", "refresh_token"), ("refresh_token", refresh)];
        match self
            .token_request(scheme, url, &client, &fields, Some(refresh))
            .await
        {
            Ok(renewed) => {
                let access = renewed.access_token.clone();
                self.inner.token_store.set(scheme, renewed);
                Ok(access)
            }
            Err(failure) => {
                if failure.rejected {
                    // The server no longer accepts the refresh token: forget the tokens.
                    self.inner.token_store.delete(scheme);
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
                idle: None,
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

    /// A fresh PKCE pair; pass `challenge` to [`Self::authorization_url`] and
    /// keep `verifier` for [`Self::exchange_code`].
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

    /// The URL to send the user to.
    pub fn authorization_url(&self, params: &AuthorizationUrlParams) -> Result<String, Error> {
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
    pub async fn exchange_code(&self, params: &ExchangeCodeParams) -> Result<TokenInfo, Error> {
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

fn info(scheme: &str, token: &StoredToken) -> TokenInfo {
    TokenInfo {
        scheme: scheme.to_owned(),
        expires_at: token.expires_at,
        scope: token.scope.clone(),
        refreshable: token.refresh_token.is_some(),
    }
}
