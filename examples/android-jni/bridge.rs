// SPDX-License-Identifier: AGPL-3.0-only
//! A minimal JNI bridge over a generated Rust SDK. The runner builds this
//! file as a `cdylib` next to the generated SDK; on Android the same library
//! is cross-compiled per ABI and loaded with `System.loadLibrary`.
//!
//! One native handle owns one Tokio runtime and one client. Every call
//! blocks the calling Java thread (never the Android main thread) until the
//! response, the client's total deadline or `cancel`. `destroy` frees the
//! handle; the Java wrapper guarantees no call is still using it.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};
use petstore_sdk::resources::pets::PetsListRequest;
use petstore_sdk::{CallOptions, ClientOptions, PetstoreClient};
use tokio::sync::watch;

const FAILURE: &str = "example/petstore/PetstoreException";

struct Handle {
    runtime: tokio::runtime::Runtime,
    client: PetstoreClient,
    /// Set once by `cancel`: calls in flight and later calls end at once.
    cancelled: watch::Sender<bool>,
}

/// Runs `body`, turning a panic into a Java exception instead of unwinding
/// across the JNI boundary (which is undefined behaviour).
fn guarded<T>(env: &mut JNIEnv, fallback: T, body: impl FnOnce(&mut JNIEnv) -> T) -> T {
    match catch_unwind(AssertUnwindSafe(|| body(env))) {
        Ok(value) => value,
        Err(_) => {
            let _ = env.throw_new("java/lang/IllegalStateException", "the native bridge panicked");
            fallback
        }
    }
}

/// The handle behind a value from `create`.
///
/// # Safety
/// `handle` came from `create` and `destroy` has not run for it; the Java
/// wrapper's lock keeps `destroy` out while any other call runs.
unsafe fn handle<'a>(handle: jlong) -> &'a Handle {
    unsafe { &*(handle as *const Handle) }
}

/// Creates a client for `base_url`. `deadline_ms` bounds each call as a whole
/// (authentication, retries, the body); `max_response_bytes` bounds one
/// response. Returns 0 after throwing when the options are refused.
#[unsafe(no_mangle)]
pub extern "system" fn Java_example_petstore_PetstoreNative_create(
    mut env: JNIEnv,
    _class: JClass,
    base_url: JString,
    deadline_ms: jlong,
    max_response_bytes: jlong,
) -> jlong {
    guarded(&mut env, 0, |env| {
        let base_url: String = match env.get_string(&base_url) {
            Ok(text) => text.into(),
            Err(_) => return 0,
        };
        let options = ClientOptions {
            base_url: Some(base_url),
            deadline: Some(Duration::from_millis(deadline_ms.max(1) as u64)),
            max_response_bytes: Some(max_response_bytes.max(1) as u64),
            ..ClientOptions::default()
        };
        let client = match PetstoreClient::new(options) {
            Ok(client) => client,
            Err(error) => {
                let _ = env.throw_new("java/lang/IllegalArgumentException", error.to_string());
                return 0;
            }
        };
        // One small runtime per handle: its worker threads end in `destroy`.
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("petstore-sdk")
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = env.throw_new("java/lang/IllegalStateException", error.to_string());
                return 0;
            }
        };
        let (cancelled, _) = watch::channel(false);
        Box::into_raw(Box::new(Handle {
            runtime,
            client,
            cancelled,
        })) as jlong
    })
}

/// Lists the pets as a JSON array. A failure throws `PetstoreException`
/// holding the runtime's diagnostic envelope as JSON.
#[unsafe(no_mangle)]
pub extern "system" fn Java_example_petstore_PetstoreNative_listPets(
    mut env: JNIEnv,
    _class: JClass,
    raw: jlong,
) -> jstring {
    guarded(&mut env, std::ptr::null_mut(), |env| {
        // SAFETY: the Java wrapper passes a live handle under its read lock.
        let handle = unsafe { handle(raw) };
        let mut cancelled = handle.cancelled.subscribe();
        let outcome = handle.runtime.block_on(async {
            if *cancelled.borrow_and_update() {
                return None;
            }
            let pets = handle.client.pets();
            let opts = CallOptions::default();
            let call = pets.list(PetsListRequest::new(), &opts);
            tokio::select! {
                result = call => Some(result),
                // Dropping the call abandons the request; a read is safe to
                // repeat, and a mutation would report OUTCOME_UNKNOWN.
                _ = cancelled.changed() => None,
            }
        });
        let failure = match outcome {
            Some(Ok(response)) => match serde_json::to_string(&response.value) {
                Ok(json) => {
                    return env
                        .new_string(json)
                        .map(|s| s.into_raw())
                        .unwrap_or(std::ptr::null_mut());
                }
                Err(error) => error.to_string(),
            },
            Some(Err(error)) => serde_json::to_string(&error.diagnostic)
                .unwrap_or_else(|_| format!("{:?}", error.diagnostic.category)),
            None => "{\"category\":\"CANCELLED\"}".to_string(),
        };
        let _ = env.throw_new(FAILURE, failure);
        std::ptr::null_mut()
    })
}

/// Ends every call in flight on this handle and every later one.
#[unsafe(no_mangle)]
pub extern "system" fn Java_example_petstore_PetstoreNative_cancel(
    mut env: JNIEnv,
    _class: JClass,
    raw: jlong,
) {
    guarded(&mut env, (), |_| {
        // SAFETY: as in `listPets`.
        let handle = unsafe { handle(raw) };
        handle.cancelled.send_replace(true);
    })
}

/// Frees the handle and stops its runtime's threads.
#[unsafe(no_mangle)]
pub extern "system" fn Java_example_petstore_PetstoreNative_destroy(
    mut env: JNIEnv,
    _class: JClass,
    raw: jlong,
) {
    guarded(&mut env, (), |_| {
        if raw == 0 {
            return;
        }
        // SAFETY: the Java wrapper calls this once, under its write lock,
        // after which it never passes `raw` again.
        let handle = unsafe { Box::from_raw(raw as *mut Handle) };
        let Handle { runtime, client, .. } = *handle;
        drop(client);
        runtime.shutdown_timeout(Duration::from_secs(1));
    })
}
