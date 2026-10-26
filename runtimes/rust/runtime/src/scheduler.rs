// SPDX-License-Identifier: Apache-2.0
//! Bounded request admission shared by the calls of one or more clients.
//!
//! A [`RequestScheduler`] keeps, per scope (a tenant label plus, by default,
//! the request origin), a count of requests in flight, a bounded
//! first-in-first-out queue of requests waiting for capacity, and a shared
//! rate-limit pause. Ordinary calls and event streams are counted apart, so
//! long-lived streams cannot starve ordinary calls. Nothing is coordinated
//! across processes.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// The pause after a `RATE_LIMITED` answer that names no wait.
const DEFAULT_PAUSE_MS: f64 = 1000.0;
const MAX_PAUSE_MS: f64 = 3_600_000.0;
/// Slack for the sums of fractional retry earnings.
const EPSILON: f64 = 1e-9;

/// How requests are grouped into scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchedulerScope {
    /// One scope per tenant and request origin.
    #[default]
    Origin,
    /// One scope per tenant, whatever the origin.
    Scheduler,
}

/// A token bucket of retries shared by the callers of one scope, so that a
/// failing upstream sees a bounded share of retries instead of every caller's
/// full retry policy at once. A scope starts with `burst` retries; each first
/// attempt admitted earns `ratio` more and every second earns
/// `min_per_second`, up to `burst`. A retry spends one; without one, the call
/// stops retrying and returns its last failure as is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryBudget {
    /// Retries earned per first attempt admitted, from 0 to 1. Default 0.1.
    pub ratio: f64,
    /// Retries earned per second whatever the traffic, from 0 to 10000.
    /// Default 1.
    pub min_per_second: f64,
    /// The most retries saved up, and the starting balance; 1 to 1000000.
    /// Default 10.
    pub burst: u32,
}

impl Default for RetryBudget {
    fn default() -> Self {
        RetryBudget {
            ratio: 0.1,
            min_per_second: 1.0,
            burst: 10,
        }
    }
}

/// Limits of a [`RequestScheduler`]. Every limit applies per scope.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerOptions {
    /// Ordinary requests in flight at once. 1 to 10000.
    pub max_active: u32,
    /// Ordinary requests, and separately event streams, that may wait for
    /// capacity; one more is rejected at once with `QUEUE_FULL`. 0 never
    /// waits. Default 100.
    pub max_queued: u32,
    /// Event streams open at once. `None`: `max_active`.
    pub max_streams: Option<u32>,
    /// Longest wait for capacity; a longer wait fails with `QUEUE_TIMEOUT`
    /// and nothing is sent. The call's own deadline and dropping its future
    /// also end the wait. Default 30 s.
    pub max_queue: Duration,
    pub scope: SchedulerScope,
    /// Retries shared by every call of one scope. `None` (default): only each
    /// call's own retry policy limits retries.
    pub retry_budget: Option<RetryBudget>,
}

impl SchedulerOptions {
    /// Options with `max_active` and every other limit at its default.
    pub fn new(max_active: u32) -> Self {
        SchedulerOptions {
            max_active,
            max_queued: 100,
            max_streams: None,
            max_queue: Duration::from_secs(30),
            scope: SchedulerScope::Origin,
            retry_budget: None,
        }
    }
}

/// Why a request was not admitted; it must not be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Full,
    Timeout,
    /// A shared rate-limit pause outlasts the wait allowed; it lasts this
    /// many more milliseconds.
    Paused(u64),
    Closed,
}

/// Counts across every scope, for telemetry and load checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SchedulerStats {
    pub active: usize,
    pub streams: usize,
    pub queued: usize,
    pub scopes: usize,
}

/// Ordinary call or event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Call = 0,
    Stream = 1,
}

type Random = Arc<dyn Fn() -> f64 + Send + Sync>;

struct Waiter {
    id: u64,
    retry: bool,
    notify: Arc<Notify>,
    random: Random,
}

struct Pool {
    active: [usize; 2],
    queue: [VecDeque<Waiter>; 2],
    paused_until: Option<Instant>,
    probing: bool,
    /// Retries left in the shared budget, as of `refilled_at`.
    tokens: f64,
    refilled_at: Instant,
}

impl Pool {
    fn new(budget: Option<&RetryBudget>) -> Self {
        Pool {
            active: [0; 2],
            queue: [VecDeque::new(), VecDeque::new()],
            paused_until: None,
            probing: false,
            tokens: budget.map_or(0.0, |b| f64::from(b.burst)),
            refilled_at: Instant::now(),
        }
    }

    /// Add what the budget earned per second since it was last refilled.
    fn refill(&mut self, budget: &RetryBudget, now: Instant) {
        let earned = now
            .saturating_duration_since(self.refilled_at)
            .as_secs_f64()
            * budget.min_per_second;
        self.tokens = (self.tokens + earned).min(f64::from(budget.burst));
        self.refilled_at = now;
    }

    /// An idle scope whose pause has passed and whose retry budget is full
    /// holds nothing worth keeping.
    fn disposable(&mut self, budget: Option<&RetryBudget>, now: Instant) -> bool {
        if self.busy() > 0 || !self.paused_for(now).is_zero() {
            return false;
        }
        let Some(budget) = budget else {
            return true;
        };
        self.refill(budget, now);
        self.tokens >= f64::from(budget.burst) - EPSILON
    }

    fn busy(&self) -> usize {
        self.active[0] + self.active[1] + self.queue[0].len() + self.queue[1].len()
    }

    fn paused_for(&self, now: Instant) -> Duration {
        self.paused_until
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(now))
    }
}

#[derive(Default)]
struct State {
    pools: HashMap<String, Pool>,
    /// Permits handed to waiters that have not picked them up yet.
    granted: HashMap<u64, Permit>,
    closed_waiters: Vec<u64>,
    closed: bool,
    next_id: u64,
}

struct Shared {
    max_active: usize,
    max_queued: usize,
    max_streams: usize,
    max_queue: Duration,
    scope: SchedulerScope,
    retry_budget: Option<RetryBudget>,
    state: Mutex<State>,
}

/// Admits requests within the configured bounds. Clone it into
/// `ClientOptions::scheduler` of several clients to share capacity and
/// rate-limit pauses between them; give each client a `tenant` to keep their
/// accounting apart.
#[derive(Clone)]
pub struct RequestScheduler {
    shared: Arc<Shared>,
}

impl fmt::Debug for RequestScheduler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestScheduler")
            .field("max_active", &self.shared.max_active)
            .field("max_queued", &self.shared.max_queued)
            .field("max_streams", &self.shared.max_streams)
            .field("max_queue", &self.shared.max_queue)
            .field("scope", &self.shared.scope)
            .finish_non_exhaustive()
    }
}

/// Capacity held by one admitted request; dropping it frees the capacity.
pub(crate) struct Permit {
    shared: Arc<Shared>,
    key: String,
    kind: Kind,
    random: Random,
    settled: bool,
}

/// Under the lock: the pool of `key`, made when missing.
fn pool_mut<'a>(shared: &Shared, state: &'a mut State, key: &str) -> &'a mut Pool {
    if !state.pools.contains_key(key) {
        // Scopes kept only for their unfinished pause or spent retry budget
        // are dropped once neither matters any more.
        let now = Instant::now();
        let budget = shared.retry_budget.as_ref();
        state.pools.retain(|_, pool| !pool.disposable(budget, now));
    }
    state
        .pools
        .entry(key.to_owned())
        .or_insert_with(|| Pool::new(shared.retry_budget.as_ref()))
}

impl fmt::Debug for Permit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Permit")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Permit {
    /// Record the answer: `Some(wait)` for a `RATE_LIMITED` answer (`wait`
    /// is `None` when it named none), `None` for any other answer. The first
    /// call counts.
    pub(crate) fn settle(&mut self, rate_limited: Option<Option<u64>>) {
        if self.settled {
            return;
        }
        self.settled = true;
        let mut state = lock(&self.shared);
        let Some(pool) = state.pools.get_mut(&self.key) else {
            return;
        };
        let Some(wait) = rate_limited else {
            pool.probing = false;
            return;
        };
        let wait = wait
            .map_or(DEFAULT_PAUSE_MS, |ms| ms as f64)
            .clamp(0.0, MAX_PAUSE_MS);
        let r = (self.random)();
        let r = if r.is_finite() {
            r.clamp(0.0, 1.0)
        } else {
            0.5
        };
        // A little spread keeps clients that share a quota from all resuming
        // at the same instant.
        let spread = (wait * 0.1).min(1000.0) * r;
        let until = Instant::now() + Duration::from_secs_f64((wait + spread) / 1000.0);
        pool.paused_until = Some(
            pool.paused_until
                .map_or(until, |current| current.max(until)),
        );
        pool.probing = true;
        // Waiters recompute how long to sleep: until the pause ends.
        for queue in &pool.queue {
            for waiter in queue {
                waiter.notify.notify_one();
            }
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let shared = Arc::clone(&self.shared);
        let mut state = lock(&shared);
        if let Some(pool) = state.pools.get_mut(&self.key) {
            pool.active[self.kind as usize] -= 1;
        }
        let granted = RequestScheduler::pump(&shared, &mut state, &self.key);
        RequestScheduler::forget(&shared, &mut state, &self.key);
        drop(state);
        for notify in granted {
            notify.notify_one();
        }
    }
}

/// Removes a waiter that gave up (or whose future was dropped) and frees a
/// permit it was granted but never picked up.
struct Withdraw<'a> {
    shared: &'a Arc<Shared>,
    key: &'a str,
    kind: Kind,
    id: u64,
    done: bool,
}

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut state = lock(self.shared);
        if let Some(pool) = state.pools.get_mut(self.key) {
            pool.queue[self.kind as usize].retain(|w| w.id != self.id);
        }
        state.closed_waiters.retain(|id| *id != self.id);
        let permit = state.granted.remove(&self.id);
        let granted = RequestScheduler::pump(self.shared, &mut state, self.key);
        RequestScheduler::forget(self.shared, &mut state, self.key);
        drop(state);
        for notify in granted {
            notify.notify_one();
        }
        drop(permit);
    }
}

fn bounded(value: u32, low: u32, high: u32, name: &str) -> Result<usize, String> {
    if (low..=high).contains(&value) {
        Ok(value as usize)
    } else {
        Err(format!(
            "{name}: expected an integer from {low} to {high}, got {value}"
        ))
    }
}

impl RequestScheduler {
    pub fn new(options: SchedulerOptions) -> Result<Self, String> {
        let max_active = bounded(options.max_active, 1, 10_000, "max_active")?;
        let max_queued = bounded(options.max_queued, 0, 1_000_000, "max_queued")?;
        let max_streams = bounded(
            options.max_streams.unwrap_or(options.max_active),
            1,
            10_000,
            "max_streams",
        )?;
        if options.max_queue > Duration::from_millis(MAX_PAUSE_MS as u64) {
            return Err("max_queue: at most one hour".to_owned());
        }
        if let Some(budget) = &options.retry_budget {
            if !(0.0..=1.0).contains(&budget.ratio) {
                return Err(format!(
                    "retry_budget.ratio: expected a number from 0 to 1, got {}",
                    budget.ratio
                ));
            }
            if !(0.0..=10_000.0).contains(&budget.min_per_second) {
                return Err(format!(
                    "retry_budget.min_per_second: expected a number from 0 to 10000, got {}",
                    budget.min_per_second
                ));
            }
            bounded(budget.burst, 1, 1_000_000, "retry_budget.burst")?;
        }
        Ok(RequestScheduler {
            shared: Arc::new(Shared {
                max_active,
                max_queued,
                max_streams,
                max_queue: options.max_queue,
                scope: options.scope,
                retry_budget: options.retry_budget,
                state: Mutex::new(State::default()),
            }),
        })
    }

    /// The scope of a request to `url` made for `tenant`.
    pub(crate) fn scope_key(&self, url: &str, tenant: Option<&str>) -> String {
        let origin = match self.shared.scope {
            SchedulerScope::Scheduler => String::new(),
            SchedulerScope::Origin => url::Url::parse(url)
                .map(|u| u.origin().ascii_serialization())
                .unwrap_or_default(),
        };
        format!("{}\n{origin}", tenant.unwrap_or(""))
    }

    /// Wait for capacity in `key`; `deadline` also ends the wait. Dropping
    /// the future removes the request from the queue. `retry` marks a repeat
    /// of an earlier attempt, which earns the retry budget nothing.
    pub(crate) async fn acquire(
        &self,
        key: &str,
        kind: Kind,
        deadline: Option<Instant>,
        random: Random,
        retry: bool,
    ) -> Result<Permit, Rejection> {
        let shared = &self.shared;
        let now = Instant::now();
        let mut until = now + shared.max_queue;
        if let Some(deadline) = deadline {
            until = until.min(deadline);
        }
        let budget = until.saturating_duration_since(now);
        let notify = Arc::new(Notify::new());
        let id;
        {
            let mut state = lock(shared);
            if state.closed {
                return Err(Rejection::Closed);
            }
            id = state.next_id;
            state.next_id += 1;
            let pool = pool_mut(shared, &mut state, key);
            let paused = pool.paused_for(now);
            let limit = Self::limit(shared, pool, kind);
            let queue = &pool.queue[kind as usize];
            if paused.is_zero() && queue.is_empty() && pool.active[kind as usize] < limit {
                Self::admit(shared, pool, kind, retry);
                return Ok(Self::permit(shared, key, kind, random));
            }
            let rejection = if paused > budget {
                Some(Rejection::Paused(
                    u64::try_from(paused.as_millis()).unwrap_or(u64::MAX).max(1),
                ))
            } else if queue.len() >= shared.max_queued {
                Some(Rejection::Full)
            } else if budget.is_zero() {
                Some(Rejection::Timeout)
            } else {
                None
            };
            if let Some(rejection) = rejection {
                Self::forget(shared, &mut state, key);
                return Err(rejection);
            }
            pool.queue[kind as usize].push_back(Waiter {
                id,
                retry,
                notify: Arc::clone(&notify),
                random,
            });
        }
        let mut withdraw = Withdraw {
            shared,
            key,
            kind,
            id,
            done: false,
        };
        loop {
            let wait = {
                let mut state = lock(shared);
                let granted = Self::pump(shared, &mut state, key);
                for other in granted {
                    other.notify_one();
                }
                if let Some(permit) = state.granted.remove(&id) {
                    withdraw.done = true;
                    return Ok(permit);
                }
                if state.closed_waiters.contains(&id) {
                    state.closed_waiters.retain(|other| *other != id);
                    withdraw.done = true;
                    return Err(Rejection::Closed);
                }
                let now = Instant::now();
                if now >= until {
                    drop(state);
                    // `withdraw` leaves the queue on drop.
                    return Err(Rejection::Timeout);
                }
                let mut wait = until - now;
                if let Some(pool) = state.pools.get(key) {
                    let paused = pool.paused_for(now);
                    if !paused.is_zero() {
                        wait = wait.min(paused);
                    }
                }
                wait.max(Duration::from_millis(1))
            };
            let _ = tokio::time::timeout(wait, notify.notified()).await;
        }
    }

    /// Take one retry from the shared budget of `key`. False when the budget
    /// is spent: the caller must not retry. Always true without a retry
    /// budget.
    pub(crate) fn spend_retry(&self, key: &str) -> bool {
        let shared = &self.shared;
        let Some(budget) = &shared.retry_budget else {
            return true;
        };
        let mut state = lock(shared);
        let pool = pool_mut(shared, &mut state, key);
        pool.refill(budget, Instant::now());
        if pool.tokens < 1.0 - EPSILON {
            return false;
        }
        pool.tokens = (pool.tokens - 1.0).max(0.0);
        true
    }

    /// Reject every waiting request and every later one; requests in flight
    /// keep their capacity until they finish.
    pub fn close(&self) {
        let mut state = lock(&self.shared);
        state.closed = true;
        let mut woken = Vec::new();
        let mut closed = Vec::new();
        for pool in state.pools.values_mut() {
            for queue in &mut pool.queue {
                for waiter in queue.drain(..) {
                    closed.push(waiter.id);
                    woken.push(waiter.notify);
                }
            }
        }
        state.closed_waiters.extend(closed);
        let keys: Vec<String> = state.pools.keys().cloned().collect();
        for key in keys {
            Self::forget(&self.shared, &mut state, &key);
        }
        drop(state);
        for notify in woken {
            notify.notify_one();
        }
    }

    /// Resolves once no request is in flight or waiting.
    pub async fn drained(&self) {
        loop {
            let stats = self.stats();
            if stats.active + stats.streams + stats.queued == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn stats(&self) -> SchedulerStats {
        let state = lock(&self.shared);
        let mut stats = SchedulerStats {
            scopes: state.pools.len(),
            ..SchedulerStats::default()
        };
        for pool in state.pools.values() {
            stats.active += pool.active[0];
            stats.streams += pool.active[1];
            stats.queued += pool.queue[0].len() + pool.queue[1].len();
        }
        stats
    }

    /// After a rate-limit pause, one request per kind probes the quota until
    /// an answer that is not rate limited arrives.
    fn limit(shared: &Shared, pool: &Pool, kind: Kind) -> usize {
        if pool.probing {
            1
        } else if kind == Kind::Call {
            shared.max_active
        } else {
            shared.max_streams
        }
    }

    /// Count an admitted request; a first attempt earns the retry budget.
    fn admit(shared: &Shared, pool: &mut Pool, kind: Kind, retry: bool) {
        pool.active[kind as usize] += 1;
        if let (Some(budget), false) = (&shared.retry_budget, retry) {
            pool.refill(budget, Instant::now());
            pool.tokens = (pool.tokens + budget.ratio).min(f64::from(budget.burst));
        }
    }

    fn permit(shared: &Arc<Shared>, key: &str, kind: Kind, random: Random) -> Permit {
        Permit {
            shared: Arc::clone(shared),
            key: key.to_owned(),
            kind,
            random,
            settled: false,
        }
    }

    /// Under the lock: admit waiting requests in order while capacity
    /// allows. Returns the waiters to wake once the lock is released.
    fn pump(shared: &Arc<Shared>, state: &mut State, key: &str) -> Vec<Arc<Notify>> {
        let mut woken = Vec::new();
        let mut admitted = Vec::new();
        let Some(pool) = state.pools.get_mut(key) else {
            return woken;
        };
        if !pool.paused_for(Instant::now()).is_zero() {
            return woken;
        }
        for kind in [Kind::Call, Kind::Stream] {
            while !pool.queue[kind as usize].is_empty()
                && pool.active[kind as usize] < Self::limit(shared, pool, kind)
            {
                let Some(waiter) = pool.queue[kind as usize].pop_front() else {
                    break;
                };
                Self::admit(shared, pool, kind, waiter.retry);
                admitted.push((waiter.id, Self::permit(shared, key, kind, waiter.random)));
                woken.push(waiter.notify);
            }
        }
        state.granted.extend(admitted);
        woken
    }

    /// Under the lock: drop an idle scope once its pause has passed and its
    /// retry budget is full again.
    fn forget(shared: &Shared, state: &mut State, key: &str) {
        let budget = shared.retry_budget.as_ref();
        let idle = state
            .pools
            .get_mut(key)
            .is_some_and(|pool| pool.disposable(budget, Instant::now()));
        if idle {
            state.pools.remove(key);
        }
    }
}
