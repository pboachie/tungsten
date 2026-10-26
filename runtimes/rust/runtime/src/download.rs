// SPDX-License-Identifier: Apache-2.0
//! Streamed downloads: the body of a 2xx answer handed to the caller chunk
//! by chunk instead of being held in memory, with a total-byte limit,
//! progress reports, the idle timeout and the call's deadline, and one
//! release of the connection (and its scheduler capacity) however the
//! download ends. The semantics of `Download` in `runtimes/ts/src/download.ts`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::classify::is_mutation;
use crate::client::ClientCore;
use crate::envelope::{Diag, scrub_diagnostic};
use crate::prepare::{Purpose, fail};
use crate::send::Handover;
use crate::stream::Sent;
use crate::transport::{BodyRead, EventBody};
use crate::types::{
    CallOptions, Category, Diagnostic, Error, OperationDescriptor, ResponseMeta, Retryable,
};
use crate::util::{SecretSet, envelope_value};

/// How far a download has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferProgress {
    /// Body bytes received so far.
    pub bytes: u64,
    /// The declared `Content-Length`, or `None` when the server sent none.
    pub total: Option<u64>,
}

/// The options of [`ClientCore::download`] next to the [`CallOptions`].
#[derive(Clone, Default)]
pub struct DownloadOptions {
    /// Most body bytes the download may deliver. A larger declared
    /// `Content-Length` fails before any byte is read; a body that grows
    /// past it ends the download (`UNEXPECTED_RESPONSE`, `code`
    /// `RESPONSE_TOO_LARGE`). `max_response_bytes` does not apply to a
    /// streamed body, only to an error body. Default: no limit.
    pub max_bytes: Option<u64>,
    /// Called after every chunk with the bytes received so far.
    pub on_progress: Option<Arc<dyn Fn(TransferProgress) + Send + Sync>>,
}

impl std::fmt::Debug for DownloadOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadOptions")
            .field("max_bytes", &self.max_bytes)
            .field("on_progress", &self.on_progress.is_some())
            .finish()
    }
}

/// Why a download ended without its whole body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    Idle,
    Deadline,
    Aborted,
    Lost,
    Truncated,
    Oversized,
    Sink,
}

/// What a failure of the download says, fixed when it opened.
#[derive(Debug)]
struct Context {
    op: String,
    mutation: bool,
    hint: Option<String>,
    secrets: SecretSet,
    idle: Option<Duration>,
    timeout: Duration,
    max_bytes: Option<u64>,
}

impl Context {
    fn failure(&self, meta: &ResponseMeta, why: Why, bytes: u64, detail: Option<&str>) -> Error {
        let after = format!(
            "after {bytes} {}",
            if bytes == 1 { "byte" } else { "bytes" }
        );
        let code = match why {
            Why::Idle => Some("STREAM_IDLE"),
            Why::Deadline => Some("DEADLINE_EXCEEDED"),
            Why::Oversized => Some("RESPONSE_TOO_LARGE"),
            Why::Sink => Some("SINK_FAILED"),
            _ => None,
        }
        .map(str::to_owned);
        let cause = match why {
            Why::Idle => format!(
                "No bytes arrived within {} ms {after}; the download was abandoned.",
                self.idle.unwrap_or(self.timeout).as_millis()
            ),
            Why::Deadline => format!(
                "The operation deadline (`deadline`) expired {after}; the download was abandoned."
            ),
            Why::Aborted => format!("The download was cancelled {after}."),
            Why::Lost => format!("The connection failed {after}, before the body ended."),
            Why::Truncated => format!(
                "The body ended {after}, short of its Content-Length of {}.",
                declared_length(meta.headers.get("content-length").map(String::as_str))
                    .unwrap_or(0)
            ),
            Why::Oversized => format!(
                "The body is larger than the limit of {} bytes (`max_bytes`); the download was abandoned {after}.",
                self.max_bytes.unwrap_or(0)
            ),
            Why::Sink => format!(
                "The sink failed ({}) {after}; the download was abandoned.",
                detail.unwrap_or("error")
            ),
        };
        let diag = if self.mutation {
            Diag::new(self.op.clone(), Category::UnexpectedResponse)
                .retryable(Retryable::Never)
                .next_action(self.hint.clone())
                .remediation(format!(
                    "The server accepted {} (HTTP {}). {cause} The call took effect; do not repeat it.",
                    self.op, meta.status
                ))
        } else {
            let category = match why {
                Why::Idle | Why::Deadline => Category::UpstreamUnavailable,
                Why::Oversized => Category::UnexpectedResponse,
                _ => Category::TransportFailed,
            };
            let again = match why {
                Why::Idle if self.idle.is_none() => "call again later or with a larger `timeout`",
                Why::Idle => "call again later or with a larger `idle_timeout`",
                Why::Deadline => "call again with a larger `deadline`",
                Why::Oversized => "raise `max_bytes` if a body this large is expected",
                _ => "call again",
            };
            let local = matches!(
                why,
                Why::Aborted | Why::Sink | Why::Oversized | Why::Deadline
            );
            Diag::new(self.op.clone(), category)
                .retryable_opt(local.then_some(Retryable::Never))
                .remediation(format!("{cause} This read has no side effects; {again}."))
        };
        let diagnostic: Diagnostic = diag
            .http_status(Some(meta.status))
            .code(code)
            .request_id(meta.request_id.clone())
            .attempts(meta.attempts)
            .build();
        fail(scrub_diagnostic(diagnostic, &self.secrets))
    }
}

/// A streamed response body ([`ClientCore::download`]). Read it with
/// [`Download::read`] or hand it to a sink with [`Download::write_to`];
/// [`Download::close`] (or dropping it) abandons it. The connection and its
/// scheduler capacity are released once the body ended, failed or was
/// closed. After the first failure every read returns that failure.
pub struct Download {
    meta: ResponseMeta,
    content_length: Option<u64>,
    content_type: Option<String>,
    filename: Option<String>,
    /// `None` for an answer without a body, or once released.
    body: Option<EventBody>,
    received: u64,
    on_progress: Option<Arc<dyn Fn(TransferProgress) + Send + Sync>>,
    context: Context,
    /// How the download ended: `Ok` at the end of the body.
    ended: Option<Result<(), Error>>,
}

impl std::fmt::Debug for Download {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Download")
            .field("operation", &self.context.op)
            .field("status", &self.meta.status)
            .field("received", &self.received)
            .finish_non_exhaustive()
    }
}

impl Download {
    /// The answer's status, headers, request id and attempts.
    pub fn meta(&self) -> &ResponseMeta {
        &self.meta
    }

    /// The declared `Content-Length`, or `None`.
    pub fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    /// The `Content-Type`, or `None`.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// The file name of `Content-Disposition`, reduced to a plain base name
    /// (no directory part, no control characters), or `None`. It names no
    /// destination: callers choose where the bytes go.
    pub fn filename(&self) -> Option<&str> {
        self.filename.as_deref()
    }

    /// Body bytes received so far.
    pub fn received_bytes(&self) -> u64 {
        self.received
    }

    /// The next chunk, `None` at the end, or the failure that ended the
    /// download. Dropping the future while it waits abandons that read; the
    /// download stays open until it is closed or dropped.
    pub async fn read(&mut self) -> Result<Option<Vec<u8>>, Error> {
        if let Some(ended) = &self.ended {
            return ended.clone().map(|()| None);
        }
        let read = match &mut self.body {
            Some(body) => body.read().await,
            None => BodyRead::End,
        };
        match read {
            BodyRead::Chunk(bytes) => {
                self.received += bytes.len() as u64;
                if self
                    .context
                    .max_bytes
                    .is_some_and(|limit| self.received > limit)
                {
                    return Err(self.end(Why::Oversized, None));
                }
                if let Some(observer) = &self.on_progress {
                    // Observers never change the transfer.
                    let progress = TransferProgress {
                        bytes: self.received,
                        total: self.content_length,
                    };
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        observer(progress);
                    }));
                }
                Ok(Some(bytes))
            }
            BodyRead::End => {
                if self
                    .content_length
                    .is_some_and(|length| self.received < length)
                {
                    return Err(self.end(Why::Truncated, None));
                }
                self.ended = Some(Ok(()));
                self.body = None;
                Ok(None)
            }
            BodyRead::Timeout => Err(self.end(Why::Idle, None)),
            BodyRead::Deadline => Err(self.end(Why::Deadline, None)),
            BodyRead::Lost => Err(self.end(Why::Lost, None)),
        }
    }

    /// Hand every chunk to `sink`, waiting for it before the next read (a
    /// slow sink slows the transfer instead of growing a buffer); `Ok`
    /// holds the bytes delivered. A sink that fails ends the download
    /// (`code` `SINK_FAILED`). The download is closed when this returns.
    pub async fn write_to<F, Fut>(mut self, mut sink: F) -> Result<u64, Error>
    where
        F: FnMut(Vec<u8>) -> Fut,
        Fut: Future<Output = std::io::Result<()>>,
    {
        loop {
            match self.read().await {
                Err(error) => return Err(error),
                Ok(None) => return Ok(self.received),
                Ok(Some(chunk)) => {
                    if let Err(error) = sink(chunk).await {
                        return Err(self.end(Why::Sink, Some(&format!("{:?}", error.kind()))));
                    }
                }
            }
        }
    }

    /// Abandon the download and release the connection. Safe to call more
    /// than once.
    pub fn close(&mut self) {
        if self.ended.is_none() {
            let failure = self
                .context
                .failure(&self.meta, Why::Aborted, self.received, None);
            self.ended = Some(Err(failure));
        }
        self.body = None;
    }

    fn end(&mut self, why: Why, detail: Option<&str>) -> Error {
        let failure = self.context.failure(&self.meta, why, self.received, detail);
        self.ended = Some(Err(failure.clone()));
        self.body = None;
        failure
    }
}

impl ClientCore {
    /// Send the operation and hand its 2xx body over unread, as a
    /// [`Download`] to read chunk by chunk or write to a sink: the body is
    /// never held in memory whole. Everything before the answer
    /// (validation, auth, idempotency, confirmation, retries, the
    /// scheduler, `deadline`) follows the rules of [`ClientCore::call`]; an
    /// error status is read and classified as for `call`. The body's reads
    /// are bounded by `ClientOptions::idle_timeout` (default the attempt
    /// `timeout`), by `deadline` and by [`DownloadOptions::max_bytes`]; the
    /// connection and its scheduler capacity are held until the download
    /// ends or is closed.
    pub async fn download(
        &self,
        op: &OperationDescriptor,
        args: Value,
        opts: &CallOptions,
        transfer: DownloadOptions,
    ) -> Result<Download, Error> {
        // The operation ends when the answer is handed over; reading the
        // body is not part of it.
        let span = self.start_span(op, crate::TelemetryEntry::Download);
        let pending =
            crate::telemetry::Pending::new(span.as_ref(), crate::telemetry::PendingKind::Operation);
        let result = crate::telemetry::scoped(
            span.clone(),
            crate::verify::with_call_deadline(
                self,
                opts,
                self.download_with(op, &args, opts, transfer),
            ),
        )
        .await;
        pending.done();
        if let Some(span) = span {
            span.end(result.as_ref().err().map(|e| &*e.diagnostic), false, None);
        }
        result
    }

    async fn download_with(
        &self,
        op: &OperationDescriptor,
        args: &Value,
        opts: &CallOptions,
        transfer: DownloadOptions,
    ) -> Result<Download, Error> {
        if let Some(limit) = transfer.max_bytes
            && limit > 9_007_199_254_740_991
        {
            return Err(fail(
                Diag::new(op.id.clone(), Category::ValidationFailed)
                    .failed_parameter("max_bytes")
                    .received_value(envelope_value(&Value::from(limit), false))
                    .expected("a non-negative safe integer")
                    .remediation("Pass max_bytes as a whole number of bytes, or leave it out for no limit. Nothing was sent.")
                    .build(),
            ));
        }
        let mut prepared = self
            .prepare(op, args, opts, Purpose::Call, None, None)
            .await?;
        let sent = match self.send_with(&prepared, opts, Handover::Bytes, None).await {
            Err(rejected)
                if rejected.diagnostic.http_status == Some(401) && prepared.oauth.is_some() =>
            {
                // An authorization-code token the server no longer accepts:
                // refresh it and send once more. Never a second time.
                self.check_send_deadline(&prepared, rejected.diagnostic.trace.attempts, None)?;
                let scheme = prepared.oauth.take().unwrap_or_default();
                let current = prepared
                    .headers
                    .get("Authorization")
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .map(str::to_owned);
                let Some(current) = current else {
                    return Err(rejected);
                };
                let refreshed = self.refresh_after_rejection(&scheme, &current).await;
                self.check_send_deadline(&prepared, rejected.diagnostic.trace.attempts, None)?;
                let Some(token) = refreshed else {
                    return Err(rejected);
                };
                prepared
                    .headers
                    .set("Authorization", format!("Bearer {token}"), true);
                prepared.secrets.insert(token);
                self.send_with(&prepared, opts, Handover::Bytes, None)
                    .await?
            }
            sent => sent?,
        };
        let (meta, body) = match sent {
            Sent::Stream(start) => (start.meta, Some(start.body)),
            Sent::Response(response, _) => (response.meta, None),
        };
        let context = Context {
            op: op.id.clone(),
            mutation: is_mutation(op),
            hint: op
                .agent
                .verify
                .as_ref()
                .map(|v| format!("Call {} to read the current state.", v.operation)),
            secrets: prepared.secrets.clone(),
            idle: self.inner.idle_timeout,
            timeout: self.timeout(opts),
            max_bytes: transfer.max_bytes,
        };
        let content_length =
            declared_length(meta.headers.get("content-length").map(String::as_str));
        if let (Some(limit), Some(length)) = (transfer.max_bytes, content_length)
            && length > limit
        {
            return Err(context.failure(&meta, Why::Oversized, 0, None));
        }
        Ok(Download {
            content_type: meta.headers.get("content-type").cloned(),
            filename: disposition_filename(
                meta.headers.get("content-disposition").map(String::as_str),
            ),
            content_length,
            meta,
            body,
            received: 0,
            on_progress: transfer.on_progress,
            context,
            ended: None,
        })
    }
}

/// A `Content-Length` of digits only, else `None`.
pub(crate) fn declared_length(value: Option<&str>) -> Option<u64> {
    let text = value?.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>()
        .ok()
        .filter(|n| *n <= 9_007_199_254_740_991)
}

/// The parameters of a header value such as `Content-Disposition`, after
/// its first `;`; names lower-cased, quoted strings unescaped.
fn header_params(value: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = value.chars().collect();
    let find = |c: char, from: usize| (from..chars.len()).find(|&k| chars[k] == c);
    let mut out: Vec<(String, String)> = Vec::new();
    let Some(first) = find(';', 0) else {
        return out;
    };
    let mut i = first + 1;
    let n = chars.len();
    while i < n {
        let eq = find('=', i);
        let semi = find(';', i);
        let Some(eq) = eq.filter(|&eq| semi.is_none_or(|s| s > eq)) else {
            match semi {
                Some(s) => {
                    i = s + 1;
                    continue;
                }
                None => break,
            }
        };
        let name: String = chars[i..eq]
            .iter()
            .collect::<String>()
            .trim()
            .to_lowercase();
        let mut j = eq + 1;
        while j < n && (chars[j] == ' ' || chars[j] == '\t') {
            j += 1;
        }
        let mut param = String::new();
        if j < n && chars[j] == '"' {
            j += 1;
            while j < n && chars[j] != '"' {
                if chars[j] == '\\' && j + 1 < n {
                    j += 1;
                }
                param.push(chars[j]);
                j += 1;
            }
            i = find(';', j).map_or(n, |s| s + 1);
        } else {
            let end = find(';', j).unwrap_or(n);
            param = chars[j..end].iter().collect::<String>().trim().to_owned();
            i = if end < n { end + 1 } else { n };
        }
        if !name.is_empty() && !out.iter().any(|(known, _)| *known == name) {
            out.push((name, param));
        }
    }
    out
}

/// An RFC 8187 `UTF-8''percent-encoded` value, decoded; `None` otherwise.
fn extended_value(value: &str) -> Option<String> {
    let quote = value.find('\'')?;
    let second = quote + 1 + value[quote + 1..].find('\'')?;
    if !value[..quote].eq_ignore_ascii_case("utf-8") {
        return None;
    }
    let encoded = &value.as_bytes()[second + 1..];
    let mut out = Vec::with_capacity(encoded.len());
    let mut i = 0;
    while i < encoded.len() {
        let c = encoded[i];
        if c == b'%' {
            let pair = encoded.get(i + 1..i + 3)?;
            let text = std::str::from_utf8(pair).ok()?;
            if !pair.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            out.push(u8::from_str_radix(text, 16).ok()?);
            i += 3;
        } else if !(0x21..=0x7e).contains(&c) {
            return None;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The file name a `Content-Disposition` value suggests (`filename*` before
/// `filename`), reduced to a base name that cannot leave a directory: the
/// part after the last `/` or `\`, control characters removed, spaces
/// trimmed, at most 255 characters; `None` when nothing usable is left
/// (empty, `.` or `..`).
pub fn disposition_filename(value: Option<&str>) -> Option<String> {
    let params = header_params(value?);
    let get = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let raw = get("filename*")
        .and_then(|v| extended_value(&v))
        .or_else(|| get("filename"))?;
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let stripped: String = base
        .chars()
        .filter(|c| !matches!(*c as u32, 0x00..=0x1f | 0x7f))
        .collect();
    let cleaned: String = stripped.trim_matches(' ').chars().take(255).collect();
    let cleaned = cleaned.trim_matches(' ');
    (!matches!(cleaned, "" | "." | "..")).then(|| cleaned.to_owned())
}
