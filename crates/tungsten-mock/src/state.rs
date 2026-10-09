// SPDX-License-Identifier: AGPL-3.0-only
//! Shared mutable state: recorded calls, queued programs and stored
//! idempotent responses. Recorded calls and stored responses are capped
//! ([`crate::MockOptions::max_recorded_calls`],
//! [`crate::MockOptions::max_idempotent_responses`]): past the cap the oldest
//! entry is dropped, so a long-running mock stays bounded.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::RecordedCall;
use crate::handler::Injection;
use crate::model::Model;
use crate::reply::Reply;

/// Lock a mutex, recovering the data if a holder panicked.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A scripted behaviour queued with `/__tungsten/program`.
#[derive(Debug, Clone)]
pub(crate) struct Program {
    pub action: Action,
    pub remaining: u64,
}

/// What a program does with a matching call.
#[derive(Debug, Clone)]
pub(crate) enum Action {
    /// Answer this instead of processing the request.
    Answer(Answer),
    /// Apply an injection, as `X-Tungsten-Inject` would.
    Inject(Injection),
}

/// A scripted response.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub status: u16,
    pub body: Option<Value>,
    pub headers: Vec<(String, String)>,
    pub code: Option<String>,
    /// Send only this many bytes of the body, then drop the connection.
    pub cut_after: Option<usize>,
}

/// The outcome of looking up an idempotency key.
#[derive(Debug)]
pub(crate) enum Idempotent {
    Fresh,
    Replay(Reply),
    Conflict,
}

#[derive(Debug)]
struct Stored {
    fingerprint: Vec<u8>,
    reply: Reply,
}

#[derive(Debug, Default)]
struct Inner {
    /// Sorted by arrival sequence number; at most `max_calls`.
    calls: VecDeque<(u64, RecordedCall)>,
    programs: BTreeMap<String, VecDeque<Program>>,
    idempotent: BTreeMap<(String, String), Stored>,
    /// Keys of `idempotent` in the order they were stored, oldest first.
    stored_order: VecDeque<(String, String)>,
}

#[derive(Debug)]
pub(crate) struct State {
    pub model: Model,
    sequence: AtomicU64,
    max_calls: usize,
    max_idempotent: usize,
    inner: Mutex<Inner>,
}

impl State {
    pub fn new(model: Model, max_calls: usize, max_idempotent: usize) -> State {
        State {
            model,
            sequence: AtomicU64::new(0),
            max_calls,
            max_idempotent,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The arrival number of a new request.
    pub fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::SeqCst)
    }

    /// Record a call at its arrival position, dropping the oldest calls
    /// beyond the cap.
    pub fn record(&self, sequence: u64, call: RecordedCall) {
        let mut inner = lock(&self.inner);
        let at = inner.calls.partition_point(|(s, _)| *s < sequence);
        inner.calls.insert(at, (sequence, call));
        while inner.calls.len() > self.max_calls {
            inner.calls.pop_front();
        }
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        lock(&self.inner)
            .calls
            .iter()
            .map(|(_, c)| c.clone())
            .collect()
    }

    pub fn reset(&self) {
        *lock(&self.inner) = Inner::default();
    }

    pub fn queue(&self, programs: Vec<(String, Program)>) {
        let mut inner = lock(&self.inner);
        for (operation, program) in programs {
            inner
                .programs
                .entry(operation)
                .or_default()
                .push_back(program);
        }
    }

    /// The next program for an operation, counting one use of it.
    pub fn take_program(&self, operation: &str) -> Option<Program> {
        let mut inner = lock(&self.inner);
        let queue = inner.programs.get_mut(operation)?;
        let front = queue.front_mut()?;
        front.remaining = front.remaining.saturating_sub(1);
        let program = front.clone();
        if front.remaining == 0 {
            queue.pop_front();
        }
        if queue.is_empty() {
            inner.programs.remove(operation);
        }
        Some(program)
    }

    pub fn idempotent(&self, operation: &str, key: &str, fingerprint: &[u8]) -> Idempotent {
        let inner = lock(&self.inner);
        match inner
            .idempotent
            .get(&(operation.to_string(), key.to_string()))
        {
            None => Idempotent::Fresh,
            Some(stored) if stored.fingerprint == fingerprint => {
                Idempotent::Replay(stored.reply.clone())
            }
            Some(_) => Idempotent::Conflict,
        }
    }

    /// Store the response of a key's first request, dropping the oldest
    /// stored responses beyond the cap (a later repeat of a dropped key is
    /// served as a fresh request).
    pub fn store(&self, operation: &str, key: &str, fingerprint: Vec<u8>, reply: Reply) {
        let mut inner = lock(&self.inner);
        let id = (operation.to_string(), key.to_string());
        let stored = Stored { fingerprint, reply };
        if inner.idempotent.insert(id.clone(), stored).is_none() {
            inner.stored_order.push_back(id);
        }
        while inner.idempotent.len() > self.max_idempotent {
            let Some(oldest) = inner.stored_order.pop_front() else {
                break;
            };
            inner.idempotent.remove(&oldest);
        }
    }
}
