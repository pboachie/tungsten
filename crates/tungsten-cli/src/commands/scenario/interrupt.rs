// SPDX-License-Identifier: AGPL-3.0-only
//! Binary-only macOS and Windows cancellation for `scenario run`.
//!
//! These platforms have no signalfd, so the binary installs one Ctrl-C/SIGTERM
//! handler (Windows: Ctrl-C, Ctrl-Break and console close) the first time a run
//! captures it. Outside a captured run the handler exits with status 130, as
//! the default disposition would interrupt the process.
use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use tungsten_mock::RunCancellation;

static INSTALLED: OnceLock<bool> = OnceLock::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub(crate) struct Signals(());

impl Signals {
    /// Called by the binary before compilation or mock worker threads start.
    pub fn capture() -> io::Result<Self> {
        let installed = *INSTALLED.get_or_init(|| {
            ctrlc::set_handler(|| {
                if ACTIVE.load(Ordering::SeqCst) {
                    PENDING.store(true, Ordering::SeqCst);
                } else {
                    std::process::exit(130);
                }
            })
            .is_ok()
        });
        if !installed || ACTIVE.swap(true, Ordering::SeqCst) {
            return Err(io::Error::other("cannot capture scenario cancellation"));
        }
        PENDING.store(false, Ordering::SeqCst);
        Ok(Self(()))
    }

    /// The same capture; these platforms have no child-reaping preflight.
    pub fn capture_interrupts() -> io::Result<Self> {
        Self::capture()
    }

    pub fn cancellation(&self) -> RunCancellation {
        RunCancellation(std::sync::Arc::new(|| PENDING.load(Ordering::SeqCst)))
    }

    /// Ends the captured scope; reports whether cancellation arrived during it.
    pub fn restore(self) -> io::Result<bool> {
        // A signal after this store exits the process instead of being lost.
        ACTIVE.store(false, Ordering::SeqCst);
        Ok(PENDING.swap(false, Ordering::SeqCst))
    }
}
