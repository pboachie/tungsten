// SPDX-License-Identifier: AGPL-3.0-only
//! IR-driven mock HTTP server (planning/05 "Mock server", planning/08 L4).
//!
//! Validates requests against the IR, answers with deterministic
//! schema-derived values, records every call, and injects failures on
//! demand. Out-of-process test drivers (TypeScript, Python) control it over
//! HTTP. This comment is the protocol those drivers rely on.
//!
//! # Request pipeline
//!
//! Every request outside `/__tungsten/` goes through these steps; the first
//! one that answers wins.
//!
//! 1. `X-Tungsten-Inject: reset` closes the connection before the body is
//!    read (the client sees a reset or an early close, never a response).
//! 2. The body is read. A body larger than [`MockOptions::max_body_bytes`]
//!    answers `413` by the error rule below, with `Connection: close`.
//! 3. Routing. The method and path are matched against every callable
//!    operation of every namespace: literal segments, `{param}` segments
//!    (one non-empty segment, percent-decoded) and mixed segments such as
//!    `{id}:archive`. Among the operations of the request's method, the one
//!    whose segments are most literal wins, comparing from the left. An rpc
//!    operation (`Operation.rpc`) also needs the JSON body's discriminator
//!    member to equal its discriminator value. Planned operations are never
//!    routed.
//! 4. Any other `X-Tungsten-Inject` value is applied here (see
//!    "Injections").
//! 5. A queued program for the matched operation answers, or applies its
//!    injection as step 4 would (see `/__tungsten/program`).
//! 6. No route answers `404`; a path served only under other methods answers
//!    `405` with `Allow`; an rpc body whose method no operation serves
//!    answers `400`.
//! 7. A gated operation (`OperationStatus::Gated`) whose gate is off answers
//!    its `disabled_status`. A gate is on when its `default_on` is set or its
//!    environment variable is listed in [`MockOptions::enabled_gates`]; the
//!    mock never reads the process environment.
//! 8. Auth (`Operation.security`, an OR of AND-sets). An `apiKey` needs its
//!    header, query parameter or cookie; `http` bearer, `oauth2` and
//!    `openIdConnect` need `Authorization: Bearer <token>`; `http` basic
//!    needs `Authorization: Basic <credentials>`. A spec scheme named in a
//!    composite profile's `satisfies` is checked through the profile's parts
//!    instead: cookie parts need the cookie, a header part with
//!    `equals_cookie` needs the header equal to that cookie's value, a
//!    header part with `mutation_only` is only needed on unsafe methods, a
//!    bearer part needs a bearer token. A bearer scheme or part with a
//!    required prefix (`HttpBearer.prefix`, `CompositePart::Bearer.prefix`,
//!    ZROtext's `ztw_`) rejects a token without it. When no alternative is
//!    satisfied the answer is `401` if no credential of any alternative was
//!    sent or a bearer token lacks its prefix (the API does not know such a
//!    token), and `403` if some were sent but an alternative is incomplete
//!    or wrong (wrong scheme, a CSRF header that differs from its cookie).
//! 9. Parameters: required path, query, header and cookie parameters must be
//!    present, and present ones must parse as their IR type (integers,
//!    numbers, booleans, enums, constants, arrays split by their style) and
//!    satisfy its format (`uuid`, `date-time`, `date`, `email`, ...),
//!    pattern, length and range. Parameters declared with a JSON `content`
//!    are parsed as JSON and validated in full. Failures answer `400`.
//! 10. Body: a required body must be present; a present body needs a
//!     `Content-Type` matching one of the operation's media types (`*/*` and
//!     `type/*` declarations match their family). JSON bodies are validated
//!     against the IR type: records (presence, closed or typed additional
//!     members; read-only members are never required in requests), enums,
//!     constants, unions by strategy (tagged by discriminator, untagged and
//!     literal by any variant), intersections, arrays (length, uniqueness),
//!     maps, nullables and primitives with their constraints. An rpc body
//!     must carry the operation's constant envelope members, and its
//!     parameters member is validated against the operation's body type.
//!     Other encodings (bytes, text, form, multipart) are checked by media
//!     type only. Failures answer `400`.
//! 11. Idempotency: for an operation with an `idempotency_key` header
//!     parameter, the first request with a key stores its response. A
//!     repeat with the same key, path, query and body replays the stored
//!     response, with a top-level boolean `created` of a JSON object body
//!     set to `false`. The same key with a different path, query or body
//!     answers `409`. Injected and programmed answers are never stored. At
//!     most [`MockOptions::max_idempotent_responses`] responses are kept;
//!     storing one more drops the oldest, whose key then counts as new.
//! 12. Success: the operation's lowest 2xx response (an exact status, else
//!     `2XX` or `default` as `200`). Its JSON body is generated from the IR
//!     type, deterministically from the seed, the operation id and the
//!     field path, so an operation answers the same bytes on every call and
//!     every run: version 4 uuids, 2026 date-times and other formats, the
//!     first enum value, constants, integers and numbers within their
//!     bounds and multiples (2026 Unix times for unbounded `*_ms` and
//!     `*_at` integers), strings matching simple patterns, otherwise URLs
//!     and emails for `*_url` and `*_email` names and text derived from the
//!     field name, fitted to their length limits; `true` for booleans,
//!     nullable values present, arrays of one item (or their minimum), maps
//!     of one entry, every record field except write-only ones (optional
//!     fields of deeply nested values are left out so recursive types end).
//!     Bytes bodies are empty with their media type; a response without
//!     content has no body. Declared response headers are sent with
//!     generated values. Responses carry no `Date` header.
//!
//! Failures produced by the mock carry a short explanation in the
//! `X-Tungsten-Reason` response header. A failure of a routed operation
//! whose status the operation declares without content (a bare `408`, say)
//! has no body, as the API answers it, unless an error code was asked for
//! (`code` of an injection or a program). Otherwise the body follows the
//! error rule: when the error model of the operation's namespace (`Namespace.errors`;
//! `Ir.errors` when no operation matched) has an envelope type and a code
//! field, the body is a generated envelope (`application/json`) whose code
//! field (a dotted path such as `error.code`) holds the best matching code
//! and whose message field, if any, holds the explanation; otherwise the
//! body is the explanation as `text/plain`. The best matching code is the
//! conventional one when the model lists it (`invalid_request` for 400,
//! `unauthorized` 401, `forbidden` 403, `not_found` 404, `conflict` 409,
//! with `idempotency_conflict` preferred for idempotency conflicts,
//! `payload_too_large` 413, `rate_limited` 429, `unavailable` 503), else the
//! first code listed with that status, else the envelope's generated value.
//!
//! # Injections
//!
//! Request header `X-Tungsten-Inject` (one value per request):
//!
//! - `timeout` or `timeout=<ms>`: process the request normally, record it,
//!   then hold the response for `<ms>` milliseconds (default 30000) before
//!   sending it.
//! - `drop-after-write`: process the request normally (an idempotency key
//!   stores its response), record it with `response_status` 0, then close
//!   the connection without a response.
//! - `reset`: close the connection before reading the body; recorded with an
//!   empty body and `response_status` 0.
//! - `status=<code>[;retry-after=<s>][;code=<error code>][;apply]`: answer
//!   `<code>` (200-599) without validating or applying the request, with a
//!   `Retry-After` header when given. A 2xx code answers the operation's
//!   generated body for that status; any other code answers the error rule
//!   with `<error code>` when given. With `apply`, the request is first
//!   processed as usual (validated, and stored under its idempotency key
//!   when it succeeds), so the injected status hides an applied effect: the
//!   ambiguous answer of a server that applied the request and then failed.
//!
//! An unknown value answers `400` (text/plain). Injections apply to routed
//! and unrouted requests alike and never consume a program. A program can
//! apply the same injections (except `reset`) to the next calls of one
//! operation, for clients that cannot set request headers (an MCP server
//! between the test and the mock).
//!
//! # Control endpoints
//!
//! - `GET /__tungsten/calls`: `200` with a JSON array of the recorded calls
//!   in arrival order (at most [`MockOptions::max_recorded_calls`], the
//!   most recent ones). Each element is a serialized [`RecordedCall`] (header
//!   pairs as two-element arrays, the body as an array of bytes) plus
//!   `body_text`, the body as a string when it is UTF-8, else `null`.
//! - `POST /__tungsten/reset`: `204`; clears the recorded calls, the queued
//!   programs and the stored idempotent responses.
//! - `POST /__tungsten/program`: queues scripted responses. The body is one
//!   object or an array of objects `{operation, status, body?, headers?,
//!   times?, code?}` or `{operation, inject, times?}`: `operation` is a
//!   callable operation id, `status` 200-599, `body` any JSON value (a
//!   string is sent as `text/plain`, anything else as `application/json`),
//!   `headers` an object of string values, `times` how many matching calls
//!   it applies to (default 1), `code` the error code used by the error rule
//!   when `body` is absent (a 2xx status without `body` answers the
//!   operation's generated body). `inject` is an `X-Tungsten-Inject` value
//!   other than `reset` (`drop-after-write`, `timeout=<ms>`,
//!   `status=<code>[;...][;apply]`), applied to the matching calls exactly
//!   as the header would be; it takes no `status`, `body`, `headers` or
//!   `code`. Programs for one operation apply in the order they were
//!   queued. Answers `200` with `{"queued": <number of programs>}`, or `400`
//!   (text/plain) naming the problem, in which case nothing is queued.
//!
//! Control requests are not recorded. Recorded calls hold every other
//! request, including rejected, injected and dropped ones, in arrival order,
//! up to [`MockOptions::max_recorded_calls`] (past it the oldest is dropped);
//! a call is recorded as soon as its answer is decided (before a `timeout`
//! hold). Header names are lowercased and the pairs sorted by name, then
//! value. `injected` holds the trimmed `X-Tungsten-Inject` value, or
//! `program` when a program answered or injected.

mod auth;
mod generate;
mod handler;
mod model;
mod params;
mod pattern;
mod reply;
mod route;
mod server;
mod state;
mod validate;

use std::net::SocketAddr;
use std::sync::Arc;

use tungsten_ir::Ir;

/// Default for [`MockOptions::max_body_bytes`]: 1 MiB.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Default for [`MockOptions::max_recorded_calls`].
pub const DEFAULT_MAX_RECORDED_CALLS: usize = 10_000;

/// Default for [`MockOptions::max_idempotent_responses`].
pub const DEFAULT_MAX_IDEMPOTENT_RESPONSES: usize = 10_000;

/// Options for [`MockServer::start`].
#[derive(Debug, Clone)]
pub struct MockOptions {
    /// Address to bind; port 0 picks a free port.
    pub addr: SocketAddr,
    /// Seed for schema-derived values (output is deterministic per seed).
    pub seed: u64,
    /// Environment variable names of runtime gates to treat as on. Gates
    /// not listed keep their IR default.
    pub enabled_gates: Vec<String>,
    /// Largest request body accepted; larger bodies answer `413`.
    pub max_body_bytes: usize,
    /// Recorded calls kept for `/__tungsten/calls`; past it the oldest call
    /// is dropped. 0 records nothing.
    pub max_recorded_calls: usize,
    /// Stored idempotent responses kept for replays; past it the oldest
    /// stored response is dropped, and a later request with its key is
    /// served as a fresh one. 0 stores nothing.
    pub max_idempotent_responses: usize,
}

impl Default for MockOptions {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            seed: 0,
            enabled_gates: vec![],
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_recorded_calls: DEFAULT_MAX_RECORDED_CALLS,
            max_idempotent_responses: DEFAULT_MAX_IDEMPOTENT_RESPONSES,
        }
    }
}

/// One request the mock received, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordedCall {
    pub method: String,
    /// Path without the query string.
    pub path: String,
    /// Raw query string without `?`.
    pub query: String,
    /// The matched IR operation id, if any.
    pub operation: Option<String>,
    /// Header names lowercased, sorted by name then value.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Status sent, or 0 when the connection was dropped.
    pub response_status: u16,
    /// The injection or program applied, if any.
    pub injected: Option<String>,
}

/// A running mock. Dropping it shuts it down.
#[derive(Debug)]
pub struct MockServer {
    addr: SocketAddr,
    state: Arc<state::State>,
    running: Option<server::Running>,
}

impl MockServer {
    /// Start serving `ir` on a background thread. Returns once the address
    /// is bound; binding errors are returned here.
    pub fn start(ir: Ir, opts: MockOptions) -> std::io::Result<MockServer> {
        let state = Arc::new(state::State::new(
            model::Model::new(ir, &opts),
            opts.max_recorded_calls,
            opts.max_idempotent_responses,
        ));
        let (addr, running) = server::spawn(opts.addr, state.clone())?;
        Ok(MockServer {
            addr,
            state,
            running: Some(running),
        })
    }

    /// The bound address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://<addr>`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Calls recorded so far.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.state.calls()
    }

    /// Clear recorded calls, programmed responses and stored idempotent
    /// responses.
    pub fn reset(&self) {
        self.state.reset();
    }

    /// Stop serving and wait for the server thread. The port is free when
    /// this returns.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Internals exposed to the private test harness.
#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod __testing {
    use serde_json::Value;
    use tungsten_ir::{Ir, TypeRef};

    use crate::MockOptions;
    use crate::generate::Generator;
    use crate::model::Model;
    use crate::validate::{Context, Validator};

    /// The mock's value generator and validator over one IR.
    #[derive(Debug)]
    pub struct Types {
        model: Model,
    }

    impl Types {
        pub fn new(ir: Ir, seed: u64) -> Types {
            Types {
                model: Model::new(
                    ir,
                    &MockOptions {
                        seed,
                        ..MockOptions::default()
                    },
                ),
            }
        }

        /// The value the mock generates for `ty` in a response, under
        /// `scope` (an operation id) and `path` (a field path).
        pub fn generate(&self, ty: &TypeRef, scope: &str, path: &str) -> Value {
            Generator::new(&self.model, Context::Response).value(ty, scope, path)
        }

        /// Like [`Types::generate`], with request presence rules
        /// (read-only fields left out, write-only fields included).
        pub fn generate_request(&self, ty: &TypeRef, scope: &str, path: &str) -> Value {
            Generator::new(&self.model, Context::Request).value(ty, scope, path)
        }

        /// Validate a value with response presence rules (write-only
        /// fields are not required). The error names the JSON Pointer and
        /// the problem.
        pub fn validate_response(&self, ty: &TypeRef, value: &Value) -> Result<(), String> {
            Validator::new(&self.model, Context::Response)
                .check(ty, value)
                .map_err(|e| e.to_string())
        }

        /// Validate a value the way request bodies are validated
        /// (read-only fields are not required).
        pub fn validate_request(&self, ty: &TypeRef, value: &Value) -> Result<(), String> {
            Validator::new(&self.model, Context::Request)
                .check(ty, value)
                .map_err(|e| e.to_string())
        }
    }

    /// A string matching `pattern` when the pattern is simple enough to
    /// sample, chosen deterministically from `seed`.
    pub fn sample_pattern(pattern: &str, seed: u64) -> Option<String> {
        crate::pattern::sample(pattern, seed, 0)
    }
}
