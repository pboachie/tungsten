// SPDX-License-Identifier: Apache-2.0
//! Event streams (`text/event-stream`): the events of an operation as an
//! asynchronous iterator, with the semantics of `ClientCore.stream` in
//! `runtimes/ts/src/client.ts`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::classify::{CallContext, OutcomeCheck, UnknownFields, is_mutation, outcome_unknown};
use crate::client::ClientCore;
use crate::envelope::{Diag, scrub_diagnostic};
use crate::idempotency::has_replay_protection;
use crate::prepare::{Purpose, fail};
use crate::sse::{SseEvent, SseParser, Utf8Decoder};
use crate::transport::{BodyRead, EventBody};
use crate::types::{
    BodyShape, CallOptions, Category, Error, OperationDescriptor, PathSegment, Response,
    ResponseMeta, Retryable, StreamDescriptor, ValidateResponses, Validation,
};
use crate::util::{
    SecretSet, envelope_value, get_path, looks_sensitive, merge_accept, redact_paths,
};

/// Default [`ClientOptions::max_collect_bytes`](crate::ClientOptions): the
/// JSON bytes of the events a collection keeps (4 MiB).
pub const DEFAULT_MAX_COLLECT_BYTES: usize = 4 * 1024 * 1024;

/// Default [`ClientOptions::max_collect_time`](crate::ClientOptions): the
/// wall-clock time a collection may take (60 s).
pub const DEFAULT_MAX_COLLECT_TIME: Duration = Duration::from_secs(60);

/// What sending a request gave: a decoded response, or an event stream whose
/// body has not been read.
pub(crate) enum Sent {
    Response(Response<Option<Value>>),
    Stream(StreamStart),
}

pub(crate) struct StreamStart {
    pub meta: ResponseMeta,
    pub body: EventBody,
}

/// One event of a stream.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEvent<T> {
    /// The event's `data`, decoded from JSON.
    pub value: T,
    /// The `event` field of the server-sent event, `message` when it has none.
    pub event: String,
    /// The last event id the stream has set so far.
    pub id: Option<String>,
    /// The last `retry` value (milliseconds) the stream has set so far.
    pub retry: Option<u64>,
    pub meta: ResponseMeta,
}

/// What iterating a stream yields: events, then the end of the stream or one
/// final error (an error before the stream starts is the only item).
pub type StreamResult<T> = std::result::Result<StreamEvent<T>, Error>;

/// An open stream.
struct Open {
    body: EventBody,
    parser: SseParser,
    decoder: Utf8Decoder,
    queue: VecDeque<SseEvent>,
    ended: bool,
    /// The parser stopped at an event over the size limit; the events queued
    /// before it are delivered first.
    oversize: bool,
    meta: ResponseMeta,
    count: usize,
    key: Option<String>,
    key_header: String,
    check: Option<OutcomeCheck>,
    secrets: SecretSet,
    timeout: std::time::Duration,
}

enum State {
    Idle { args: Value, opts: Box<CallOptions> },
    Open(Box<Open>),
    Done,
}

/// An asynchronous event iterator: call [`EventStream::next`] until it
/// returns `None`. The request is sent by the first call. After an error item
/// it returns `None`; dropping the stream closes the connection.
pub struct EventStream {
    core: ClientCore,
    op: Arc<OperationDescriptor>,
    spec: Arc<StreamDescriptor>,
    state: State,
}

impl std::fmt::Debug for EventStream {
    /// The operation only: arguments and options can hold secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream")
            .field("operation", &self.op.id)
            .finish_non_exhaustive()
    }
}

impl EventStream {
    pub(crate) fn new(
        core: ClientCore,
        op: Arc<OperationDescriptor>,
        spec: Arc<StreamDescriptor>,
        args: Value,
        opts: CallOptions,
    ) -> Self {
        EventStream {
            core,
            op,
            spec,
            state: State::Idle {
                args,
                opts: Box::new(opts),
            },
        }
    }

    /// The next event, the final error, or `None` at the end of the stream.
    /// Dropping the returned future before it completes loses nothing: the
    /// state only changes once an await has finished.
    pub async fn next(&mut self) -> Option<StreamResult<Value>> {
        loop {
            match &mut self.state {
                State::Done => return None,
                State::Idle { args, opts } => {
                    match self
                        .core
                        .open_stream(&self.op, &self.spec, args, opts)
                        .await
                    {
                        Ok(open) => self.state = State::Open(Box::new(open)),
                        Err(error) => {
                            self.state = State::Done;
                            return Some(Err(error));
                        }
                    }
                }
                State::Open(open) => {
                    if let Some(event) = open.queue.pop_front() {
                        match self.core.accept(&self.op, &self.spec, open, event) {
                            Accepted::Event(item) => return Some(Ok(item)),
                            Accepted::Done => {
                                self.state = State::Done;
                                return None;
                            }
                            Accepted::Failed(error) => {
                                self.state = State::Done;
                                return Some(Err(error));
                            }
                        }
                    }
                    if open.oversize {
                        let error = self.core.oversize(&self.op, open);
                        self.state = State::Done;
                        return Some(Err(error));
                    }
                    if open.ended {
                        self.state = State::Done;
                        return None;
                    }
                    match open.body.read().await {
                        BodyRead::Chunk(bytes) => {
                            let text = open.decoder.decode(&bytes);
                            let events = open.parser.push(&text);
                            open.queue.extend(events);
                            open.oversize = open.parser.exceeded();
                        }
                        BodyRead::End => {
                            let text = open.decoder.finish();
                            let mut events = open.parser.push(&text);
                            events.extend(open.parser.end());
                            open.queue.extend(events);
                            open.oversize = open.parser.exceeded();
                            open.ended = true;
                        }
                        BodyRead::Timeout => {
                            let error = self.core.interrupted(&self.op, open, true);
                            self.state = State::Done;
                            return Some(Err(error));
                        }
                        BodyRead::Lost => {
                            let error = self.core.interrupted(&self.op, open, false);
                            self.state = State::Done;
                            return Some(Err(error));
                        }
                    }
                }
            }
        }
    }

    /// Every event of the stream, in order, followed by the final error if the
    /// stream failed. The collection is bounded: it ends with an
    /// `UNEXPECTED_RESPONSE` error item once the events kept hold more than
    /// `ClientOptions::max_collect_bytes` of JSON (4 MiB by default) or it has
    /// run longer than `ClientOptions::max_collect_time` (60 s by default).
    pub async fn collect_values(mut self) -> Vec<StreamResult<Value>> {
        let mut limits = Limits::new(&self.core);
        let mut out = Vec::new();
        loop {
            let item = match limits.pull(self.next()).await {
                Ok(Some(item)) => item,
                Ok(None) => return out,
                Err(over) => {
                    out.push(Err(self.core.over_budget(&self.op, over, out.len())));
                    return out;
                }
            };
            match item {
                Ok(event) => {
                    if let Err(over) = limits.admit(&event.value) {
                        out.push(Err(self.core.over_budget(&self.op, over, out.len())));
                        return out;
                    }
                    out.push(Ok(event));
                }
                Err(error) => {
                    out.push(Err(error));
                    return out;
                }
            }
        }
    }

    /// Events typed as `T` (an event that does not decode is an
    /// `UNEXPECTED_RESPONSE` error item).
    pub fn typed<T: DeserializeOwned>(self) -> TypedEvents<T> {
        TypedEvents {
            inner: self,
            seen: 0,
            marker: std::marker::PhantomData,
        }
    }
}

/// [`EventStream`] with typed events.
#[derive(Debug)]
pub struct TypedEvents<T> {
    inner: EventStream,
    seen: usize,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned + serde::Serialize> TypedEvents<T> {
    /// [`EventStream::collect_values`] for typed events: each is encoded
    /// back to JSON (the bytes counted are those of the encoded value).
    pub async fn collect_values(mut self) -> Vec<StreamResult<Value>> {
        let mut limits = Limits::new(&self.inner.core);
        let mut out = Vec::new();
        loop {
            let item = match limits.pull(self.next()).await {
                Ok(Some(item)) => item,
                Ok(None) => return out,
                Err(over) => {
                    let error = self.inner.core.over_budget(&self.inner.op, over, out.len());
                    out.push(Err(error));
                    return out;
                }
            };
            let event = match item {
                Ok(event) => event,
                Err(error) => {
                    out.push(Err(error));
                    return out;
                }
            };
            let StreamEvent {
                value,
                event: name,
                id,
                retry,
                meta,
            } = event;
            let value = match serde_json::to_value(&value) {
                Ok(value) => value,
                Err(error) => {
                    out.push(Err(Error::new(
                        Diag::new(self.inner.op.id.clone(), Category::MalformedRequest)
                            .failed_parameter("event")
                            .expected("a value that serializes to JSON")
                            .remediation(error.to_string())
                            .build(),
                    )));
                    return out;
                }
            };
            if let Err(over) = limits.admit(&value) {
                let error = self.inner.core.over_budget(&self.inner.op, over, out.len());
                out.push(Err(error));
                return out;
            }
            out.push(Ok(StreamEvent {
                value,
                event: name,
                id,
                retry,
                meta,
            }));
        }
    }
}

impl<T: DeserializeOwned> TypedEvents<T> {
    pub async fn next(&mut self) -> Option<StreamResult<T>> {
        let item = self.inner.next().await?;
        let index = self.seen;
        self.seen += 1;
        Some(crate::dispatch::decode_event(
            &self.inner.op.id,
            index,
            item,
        ))
    }
}

/// Which limit of a collection was passed.
#[derive(Clone, Copy)]
enum Over {
    Bytes,
    Time,
}

/// The limits of collecting a stream.
struct Limits {
    deadline: tokio::time::Instant,
    bytes: usize,
    max_bytes: usize,
}

impl Limits {
    fn new(core: &ClientCore) -> Self {
        Limits {
            deadline: tokio::time::Instant::now() + core.inner.max_collect_time,
            bytes: 0,
            max_bytes: core.inner.max_collect_bytes,
        }
    }

    /// Await `next` until the deadline.
    async fn pull<F: std::future::Future>(&self, next: F) -> std::result::Result<F::Output, Over> {
        tokio::time::timeout_at(self.deadline, next)
            .await
            .map_err(|_| Over::Time)
    }

    /// Count the JSON bytes of an event, and check the deadline (a stream
    /// that never waits would not trip the timeout of `pull`).
    fn admit(&mut self, value: &Value) -> std::result::Result<(), Over> {
        if tokio::time::Instant::now() >= self.deadline {
            return Err(Over::Time);
        }
        self.bytes = self
            .bytes
            .saturating_add(serde_json::to_string(value).map_or(0, |text| text.len()));
        if self.bytes > self.max_bytes {
            Err(Over::Bytes)
        } else {
            Ok(())
        }
    }
}

enum Accepted {
    Event(StreamEvent<Value>),
    /// The done sentinel: the stream ends without delivering it.
    Done,
    Failed(Error),
}

/// The args of a stream call: the stream's request flag set to `true`, in the
/// body field of a merged body or inside an object body argument.
fn with_stream_flag(op: &OperationDescriptor, spec: &StreamDescriptor, args: &Value) -> Value {
    let (Some(flag), Some(body), Value::Object(map)) = (&spec.flag, &op.body, args) else {
        return args.clone();
    };
    let mut out: Map<String, Value> = map.clone();
    match &body.shape {
        BodyShape::Merged { fields } => {
            if let Some(field) = fields.iter().find(|f| &f.wire == flag) {
                out.insert(field.arg.clone(), Value::Bool(true));
            }
        }
        BodyShape::Arg { arg } => {
            if let Some(Value::Object(inner)) = map.get(arg) {
                let mut inner = inner.clone();
                inner.insert(flag.clone(), Value::Bool(true));
                out.insert(arg.clone(), Value::Object(inner));
            }
        }
    }
    Value::Object(out)
}

impl ClientCore {
    /// Iterate the events of an operation's event stream (see
    /// [`EventStream`]). `spec` is the stream's descriptor.
    pub fn stream(
        &self,
        op: Arc<OperationDescriptor>,
        spec: Arc<StreamDescriptor>,
        args: Value,
        opts: CallOptions,
    ) -> EventStream {
        EventStream::new(self.clone(), op, spec, args, opts)
    }

    async fn open_stream(
        &self,
        op: &OperationDescriptor,
        spec: &StreamDescriptor,
        args: &Value,
        opts: &CallOptions,
    ) -> std::result::Result<Open, Error> {
        let mut opts = opts.clone();
        let mut headers = BTreeMap::new();
        let mut accept = String::new();
        let own = std::mem::take(&mut opts.headers);
        for (own, map) in [(false, &self.inner.headers), (true, &own)] {
            for (name, value) in map {
                if name.eq_ignore_ascii_case("accept") {
                    accept.clone_from(value);
                } else if own {
                    headers.insert(name.clone(), value.clone());
                }
            }
        }
        headers.insert("Accept".to_owned(), merge_accept(&accept));
        opts.headers = headers;
        let args = with_stream_flag(op, spec, args);
        let prepared = self
            .prepare(op, &args, &opts, Purpose::Call, None, None)
            .await?;
        let start = match self.send_with(&prepared, &opts, true).await? {
            Sent::Stream(start) => start,
            Sent::Response(response) => {
                let content_type = response.meta.headers.get("content-type").cloned();
                let after = if is_mutation(op) {
                    " The call took effect; do not repeat it."
                } else {
                    ""
                };
                return Err(fail(
                    Diag::new(op.id.clone(), Category::UnexpectedResponse)
                        .http_status(Some(response.meta.status))
                        .request_id(response.meta.request_id.clone())
                        .failed_parameter("response")
                        .received_value(envelope_value(
                            &content_type.clone().map_or(Value::Null, Value::String),
                            false,
                        ))
                        .expected("a text/event-stream body")
                        .remediation(format!(
                            "The success response is not an event stream (Content-Type {}).{after}",
                            content_type.as_deref().unwrap_or("missing")
                        ))
                        .retryable(Retryable::Never)
                        .attempts(response.meta.attempts)
                        .build(),
                ));
            }
        };
        let mutation = is_mutation(op);
        let check = if mutation && !has_replay_protection(op, prepared.key.as_deref()) {
            self.outcome_check(op, &prepared.args)
        } else {
            None
        };
        Ok(Open {
            body: start.body,
            parser: SseParser::with_max_event_bytes(self.inner.max_event_bytes),
            decoder: Utf8Decoder::new(),
            queue: VecDeque::new(),
            ended: false,
            oversize: false,
            meta: start.meta,
            count: 0,
            key: prepared.key.clone(),
            key_header: prepared.key_header.clone(),
            check,
            secrets: prepared.secrets.clone(),
            timeout: self.timeout(&opts),
        })
    }

    /// The final error of a stream that broke after `open.count` events.
    fn interrupted(&self, op: &OperationDescriptor, open: &Open, timed_out: bool) -> Error {
        let mutation = is_mutation(op);
        let after = format!(
            "after {} {}",
            open.count,
            if open.count == 1 { "event" } else { "events" }
        );
        let fields = || UnknownFields {
            http_status: Some(open.meta.status),
            request_id: open.meta.request_id.clone(),
            ..UnknownFields::default()
        };
        let ctx = CallContext {
            api: &self.inner.api,
            op,
            key: open.key.as_deref(),
            key_header: &open.key_header,
            attempts: open.meta.attempts,
            check: open.check.as_ref(),
        };
        let diagnostic = if timed_out {
            let cause = format!(
                "No event arrived within {} ms {after}; the stream was abandoned.",
                open.timeout.as_millis()
            );
            if mutation {
                outcome_unknown(&ctx, &cause, fields())
            } else {
                Diag::new(op.id.clone(), Category::UpstreamUnavailable)
                    .http_status(Some(open.meta.status))
                    .request_id(open.meta.request_id.clone())
                    .remediation(format!(
                        "{cause} This read has no side effects; call again later or with a larger `timeout`."
                    ))
                    .attempts(open.meta.attempts)
                    .build()
            }
        } else {
            let cause = format!(
                "The event stream was cut off {after}: the connection failed before it ended."
            );
            if mutation {
                outcome_unknown(&ctx, &cause, fields())
            } else {
                Diag::new(op.id.clone(), Category::TransportFailed)
                    .http_status(Some(open.meta.status))
                    .request_id(open.meta.request_id.clone())
                    .remediation(format!(
                        "{cause} This read has no side effects; call again."
                    ))
                    .attempts(open.meta.attempts)
                    .build()
            }
        };
        fail(scrub_diagnostic(diagnostic, &open.secrets))
    }

    /// The error that ends a collection which passed a limit, after `kept`
    /// events.
    fn over_budget(&self, op: &OperationDescriptor, over: Over, kept: usize) -> Error {
        let what = match over {
            Over::Bytes => format!("at most {} bytes of events", self.inner.max_collect_bytes),
            Over::Time => format!(
                "at most {} ms to collect the events",
                self.inner.max_collect_time.as_millis()
            ),
        };
        let after = if is_mutation(op) {
            " The call took effect; do not repeat it."
        } else {
            ""
        };
        fail(
            Diag::new(op.id.clone(), Category::UnexpectedResponse)
                .failed_parameter("events")
                .expected(what.clone())
                .remediation(format!(
                    "The stream was abandoned after {kept} {} ({what}; ClientOptions::max_collect_bytes and max_collect_time).{after} The events collected so far precede this error.",
                    if kept == 1 { "event" } else { "events" }
                ))
                .retryable(Retryable::Never)
                .build(),
        )
    }

    /// The final error of a stream whose next event is over the size limit.
    fn oversize(&self, op: &OperationDescriptor, open: &Open) -> Error {
        let limit = self.inner.max_event_bytes;
        let after = if is_mutation(op) {
            " The call took effect; do not repeat it."
        } else {
            ""
        };
        fail(scrub_diagnostic(
            Diag::new(op.id.clone(), Category::UnexpectedResponse)
                .http_status(Some(open.meta.status))
                .request_id(open.meta.request_id.clone())
                .failed_parameter(format!("events[{}]", open.count))
                .expected(format!("an event of at most {limit} bytes"))
                .remediation(format!(
                    "Event {} of the stream is larger than the limit of {limit} bytes (ClientOptions::max_event_bytes); the stream was abandoned.{after} Events before it were delivered. Raise max_event_bytes if the server sends events this large on purpose.",
                    open.count
                ))
                .retryable(Retryable::Never)
                .attempts(open.meta.attempts)
                .build(),
            &open.secrets,
        ))
    }

    /// Decode, check and number one server-sent event.
    fn accept(
        &self,
        op: &OperationDescriptor,
        spec: &StreamDescriptor,
        open: &mut Open,
        event: SseEvent,
    ) -> Accepted {
        if spec.done.as_deref() == Some(event.data.as_str()) {
            return Accepted::Done;
        }
        let index = open.count;
        let mutation = is_mutation(op);
        let after_effect = if mutation {
            " The call took effect; do not repeat it."
        } else {
            ""
        };
        let failure = |open: &Open, diag: Diag| -> Error {
            fail(scrub_diagnostic(
                diag.http_status(Some(open.meta.status))
                    .request_id(open.meta.request_id.clone())
                    .retryable(Retryable::Never)
                    .attempts(open.meta.attempts)
                    .build(),
                &open.secrets,
            ))
        };
        let value: Value = match serde_json::from_str(&event.data) {
            Ok(mut value) => {
                crate::util::integral_numbers(&mut value);
                value
            }
            Err(_) => {
                return Accepted::Failed(failure(
                    open,
                    Diag::new(op.id.clone(), Category::UnexpectedResponse)
                        .failed_parameter(format!("events[{index}]"))
                        .received_value(envelope_value(&Value::String(event.data.clone()), false))
                        .expected("JSON in the data of every event")
                        .remediation(format!(
                            "Event {index} of the stream (event \"{}\") does not carry JSON in its data.{after_effect} Events before it were delivered.",
                            event.event
                        )),
                ));
            }
        };
        let mode = self.inner.validate_responses;
        if mode != ValidateResponses::Off
            && let Some(validator) = &spec.event
            && let Validation::Invalid(issues) = crate::validate::judge(validator.as_ref(), &value)
        {
            let issue = issues.first();
            let path: Vec<String> = issue
                .map(|i| {
                    i.path
                        .iter()
                        .map(|s| match s {
                            PathSegment::Key(k) => k.clone(),
                            PathSegment::Index(n) => n.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let message = issue.map_or("a valid event", |i| i.message.as_str());
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
            let redacted = redact_paths(&value, &op.agent.sensitive_response_fields);
            let received = get_path(Some(&redacted), &path)
                .cloned()
                .unwrap_or(Value::Null);
            let error = failure(
                open,
                Diag::new(op.id.clone(), Category::UnexpectedResponse)
                    .failed_parameter(format!("events[{index}]{path_text}"))
                    .received_value(envelope_value(
                        &received,
                        path.iter().any(|s| looks_sensitive(s)),
                    ))
                    .expected(message)
                    .remediation(format!(
                        "Event {index} of the stream (event \"{}\") does not match the API description at events[{index}]{path_text} ({message}).{after_effect}",
                        event.event
                    )),
            );
            if mode == ValidateResponses::Strict {
                return Accepted::Failed(error);
            }
            self.emit(&error.diagnostic);
        }
        open.count += 1;
        Accepted::Event(StreamEvent {
            value,
            event: event.event,
            id: event.id,
            retry: event.retry,
            meta: open.meta.clone(),
        })
    }
}
