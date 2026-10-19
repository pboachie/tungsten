// SPDX-License-Identifier: Apache-2.0
//! Bounded in-memory authorization transactions; no persistent store is implied.
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::time::{Duration, Instant};

use crate::oauth::{CodeClient, StoredToken};
use crate::types::AuthorizationCodeFlow;

pub const ATTEMPT_LIFETIME: Duration = Duration::from_secs(600);
pub const MAX_ATTEMPTS: usize = 32;
pub const MAX_CAPTURE_BYTES: usize = 32_768;
pub const MAX_CALLBACK_BYTES: usize = 16_384;
pub const MAX_ITEMS: usize = 64;
pub const TOKEN_RESPONSE_BYTES: u64 = 65_536;

/// Explicitly qualified issuer defense; never falls back when `iss` is absent.
#[derive(Clone, PartialEq, Eq)]
pub enum IssuerDefense {
    IssuerResponse { issuer: String },
    DistinctRedirect { qualification: String },
}

impl fmt::Debug for IssuerDefense {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IssuerDefense { .. }")
    }
}

/// Managed begin parameters. Defaults are refused until caller/provider binding is qualified.
#[derive(Clone, Default)]
pub struct AuthorizationUrlParams {
    pub session_binding: String,
    pub issuer_defense: Option<IssuerDefense>,
    pub s256_supported: bool,
    pub redirect_uri: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub resource: Option<String>,
    pub audience: Option<String>,
    pub account_context: Option<String>,
    pub extra: BTreeMap<String, String>,
}

impl fmt::Debug for AuthorizationUrlParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizationUrlParams { .. }")
    }
}

#[derive(Clone, Default)]
pub struct ExchangeCodeParams {
    pub attempt_handle: String,
    pub callback_url: String,
    pub session_binding: String,
}

impl fmt::Debug for ExchangeCodeParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExchangeCodeParams { .. }")
    }
}

/// Safe to retain in memory; the URL/handle are excluded from `Debug` output.
#[derive(Clone)]
pub struct AuthorizationAttempt {
    pub attempt_handle: String,
    pub url: String,
    pub expires_in_ms: u64,
}

impl fmt::Debug for AuthorizationAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationAttempt")
            .field("expires_in_ms", &self.expires_in_ms)
            .finish_non_exhaustive()
    }
}

pub(crate) struct PendingAuthorization {
    pub scheme: String,
    pub generation: u64,
    pub started: Instant,
    pub binding: String,
    pub redirect: String,
    pub client: CodeClient,
    pub flow: AuthorizationCodeFlow,
    pub params: AuthorizationUrlParams,
    pub state: String,
    pub verifier: Option<String>,
    pub claimed: bool,
    pub canceled: bool,
    pub timer: Option<tokio::task::AbortHandle>,
}

impl Drop for PendingAuthorization {
    fn drop(&mut self) {
        self.verifier = None;
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
    }
}

#[derive(Default)]
pub(crate) struct AuthorizationState {
    pub attempts: HashMap<String, PendingAuthorization>,
    pub generations: HashMap<String, u64>,
    pub token_epochs: HashMap<String, u64>,
    pub logging_out: HashMap<String, usize>,
}

impl AuthorizationState {
    pub fn generation(&self, scheme: &str) -> u64 {
        *self.generations.get(scheme).unwrap_or(&0)
    }
    pub fn token_epoch(&self, scheme: &str) -> u64 {
        *self.token_epochs.get(scheme).unwrap_or(&0)
    }
    /// Called only inside the store's final atomic mutation predicate.
    pub fn mutate_token(&mut self, scheme: &str) -> Option<u64> {
        let epoch = self.token_epoch(scheme).checked_add(1)?;
        self.token_epochs.insert(scheme.to_owned(), epoch);
        Some(epoch)
    }
    pub fn active(&self, scheme: &str, generation: u64) -> bool {
        self.generation(scheme) == generation
            && self.logging_out.get(scheme).copied().unwrap_or(0) == 0
    }
    pub fn cancel(&mut self, handle: &str) {
        if let Some(pending) = self.attempts.get_mut(handle) {
            pending.canceled = true;
            pending.verifier = None;
            if !pending.claimed {
                self.attempts.remove(handle);
            }
        }
    }
    pub fn sweep(&mut self) {
        let expired: Vec<String> = self
            .attempts
            .iter()
            .filter(|(_, a)| a.started.elapsed() >= ATTEMPT_LIFETIME)
            .map(|(h, _)| h.clone())
            .collect();
        for handle in expired {
            self.cancel(&handle);
        }
    }
}

pub(crate) fn same_token(current: Option<&StoredToken>, previous: &StoredToken) -> bool {
    current.is_some_and(|s| {
        s.access_token == previous.access_token && s.refresh_token == previous.refresh_token
    })
}

fn reserved(key: &str) -> bool {
    matches!(
        key,
        "response_type"
            | "client_id"
            | "client_secret"
            | "redirect_uri"
            | "scope"
            | "resource"
            | "audience"
            | "state"
            | "code_challenge"
            | "code_challenge_method"
            | "code_verifier"
            | "grant_type"
            | "code"
            | "iss"
            | "error"
            | "error_description"
            | "error_uri"
            | "request"
            | "request_uri"
            | "response_mode"
    )
}

fn decode(value: &str) -> Result<String, ()> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.bytes();
    while let Some(byte) = chars.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let hi = chars
                    .next()
                    .and_then(|c| char::from(c).to_digit(16))
                    .ok_or(())?;
                let lo = chars
                    .next()
                    .and_then(|c| char::from(c).to_digit(16))
                    .ok_or(())?;
                ((hi << 4) | lo) as u8
            }
            other => other,
        });
    }
    String::from_utf8(bytes).map_err(|_| ())
}

pub(crate) fn callback_fields(query: &str) -> Result<BTreeMap<String, String>, ()> {
    if query.len() > MAX_CALLBACK_BYTES
        || query.bytes().filter(|b| *b == b'&').count() + usize::from(!query.is_empty()) > MAX_ITEMS
    {
        return Err(());
    }
    let mut fields = BTreeMap::new();
    let mut size = 0;
    for pair in query.split('&').filter(|_| !query.is_empty()) {
        let (key, value) = pair.split_once('=').ok_or(())?;
        if key.is_empty() {
            return Err(());
        }
        let (key, value) = (decode(key)?, decode(value)?);
        size += key.len() + value.len();
        if size > MAX_CALLBACK_BYTES || fields.contains_key(&key) {
            return Err(());
        }
        fields.insert(key, value);
    }
    Ok(fields)
}

pub(crate) fn qualified_endpoint(value: &str, redirect: bool) -> Result<(), ()> {
    let url = url::Url::parse(value).map_err(|_| ())?;
    if !url.username().is_empty() || url.password().is_some() || value.contains('#') {
        return Err(());
    }
    if redirect {
        if value.contains('?') {
            return Err(());
        }
    } else if !matches!(url.scheme(), "https" | "http")
        || callback_fields(url.query().unwrap_or(""))?
            .keys()
            .any(|k| reserved(k))
    {
        return Err(());
    }
    Ok(())
}

pub(crate) fn qualify(
    params: &AuthorizationUrlParams,
    flow: &AuthorizationCodeFlow,
    client: &CodeClient,
    redirect: &str,
    scheme: &str,
) -> Result<(), ()> {
    if !params.s256_supported
        || params.session_binding.is_empty()
        || flow.scopes.len() > MAX_ITEMS
        || params.scopes.as_ref().is_some_and(|s| s.len() > MAX_ITEMS)
        || params.extra.len() > MAX_ITEMS
    {
        return Err(());
    }
    let defense = match params.issuer_defense.as_ref().ok_or(())? {
        IssuerDefense::IssuerResponse { issuer } => issuer,
        IssuerDefense::DistinctRedirect { qualification } => qualification,
    };
    if defense.is_empty() {
        return Err(());
    }
    let mut size = 0usize;
    let mut charge = |s: &str| -> Result<(), ()> {
        size = size.checked_add(s.len()).ok_or(())?;
        if size > MAX_CAPTURE_BYTES {
            return Err(());
        }
        Ok(())
    };
    for value in [
        scheme,
        &client.client_id,
        client.client_secret.as_deref().unwrap_or(""),
        client.redirect_uri.as_deref().unwrap_or(""),
        redirect,
        &flow.authorization_url,
        &flow.token_url,
        flow.refresh_url.as_deref().unwrap_or(""),
        &params.session_binding,
        defense,
        params.resource.as_deref().unwrap_or(""),
        params.audience.as_deref().unwrap_or(""),
        params.account_context.as_deref().unwrap_or(""),
    ] {
        charge(value)?;
    }
    for scope in flow.scopes.iter().chain(params.scopes.iter().flatten()) {
        charge(scope)?;
    }
    for (key, value) in &params.extra {
        if reserved(key) {
            return Err(());
        }
        charge(key)?;
        charge(value)?;
    }
    qualified_endpoint(&flow.authorization_url, false)?;
    qualified_endpoint(&flow.token_url, false)?;
    if let Some(refresh_url) = &flow.refresh_url {
        qualified_endpoint(refresh_url, false)?;
    }
    qualified_endpoint(redirect, true)
}
