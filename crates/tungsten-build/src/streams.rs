// SPDX-License-Identifier: AGPL-3.0-only
//! Event streams (planning/03 `StreamSpec`): a success response with a
//! `text/event-stream` media type makes the operation streamable.
//!
//! - The stream is carried by the lowest success status (exact statuses
//!   ascending) that declares the media type; another success status that
//!   declares it too is ignored (TG0531).
//! - The event type is the media type's schema, usually a union tagged by
//!   `type`. A media type without a schema gives events of any JSON type
//!   (TG0530).
//! - `x-tungsten-stream-done` on the media type object, else on the
//!   operation, names a `data` value that ends the stream (`[DONE]`).
//! - A boolean `stream` member of the JSON request body is the flag that
//!   selects the stream.

use serde_json::Value;
use tungsten_core::Diagnostic;
use tungsten_ir::{
    BodyEncoding, Operation, Primitive, Response, ResponseKind, Shape, StatusMatch, StreamSpec,
    Streaming, TypeRef,
};
use tungsten_openapi::RefTarget;

use crate::ctx::{Ctx, child};

/// The extension that declares the end-of-stream sentinel.
pub(crate) const DONE_EXTENSION: &str = "x-tungsten-stream-done";
/// The request body member that selects the stream.
const FLAG_FIELD: &str = "stream";

/// Whether a media type is `text/event-stream` (parameters and case ignored).
pub(crate) fn is_event_stream(media_type: &str) -> bool {
    media_type
        .split(';')
        .next()
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// Set `streaming` and `stream` of an operation whose success responses
/// carry an event stream.
pub(crate) fn apply(cx: &mut Ctx<'_>, op: &mut Operation, at: &RefTarget) {
    let carriers: Vec<&Response> = op
        .responses
        .iter()
        .filter(|r| {
            r.kind == ResponseKind::Success
                && matches!(r.status, StatusMatch::Exact(_))
                && r.content.iter().any(|c| is_event_stream(&c.media_type))
        })
        .collect();
    let Some(response) = carriers.first() else {
        return;
    };
    if carriers.len() > 1 {
        cx.report(
            Diagnostic::warning(
                "TG0531",
                format!(
                    "`{}` declares an event stream for more than one success status; only the first is streamed",
                    op.id.0
                ),
            ),
            at,
        );
    }
    let Some(content) = response
        .content
        .iter()
        .find(|c| is_event_stream(&c.media_type))
    else {
        return;
    };
    let StatusMatch::Exact(code) = response.status else {
        return;
    };
    let response_at = child(&child(at, "responses"), &code.to_string());
    let media_at = cx.deref(&response_at).map_or(response_at, |t| {
        child(&child(&t, "content"), &content.media_type)
    });
    let media = cx.get(&media_at);
    let typed = media.is_some_and(|m| m.get("schema").is_some());
    if !typed {
        cx.report(
            Diagnostic::warning(
                "TG0530",
                format!(
                    "the event stream of `{}` declares no schema; events are decoded as any JSON value",
                    op.id.0
                ),
            )
            .with_help("declare the schema of one event under the text/event-stream media type"),
            &media_at,
        );
    }
    let done = media
        .and_then(|m| m.get(DONE_EXTENSION))
        .or_else(|| op.extensions.get(DONE_EXTENSION))
        .and_then(Value::as_str)
        .map(str::to_string);
    let event = if typed {
        content.ty.clone()
    } else {
        TypeRef::Inline(Box::new(Shape::Any))
    };
    let also_plain = response
        .content
        .iter()
        .any(|c| !is_event_stream(&c.media_type));
    let request_flag = flag_of(cx, op);
    op.streaming = Some(Streaming::Sse);
    op.stream = Some(StreamSpec {
        status: response.status,
        media_type: content.media_type.clone(),
        event,
        done,
        request_flag,
        also_plain,
    });
}

/// `stream` when the JSON request body is a record with a boolean member of
/// that name.
fn flag_of(cx: &Ctx<'_>, op: &Operation) -> Option<String> {
    let body = op.body.as_ref()?;
    let json = body
        .content
        .iter()
        .find(|c| c.encoding == BodyEncoding::Json)?;
    let fields = cx.tb.record_fields(&json.ty)?;
    let field = fields.iter().find(|f| f.wire_name == FLAG_FIELD)?;
    let boolean = matches!(
        cx.tb.shape_of(&field.ty),
        Some(Shape::Primitive {
            primitive: Primitive::Bool,
            ..
        })
    );
    boolean.then(|| FLAG_FIELD.to_string())
}
