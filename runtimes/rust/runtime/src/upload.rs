// SPDX-License-Identifier: Apache-2.0
//! Streamed upload sources: a request body read chunk by chunk from the
//! caller instead of held in memory, with an explicit replay rule. The
//! semantics of `UploadSource` in `runtimes/ts/src/upload.ts`.
//!
//! A one-shot source is sent at most once: once it was read, the request is
//! never repeated (no retry, no redirect). A recreatable source is opened
//! again for every attempt. A declared length or SHA-256 is checked against
//! the bytes read; the last chunk is held back until both match, so a source
//! that differs never completes its request.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, ReadBuf};

use crate::client::ClientCore;
use crate::types::{CallOptions, OperationDescriptor, PreviewResult, Result};

/// A source's bytes, read by the transport.
pub type UploadReader = Box<dyn AsyncRead + Send + Unpin>;

type Opener = dyn Fn() -> std::io::Result<UploadReader> + Send + Sync;

/// How an [`UploadSource`] may be sent again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadReplay {
    /// Read at most once: a request whose body was read is never sent again.
    OneShot,
    /// Opened again from the start for every attempt.
    Recreatable,
}

impl UploadReplay {
    /// `one_shot` or `recreatable`, as the other runtimes name it.
    pub fn as_str(self) -> &'static str {
        match self {
            UploadReplay::OneShot => "one_shot",
            UploadReplay::Recreatable => "recreatable",
        }
    }
}

/// A request body streamed from the caller, sent with
/// [`ClientCore::call_upload`] as the whole binary body of an operation.
#[derive(Clone)]
pub struct UploadSource {
    replay: UploadReplay,
    length: Option<u64>,
    sha256: Option<String>,
    open: Arc<Opener>,
    opened: Arc<AtomicBool>,
}

impl std::fmt::Debug for UploadSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadSource")
            .field("replay", &self.replay)
            .field("length", &self.length)
            .field("sha256", &self.sha256)
            .finish_non_exhaustive()
    }
}

impl PartialEq for UploadSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.opened, &other.opened)
    }
}

impl Eq for UploadSource {}

impl UploadSource {
    /// A source that can be read once.
    pub fn one_shot(reader: impl AsyncRead + Send + Unpin + 'static) -> Self {
        let slot: Mutex<Option<UploadReader>> = Mutex::new(Some(Box::new(reader)));
        Self::new(UploadReplay::OneShot, move || {
            slot.lock()
                .ok()
                .and_then(|mut taken| taken.take())
                .ok_or_else(|| std::io::Error::other("the one-shot source was already read"))
        })
    }

    /// A source that `open` recreates from the start for every attempt (a
    /// file opened again, an object re-fetched); every attempt must produce
    /// the same bytes.
    pub fn recreatable(
        open: impl Fn() -> std::io::Result<UploadReader> + Send + Sync + 'static,
    ) -> Self {
        Self::new(UploadReplay::Recreatable, open)
    }

    fn new(
        replay: UploadReplay,
        open: impl Fn() -> std::io::Result<UploadReader> + Send + Sync + 'static,
    ) -> Self {
        UploadSource {
            replay,
            length: None,
            sha256: None,
            open: Arc::new(open),
            opened: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Declare the exact number of bytes; sent as `Content-Length`.
    pub fn with_length(mut self, length: u64) -> std::result::Result<Self, String> {
        if length > 9_007_199_254_740_991 {
            return Err("UploadSource length must be a non-negative safe integer".to_owned());
        }
        self.length = Some(length);
        Ok(self)
    }

    /// Declare the SHA-256 of the bytes as 64 hex digits. It is the
    /// `content_hash` idempotency key and binds confirmation tokens to the
    /// bytes; operations whose key or confirmation depends on the body
    /// require it.
    pub fn with_sha256(mut self, hex: &str) -> std::result::Result<Self, String> {
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("UploadSource sha256 must be 64 hexadecimal digits".to_owned());
        }
        self.sha256 = Some(hex.to_ascii_lowercase());
        Ok(self)
    }

    /// One-shot or recreatable.
    pub fn replay(&self) -> UploadReplay {
        self.replay
    }

    /// The declared length, if any.
    pub fn length(&self) -> Option<u64> {
        self.length
    }

    /// The declared SHA-256 (lower-case hex), if any.
    pub fn sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }

    /// Whether an attempt has started reading the source.
    pub fn opened(&self) -> bool {
        self.opened.load(Ordering::SeqCst)
    }

    /// Whether another attempt may send this source.
    pub fn sendable(&self) -> bool {
        self.replay == UploadReplay::Recreatable || !self.opened()
    }

    /// The body of one attempt.
    pub(crate) fn attempt(&self) -> UploadAttempt {
        UploadAttempt {
            source: self.clone(),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// What stands for the source in canonical JSON (it is never read for it).
    pub(crate) fn marker(&self) -> Value {
        json!({ "length": self.length, "sha256": self.sha256 })
    }
}

/// Why a source ended an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UploadFailure {
    /// `read` bytes, not the declared length.
    Length(u64),
    Sha256,
    /// The source failed; the detail names the error kind.
    Source(String),
}

#[derive(Default)]
struct State {
    reader: Option<UploadReader>,
    started: bool,
    finished: bool,
    read: u64,
    sent: u64,
    hash: Option<Sha256>,
    pending: Option<Bytes>,
    failure: Option<UploadFailure>,
}

/// One attempt's body and how it went.
#[derive(Clone)]
pub(crate) struct UploadAttempt {
    source: UploadSource,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for UploadAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadAttempt").finish_non_exhaustive()
    }
}

impl UploadAttempt {
    /// Bytes handed to the transport so far.
    pub(crate) fn sent(&self) -> u64 {
        self.state.lock().map_or(0, |s| s.sent)
    }

    /// Why the source ended the attempt, if it did.
    pub(crate) fn failure(&self) -> Option<UploadFailure> {
        self.state.lock().ok().and_then(|s| s.failure.clone())
    }

    /// The body the transport sends.
    pub(crate) fn body(&self) -> reqwest::Body {
        reqwest::Body::wrap(AttemptBody {
            attempt: self.clone(),
        })
    }
}

#[derive(Debug)]
struct Rejected;

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upload source rejected")
    }
}

impl std::error::Error for Rejected {}

struct AttemptBody {
    attempt: UploadAttempt,
}

impl AttemptBody {
    fn stop(
        state: &mut State,
        why: UploadFailure,
    ) -> Poll<Option<std::result::Result<http_body::Frame<Bytes>, Rejected>>> {
        state.failure = Some(why);
        state.finished = true;
        state.reader = None;
        Poll::Ready(Some(Err(Rejected)))
    }

    fn emit(
        state: &mut State,
        data: Bytes,
    ) -> Poll<Option<std::result::Result<http_body::Frame<Bytes>, Rejected>>> {
        state.sent += data.len() as u64;
        Poll::Ready(Some(Ok(http_body::Frame::data(data))))
    }
}

impl http_body::Body for AttemptBody {
    type Data = Bytes;
    type Error = Rejected;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<http_body::Frame<Bytes>, Rejected>>> {
        let attempt = self.attempt.clone();
        let source = &attempt.source;
        let Ok(mut guard) = attempt.state.lock() else {
            return Poll::Ready(Some(Err(Rejected)));
        };
        let state = &mut *guard;
        if state.finished {
            return Poll::Ready(None);
        }
        if !state.started {
            state.started = true;
            source.opened.store(true, Ordering::SeqCst);
            match (source.open)() {
                Ok(reader) => state.reader = Some(reader),
                Err(error) => {
                    return Self::stop(state, UploadFailure::Source(format!("{:?}", error.kind())));
                }
            }
            if source.sha256.is_some() {
                state.hash = Some(Sha256::new());
            }
        }
        let verify = source.length.is_some() || source.sha256.is_some();
        loop {
            let Some(reader) = state.reader.as_mut() else {
                return Poll::Ready(None);
            };
            let mut buf = vec![0u8; 64 * 1024];
            let mut read = ReadBuf::new(&mut buf);
            match Pin::new(reader).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    return Self::stop(state, UploadFailure::Source(format!("{:?}", error.kind())));
                }
                Poll::Ready(Ok(())) => {}
            }
            let filled = read.filled().len();
            if filled == 0 {
                // The end of the source.
                if source.length.is_some_and(|length| state.read != length) {
                    let read = state.read;
                    return Self::stop(state, UploadFailure::Length(read));
                }
                if let Some(hash) = state.hash.take() {
                    let digest: String =
                        hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
                    if source.sha256.as_deref() != Some(digest.as_str()) {
                        return Self::stop(state, UploadFailure::Sha256);
                    }
                }
                state.finished = true;
                state.reader = None;
                return match state.pending.take() {
                    Some(last) => Self::emit(state, last),
                    None => Poll::Ready(None),
                };
            }
            buf.truncate(filled);
            state.read += filled as u64;
            if let Some(hash) = state.hash.as_mut() {
                hash.update(&buf);
            }
            if source.length.is_some_and(|length| state.read > length) {
                let read = state.read;
                return Self::stop(state, UploadFailure::Length(read));
            }
            let chunk = Bytes::from(buf);
            if !verify {
                return Self::emit(state, chunk);
            }
            // The last chunk waits until the declared length and digest match.
            if let Some(ready) = state.pending.replace(chunk) {
                return Self::emit(state, ready);
            }
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self.attempt.source.length {
            Some(length) => http_body::SizeHint::with_exact(length),
            None => http_body::SizeHint::default(),
        }
    }
}

/// The upload of the call running in this task: the operation it belongs to,
/// its body argument and the source.
#[derive(Debug, Clone)]
pub(crate) struct Active {
    pub op: String,
    pub arg: String,
    pub source: UploadSource,
}

tokio::task_local! {
    static ACTIVE: Option<Active>;
}

/// The upload of `operation` in the call running in this task, if any.
pub(crate) fn active_for(operation: &str) -> Option<Active> {
    ACTIVE
        .try_with(|active| active.clone())
        .ok()
        .flatten()
        .filter(|active| active.op == operation)
}

/// The arguments' digest text when `operation` carries an upload: the
/// canonical JSON with the source's argument as `null`, then its declared
/// length and SHA-256 in a separate manifest, as in the TypeScript runtime.
pub(crate) fn digest_text(operation: &str, args: &Value) -> Option<String> {
    let active = active_for(operation)?;
    let mut value = args.clone();
    if let Value::Object(map) = &mut value {
        map.insert(active.arg.clone(), Value::Null);
    }
    let manifest = json!([{ "path": [active.arg], "upload": active.source.marker() }]);
    Some(format!(
        "{}\nblob-content-v1\n{}",
        crate::util::canonical_json(&value),
        crate::util::canonical_json(&manifest)
    ))
}

/// The arguments of an upload call: `args` with the body argument set to an
/// empty binary value, which validation accepts and the source replaces.
fn with_placeholder(op: &OperationDescriptor, args: Value) -> (Value, Option<String>) {
    let Some(crate::types::BodyShape::Arg { arg }) = op.body.as_ref().map(|b| &b.shape) else {
        return (args, None);
    };
    let mut map = match args {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let placeholder =
        serde_json::to_value(crate::value::Binary::new(Vec::new())).unwrap_or(Value::Null);
    map.insert(arg.clone(), placeholder);
    (Value::Object(map), Some(arg.clone()))
}

impl ClientCore {
    /// [`ClientCore::call`] with `source` as the operation's whole binary
    /// request body (the body argument in `args` is replaced). A one-shot
    /// source is never sent twice: a failure after it was read is returned,
    /// not retried. A declared length or SHA-256 that the bytes do not match
    /// abandons the request before its last bytes (`VALIDATION_FAILED`,
    /// `code` `UPLOAD_MISMATCH`); a source that fails ends it
    /// (`UPLOAD_SOURCE_FAILED`, `OUTCOME_UNKNOWN` for a mutation).
    pub async fn call_upload(
        &self,
        op: &OperationDescriptor,
        args: Value,
        opts: &CallOptions,
        source: UploadSource,
    ) -> Result<Option<Value>> {
        let (args, arg) = with_placeholder(op, args);
        let active = arg.map(|arg| Active {
            op: op.id.clone(),
            arg,
            source,
        });
        ACTIVE.scope(active, self.call(op, args, opts)).await
    }

    /// [`ClientCore::preview`] of an upload call: the source is never read;
    /// a confirmation token binds to its declared length and SHA-256.
    pub async fn preview_upload(
        &self,
        op: &OperationDescriptor,
        args: Value,
        opts: &CallOptions,
        source: UploadSource,
    ) -> Result<PreviewResult> {
        let (args, arg) = with_placeholder(op, args);
        let active = arg.map(|arg| Active {
            op: op.id.clone(),
            arg,
            source,
        });
        ACTIVE.scope(active, self.preview(op, args, opts)).await
    }
}

/// The error of an attempt whose upload source ended it.
pub(crate) fn attempt_failure(
    ctx: &crate::classify::CallContext<'_>,
    sent: &UploadAttempt,
    mutation: bool,
) -> Option<crate::types::Error> {
    use crate::envelope::Diag;
    use crate::types::{Category, Retryable};
    let why = sent.failure()?;
    let op = ctx.op;
    let bytes = sent.sent();
    let diagnostic = match why {
        UploadFailure::Source(detail) => {
            let cause = format!(
                "The upload source failed ({detail}) after {bytes} {} sent; the request was abandoned with an incomplete body.",
                if bytes == 1 { "byte was" } else { "bytes were" }
            );
            if mutation {
                crate::classify::outcome_unknown(
                    ctx,
                    &cause,
                    crate::classify::UnknownFields {
                        code: Some("UPLOAD_SOURCE_FAILED".to_owned()),
                        ..Default::default()
                    },
                )
            } else {
                Diag::new(op.id.clone(), Category::TransportFailed)
                    .code(Some("UPLOAD_SOURCE_FAILED".to_owned()))
                    .failed_parameter("body")
                    .retryable(Retryable::Never)
                    .attempts(ctx.attempts)
                    .remediation(format!(
                        "{cause} This read has no side effects; fix the source and call again."
                    ))
                    .build()
            }
        }
        why => {
            let (declared, expected) = match why {
                UploadFailure::Length(read) => (
                    format!("{read} bytes, not the declared length"),
                    "exactly the declared length of the upload",
                ),
                _ => (
                    "bytes whose SHA-256 differs from the declared sha256".to_owned(),
                    "bytes matching the declared sha256 of the upload",
                ),
            };
            Diag::new(op.id.clone(), Category::ValidationFailed)
                .code(Some("UPLOAD_MISMATCH".to_owned()))
                .failed_parameter("body")
                .retryable(Retryable::Never)
                .attempts(ctx.attempts)
                .expected(expected)
                .remediation(format!(
                    "The upload source produced {declared}; the request was abandoned before its last bytes were sent, so the server did not receive a complete body. Fix the source or its declaration and call again."
                ))
                .build()
        }
    };
    Some(crate::prepare::fail(diagnostic))
}

/// The note on a failure that is not retried because its one-shot upload was read.
pub(crate) fn one_shot_spent(mut error: crate::types::Error) -> crate::types::Error {
    error.diagnostic.remediation.push_str(
        " The upload body is one-shot and was already read, so it was not sent again; pass UploadSource::recreatable(...) to allow retries.",
    );
    error
}
