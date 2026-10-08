// SPDX-License-Identifier: Apache-2.0
//! `ClientCore`: validate, authenticate, apply idempotency and confirmation
//! rules, send with retries, classify the response (PHASE-5 CONTRACT: the
//! signatures below are final; bodies are the runtime agent's).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::types::{
    ApiDescriptor, CallOptions, ClientOptions, MacroDescriptor, OperationDescriptor, Outcome, Page,
    Predicate, PreviewResult, Result,
};

/// The client could not be built (an unusable base URL, TLS roots, or a
/// credential that is not valid for its scheme's shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// The engine behind every generated client. Cheap to clone (shares one
/// connection pool, one confirmation key and one idempotency store).
/// Never panics and never returns an error value for API or transport
/// failures other than through [`Result`]: a failed call is
/// `Err(Error { diagnostic, partial })`.
#[derive(Debug, Clone)]
pub struct ClientCore {
    api: Arc<ApiDescriptor>,
}

impl ClientCore {
    pub fn new(
        api: ApiDescriptor,
        options: ClientOptions,
    ) -> std::result::Result<Self, ConfigError> {
        let _ = options;
        Ok(ClientCore { api: Arc::new(api) })
    }

    pub fn api(&self) -> &ApiDescriptor {
        &self.api
    }

    /// Make operations resolvable by id (verification, endpoint previews,
    /// macros). Operations passed to `call` are registered too.
    pub fn register(&self, ops: impl IntoIterator<Item = Arc<OperationDescriptor>>) {
        let _ = ops;
        unimplemented!("PHASE-5 stub")
    }

    pub fn operation(&self, id: &str) -> Option<Arc<OperationDescriptor>> {
        let _ = id;
        unimplemented!("PHASE-5 stub")
    }

    /// Validate, authenticate, apply idempotency and confirmation rules,
    /// send with retries, classify the response. `args` is the arguments
    /// object (a JSON object keyed by argument name).
    pub async fn call(&self, op: &OperationDescriptor, args: Value, opts: &CallOptions) -> Outcome {
        let _ = (op, args, opts);
        unimplemented!("PHASE-5 stub")
    }

    /// Run the operation's preview mode.
    pub async fn preview(
        &self,
        op: &OperationDescriptor,
        args: Value,
        opts: &CallOptions,
    ) -> Result<PreviewResult> {
        let _ = (op, args, opts);
        unimplemented!("PHASE-5 stub")
    }

    /// Iterate pages; the stream ends after the last page or the first error.
    pub fn pages(&self, op: Arc<OperationDescriptor>, args: Value, opts: CallOptions) -> Pages {
        Pages::new(self.clone(), op, args, opts)
    }

    /// Call a read operation until `until` holds or the budget runs out. On
    /// a timeout the last body is returned with `timed_out` true.
    pub async fn poll(
        &self,
        op: &OperationDescriptor,
        args: Value,
        until: &Predicate,
        interval: Duration,
        budget: Duration,
        opts: &CallOptions,
    ) -> Result<Polled> {
        let _ = (op, args, until, interval, budget, opts);
        unimplemented!("PHASE-5 stub")
    }

    /// Run a compiled macro; returns its output or the first failing step's
    /// envelope, with the completed steps' results as `partial`. A
    /// `destructive` or `irreversible` macro needs `opts.confirm`.
    pub async fn run_macro(
        &self,
        macro_: &MacroDescriptor,
        input: Value,
        opts: &CallOptions,
    ) -> Outcome {
        let _ = (macro_, input, opts);
        unimplemented!("PHASE-5 stub")
    }

    /// Preview a macro without sending anything (see `MacroStepPreview`).
    pub async fn preview_macro(
        &self,
        macro_: &MacroDescriptor,
        input: Value,
        opts: &CallOptions,
    ) -> Result<PreviewResult> {
        let _ = (macro_, input, opts);
        unimplemented!("PHASE-5 stub")
    }
}

/// The body a [`ClientCore::poll`] ended with.
#[derive(Debug, Clone, PartialEq)]
pub struct Polled {
    pub body: Option<Value>,
    pub timed_out: bool,
}

/// An asynchronous page iterator: call [`Pages::next`] until it returns
/// `None`. After an error item it returns `None`.
#[derive(Debug)]
pub struct Pages {
    core: ClientCore,
    op: Arc<OperationDescriptor>,
    args: Value,
    opts: CallOptions,
}

impl Pages {
    fn new(core: ClientCore, op: Arc<OperationDescriptor>, args: Value, opts: CallOptions) -> Self {
        Pages {
            core,
            op,
            args,
            opts,
        }
    }

    pub async fn next(&mut self) -> Option<Result<Page<Value>>> {
        let _ = (&self.core, &self.op, &self.args, &self.opts);
        unimplemented!("PHASE-5 stub")
    }

    /// Items typed as `T` (an item that does not decode is an
    /// `UNEXPECTED_RESPONSE` error item).
    pub fn typed<T: DeserializeOwned>(self) -> TypedPages<T> {
        TypedPages {
            inner: self,
            marker: std::marker::PhantomData,
        }
    }
}

/// [`Pages`] with typed items.
#[derive(Debug)]
pub struct TypedPages<T> {
    inner: Pages,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> TypedPages<T> {
    pub async fn next(&mut self) -> Option<Result<Page<T>>> {
        let page = self.inner.next().await?;
        Some(crate::dispatch::decode_page(&self.inner.op.id, page))
    }
}
