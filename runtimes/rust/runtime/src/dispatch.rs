// SPDX-License-Identifier: Apache-2.0
//! The dynamic entry point of a generated client and the helpers typed
//! wrappers use.

use std::future::Future;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::ClientCore;
use crate::envelope::Diag;
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
struct Mismatch {
    path: String,
    segments: Vec<String>,
    message: String,
}

fn decode_value<T: DeserializeOwned>(value: &Value) -> std::result::Result<T, Mismatch> {
    serde_path_to_error::deserialize::<_, T>(value).map_err(|error| {
        let (path, segments) = path_parts(error.path());
        Mismatch {
            path,
            segments,
            message: error.inner().to_string(),
        }
    })
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
