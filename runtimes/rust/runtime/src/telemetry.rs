// SPDX-License-Identifier: Apache-2.0
//! Optional, local telemetry for calls made through [`crate::ClientCore`].
//!
//! Off by default: nothing is recorded or sent unless
//! [`crate::ClientOptions::telemetry`] names an observer. Events describe one
//! logical operation, each attempt it sends and the phases around them
//! (authentication, scheduler queueing, retry backoff). They carry only
//! bounded values: the API name and operation id from the compiled
//! descriptor, enumerated phases, outcomes, categories and reasons, HTTP
//! status codes, attempt numbers and durations. URLs, headers, bodies,
//! argument values, credentials, confirmation tokens, tenant labels and error
//! text are never part of an event, so there is nothing to redact.
//!
//! The TypeScript, Python and Rust runtimes emit the same events with the
//! same field names and meanings; [`TelemetryEvent`] serializes to the same
//! JSON object.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use crate::types::{Category, Diagnostic};

/// Version of the event contract; incremented for incompatible changes.
pub const TELEMETRY_SCHEMA_VERSION: u32 = 1;

/// What an event marks. `phase` events are emitted when a phase ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum TelemetryEventName {
    #[serde(rename = "operation.start")]
    OperationStart,
    #[serde(rename = "operation.end")]
    OperationEnd,
    #[serde(rename = "attempt.start")]
    AttemptStart,
    #[serde(rename = "attempt.end")]
    AttemptEnd,
    #[serde(rename = "phase")]
    Phase,
}

/// `auth`: resolving credentials (including a token fetch or refresh).
/// `queue`: waiting for request scheduler capacity. `retry_backoff`: the
/// wait before a permitted retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryPhase {
    Auth,
    Queue,
    RetryBackoff,
}

/// How an operation, attempt or phase ended. `OutcomeUnknown`: a mutation
/// may have taken effect (its category is `OUTCOME_UNKNOWN`), which takes
/// precedence over `DeadlineExceeded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryOutcome {
    Ok,
    Error,
    Cancelled,
    DeadlineExceeded,
    OutcomeUnknown,
}

/// Why a `queue` phase ended: `admitted`, the scheduler's rejection (`full`,
/// `timeout`, `paused`, `closed`), the call's deadline, or cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryQueueReason {
    Admitted,
    Full,
    Timeout,
    Paused,
    Closed,
    Deadline,
    Cancelled,
}

/// The public entry point that started the operation. One `pages`
/// operation covers every page it fetched, one `poll` operation every read
/// it made; their attempts are numbered across the whole operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryEntry {
    Call,
    Pages,
    Poll,
    Preview,
}

/// One telemetry event. Fields that do not apply to an event are `None`
/// (`null` in JSON).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TelemetryEvent {
    pub schema_version: u32,
    pub name: TelemetryEventName,
    /// The API name of the compiled descriptor.
    pub api: String,
    /// The operation id of the compiled descriptor.
    pub operation: String,
    pub entry: TelemetryEntry,
    /// Correlates the events of one operation; increases per client from 1.
    /// An identifier, not a label: do not use it as a metric dimension.
    pub call_id: u64,
    /// Whether the operation may change server state.
    pub mutation: bool,
    /// The attempt number within the operation (from 1), for attempt events
    /// and for the `queue` and `retry_backoff` phases.
    pub attempt: Option<u32>,
    pub phase: Option<TelemetryPhase>,
    /// Milliseconds, for `.end` and `phase` events.
    pub duration_ms: Option<f64>,
    pub outcome: Option<TelemetryOutcome>,
    /// The diagnostic category of a failed operation, attempt or phase.
    pub category: Option<Category>,
    /// The HTTP status of an attempt that received a response, and of a
    /// `call` operation's final answer.
    pub http_status: Option<u16>,
    /// For a `queue` phase.
    pub reason: Option<TelemetryQueueReason>,
    /// Attempts sent, on `operation.end`.
    pub attempts: Option<u32>,
}

/// Receives telemetry events. It is called synchronously on the request
/// path: keep it fast and hand slow work elsewhere (a bounded buffer such as
/// [`MemoryTelemetryCollector`]). An observer that panics never changes the
/// call. Implemented for `Fn(&TelemetryEvent) + Send + Sync` closures.
pub trait TelemetryObserver: Send + Sync {
    fn on_event(&self, event: &TelemetryEvent);
}

impl<F: Fn(&TelemetryEvent) + Send + Sync> TelemetryObserver for F {
    fn on_event(&self, event: &TelemetryEvent) {
        self(event)
    }
}

const DEFAULT_CAPACITY: usize = 1024;
const MAX_CAPACITY: usize = 65536;

/// An observer keeping the most recent events in memory, for tests,
/// debugging and exporters that drain it periodically. Holds at most
/// `capacity` events (default 1024, at most 65536); when full, the oldest
/// event is dropped and counted in [`MemoryTelemetryCollector::dropped`].
pub struct MemoryTelemetryCollector {
    capacity: usize,
    events: Mutex<VecDeque<TelemetryEvent>>,
    dropped: AtomicU64,
}

impl MemoryTelemetryCollector {
    /// A collector holding at most `capacity` events (clamped to 1..=65536).
    pub fn new(capacity: usize) -> Self {
        MemoryTelemetryCollector {
            capacity: capacity.clamp(1, MAX_CAPACITY),
            events: Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The buffered events, oldest first.
    pub fn events(&self) -> Vec<TelemetryEvent> {
        crate::idempotency::lock(&self.events)
            .iter()
            .cloned()
            .collect()
    }

    /// Events dropped because the buffer was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Remove and return the buffered events, oldest first.
    pub fn drain(&self) -> Vec<TelemetryEvent> {
        crate::idempotency::lock(&self.events).drain(..).collect()
    }

    /// Drop the buffered events and reset the drop count.
    pub fn clear(&self) {
        crate::idempotency::lock(&self.events).clear();
        self.dropped.store(0, Ordering::Relaxed);
    }
}

impl Default for MemoryTelemetryCollector {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl fmt::Debug for MemoryTelemetryCollector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryTelemetryCollector")
            .field("capacity", &self.capacity)
            .field("dropped", &self.dropped())
            .finish_non_exhaustive()
    }
}

impl TelemetryObserver for MemoryTelemetryCollector {
    fn on_event(&self, event: &TelemetryEvent) {
        let mut events = crate::idempotency::lock(&self.events);
        if events.len() >= self.capacity {
            events.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        events.push_back(event.clone());
    }
}

/// How a result ended, for `outcome`.
pub(crate) fn outcome_of(error: Option<&Diagnostic>, cancelled: bool) -> TelemetryOutcome {
    let Some(error) = error else {
        return TelemetryOutcome::Ok;
    };
    if error.category == Category::OutcomeUnknown {
        TelemetryOutcome::OutcomeUnknown
    } else if cancelled {
        TelemetryOutcome::Cancelled
    } else if error.code.as_deref() == Some("DEADLINE_EXCEEDED") {
        TelemetryOutcome::DeadlineExceeded
    } else {
        TelemetryOutcome::Error
    }
}

/// The category of a failure; none for a cancelled one.
fn category_of(error: Option<&Diagnostic>, outcome: TelemetryOutcome) -> Option<Category> {
    error
        .filter(|_| outcome != TelemetryOutcome::Cancelled)
        .map(|error| error.category)
}

fn status_of(status: Option<u16>) -> Option<u16> {
    status.filter(|status| (100..=599).contains(status))
}

fn millis(start: Instant) -> f64 {
    (start.elapsed().as_secs_f64() * 1_000_000.0).round() / 1000.0
}

/// Per-client telemetry state: the observer and the call id counter.
pub(crate) struct Telemetry {
    observer: Arc<dyn TelemetryObserver>,
    next: AtomicU64,
}

impl Telemetry {
    pub(crate) fn new(observer: Arc<dyn TelemetryObserver>) -> Self {
        Telemetry {
            observer,
            next: AtomicU64::new(0),
        }
    }

    /// Start an operation and emit `operation.start`.
    pub(crate) fn start(
        self: &Arc<Self>,
        api: &str,
        operation: &str,
        entry: TelemetryEntry,
        mutation: bool,
    ) -> Arc<Span> {
        let span = Arc::new(Span {
            telemetry: self.clone(),
            api: api.to_owned(),
            operation: operation.to_owned(),
            entry,
            call_id: self.next.fetch_add(1, Ordering::Relaxed) + 1,
            mutation,
            start: Instant::now(),
            attempts: AtomicU64::new(0),
            ended: std::sync::atomic::AtomicBool::new(false),
        });
        span.emit(TelemetryEventName::OperationStart, Fields::default());
        span
    }

    fn deliver(&self, event: &TelemetryEvent) {
        // A failing observer never changes the call.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.observer.on_event(event)
        }));
    }
}

#[derive(Default)]
pub(crate) struct Fields {
    pub attempt: Option<u32>,
    pub phase: Option<TelemetryPhase>,
    pub duration_ms: Option<f64>,
    pub outcome: Option<TelemetryOutcome>,
    pub category: Option<Category>,
    pub http_status: Option<u16>,
    pub reason: Option<TelemetryQueueReason>,
    pub attempts: Option<u32>,
}

/// The events of one logical operation.
pub(crate) struct Span {
    telemetry: Arc<Telemetry>,
    api: String,
    operation: String,
    entry: TelemetryEntry,
    call_id: u64,
    mutation: bool,
    start: Instant,
    attempts: AtomicU64,
    ended: std::sync::atomic::AtomicBool,
}

impl Span {
    fn emit(&self, name: TelemetryEventName, fields: Fields) {
        self.telemetry.deliver(&TelemetryEvent {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            name,
            api: self.api.clone(),
            operation: self.operation.clone(),
            entry: self.entry,
            call_id: self.call_id,
            mutation: self.mutation,
            attempt: fields.attempt,
            phase: fields.phase,
            duration_ms: fields.duration_ms,
            outcome: fields.outcome,
            category: fields.category,
            http_status: fields.http_status,
            reason: fields.reason,
            attempts: fields.attempts,
        });
    }

    /// The attempt number most recently started.
    pub(crate) fn attempt(&self) -> u32 {
        u32::try_from(self.attempts.load(Ordering::Relaxed)).unwrap_or(u32::MAX)
    }

    /// The number of the attempt about to be sent, counted across the whole
    /// operation (an OAuth re-send after a rejected token continues it).
    pub(crate) fn next_attempt(&self) -> u32 {
        u32::try_from(self.attempts.fetch_add(1, Ordering::Relaxed) + 1).unwrap_or(u32::MAX)
    }

    pub(crate) fn phase(
        &self,
        phase: TelemetryPhase,
        start: Instant,
        error: Option<&Diagnostic>,
        cancelled: bool,
        attempt: Option<u32>,
        reason: Option<TelemetryQueueReason>,
    ) {
        let outcome = outcome_of(error, cancelled);
        self.emit(
            TelemetryEventName::Phase,
            Fields {
                attempt,
                phase: Some(phase),
                duration_ms: Some(millis(start)),
                outcome: Some(outcome),
                category: category_of(error, outcome),
                reason,
                ..Fields::default()
            },
        );
    }

    /// A phase that ended without a diagnostic of its own (`outcome` given).
    pub(crate) fn phase_outcome(
        &self,
        phase: TelemetryPhase,
        start: Instant,
        outcome: TelemetryOutcome,
        attempt: Option<u32>,
        reason: Option<TelemetryQueueReason>,
    ) {
        self.emit(
            TelemetryEventName::Phase,
            Fields {
                attempt,
                phase: Some(phase),
                duration_ms: Some(millis(start)),
                outcome: Some(outcome),
                reason,
                ..Fields::default()
            },
        );
    }

    pub(crate) fn attempt_start(&self, attempt: u32) -> Instant {
        self.emit(
            TelemetryEventName::AttemptStart,
            Fields {
                attempt: Some(attempt),
                ..Fields::default()
            },
        );
        Instant::now()
    }

    pub(crate) fn attempt_end(
        &self,
        attempt: u32,
        start: Instant,
        error: Option<&Diagnostic>,
        cancelled: bool,
        status: Option<u16>,
    ) {
        let outcome = outcome_of(error, cancelled);
        self.emit(
            TelemetryEventName::AttemptEnd,
            Fields {
                attempt: Some(attempt),
                duration_ms: Some(millis(start)),
                outcome: Some(outcome),
                category: category_of(error, outcome),
                http_status: status_of(status),
                ..Fields::default()
            },
        );
    }

    /// Emit `operation.end` once.
    pub(crate) fn end(&self, error: Option<&Diagnostic>, cancelled: bool, status: Option<u16>) {
        if self.ended.swap(true, Ordering::Relaxed) {
            return;
        }
        let outcome = if error.is_none() && cancelled {
            TelemetryOutcome::Cancelled
        } else {
            outcome_of(error, cancelled)
        };
        self.emit(
            TelemetryEventName::OperationEnd,
            Fields {
                duration_ms: Some(millis(self.start)),
                outcome: Some(outcome),
                category: category_of(error, outcome),
                http_status: status_of(status),
                attempts: Some(self.attempt()),
                ..Fields::default()
            },
        );
    }
}

/// Reports a phase, attempt or operation as cancelled when the future
/// running it is dropped before it is [`Pending::done`].
pub(crate) struct Pending {
    span: Option<Arc<Span>>,
    kind: PendingKind,
    start: Instant,
}

#[derive(Clone, Copy)]
pub(crate) enum PendingKind {
    Operation,
    Attempt(u32),
    Phase(TelemetryPhase, Option<u32>),
}

impl Pending {
    pub(crate) fn new(span: Option<&Arc<Span>>, kind: PendingKind) -> Self {
        Pending {
            span: span.cloned(),
            kind,
            start: Instant::now(),
        }
    }

    /// The work finished; nothing is reported on drop.
    pub(crate) fn done(mut self) -> Instant {
        self.span = None;
        self.start
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        let Some(span) = self.span.take() else {
            return;
        };
        // Interrupted: by the call's deadline, else by cancellation (the
        // future was dropped).
        let expired = crate::verify::call_deadline_expired();
        let outcome = if expired {
            TelemetryOutcome::DeadlineExceeded
        } else {
            TelemetryOutcome::Cancelled
        };
        match self.kind {
            PendingKind::Operation => {
                if !span.ended.swap(true, Ordering::Relaxed) {
                    span.emit(
                        TelemetryEventName::OperationEnd,
                        Fields {
                            duration_ms: Some(millis(span.start)),
                            outcome: Some(outcome),
                            attempts: Some(span.attempt()),
                            ..Fields::default()
                        },
                    );
                }
            }
            PendingKind::Attempt(attempt) => span.emit(
                TelemetryEventName::AttemptEnd,
                Fields {
                    attempt: Some(attempt),
                    duration_ms: Some(millis(self.start)),
                    outcome: Some(outcome),
                    ..Fields::default()
                },
            ),
            PendingKind::Phase(phase, attempt) => span.phase_outcome(
                phase,
                self.start,
                outcome,
                attempt,
                (phase == TelemetryPhase::Queue).then_some(if expired {
                    TelemetryQueueReason::Deadline
                } else {
                    TelemetryQueueReason::Cancelled
                }),
            ),
        }
    }
}

// The span of the operation a public entry point started; only work
// belonging to that operation reads it.
tokio::task_local! {
    static SPAN: Option<Arc<Span>>;
}

/// The span of the current operation, if telemetry is on.
pub(crate) fn current() -> Option<Arc<Span>> {
    SPAN.try_with(|span| span.clone()).ok().flatten()
}

/// Run `work` as part of `span` (none: outside any operation).
pub(crate) async fn scoped<F: std::future::Future>(span: Option<Arc<Span>>, work: F) -> F::Output {
    SPAN.scope(span, work).await
}

/// The `reason` of a `queue` phase: why admission ended.
pub(crate) fn queue_reason(refused: Option<&Diagnostic>) -> TelemetryQueueReason {
    match refused.and_then(|d| d.code.as_deref()) {
        None if refused.is_none() => TelemetryQueueReason::Admitted,
        Some("QUEUE_FULL") => TelemetryQueueReason::Full,
        Some("QUEUE_TIMEOUT") => TelemetryQueueReason::Timeout,
        Some("RATE_LIMIT_PAUSED") => TelemetryQueueReason::Paused,
        Some("SCHEDULER_CLOSED") => TelemetryQueueReason::Closed,
        Some("DEADLINE_EXCEEDED") => TelemetryQueueReason::Deadline,
        _ => TelemetryQueueReason::Cancelled,
    }
}
