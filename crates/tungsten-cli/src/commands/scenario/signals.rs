// SPDX-License-Identifier: AGPL-3.0-only
//! Binary-only Linux cancellation without installing process-global handlers.
use std::io::{self, Read};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
use nix::sys::signalfd::{SfdFlags, SignalFd};
use tungsten_mock::RunCancellation;

struct Mask {
    previous: SigSet,
    armed: bool,
    // A thread mask must be restored on the thread that changed it.
    _thread: PhantomData<Rc<()>>,
}
impl Drop for Mask {
    fn drop(&mut self) {
        if self.armed {
            let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&self.previous), None);
        }
    }
}

pub(crate) struct Signals {
    mask: Mask,
    fd: Arc<SignalFd>,
    cancelled: Arc<AtomicBool>,
}

/// Bounded even when a sender continuously queues signals or a descriptor fails.
pub(crate) fn drain(mut read: impl FnMut() -> io::Result<bool>) -> io::Result<bool> {
    let mut seen = false;
    for _ in 0..64 {
        if !read()? {
            return Ok(seen);
        }
        seen = true;
    }
    Err(io::Error::other(
        "scenario cancellation signal drain limit exceeded",
    ))
}

/// Only the Linux binary uses this observable-disposition guard. This cannot
/// establish exclusive reaping in an embedding or inspect every sigaction flag.
pub(crate) fn child_reaping_status(status: &[u8]) -> io::Result<()> {
    let invalid = || io::Error::other("cannot establish default SIGCHLD ownership");
    if status.len() > 16 * 1024 {
        return Err(invalid());
    }
    let status = std::str::from_utf8(status).map_err(|_| invalid())?;
    let field = |name: &str| -> io::Result<u64> {
        let mut fields = status.lines().filter_map(|line| line.strip_prefix(name));
        let raw = fields.next().ok_or_else(invalid)?.trim();
        if fields.next().is_some()
            || raw.is_empty()
            || raw.len() > 16
            || !raw.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        u64::from_str_radix(raw, 16).map_err(|_| invalid())
    };
    let child = 1_u64 << (Signal::SIGCHLD as u32 - 1);
    if (field("SigIgn:")? | field("SigCgt:")?) & child != 0 {
        return Err(invalid());
    }
    Ok(())
}

impl Signals {
    /// Called by the binary before compilation or mock worker threads start.
    pub fn capture() -> io::Result<Self> {
        let mut status = Vec::new();
        std::fs::File::open("/proc/self/status")?
            .take(16 * 1024 + 1)
            .read_to_end(&mut status)?;
        child_reaping_status(&status)?;
        Self::capture_interrupts()
    }

    /// SIGINT/SIGTERM capture alone, for commands that start no children
    /// (support-bundle export). Also called before any worker thread starts,
    /// so every thread inherits the blocked mask.
    pub fn capture_interrupts() -> io::Result<Self> {
        let mut set = SigSet::empty();
        set.add(Signal::SIGINT);
        set.add(Signal::SIGTERM);
        let mut previous = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&set), Some(&mut previous))?;
        let mask = Mask {
            previous,
            armed: true,
            _thread: PhantomData,
        };
        let fd = Arc::new(SignalFd::with_flags(
            &set,
            SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC,
        )?);
        Ok(Self {
            mask,
            fd,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn cancellation(&self) -> RunCancellation {
        let fd = self.fd.clone();
        let cancelled = self.cancelled.clone();
        RunCancellation(Arc::new(move || {
            if drain(|| {
                fd.read_signal()
                    .map(|value| value.is_some())
                    .map_err(Into::into)
            })
            .unwrap_or(true)
            {
                cancelled.store(true, Ordering::SeqCst);
            }
            cancelled.load(Ordering::SeqCst)
        }))
    }

    pub fn restore(mut self) -> io::Result<bool> {
        // Consume cancellation signals owned by this run before unblocking.
        let seen = drain(|| {
            self.fd
                .read_signal()
                .map(|value| value.is_some())
                .map_err(Into::into)
        })?;
        pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&self.mask.previous), None)?;
        self.mask.armed = false;
        Ok(seen || self.cancelled.load(Ordering::SeqCst))
    }
}
