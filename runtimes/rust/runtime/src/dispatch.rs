// SPDX-License-Identifier: Apache-2.0
//! The dynamic entry point of a generated client and the helpers typed
//! wrappers use.

use std::future::Future;
use std::ops::ControlFlow;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::ClientCore;
use crate::envelope::Diag;
use crate::stream::{StreamEvent, StreamResult};
use crate::types::{
    CallOptions, Category, Error, MacroDescriptor, OperationDescriptor, Outcome, Page,
    PreviewResult, Response, Result, Retryable,
};
use crate::util::{envelope_value, get_path, looks_sensitive};

/// Implemented by every generated client (`<Api>Client`). It lets drivers
/// that only know operation ids and JSON (the contract suite, the generated
/// CLI) run the same typed methods as application code: `invoke` decodes
/// the arguments into the operation's request struct, calls the typed
/// method and encodes its response back to JSON. When the arguments do not
/// decode, `invoke` calls [`ClientCore::call`] with them, so the
/// `VALIDATION_FAILED` envelope is the pre-flight one.
///
/// Arguments are keyed by *argument name* (the `name` of the operation's
/// [`crate::ParamDescriptor`]s and merged body fields' `arg`).
pub trait Dispatch: Send + Sync {
    fn core(&self) -> &ClientCore;

    /// Every callable operation, in IR order.
    fn operations(&self) -> &[OperationDescriptor];

    /// Every macro, in IR order.
    fn macros(&self) -> &[MacroDescriptor];

    fn invoke(
        &self,
        operation: &str,
        args: Value,
        opts: CallOptions,
    ) -> impl Future<Output = Outcome> + Send;

    fn preview(
        &self,
        operation: &str,
        args: Value,
        opts: CallOptions,
    ) -> impl Future<Output = Result<PreviewResult>> + Send;

    /// Iterate every page of a paginated operation; each page's items are
    /// the decoded JSON items.
    fn pages(
        &self,
        operation: &str,
        args: Value,
        opts: CallOptions,
    ) -> impl Future<Output = Vec<Result<Page<Value>>>> + Send;

    /// Like [`Dispatch::pages`], but hands each page to `sink` as soon as it
    /// arrives and stops fetching when `sink` returns
    /// [`ControlFlow::Break`], so a caller can print or drop a page before
    /// the next request and never holds more than one page. The default
    /// collects through [`Dispatch::pages`] first; a generated client
    /// overrides it to iterate lazily.
    fn pages_each<'a>(
        &'a self,
        operation: &'a str,
        args: Value,
        opts: CallOptions,
        sink: &'a mut (dyn FnMut(Result<Page<Value>>) -> ControlFlow<()> + Send),
    ) -> impl Future<Output = ()> + Send + 'a {
        async move {
            for page in self.pages(operation, args, opts).await {
                if sink(page).is_break() {
                    break;
                }
            }
        }
    }

    /// Collect the events of an operation's event stream, each as its decoded
    /// JSON value, through the typed `<method>_stream`. The final error, if
    /// any, is the last item. A client whose API has no event stream keeps
    /// this default, which answers `VALIDATION_FAILED`.
    fn stream(
        &self,
        operation: &str,
        _args: Value,
        _opts: CallOptions,
    ) -> impl Future<Output = Vec<StreamResult<Value>>> + Send {
        let operation = operation.to_owned();
        async move { vec![Err(no_stream(&operation))] }
    }

    fn run_macro(
        &self,
        name: &str,
        input: Value,
        opts: CallOptions,
    ) -> impl Future<Output = Outcome> + Send;

    fn preview_macro(
        &self,
        name: &str,
        input: Value,
        opts: CallOptions,
    ) -> impl Future<Output = Result<PreviewResult>> + Send;
}

/// `VALIDATION_FAILED` for an operation without an event stream.
pub fn no_stream(operation: &str) -> Error {
    Error::new(
        Diag::new(operation, Category::ValidationFailed)
            .failed_parameter("operation")
            .expected("an operation with an event stream")
            .remediation(format!(
                "{operation} has no event stream (its success response is not text/event-stream); use invoke instead. Nothing was sent."
            ))
            .build(),
    )
}

/// The spelling of a `serde_path_to_error` path as `.key` and `[index]`
/// segments, with the segments themselves.
fn path_parts(path: &serde_path_to_error::Path) -> (String, Vec<String>) {
    use serde_path_to_error::Segment;
    let mut text = String::new();
    let mut segments = Vec::new();
    for segment in path.iter() {
        match segment {
            Segment::Seq { index } => {
                text.push_str(&format!("[{index}]"));
                segments.push(index.to_string());
            }
            Segment::Map { key } => {
                text.push_str(&format!(".{key}"));
                segments.push(key.clone());
            }
            Segment::Enum { variant } => {
                text.push_str(&format!(".{variant}"));
                segments.push(variant.clone());
            }
            Segment::Unknown => {}
        }
    }
    (text, segments)
}

/// Why a value did not decode: the failing path as text and as segments,
/// and the message.
#[derive(Debug)]
pub struct Mismatch {
    path: String,
    segments: Vec<String>,
    message: String,
}

fn mismatch_of<E: std::fmt::Display>(error: serde_path_to_error::Error<E>) -> Mismatch {
    let (path, segments) = path_parts(error.path());
    Mismatch {
        path,
        segments,
        message: error.inner().to_string(),
    }
}

fn decode_value<T: DeserializeOwned>(value: &Value) -> std::result::Result<T, Mismatch> {
    serde_path_to_error::deserialize::<_, T>(value).map_err(mismatch_of)
}

/// Decode a JSON body straight from its bytes into `T`: the value its JSON
/// form (as [`crate::internals::decode_body`] builds it, integral floats as
/// integers) decodes to, without building that form. `None` when the body
/// does not decode or is not one JSON text; that is reported from the JSON
/// form, with the failing path, so no path is tracked here (tracking costs
/// most of a decode).
fn decode_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let mut json = serde_json::Deserializer::from_slice(bytes);
    let value = T::deserialize(crate::integral::Integral(&mut json)).ok()?;
    json.end().ok()?;
    Some(value)
}

/// Where a typed call takes a success body straight from its bytes, before
/// the runtime builds the body's JSON form.
pub(crate) trait BodySink: Send {
    /// Take the JSON body `bytes`, judging it with the operation's response
    /// validator when `judge`; true when taken, so the runtime builds no JSON
    /// form and the call's value is the sink's. False leaves the body to the
    /// JSON path: a body that does not decode, or does not pass validation,
    /// is reported from its JSON form, exactly as a dynamic call reports it.
    fn take(&mut self, bytes: &[u8], judge: bool) -> bool;
}

/// The sink of [`crate::ClientCore::call_typed`]: the body as `T`, judged by
/// the operation's response validator.
pub(crate) struct TypedSink<T> {
    validator: Option<std::sync::Arc<dyn crate::types::Validator>>,
    value: Option<T>,
}

impl<T> TypedSink<T> {
    pub(crate) fn new(validator: Option<std::sync::Arc<dyn crate::types::Validator>>) -> Self {
        TypedSink {
            validator,
            value: None,
        }
    }

    pub(crate) fn take_value(&mut self) -> Option<T> {
        self.value.take()
    }
}

impl<T: DeserializeOwned + Send + 'static> BodySink for TypedSink<T> {
    fn take(&mut self, bytes: &[u8], judge: bool) -> bool {
        let validator = self.validator.as_deref().filter(|_| judge);
        // A validator that panics is reported by the JSON path.
        let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let value: T = decode_bytes(bytes)?;
            match validator {
                None => Some(value),
                Some(validator) => match validator.check_decoded(&value) {
                    Some(issues) if issues.is_empty() => Some(value),
                    _ => None,
                },
            }
        }))
        .ok()
        .flatten();
        match decoded {
            Some(value) => {
                self.value = Some(value);
                true
            }
            None => false,
        }
    }
}

fn mismatch_error(
    operation: &str,
    prefix: &str,
    mismatch: &Mismatch,
    root: &Value,
    meta: &crate::types::ResponseMeta,
) -> Error {
    let shown = if mismatch.segments.is_empty() && prefix.is_empty() {
        envelope_value(root, false)
    } else {
        let found = get_path(Some(root), &mismatch.segments)
            .cloned()
            .unwrap_or(Value::Null);
        envelope_value(&found, mismatch.segments.iter().any(|s| looks_sensitive(s)))
    };
    let location = format!("response{prefix}{}", mismatch.path);
    Error::with_partial(
        Diag::new(operation, Category::UnexpectedResponse)
            .http_status((meta.status > 0).then_some(meta.status))
            .request_id(meta.request_id.clone())
            .failed_parameter(location.clone())
            .received_value(shown)
            .expected(mismatch.message.clone())
            .retryable(Retryable::Never)
            .remediation(format!(
                "The response of {operation} could not be decoded into the SDK's type at {location} ({}). If the call changed state it already took effect; do not repeat it.",
                mismatch.message
            ))
            .attempts(meta.attempts)
            .build(),
        root.clone(),
    )
}

/// Decode a dynamic outcome into a typed one. A missing body decodes as
/// JSON `null` (so `()` and `Option<T>` work). A body that does not decode
/// is `UNEXPECTED_RESPONSE`, with the body as `partial`.
pub fn decode<T: DeserializeOwned>(operation: &str, outcome: Outcome) -> Result<T> {
    let response = outcome?;
    let body = response.value.unwrap_or(Value::Null);
    match decode_value::<T>(&body) {
        Ok(value) => Ok(Response {
            value,
            meta: response.meta,
            verification: response.verification,
        }),
        Err(mismatch) => Err(mismatch_error(
            operation,
            "",
            &mismatch,
            &body,
            &response.meta,
        )),
    }
}

/// The result of picking the body type by status: `None` when the status
/// has no declared body type.
pub type StatusDecode<T> = Option<std::result::Result<T, Mismatch>>;

/// A typed result whose body type depends on the response status (an
/// operation whose success statuses answer with different bodies).
/// Generated code implements it for the operation's response enum.
pub trait ByStatus: Sized {
    /// Decode `body` as the variant declared for `status`; `None` when no
    /// variant is declared for it.
    fn from_status(status: u16, body: &Value) -> StatusDecode<Self>;
}

/// Decode `body` as `V` and wrap it; the building block of
/// [`ByStatus::from_status`].
pub fn decode_variant<V: DeserializeOwned, T>(
    body: &Value,
    wrap: fn(V) -> T,
) -> std::result::Result<T, Mismatch> {
    decode_value::<V>(body).map(wrap)
}

/// Like [`decode`], choosing the body type by the response status. A status
/// without a declared body type is `UNEXPECTED_RESPONSE`.
pub fn decode_by_status<T: ByStatus>(operation: &str, outcome: Outcome) -> Result<T> {
    let response = outcome?;
    let body = response.value.unwrap_or(Value::Null);
    let decoded = T::from_status(response.meta.status, &body).unwrap_or_else(|| {
        Err(Mismatch {
            path: String::new(),
            segments: vec![],
            message: format!("no body is declared for status {}", response.meta.status),
        })
    });
    match decoded {
        Ok(value) => Ok(Response {
            value,
            meta: response.meta,
            verification: response.verification,
        }),
        Err(mismatch) => Err(mismatch_error(
            operation,
            "",
            &mismatch,
            &body,
            &response.meta,
        )),
    }
}

/// Decode the value of a stream event the same way; `index` is the event's
/// position in the stream.
pub fn decode_event<T: DeserializeOwned>(
    operation: &str,
    index: usize,
    item: StreamResult<Value>,
) -> StreamResult<T> {
    let StreamEvent {
        value,
        event,
        id,
        retry,
        meta,
        reconnects,
    } = item?;
    match decode_value::<T>(&value) {
        Ok(decoded) => Ok(StreamEvent {
            value: decoded,
            event,
            id,
            retry,
            meta,
            reconnects,
        }),
        Err(mismatch) => {
            let location = format!("events[{index}]{}", mismatch.path);
            let found = get_path(Some(&value), &mismatch.segments)
                .cloned()
                .unwrap_or(Value::Null);
            Err(Error::new(
                Diag::new(operation, Category::UnexpectedResponse)
                    .http_status((meta.status > 0).then_some(meta.status))
                    .request_id(meta.request_id.clone())
                    .failed_parameter(location.clone())
                    .received_value(envelope_value(
                        &found,
                        mismatch.segments.iter().any(|s| looks_sensitive(s)),
                    ))
                    .expected(mismatch.message.clone())
                    .retryable(Retryable::Never)
                    .remediation(format!(
                        "Event {index} of the stream of {operation} could not be decoded into the SDK's type at {location} ({}). If the call changed state it already took effect; do not repeat it.",
                        mismatch.message
                    ))
                    .attempts(meta.attempts)
                    .build(),
            ))
        }
    }
}

/// Decode the items of a page the same way.
pub fn decode_page<T: DeserializeOwned>(
    operation: &str,
    page: Result<Page<Value>>,
) -> Result<Page<T>> {
    let response = page?;
    let Page { items, body, next } = response.value;
    let mut typed = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        match decode_value::<T>(item) {
            Ok(value) => typed.push(value),
            Err(mismatch) => {
                return Err(mismatch_error(
                    operation,
                    &format!(".items[{index}]"),
                    &mismatch,
                    item,
                    &response.meta,
                ));
            }
        }
    }
    Ok(Response {
        value: Page {
            items: typed,
            body,
            next,
        },
        meta: response.meta,
        verification: response.verification,
    })
}
