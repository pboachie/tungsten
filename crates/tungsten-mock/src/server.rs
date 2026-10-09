// SPDX-License-Identifier: AGPL-3.0-only
//! The HTTP/1.1 server: a current-thread tokio runtime on its own thread,
//! one hyper connection task per client. A dropped outcome makes the
//! service fail, which closes the connection without a response.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::handler::{self, Outcome};
use crate::state::State;

/// Time a client gets to send its request headers.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Pause after a failed accept (for example, out of file descriptors).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(10);

/// The error that closes a connection without a response.
#[derive(Debug)]
struct ConnectionDropped;

impl fmt::Display for ConnectionDropped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("connection dropped by injection")
    }
}

impl std::error::Error for ConnectionDropped {}

/// The server thread and its stop signal.
#[derive(Debug)]
pub(crate) struct Running {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    /// Stop accepting, close every connection and wait for the thread; the
    /// listening socket is closed when this returns.
    pub fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Bind `addr` and serve on a new thread.
pub(crate) fn spawn(addr: SocketAddr, state: Arc<State>) -> std::io::Result<(SocketAddr, Running)> {
    let std_listener = std::net::TcpListener::bind(addr)?;
    std_listener.set_nonblocking(true)?;
    let local = std_listener.local_addr()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let listener = {
        let _guard = runtime.enter();
        TcpListener::from_std(std_listener)?
    };
    let (stop, stopped) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("tungsten-mock".into())
        .spawn(move || {
            runtime.block_on(serve(listener, state, stopped));
            // Dropping the runtime here cancels every connection task.
            drop(runtime);
        })?;
    Ok((
        local,
        Running {
            stop: Some(stop),
            thread: Some(thread),
        },
    ))
}

async fn serve(listener: TcpListener, state: Arc<State>, mut stopped: oneshot::Receiver<()>) {
    loop {
        tokio::select! {
            _ = &mut stopped => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(connection(stream, state.clone()));
                }
                Err(_) => tokio::time::sleep(ACCEPT_BACKOFF).await,
            },
        }
    }
}

async fn connection(stream: TcpStream, state: Arc<State>) {
    let _ = stream.set_nodelay(true);
    let service = service_fn(move |req| {
        let state = state.clone();
        async move {
            match handler::handle(state, req).await {
                Outcome::Reply(reply) => Ok(reply.into_response()),
                Outcome::Drop => Err(ConnectionDropped),
            }
        }
    });
    // No `Date` header: identical requests get identical bytes.
    let _ = http1::Builder::new()
        .auto_date_header(false)
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .await;
}
