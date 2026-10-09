// SPDX-License-Identifier: Apache-2.0
//! Event streams (`text/event-stream`): the events of an operation as an
//! asynchronous iterator, with the semantics of `ClientCore.stream` in
//! `runtimes/ts/src/client.ts`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

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
use crate::util::{SecretSet, envelope_value, get_path, looks_sensitive, redact_paths};

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
    pub async fn next(&mut self) -> Option<StreamResult<Value>> {
        loop {
            match std::mem::replace(&mut self.state, State::Done) {
                State::Done => return None,
                State::Idle { args, opts } => {
                    match self
                        .core
                        .open_stream(&self.op, &self.spec, &args, &opts)
                        .await
                    {
                        Ok(open) => self.state = State::Open(Box::new(open)),
                        Err(error) => return Some(Err(error)),
                    }
                }
                State::Open(mut open) => {
                    if let Some(event) = open.queue.pop_front() {
                        match self.core.accept(&self.op, &self.spec, &mut open, event) {
                            Accepted::Event(item) => {
                                self.state = State::Open(open);
                                return Some(Ok(item));
                            }
                            Accepted::Done => return None,
                            Accepted::Failed(error) => return Some(Err(error)),
                        }
                    }
                    if open.ended {
                        return None;
                    }
                    match open.body.read().await {
                        BodyRead::Chunk(bytes) => {
                            let text = open.decoder.decode(&bytes);
                            let events = open.parser.push(&text);
                            open.queue.extend(events);
                            self.state = State::Open(open);
                        }
                        BodyRead::End => {
                            let text = open.decoder.finish();
                            let mut events = open.parser.push(&text);
                            events.extend(open.parser.end());
                            open.queue.extend(events);
                            open.ended = true;
                            self.state = State::Open(open);
                        }
                        BodyRead::Timeout => {
                            return Some(Err(self.core.interrupted(&self.op, &open, true)));
                        }
                        BodyRead::Lost => {
                            return Some(Err(self.core.interrupted(&self.op, &open, false)));
                        }
                    }
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
        headers.insert("Accept".to_owned(), "text/event-stream".to_owned());
        headers.extend(std::mem::take(&mut opts.headers));
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
            parser: SseParser::new(),
            decoder: Utf8Decoder::new(),
            queue: VecDeque::new(),
            ended: false,
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
