// SPDX-License-Identifier: Apache-2.0
//! The blocking facade of generated clients.
//!
//! A [`Runtime`] owns a current-thread tokio runtime that lives on a thread
//! of its own (the way `reqwest::blocking` does it), so connections stay
//! serviced between calls and a blocking call never needs the caller's
//! thread to be outside of an async context: waiting for the answer is a
//! plain channel receive, which cannot panic inside an existing tokio
//! runtime. It does block the calling thread, so async code should use the
//! async client, or move blocking calls to `spawn_blocking`.

use std::future::Future;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::de::DeserializeOwned;

use crate::client::{ConfigError, TypedPages};
use crate::envelope::Diag;
use crate::idempotency::lock;
use crate::types::{Category, Error, Page, Result, Retryable};

struct Shared {
    handle: tokio::runtime::Handle,
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(stop) = lock(&self.stop).take() {
            let _ = stop.send(());
        }
        if let Some(thread) = lock(&self.thread).take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// A current-thread tokio runtime on its own thread, shared by the clones of
/// the blocking client that owns it. Dropping the last clone stops the
/// thread.
#[derive(Clone)]
pub struct Runtime {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").finish_non_exhaustive()
    }
}

impl Runtime {
    /// Start the runtime thread.
    pub fn new() -> std::result::Result<Self, ConfigError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| ConfigError {
                message: format!("the blocking runtime could not be built: {e}"),
            })?;
        let handle = runtime.handle().clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("tungsten-blocking".to_owned())
            .spawn(move || {
                runtime.block_on(async {
                    let _ = stopped.await;
                });
            })
            .map_err(|e| ConfigError {
                message: format!("the blocking runtime thread could not be started: {e}"),
            })?;
        Ok(Runtime {
            shared: Arc::new(Shared {
                handle,
                stop: Mutex::new(Some(stop)),
                thread: Mutex::new(Some(thread)),
            }),
        })
    }

    /// Run `future` on the runtime thread and wait for its result; `None`
    /// when the task ended without one (it panicked, or the runtime stopped).
    fn exec<R, F>(&self, future: F) -> Option<R>
    where
        F: Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        drop(self.shared.handle.spawn(async move {
            let _ = tx.send(future.await);
        }));
        rx.recv().ok()
    }

    /// Run a call of `operation` to completion and return its result. If the
    /// runtime thread stops before the call finishes, the result is an
    /// `OUTCOME_UNKNOWN` envelope (the request may have been sent).
    pub fn run<T, F>(&self, operation: &str, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        self.exec(future).unwrap_or_else(|| Err(stopped(operation)))
    }

    /// Iterate `pages` with blocking `next` calls.
    pub fn pages<T>(&self, operation: &str, pages: TypedPages<T>) -> Pages<T> {
        Pages {
            runtime: self.clone(),
            operation: operation.to_owned(),
            inner: Some(pages),
        }
    }
}

fn stopped(operation: &str) -> Error {
    Error::new(
        Diag::new(operation, Category::OutcomeUnknown)
            .retryable(Retryable::SameKeyOnly)
            .remediation(
                "The thread that runs the blocking client stopped before the call finished, so it may or may not have taken effect. Check whether it did before calling again, and create a new blocking client.",
            )
            .build(),
    )
}

/// A blocking page iterator: each item is a page or the error that ended the
/// iteration.
pub struct Pages<T> {
    runtime: Runtime,
    operation: String,
    inner: Option<TypedPages<T>>,
}

impl<T> std::fmt::Debug for Pages<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pages")
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

impl<T: DeserializeOwned + Send + 'static> Iterator for Pages<T> {
    type Item = Result<Page<T>>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut pages = self.inner.take()?;
        let step = self.runtime.exec(async move {
            let item = pages.next().await;
            (pages, item)
        });
        match step {
            Some((pages, item)) => {
                if item.as_ref().is_some_and(|i| i.is_ok()) {
                    self.inner = Some(pages);
                }
                item
            }
            None => Some(Err(stopped(&self.operation))),
        }
    }
}
