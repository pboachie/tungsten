// SPDX-License-Identifier: Apache-2.0
//! The dynamic entry point of a generated client and the helpers typed
//! wrappers use.

use std::future::Future;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::ClientCore;
use crate::types::{
    CallOptions, MacroDescriptor, OperationDescriptor, Outcome, Page, PreviewResult, Result,
};

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

/// Decode a dynamic outcome into a typed one. A missing body decodes as
/// JSON `null` (so `()` and `Option<T>` work). A body that does not decode
/// is `UNEXPECTED_RESPONSE`, with the body as `partial`.
pub fn decode<T: DeserializeOwned>(operation: &str, outcome: Outcome) -> Result<T> {
    let _ = (operation, outcome);
    unimplemented!("PHASE-5 stub")
}

/// Decode the items of a page the same way.
pub fn decode_page<T: DeserializeOwned>(
    operation: &str,
    page: Result<Page<Value>>,
) -> Result<Page<T>> {
    let _ = (operation, page);
    unimplemented!("PHASE-5 stub")
}
