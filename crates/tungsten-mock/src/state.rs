// SPDX-License-Identifier: AGPL-3.0-only
//! Shared mutable state: recorded calls, queued programs and stored
//! idempotent responses.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::RecordedCall;
use crate::model::Model;
use crate::reply::Reply;

/// Lock a mutex, recovering the data if a holder panicked.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A scripted response queued with `/__tungsten/program`.
#[derive(Debug, Clone)]
pub(crate) struct Program {
    pub status: u16,
    pub body: Option<Value>,
    pub headers: Vec<(String, String)>,
    pub code: Option<String>,
    pub remaining: u64,
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
    /// Sorted by arrival sequence number.
    calls: Vec<(u64, RecordedCall)>,
    programs: BTreeMap<String, VecDeque<Program>>,
    idempotent: BTreeMap<(String, String), Stored>,
}

#[derive(Debug)]
pub(crate) struct State {
    pub model: Model,
    sequence: AtomicU64,
    inner: Mutex<Inner>,
}

impl State {
    pub fn new(model: Model) -> State {
        State {
            model,
            sequence: AtomicU64::new(0),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The arrival number of a new request.
    pub fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::SeqCst)
    }

    /// Record a call at its arrival position.
    pub fn record(&self, sequence: u64, call: RecordedCall) {
        let mut inner = lock(&self.inner);
        let at = inner.calls.partition_point(|(s, _)| *s < sequence);
        inner.calls.insert(at, (sequence, call));
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

    pub fn store(&self, operation: &str, key: &str, fingerprint: Vec<u8>, reply: Reply) {
        lock(&self.inner).idempotent.insert(
            (operation.to_string(), key.to_string()),
            Stored { fingerprint, reply },
        );
    }
}
