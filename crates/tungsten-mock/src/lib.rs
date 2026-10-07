// SPDX-License-Identifier: AGPL-3.0-only
//! IR-driven mock HTTP server (planning/05 "Mock server", planning/08 L4).
//!
//! Validates requests against the IR, answers from examples or
//! deterministic schema-derived values, records every call, and injects
//! failures on demand. Out-of-process test drivers (TypeScript, Python)
//! control it over HTTP:
//!
//! - request header `X-Tungsten-Inject`: `timeout` (hold the response past
//!   the client deadline), `drop-after-write` (read the request, apply it,
//!   then close the connection without a response), `reset` (close before
//!   reading the body), `status=<code>[;retry-after=<s>][;code=<error code>]`
//!   (answer with that status and, when the IR has an error envelope, a body
//!   carrying the code);
//! - `GET /__tungsten/calls` recorded calls as JSON, `POST /__tungsten/reset`
//!   clears calls and programs, `POST /__tungsten/program` queues scripted
//!   responses `{operation, status, body?, headers?, times?}` for the next
//!   matching calls.
//!
//! PHASE-2 CONTRACT: the signatures below are used by the CLI (`tungsten
//! mock`) and the test harness. The stub refuses to start.

use std::net::SocketAddr;

use tungsten_ir::Ir;

/// Options for [`MockServer::start`].
#[derive(Debug, Clone)]
pub struct MockOptions {
    /// Address to bind; port 0 picks a free port.
    pub addr: SocketAddr,
    /// Seed for schema-derived values (output is deterministic per seed).
    pub seed: u64,
}

impl Default for MockOptions {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            seed: 0,
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
}

impl MockServer {
    /// Start serving `ir` on a background thread.
    pub fn start(ir: Ir, opts: MockOptions) -> std::io::Result<MockServer> {
        let _ = (ir, opts);
        Err(std::io::Error::other("mock server not implemented"))
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
        vec![]
    }

    /// Clear recorded calls and programmed responses.
    pub fn reset(&self) {}

    /// Stop serving and wait for the server thread.
    pub fn shutdown(self) {}
}
