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
/// Scopes whose retry budget is remembered; the least recently used is
/// forgotten (and starts again from the reserve) beyond this.
const MAX_BUDGETS: usize = 1024;

/// How requests are grouped into scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchedulerScope {
    /// One scope per tenant and request origin.
    #[default]
    Origin,
    /// One scope per tenant, whatever the origin.
    Scheduler,
}

/// Limits of a [`RequestScheduler`]. Every limit applies per scope, except
/// the optional `max_active_total` and `max_streams_total`.
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
    /// Retries per call a scope may spend over time, shared by every caller
    /// of the scope (0 to 1). Each call adds this much to the scope's retry
    /// budget and each retry spends 1; a retry the budget cannot pay for is
    /// not made and the call returns its last failure. `None` (default):
    /// retries are not budgeted.
    pub retry_ratio: Option<f64>,
    /// Retries a scope's budget holds before any call added to it, and the
    /// base of its cap (`retry_reserve + 100 × retry_ratio`). Default 10.
    pub retry_reserve: u32,
    /// Ordinary requests in flight at once across every scope, on top of each
    /// scope's `max_active`. Scopes waiting for this capacity take turns, one
    /// request each, so a busy scope cannot starve a quiet one. `None`
    /// (default): no cap across scopes. 1 to 1000000.
    pub max_active_total: Option<u32>,
    /// Event streams open at once across every scope, on top of each scope's
    /// `max_streams`. `None` (default): no cap across scopes.
    pub max_streams_total: Option<u32>,
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
            retry_ratio: None,
            retry_reserve: 10,
            max_active_total: None,
            max_streams_total: None,
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

/// Balance and last use of each scope's retry budget, and the use clock.
type Budgets = (HashMap<String, (f64, u64)>, u64);

struct Waiter {
    id: u64,
    notify: Arc<Notify>,
    random: Random,
}

#[derive(Default)]
struct Pool {
    active: [usize; 2],
    queue: [VecDeque<Waiter>; 2],
    paused_until: Option<Instant>,
    probing: bool,
}

impl Pool {
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
    /// Requests in flight across every scope, per kind.
    total: [usize; 2],
    /// The scope served last under a cap across scopes, per kind.
    served: [Option<String>; 2],
}

struct Shared {
    max_active: usize,
    max_queued: usize,
    max_streams: usize,
    max_queue: Duration,
    scope: SchedulerScope,
    retry_ratio: Option<f64>,
    retry_reserve: f64,
    /// Caps across every scope, per kind.
    max_total: [Option<usize>; 2],
    state: Mutex<State>,
    /// Retry budget balance and last use of each scope.
    budgets: Mutex<Budgets>,
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
        state.total[self.kind as usize] -= 1;
        let granted = RequestScheduler::pump(&shared, &mut state, &self.key);
        RequestScheduler::forget(&mut state, &self.key);
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
        RequestScheduler::forget(&mut state, self.key);
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
        if let Some(ratio) = options.retry_ratio
            && !(ratio.is_finite() && (0.0..=1.0).contains(&ratio))
        {
            return Err(format!(
                "retry_ratio must be a number from 0 to 1, got {ratio}"
            ));
        }
        let retry_reserve = bounded(options.retry_reserve, 0, 10_000, "retry_reserve")?;
        let max_active_total = options
            .max_active_total
            .map(|cap| bounded(cap, 1, 1_000_000, "max_active_total"))
            .transpose()?;
        let max_streams_total = options
            .max_streams_total
            .map(|cap| bounded(cap, 1, 1_000_000, "max_streams_total"))
            .transpose()?;
        Ok(RequestScheduler {
            shared: Arc::new(Shared {
                max_active,
                max_queued,
                max_streams,
                max_queue: options.max_queue,
                scope: options.scope,
                retry_ratio: options.retry_ratio,
                retry_reserve: retry_reserve as f64,
                max_total: [max_active_total, max_streams_total],
                state: Mutex::new(State::default()),
                budgets: Mutex::new((HashMap::new(), 0)),
            }),
        })
    }

    /// Add one call's share to the retry budget of `key`.
    pub(crate) fn record_call(&self, key: &str) {
        let Some(ratio) = self.shared.retry_ratio else {
            return;
        };
        let cap = self.shared.retry_reserve + 100.0 * ratio;
        self.update_budget(key, |balance| Some((balance + ratio).min(cap)));
    }

    /// Spend one retry from the budget of `key`; false when it cannot pay.
    pub(crate) fn allow_retry(&self, key: &str) -> bool {
        if self.shared.retry_ratio.is_none() {
            return true;
        }
        self.update_budget(key, |balance| (balance >= 1.0).then_some(balance - 1.0))
    }

    /// Apply `change` to the balance of `key`; false when it declined.
    fn update_budget(&self, key: &str, change: impl FnOnce(f64) -> Option<f64>) -> bool {
        let mut guard = self
            .shared
            .budgets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (budgets, clock) = &mut *guard;
        let balance = budgets
            .get(key)
            .map_or(self.shared.retry_reserve, |(balance, _)| *balance);
        let Some(next) = change(balance) else {
            return false;
        };
        *clock += 1;
        budgets.insert(key.to_owned(), (next, *clock));
        if budgets.len() > MAX_BUDGETS
            && let Some(oldest) = budgets
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
        {
            budgets.remove(&oldest);
        }
        true
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
    /// the future removes the request from the queue.
    pub(crate) async fn acquire(
        &self,
        key: &str,
        kind: Kind,
        deadline: Option<Instant>,
        random: Random,
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
            let room = Self::room(shared, &state, kind);
            let pool = state.pools.entry(key.to_owned()).or_default();
            let paused = pool.paused_for(now);
            let limit = Self::limit(shared, pool, kind);
            let queue = &pool.queue[kind as usize];
            if paused.is_zero() && queue.is_empty() && pool.active[kind as usize] < limit && room {
                pool.active[kind as usize] += 1;
                Self::count(shared, &mut state, key, kind);
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
                Self::forget(&mut state, key);
                return Err(rejection);
            }
            pool.queue[kind as usize].push_back(Waiter {
                id,
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
            Self::forget(&mut state, &key);
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
        if shared.max_total.iter().any(Option::is_some) {
            return Self::pump_all(shared, state);
        }
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
                pool.active[kind as usize] += 1;
                admitted.push((waiter.id, Self::permit(shared, key, kind, waiter.random)));
                woken.push(waiter.notify);
            }
        }
        for (_, permit) in &admitted {
            state.total[permit.kind as usize] += 1;
        }
        state.granted.extend(admitted);
        woken
    }

    /// Whether the cap across scopes, if any, leaves room for one more.
    fn room(shared: &Shared, state: &State, kind: Kind) -> bool {
        shared.max_total[kind as usize].is_none_or(|cap| state.total[kind as usize] < cap)
    }

    /// Under the lock: count a request admitted in `key`.
    fn count(shared: &Shared, state: &mut State, key: &str, kind: Kind) {
        state.total[kind as usize] += 1;
        if shared.max_total[kind as usize].is_some() {
            state.served[kind as usize] = Some(key.to_owned());
        }
    }

    /// Under the lock, with a cap across scopes: admit waiting requests while
    /// capacity allows, one per scope in turn (scopes in key order, starting
    /// after the one served last), each scope's own queue first in, first
    /// out. Returns the waiters to wake once the lock is released.
    fn pump_all(shared: &Arc<Shared>, state: &mut State) -> Vec<Arc<Notify>> {
        let mut woken = Vec::new();
        let now = Instant::now();
        for kind in [Kind::Call, Kind::Stream] {
            let mut keys: Vec<String> = state
                .pools
                .iter()
                .filter(|(_, pool)| {
                    !pool.queue[kind as usize].is_empty() && pool.paused_for(now).is_zero()
                })
                .map(|(key, _)| key.clone())
                .collect();
            if keys.is_empty() {
                continue;
            }
            keys.sort();
            let start = state.served[kind as usize]
                .as_ref()
                .and_then(|last| keys.iter().position(|key| key > last))
                .unwrap_or(0);
            keys.rotate_left(start);
            let mut progress = true;
            while progress && Self::room(shared, state, kind) {
                progress = false;
                for key in &keys {
                    if !Self::room(shared, state, kind) {
                        break;
                    }
                    let Some(pool) = state.pools.get_mut(key) else {
                        continue;
                    };
                    if pool.active[kind as usize] >= Self::limit(shared, pool, kind) {
                        continue;
                    }
                    let Some(waiter) = pool.queue[kind as usize].pop_front() else {
                        continue;
                    };
                    pool.active[kind as usize] += 1;
                    Self::count(shared, state, key, kind);
                    let permit = Self::permit(shared, key, kind, waiter.random);
                    state.granted.insert(waiter.id, permit);
                    woken.push(waiter.notify);
                    progress = true;
                }
            }
        }
        woken
    }

    /// Under the lock: drop an idle scope once its pause has passed.
    fn forget(state: &mut State, key: &str) {
        let idle = state
            .pools
            .get(key)
            .is_some_and(|pool| pool.busy() == 0 && pool.paused_for(Instant::now()).is_zero());
        if idle {
            state.pools.remove(key);
        }
    }
}
