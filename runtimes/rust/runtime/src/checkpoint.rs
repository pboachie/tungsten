// SPDX-License-Identifier: Apache-2.0
//! Durable checkpoints for resumable read-only page exports.
//!
//! A checkpoint is a caller-owned token,
//! `tgp1.<base64url(canonical JSON payload)>.<base64url(HMAC-SHA-256(key, …))>`,
//! recording how far a traversal got after a page the application
//! committed. The payload binds the traversal to the API, operation,
//! contract shape, base URL, tenant, caller scope and every argument but the
//! position, as an HMAC under the same key, so neither arguments nor
//! credentials are stored. The next-page value (a provider cursor or URL) is
//! stored as is: treat the token as sensitive and keep it out of logs.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value, json};
use url::Url;

use crate::client::ClientCore;
use crate::envelope::Diag;
use crate::helpers::arg_name;
use crate::pages::{PageState, PageStop};
use crate::types::{
    CallOptions, Category, Error, OperationDescriptor, PaginationDescriptor, Response, Result,
    Retryable, Safety,
};
use crate::util::{base64url, canonical_json, hmac_sha256, timing_safe_equal};

/// Version prefix of checkpoint tokens.
pub const CHECKPOINT_PREFIX: &str = "tgp1.";
/// Longest accepted checkpoint token, in characters.
pub const MAX_CHECKPOINT_CHARS: usize = 16384;
const SEAL: &str = "tungsten-page-checkpoint\n";
const BINDING: &str = "tungsten-page-binding\n";
const MAX_SAFE: u64 = 9_007_199_254_740_991;

/// A durable, resumable read-only export ([`ClientCore::export_pages`]).
/// The checkpoint token is bound to the API, operation, pagination
/// contract, base URL, `ClientOptions::tenant`, `scope` and every argument
/// except the page position; a token from another traversal, key or runtime
/// contract is refused before any request.
#[derive(Clone, Default)]
pub struct CheckpointOptions {
    /// HMAC-SHA-256 key of at least 32 bytes that seals and verifies
    /// tokens. Keep it as a secret, stable across restarts.
    pub key: Vec<u8>,
    /// The account or principal the export reads for, when
    /// `ClientOptions::tenant` does not already say. Never a credential.
    pub scope: Option<String>,
    /// A token from an acknowledged page to resume after; `None` starts from
    /// the first page. Restarting is the caller's explicit choice.
    pub resume: Option<String>,
    /// Refuse a token issued more than this many milliseconds ago.
    pub max_age_ms: Option<u64>,
}

impl std::fmt::Debug for CheckpointOptions {
    /// The scope only: the key is a secret and a token holds a cursor.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointOptions")
            .field("scope", &self.scope)
            .field("resume", &self.resume.as_ref().map(|_| "<redacted>"))
            .field("max_age_ms", &self.max_age_ms)
            .finish_non_exhaustive()
    }
}

/// A page of a checkpointed export. Persist `checkpoint` only after the
/// page's items are committed to the sink: a crash before persisting
/// replays this page on resume, so the sink must deduplicate.
#[derive(Clone)]
pub struct CheckpointPage<T> {
    pub items: Vec<T>,
    /// The full page body.
    pub body: Value,
    /// Cursor, offset, page number or URL for the next page; `None` at the end.
    pub next: Option<Value>,
    /// Sensitive token (it holds the provider's next-page cursor or URL)
    /// recording progress through this page.
    pub checkpoint: String,
    /// The token records a finished traversal; resuming it reads nothing.
    pub complete: bool,
}

impl<T> std::fmt::Debug for CheckpointPage<T> {
    /// Counts only: items, bodies and the token can hold secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointPage")
            .field("items", &self.items.len())
            .field("complete", &self.complete)
            .finish_non_exhaustive()
    }
}

/// Where a checkpointed traversal stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointState {
    /// More pages to read.
    Partial,
    /// Finished.
    Complete,
    /// Stopped by a server fault (a repeated cursor or a cross-origin
    /// `Link`) that resuming would repeat.
    Stopped,
}

impl CheckpointState {
    fn as_str(self) -> &'static str {
        match self {
            CheckpointState::Partial => "partial",
            CheckpointState::Complete => "complete",
            CheckpointState::Stopped => "stopped",
        }
    }
}

/// The secret-free part of a checkpoint ([`describe_checkpoint`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckpointStatus {
    pub version: u32,
    pub operation: String,
    pub style: String,
    pub state: CheckpointState,
    /// Pages acknowledged across every run of this export.
    pub pages: u64,
    /// Epoch milliseconds the token was issued.
    pub issued_at: u64,
}

struct Payload {
    status: CheckpointStatus,
    binding: String,
    next: Value,
}

fn read(token: &str) -> Option<Payload> {
    if token.len() > MAX_CHECKPOINT_CHARS {
        return None;
    }
    let (body, _) = token.strip_prefix(CHECKPOINT_PREFIX)?.split_once('.')?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body)
        .ok()?;
    let Value::Object(map) = serde_json::from_slice::<Value>(&bytes).ok()? else {
        return None;
    };
    let text = |key: &str| map.get(key).and_then(Value::as_str).map(str::to_owned);
    let count = |key: &str| {
        map.get(key)
            .and_then(Value::as_u64)
            .filter(|n| *n <= MAX_SAFE)
    };
    if map.get("v") != Some(&json!(1)) {
        return None;
    }
    let state = match map.get("state").and_then(Value::as_str)? {
        "partial" => CheckpointState::Partial,
        "complete" => CheckpointState::Complete,
        "stopped" => CheckpointState::Stopped,
        _ => return None,
    };
    Some(Payload {
        status: CheckpointStatus {
            version: 1,
            operation: text("operation")?,
            style: text("style")?,
            state,
            pages: count("pages")?,
            issued_at: count("issued")?,
        },
        binding: text("binding")?,
        next: map.get("next")?.clone(),
    })
}

fn seal_of(key: &[u8], body: &str) -> String {
    base64url(&hmac_sha256(key, format!("{SEAL}{body}").as_bytes()))
}

fn open(key: &[u8], token: &str) -> Option<Payload> {
    let payload = read(token)?;
    let (body, seal) = token.strip_prefix(CHECKPOINT_PREFIX)?.split_once('.')?;
    timing_safe_equal(&seal_of(key, body), seal).then_some(payload)
}

/// The secret-free status of a well-formed checkpoint token (operation,
/// style, state, pages, issue time) for logs and support, or `None` when it
/// is not one. Its seal is not checked; the cursor is omitted.
pub fn describe_checkpoint(token: &str) -> Option<CheckpointStatus> {
    read(token).map(|payload| payload.status)
}

fn style_of(pagination: &PaginationDescriptor) -> (&'static str, &str) {
    match pagination {
        PaginationDescriptor::Cursor { request_param, .. } => ("cursor", request_param),
        PaginationDescriptor::Offset { offset_param, .. } => ("offset", offset_param),
        PaginationDescriptor::Page { page_param, .. } => ("page", page_param),
        PaginationDescriptor::LinkHeader { .. } => ("link_header", ""),
    }
}

fn origin_of(url: &str) -> Option<url::Origin> {
    Url::parse(url)
        .ok()
        .map(|u| u.origin())
        .filter(url::Origin::is_tuple)
}

/// An asynchronous checkpointed page iterator: call [`ExportPages::next`]
/// until it returns `None`. After an error item it returns `None`.
pub struct ExportPages {
    core: ClientCore,
    op: Arc<OperationDescriptor>,
    opts: CallOptions,
    state: PageState,
    deadline: Option<std::time::Instant>,
    key: Vec<u8>,
    binding: String,
    style: &'static str,
    pages: u64,
    resuming: bool,
    refusal: Option<Error>,
    finished: bool,
}

impl std::fmt::Debug for ExportPages {
    /// The operation only: arguments, options and the key can hold secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportPages")
            .field("operation", &self.op.id)
            .field("pages", &self.pages)
            .finish_non_exhaustive()
    }
}

fn refusal(op: &str, parameter: &str, expected: &str, remediation: String) -> Error {
    Error::new(
        Diag::new(op, Category::ValidationFailed)
            .failed_parameter(parameter)
            .expected(expected)
            .retryable(Retryable::Never)
            .remediation(remediation)
            .build(),
    )
}

impl ExportPages {
    pub(crate) fn new(
        core: ClientCore,
        op: Arc<OperationDescriptor>,
        args: Value,
        cp: CheckpointOptions,
        opts: CallOptions,
    ) -> Self {
        let deadline = crate::verify::resolve_deadline(&core, &opts);
        let mut out = ExportPages {
            state: PageState::new(args.clone()),
            core,
            op,
            opts,
            deadline,
            key: Vec::new(),
            binding: String::new(),
            style: "",
            pages: 0,
            resuming: false,
            refusal: None,
            finished: false,
        };
        if let Err(error) = out.prepare(args, cp) {
            out.refusal = Some(error);
        }
        out
    }

    fn prepare(&mut self, args: Value, cp: CheckpointOptions) -> std::result::Result<(), Error> {
        let op = Arc::clone(&self.op);
        let id = op.id.as_str();
        let Some(pagination) = &op.pagination else {
            return Err(refusal(
                id,
                "operation",
                "an operation with a pagination contract",
                "This operation declares no pagination contract, so it cannot be resumed from a checkpoint; call it directly. Nothing was sent.".into(),
            ));
        };
        if op.agent.safety != Safety::ReadOnly {
            return Err(refusal(
                id,
                "operation",
                "a read-only operation",
                "Checkpointed exports run read-only operations only; nothing was sent.".into(),
            ));
        }
        if cp.key.len() < 32 {
            return Err(refusal(
                id,
                "checkpoint.key",
                "a key of at least 32 bytes",
                "Pass checkpoint.key, a secret of at least 32 bytes kept stable across restarts. Nothing was sent.".into(),
            ));
        }
        let Value::Object(map) = &args else {
            return Err(refusal(
                id,
                "args",
                "an object of named arguments",
                "Pass the operation's arguments as an object. Nothing was sent.".into(),
            ));
        };
        let (style, param) = style_of(pagination);
        let mut rest: Map<String, Value> = map.clone();
        if !param.is_empty() {
            rest.remove(&arg_name(&op, param));
        }
        let base = self.core.inner.base_url.clone().unwrap_or_default();
        let identity = json!({
            "api": self.core.inner.api.name,
            "operation": op.id,
            "method": op.method.as_str(),
            "path": op.path,
            "style": style,
            "position": param,
            "base": base,
            "tenant": self.core.inner.tenant.clone().unwrap_or_default(),
            "scope": cp.scope.clone().unwrap_or_default(),
            "args": Value::Object(rest),
        });
        let binding = base64url(&hmac_sha256(
            &cp.key,
            format!("{BINDING}{}", canonical_json(&identity)).as_bytes(),
        ));
        self.key = cp.key;
        self.binding = binding;
        self.style = style;
        let Some(token) = cp.resume else {
            return Ok(());
        };
        let restart = "Restart the export from the first page only if that is intended: omit checkpoint.resume. Nothing was sent.";
        let refuse = |expected: &str, remediation: String| {
            Err(refusal(id, "checkpoint", expected, remediation))
        };
        let Some(saved) = open(&self.key, &token) else {
            return refuse(
                "a checkpoint token sealed under checkpoint.key",
                format!(
                    "The checkpoint is malformed, altered or sealed under another key. {restart}"
                ),
            );
        };
        if saved.status.operation != op.id || saved.status.style != style {
            return refuse(
                &format!("a checkpoint of {} ({style})", op.id),
                format!(
                    "The checkpoint belongs to {} ({}), not this operation and pagination contract. {restart}",
                    saved.status.operation, saved.status.style
                ),
            );
        }
        if !timing_safe_equal(&saved.binding, &self.binding) {
            return refuse(
                "a checkpoint of this traversal",
                format!(
                    "The checkpoint was issued for other arguments (filters, order, page size), base URL, tenant, scope or API. Resume with the original ones, or: {restart}"
                ),
            );
        }
        if let Some(max_age) = cp.max_age_ms
            && self.core.now().saturating_sub(saved.status.issued_at) > max_age
        {
            return refuse(
                &format!("a checkpoint issued within {max_age} ms"),
                format!(
                    "The checkpoint is older than checkpoint.max_age_ms, so the provider's cursor may have expired. {restart}"
                ),
            );
        }
        match saved.status.state {
            CheckpointState::Complete => {
                self.finished = true;
                return Ok(());
            }
            CheckpointState::Stopped => {
                return refuse(
                    "a resumable checkpoint",
                    format!(
                        "The export stopped on a server fault (a repeated cursor or a cross-origin next-page link) that resuming would repeat. {restart}"
                    ),
                );
            }
            CheckpointState::Partial => {}
        }
        let next = saved.next;
        let valid = match style {
            "link_header" => next
                .as_str()
                .and_then(origin_of)
                .is_some_and(|origin| origin_of(&base) == Some(origin)),
            "cursor" => !next.is_null() && next.as_str() != Some(""),
            _ => next.as_u64().is_some_and(|n| n <= MAX_SAFE),
        };
        if !valid {
            return refuse(
                "a resumable checkpoint",
                format!(
                    "The checkpoint's saved position cannot be resumed against this base URL. {restart}"
                ),
            );
        }
        self.pages = saved.status.pages;
        self.resuming = true;
        self.state = PageState::resumed(&op, args, next);
        Ok(())
    }

    pub async fn next(&mut self) -> Option<Result<CheckpointPage<Value>>> {
        if let Some(error) = self.refusal.take() {
            self.finished = true;
            return Some(Err(error));
        }
        if self.finished {
            return None;
        }
        let item = crate::verify::with_deadline_at(
            self.deadline,
            self.core
                .page_step(&self.op, &mut self.state, &self.opts, None),
        )
        .await;
        let resuming = std::mem::replace(&mut self.resuming, false);
        match item? {
            Err(mut error) => {
                self.finished = true;
                if resuming && matches!(error.diagnostic.http_status, Some(400 | 404 | 410 | 422)) {
                    error.diagnostic.remediation = format!(
                        "{} This request resumed from a saved checkpoint: if the provider's cursor or page position expired, restart the export from the first page (omit checkpoint.resume) and let the sink deduplicate.",
                        error.diagnostic.remediation
                    );
                }
                Some(Err(error))
            }
            Ok(response) => {
                self.pages = (self.pages + 1).min(MAX_SAFE);
                let state = match self.state.last_stop {
                    Some(PageStop::Cycle | PageStop::Origin) => CheckpointState::Stopped,
                    _ if self.state.last_next.is_none() => CheckpointState::Complete,
                    _ => CheckpointState::Partial,
                };
                let next = match state {
                    CheckpointState::Partial => self.state.last_next.clone().unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                let payload = json!({
                    "v": 1,
                    "operation": self.op.id,
                    "style": self.style,
                    "binding": self.binding,
                    "state": state.as_str(),
                    "next": next,
                    "pages": self.pages,
                    "issued": self.core.now().min(MAX_SAFE),
                });
                let body = base64url(canonical_json(&payload).as_bytes());
                let checkpoint = format!("{CHECKPOINT_PREFIX}{body}.{}", seal_of(&self.key, &body));
                let page = response.value;
                Some(Ok(Response {
                    value: CheckpointPage {
                        items: page.items,
                        body: page.body,
                        next: page.next,
                        checkpoint,
                        complete: state == CheckpointState::Complete,
                    },
                    meta: response.meta,
                    verification: response.verification,
                }))
            }
        }
    }
}
