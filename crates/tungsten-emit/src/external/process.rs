// SPDX-License-Identifier: AGPL-3.0-only
//! Owned external processes and bounded pipe/cleanup work. This is not isolation.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use process_wrap::std::{ChildWrapper, CommandWrap};

use super::{Command, Limits};

const POLL: Duration = Duration::from_millis(2);
const STDERR_KEEP: usize = 8 * 1024;
const STDERR_LINES: usize = 12;
// stdin, stdout, stderr and process observation/reaping. A worker blocked by an
// escaped descendant or OS failure retains its slot. Refuse new execution
// rather than accumulating unbounded detached workers.
const WORKERS_PER_RUN: usize = 4;
const MAX_WORKERS: usize = 16;
static RESERVED_WORKERS: AtomicUsize = AtomicUsize::new(0);
static RETIRED_WORKERS: Mutex<Vec<Retired>> = Mutex::new(Vec::new());
#[cfg(feature = "testing")]
static FAIL_WORKER_START: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "testing")]
static PAUSE_CLEANUP: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "testing")]
static PAUSE_BEFORE_LAUNCH: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "testing")]
static DELAY_PREFLIGHT: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub(crate) struct Finished {
    pub stdout: Vec<u8>,
    pub stderr_tail: String,
    pub code: Option<i32>,
}

#[derive(Debug)]
pub(crate) enum Failure {
    Spawn(io::Error),
    Timeout(Duration),
    TooLarge(usize),
    Io(io::Error),
    Capacity,
    Cleanup {
        cause: Option<Box<Failure>>,
        signalled: bool,
        reaped: bool,
        unfinished: usize,
    },
}

struct Permit;

impl Permit {
    fn reserve(count: usize) -> Result<Vec<Self>, Failure> {
        RESERVED_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used <= MAX_WORKERS - count).then_some(used + count)
            })
            .map_err(|_| Failure::Capacity)?;
        Ok((0..count).map(|_| Self).collect())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        RESERVED_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

// Keep handles and permits for unfinished work, rather than detaching threads.
// Later invocations join only finished workers before reserving new capacity.
struct Retired {
    handle: JoinHandle<()>,
    _permit: Permit,
}

struct Worker<T> {
    owned: Option<Retired>,
    result: mpsc::Receiver<T>,
}

fn reap_finished_workers() {
    let mut retired = RETIRED_WORKERS.lock().unwrap_or_else(|e| e.into_inner());
    let mut index = 0;
    while index < retired.len() {
        if retired[index].handle.is_finished() {
            let worker = retired.swap_remove(index);
            let _ = worker.handle.join();
        } else {
            index += 1;
        }
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        if let Some(worker) = self.owned.take() {
            RETIRED_WORKERS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(worker);
        }
    }
}

impl<T: Send + 'static> Worker<T> {
    fn start(permit: Permit, work: impl FnOnce() -> T + Send + 'static) -> io::Result<Self> {
        #[cfg(feature = "testing")]
        if FAIL_WORKER_START
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .ok()
            == Some(1)
        {
            return Err(io::Error::other("injected external worker start failure"));
        }
        let (send, result) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("tungsten-external".into())
            .spawn(move || {
                let _ = send.send(work());
            })?;
        Ok(Self {
            owned: Some(Retired {
                handle,
                _permit: permit,
            }),
            result,
        })
    }
}

fn collect<T>(worker: &mut Option<Worker<T>>) -> io::Result<Option<T>> {
    if worker
        .as_ref()
        .is_some_and(|w| w.owned.as_ref().is_some_and(|r| r.handle.is_finished()))
    {
        let mut worker = worker.take().expect("finished worker");
        let owned = worker.owned.take().expect("owned worker");
        owned
            .handle
            .join()
            .map_err(|_| io::Error::other("external process worker panicked"))?;
        worker
            .result
            .try_recv()
            .map(Some)
            .map_err(|_| io::Error::other("external process worker returned no result"))
    } else {
        Ok(None)
    }
}

enum Keep {
    Head(usize),
    Tail(usize),
}

fn read(mut pipe: impl Read, keep: Keep, overflow: &AtomicBool) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        bytes.extend_from_slice(&chunk[..n]);
        match keep {
            Keep::Head(max) if bytes.len() > max => {
                overflow.store(true, Ordering::Release);
                break;
            }
            Keep::Tail(max) if bytes.len() > 2 * max => {
                bytes.drain(..bytes.len() - max);
            }
            _ => {}
        }
    }
    if let Keep::Tail(max) = keep
        && bytes.len() > max
    {
        bytes.drain(..bytes.len() - max);
    }
    Ok(bytes)
}

fn tail_lines(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n")
}

struct OwnedChild {
    child: Box<dyn ChildWrapper>,
    armed: bool,
}

impl OwnedChild {
    #[cfg(any(
        target_os = "android",
        target_os = "freebsd",
        target_os = "haiku",
        all(target_os = "linux", not(target_env = "uclibc"))
    ))]
    fn observe(&mut self) -> io::Result<Option<Option<i32>>> {
        use nix::errno::Errno;
        use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid};
        use nix::unistd::Pid;

        let pid = i32::try_from(self.child.id()).map_err(io::Error::other)?;
        // Keep the leader unreaped until its group is signalled. Otherwise a
        // later group kill could use a recycled identity. Embeddings must own
        // reaping exclusively and must not auto-reap children or change SIGCHLD.
        match waitid(
            Id::Pid(Pid::from_raw(pid)),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
        ) {
            Ok(WaitStatus::Exited(_, code)) => Ok(Some(Some(code))),
            Ok(WaitStatus::Signaled(..)) => Ok(Some(None)),
            Ok(_) | Err(Errno::EINTR) => Ok(None),
            Err(Errno::ECHILD) => {
                self.armed = false;
                Err(io::Error::other("external process child ownership lost"))
            }
            Err(e) => Err(e.into()),
        }
    }

    #[cfg(target_vendor = "apple")]
    fn observe(&mut self) -> io::Result<Option<Option<i32>>> {
        use rustix::io::Errno;
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let raw = i32::try_from(self.child.id()).map_err(io::Error::other)?;
        let pid = Pid::from_raw(raw)
            .ok_or_else(|| io::Error::other("invalid external child identity"))?;
        // nix's safe waitid API is not available on Apple. Retain the same
        // non-reaping ownership contract through rustix's safe Darwin wrapper.
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) if status.exited() => Ok(Some(status.exit_status())),
            Ok(Some(status)) if status.killed() || status.dumped() => Ok(Some(None)),
            Ok(_) | Err(Errno::INTR) => Ok(None),
            Err(Errno::CHILD) => {
                self.armed = false;
                Err(io::Error::other("external process child ownership lost"))
            }
            Err(e) => Err(e.into()),
        }
    }

    #[cfg(all(
        unix,
        not(any(
            target_vendor = "apple",
            target_os = "android",
            target_os = "freebsd",
            target_os = "haiku",
            all(target_os = "linux", not(target_env = "uclibc"))
        ))
    ))]
    fn observe(&mut self) -> io::Result<Option<Option<i32>>> {
        self.armed = false;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "non-reaping external process observation is unsupported on this platform",
        ))
    }

    #[cfg(not(unix))]
    fn observe(&mut self) -> io::Result<Option<Option<i32>>> {
        // The Windows process handle and Job Object retain their identities
        // after leader exit. Do not consume the wrapper's job notifications.
        self.child
            .inner_mut()
            .try_wait()
            .map(|status| status.map(|status| status.code()))
    }

    fn stop_and_reap(mut self, state: &CleanupState) {
        // Revalidate ownership before signalling; ECHILD disarms all kills.
        loop {
            match self.observe() {
                Ok(_) if self.armed => break,
                _ if !self.armed => return,
                // An observation failure is not permission to abandon an
                // unreaped leader. Retain this admitted owner and its slot.
                _ => thread::sleep(POLL),
            }
        }
        self.armed = false;
        state
            .signalled
            .store(self.child.start_kill().is_ok(), Ordering::Release);
        #[cfg(feature = "testing")]
        while PAUSE_CLEANUP.load(Ordering::Acquire) {
            thread::sleep(POLL);
        }
        // Reap only our leader. Whole-group wait() is not bounded and does not
        // establish that escaped descendants settled. This worker owns the
        // handle/permit until wait actually finishes, even after caller timeout.
        loop {
            match self.child.inner_mut().wait() {
                Ok(_) => {
                    state.reaped.store(true, Ordering::Release);
                    return;
                }
                #[cfg(unix)]
                Err(e) if e.raw_os_error() == Some(nix::errno::Errno::ECHILD as i32) => return,
                // Do not release the handle/slot on an unresolved wait error.
                // No further signal is issued after any wait attempt.
                Err(_) => thread::sleep(POLL),
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.armed && self.observe().is_ok() && self.armed {
            // Best effort only for unexpected unwinding. Never block
            // the caller in Drop, or signal after loss of Unix child ownership.
            let _ = self.child.start_kill();
        }
    }
}

#[derive(Default)]
struct CleanupState {
    spawned: AtomicBool,
    signalled: AtomicBool,
    reaped: AtomicBool,
    timed_out: AtomicBool,
}

struct Lifecycle {
    code: Option<i32>,
    failure: Option<Failure>,
}

fn supervise(
    child: &mut OwnedChild,
    overflow: &AtomicBool,
    abort: &AtomicBool,
    state: &CleanupState,
    execution_deadline: Instant,
    limits: Limits,
) -> Lifecycle {
    let mut out = Lifecycle {
        code: None,
        failure: None,
    };
    loop {
        if overflow.load(Ordering::Acquire) {
            out.failure = Some(Failure::TooLarge(limits.max_output_bytes));
            break;
        }
        if abort.load(Ordering::Acquire) {
            break;
        }
        match child.observe() {
            Ok(Some(code)) => {
                out.code = code;
                break;
            }
            Err(e) => {
                out.failure = Some(Failure::Io(e));
                break;
            }
            Ok(None) if Instant::now() < execution_deadline => thread::sleep(POLL),
            Ok(None) => {
                state.timed_out.store(true, Ordering::Release);
                out.failure = Some(Failure::Timeout(limits.timeout));
                break;
            }
        }
    }
    out
}

struct Pipes {
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
}

fn launch(
    command: &Command,
    extra: &[String],
    cwd: &Path,
    execution_deadline: Instant,
    abort: &AtomicBool,
    timeout: Duration,
) -> Result<OwnedChild, Failure> {
    let mut native = std::process::Command::new(&command.program);
    native
        .args(&command.args)
        .args(extra)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrapped = CommandWrap::from(native);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    {
        wrapped.wrap(process_wrap::std::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ));
        wrapped.wrap(process_wrap::std::JobObject);
    }
    #[cfg(not(any(unix, windows)))]
    return Err(Failure::Spawn(io::Error::new(
        io::ErrorKind::Unsupported,
        "external process ownership is unsupported on this platform",
    )));
    // Command construction also consumes time; check again at the launch API.
    if abort.load(Ordering::Acquire) || Instant::now() >= execution_deadline {
        return Err(Failure::Timeout(timeout));
    }
    Ok(OwnedChild {
        child: wrapped.spawn().map_err(Failure::Spawn)?,
        armed: true,
    })
}

// The admitted supervisor owns spawning itself: failure to create that worker
// cannot leave an unowned process or an unreaped leader outside the slot bound.
struct Invocation {
    command: Command,
    extra: Vec<String>,
    cwd: std::path::PathBuf,
    execution_deadline: Instant,
    limits: Limits,
}

fn launch_and_supervise(
    invocation: Invocation,
    pipes: mpsc::Sender<Pipes>,
    overflow: &AtomicBool,
    abort: &AtomicBool,
    state: &CleanupState,
) -> Lifecycle {
    let Invocation {
        command,
        extra,
        cwd,
        execution_deadline,
        limits,
    } = invocation;
    #[cfg(feature = "testing")]
    while PAUSE_BEFORE_LAUNCH.load(Ordering::Acquire) {
        thread::sleep(POLL);
    }
    // A scheduled worker is not permission to execute later. Refuse known stale
    // work immediately before the OS spawn; an OS call already in progress is
    // a different uncertainty and remains owned by this worker and its permit.
    if abort.load(Ordering::Acquire) || Instant::now() >= execution_deadline {
        state.timed_out.store(true, Ordering::Release);
        return Lifecycle {
            code: None,
            failure: Some(Failure::Timeout(limits.timeout)),
        };
    }
    let mut child = match launch(
        &command,
        &extra,
        &cwd,
        execution_deadline,
        abort,
        limits.timeout,
    ) {
        Ok(child) => child,
        Err(failure) => {
            if matches!(&failure, Failure::Timeout(_)) {
                state.timed_out.store(true, Ordering::Release);
            }
            return Lifecycle {
                code: None,
                failure: Some(failure),
            };
        }
    };
    state.spawned.store(true, Ordering::Release);
    let stdio = match (
        child.child.stdin().take(),
        child.child.stdout().take(),
        child.child.stderr().take(),
    ) {
        (Some(stdin), Some(stdout), Some(stderr)) => Pipes {
            stdin,
            stdout,
            stderr,
        },
        _ => {
            child.stop_and_reap(state);
            return Lifecycle {
                code: None,
                failure: Some(Failure::Io(io::Error::other(
                    "external process pipe setup failed",
                ))),
            };
        }
    };
    if pipes.send(stdio).is_err() {
        abort.store(true, Ordering::Release);
    }
    // Keep ownership outside the unwind boundary so an observer panic still
    // terminates/reaps in this admitted worker, retaining its handle and slot.
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supervise(
            &mut child,
            overflow,
            abort,
            state,
            execution_deadline,
            limits,
        )
    }))
    .unwrap_or_else(|_| Lifecycle {
        code: None,
        failure: Some(Failure::Io(io::Error::other(
            "external process supervisor panicked",
        ))),
    });
    child.stop_and_reap(state);
    out
}

/// The process deadline includes spawning, pipe work and cleanup. Up to one
/// quarter (at most one second) is reserved for termination/reaping/draining.
/// OS calls are not hard real-time; a blocked worker retains a capacity slot.
pub(crate) fn run(
    command: &Command,
    extra: &[&str],
    cwd: &Path,
    input: Vec<u8>,
    limits: Limits,
) -> Result<Finished, Failure> {
    let started = Instant::now();
    if limits.timeout.is_zero() {
        return Err(Failure::Timeout(limits.timeout));
    }
    let deadline = started.checked_add(limits.timeout).ok_or_else(|| {
        Failure::Spawn(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid emitter timeout",
        ))
    })?;
    let execution_deadline = deadline - (limits.timeout / 4).min(Duration::from_secs(1));
    if !cfg!(any(
        windows,
        target_vendor = "apple",
        target_os = "android",
        target_os = "freebsd",
        target_os = "haiku",
        all(target_os = "linux", not(target_env = "uclibc"))
    )) {
        return Err(Failure::Spawn(io::Error::new(
            io::ErrorKind::Unsupported,
            "non-reaping external process observation is unsupported on this platform",
        )));
    }
    #[cfg(target_os = "linux")]
    {
        let mut status = Vec::new();
        std::fs::File::open("/proc/self/status")
            .and_then(|file| file.take(16 * 1024 + 1).read_to_end(&mut status))
            .map_err(Failure::Io)?;
        child_reaping_status(&status).map_err(Failure::Io)?;
    }
    reap_finished_workers();
    #[cfg(feature = "testing")]
    if DELAY_PREFLIGHT.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(40));
    }
    // Preflight, joining finished work or scheduling can consume the budget.
    // Do not admit a known-expired invocation to the worker/process path.
    if Instant::now() >= execution_deadline {
        return Err(Failure::Timeout(limits.timeout));
    }
    let mut permits = Permit::reserve(WORKERS_PER_RUN)?;
    let overflow = Arc::new(AtomicBool::new(false));
    let abort = Arc::new(AtomicBool::new(false));
    let state = Arc::new(CleanupState::default());
    let (send_pipes, receive_pipes) = mpsc::channel();
    let mut lifecycle = {
        let (overflow, abort, state) = (overflow.clone(), abort.clone(), state.clone());
        let (command, extra, cwd) = (
            command.clone(),
            extra.iter().map(|s| s.to_string()).collect(),
            cwd.to_path_buf(),
        );
        Some(
            Worker::start(permits.pop().expect("reserved worker"), move || {
                launch_and_supervise(
                    Invocation {
                        command,
                        extra,
                        cwd,
                        execution_deadline,
                        limits,
                    },
                    send_pipes,
                    &overflow,
                    &abort,
                    &state,
                )
            })
            .map_err(Failure::Io)?,
        )
    };
    let pipes = loop {
        match receive_pipes.try_recv() {
            Ok(pipes) => break Some(pipes),
            Err(mpsc::TryRecvError::Disconnected) => break None,
            Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => thread::sleep(POLL),
            Err(mpsc::TryRecvError::Empty) => {
                state.timed_out.store(true, Ordering::Release);
                abort.store(true, Ordering::Release);
                break None;
            }
        }
    };
    drop(receive_pipes);
    let mut failure = None;
    let mut start = |result: io::Result<_>| match result {
        Ok(worker) => Some(worker),
        Err(e) => {
            abort.store(true, Ordering::Release);
            failure = Some(Failure::Io(e));
            None
        }
    };
    let (mut output, mut errors, mut input) = if let Some(Pipes {
        mut stdin,
        stdout,
        stderr,
    }) = pipes
    {
        let output = {
            let overflow = overflow.clone();
            start(Worker::start(
                permits.pop().expect("reserved worker"),
                move || read(stdout, Keep::Head(limits.max_output_bytes), &overflow),
            ))
        };
        let errors = start(Worker::start(
            permits.pop().expect("reserved worker"),
            move || read(stderr, Keep::Tail(STDERR_KEEP), &AtomicBool::new(false)),
        ));
        // Early stdin closure remains valid emitter behavior.
        let input = start(Worker::start(
            permits.pop().expect("reserved worker"),
            move || {
                let _ = stdin.write_all(&input);
                Ok(Vec::new())
            },
        ));
        (output, errors, input)
    } else {
        (None, None, None)
    };
    let (mut stdout, mut stderr, mut discarded, mut completed) =
        (Vec::new(), Vec::new(), Vec::new(), None);
    loop {
        for (worker, bytes) in [
            (&mut output, &mut stdout),
            (&mut errors, &mut stderr),
            (&mut input, &mut discarded),
        ] {
            match collect(worker) {
                Ok(Some(Ok(result))) => *bytes = result,
                Ok(Some(Err(e))) | Err(e) => {
                    failure.get_or_insert(Failure::Io(e));
                    abort.store(true, Ordering::Release);
                }
                Ok(None) => {}
            }
        }
        match collect(&mut lifecycle) {
            Ok(Some(result)) => completed = Some(result),
            Err(e) => {
                failure.get_or_insert(Failure::Io(e));
            }
            Ok(None) => {}
        }
        let unfinished = usize::from(output.is_some())
            + usize::from(errors.is_some())
            + usize::from(input.is_some())
            + usize::from(lifecycle.is_some());
        if unfinished == 0 || Instant::now() >= deadline {
            if overflow.load(Ordering::Acquire) {
                failure = Some(Failure::TooLarge(limits.max_output_bytes));
            } else if state.timed_out.load(Ordering::Acquire) {
                failure.get_or_insert(Failure::Timeout(limits.timeout));
            } else if let Some(result) = &mut completed {
                failure = failure.or(result.failure.take());
            }
            let signalled = state.signalled.load(Ordering::Acquire);
            let reaped = state.reaped.load(Ordering::Acquire);
            if unfinished != 0 || (state.spawned.load(Ordering::Acquire) && (!signalled || !reaped))
            {
                return Err(Failure::Cleanup {
                    cause: failure.map(Box::new),
                    signalled,
                    reaped,
                    unfinished,
                });
            }
            if let Some(failure) = failure {
                return Err(failure);
            }
            return Ok(Finished {
                stdout,
                stderr_tail: tail_lines(&stderr),
                code: completed.and_then(|r| r.code),
            });
        }
        thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(feature = "testing")]
pub(super) fn reserved_workers() -> usize {
    reap_finished_workers();
    RESERVED_WORKERS.load(Ordering::Acquire)
}

#[cfg(feature = "testing")]
pub(super) fn with_capacity_reserved<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    reap_finished_workers();
    let _permits =
        Permit::reserve(MAX_WORKERS).map_err(|_| "worker capacity already reserved".to_string())?;
    Ok(f())
}

#[cfg(feature = "testing")]
pub(super) fn with_worker_start_failure<R>(nth: usize, f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FAIL_WORKER_START.store(0, Ordering::Release);
        }
    }
    FAIL_WORKER_START.store(nth, Ordering::Release);
    let _reset = Reset;
    f()
}

#[cfg(feature = "testing")]
pub(super) fn with_cleanup_paused<R>(f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            PAUSE_CLEANUP.store(false, Ordering::Release);
        }
    }
    PAUSE_CLEANUP.store(true, Ordering::Release);
    let _reset = Reset;
    f()
}

#[cfg(feature = "testing")]
pub(super) fn with_launch_paused<R>(f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            PAUSE_BEFORE_LAUNCH.store(false, Ordering::Release);
        }
    }
    PAUSE_BEFORE_LAUNCH.store(true, Ordering::Release);
    let _reset = Reset;
    f()
}

#[cfg(feature = "testing")]
pub(super) fn with_preflight_delay<R>(f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            DELAY_PREFLIGHT.store(false, Ordering::Release);
        }
    }
    DELAY_PREFLIGHT.store(true, Ordering::Release);
    let _reset = Reset;
    f()
}

/// Observable Linux disposition only. It cannot detect every competing reaper
/// or sigaction flag, and cannot make an embedding's ownership promise true.
#[cfg(target_os = "linux")]
pub(super) fn child_reaping_status(status: &[u8]) -> io::Result<()> {
    let invalid = || {
        io::Error::other(
            "external processes require readable /proc/self/status and default SIGCHLD disposition",
        )
    };
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
    let child = 1_u64 << (nix::sys::signal::Signal::SIGCHLD as u32 - 1);
    if (field("SigIgn:")? | field("SigCgt:")?) & child != 0 {
        return Err(invalid());
    }
    Ok(())
}
