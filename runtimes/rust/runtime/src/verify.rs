// SPDX-License-Identifier: Apache-2.0
//! Polling and verification hooks.

use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::client::{ClientCore, Polled};
use crate::expr::{evaluate_expr, evaluate_predicate, resolve_ref};
use crate::helpers::{arg_name, with_wire_names};
use crate::prepare::MacroClaim;
use crate::types::{CallOptions, OperationDescriptor, Predicate, Response, Result, Verification};
use crate::util::redact_paths;

const DEFAULT_POLL_INTERVAL_MS: f64 = 1000.0;
const DEFAULT_VERIFY_BUDGET_MS: f64 = 30_000.0;

// A private future-scoped context, so public CallOptions struct literals stay
// compatible. Borrowed preparation/transport futures share the deadline;
// unrelated tasks and independently owned refreshes do not inherit it.
tokio::task_local! {
    static POLL_DEADLINE: Option<Instant>;
}

// The total deadline of one logical call (`deadline` in `CallOptions` or
// `ClientOptions`), fixed by the public entry point; nested pages, polls,
// verification and macro steps inherit it and cannot restart it.
tokio::task_local! {
    static CALL_DEADLINE: Option<Instant>;
}

pub(crate) fn poll_deadline() -> Option<Instant> {
    POLL_DEADLINE.try_with(|deadline| *deadline).ok().flatten()
}

pub(crate) fn call_deadline() -> Option<Instant> {
    CALL_DEADLINE.try_with(|deadline| *deadline).ok().flatten()
}

/// The earliest deadline bounding the current work: the call's total
/// deadline or a polling budget.
pub(crate) fn effective_deadline() -> Option<Instant> {
    match (call_deadline(), poll_deadline()) {
        (Some(call), Some(poll)) => Some(call.min(poll)),
        (call, poll) => call.or(poll),
    }
}

/// Whether the call's total deadline has passed.
pub(crate) fn call_deadline_expired() -> bool {
    call_deadline().is_some_and(|deadline| Instant::now() >= deadline)
}

pub(crate) fn deadline_exceeded(operation: &str, during: &str) -> crate::types::Error {
    crate::prepare::fail(
        crate::envelope::Diag::new(operation, crate::types::Category::TransportFailed)
            .code(Some("DEADLINE_EXCEEDED".to_owned()))
            .retryable(crate::types::Retryable::Never)
            .remediation(format!(
                "The operation deadline (`deadline`) expired {during}, so the request was not sent. Call again with a larger `deadline`."
            ))
            .build(),
    )
}

/// Run `work` under the total deadline `opts.deadline` (else the client's
/// default), measured from now; an enclosing call's deadline is never
/// extended.
pub(crate) async fn with_call_deadline<F: std::future::Future>(
    core: &ClientCore,
    opts: &CallOptions,
    work: F,
) -> F::Output {
    with_deadline_at(resolve_deadline(core, opts), work).await
}

/// The absolute deadline a call starting now gets from `opts.deadline`
/// (else the client's default), never later than an enclosing call's.
pub(crate) fn resolve_deadline(core: &ClientCore, opts: &CallOptions) -> Option<Instant> {
    let Some(budget) = opts
        .deadline
        .or(core.inner.deadline)
        .filter(|d| !d.is_zero())
    else {
        return call_deadline();
    };
    let now = Instant::now();
    let at = now
        .checked_add(budget)
        .unwrap_or_else(|| now + Duration::from_secs(86_400 * 365));
    Some(call_deadline().map_or(at, |outer| outer.min(at)))
}

/// Run `work` under the absolute call deadline `at` (none: unbounded).
pub(crate) async fn with_deadline_at<F: std::future::Future>(
    at: Option<Instant>,
    work: F,
) -> F::Output {
    match at {
        Some(_) => CALL_DEADLINE.scope(at, work).await,
        None => work.await,
    }
}

/// Why [`detached`] returned without its work's result.
pub(crate) enum Abandoned {
    /// The call's deadline (or polling budget) passed first.
    Expired,
    /// The task ended without a result (it panicked, or the runtime stopped).
    Stopped,
}

/// Run `work` as its own task and wait for it only until the current
/// deadline. The task inherits no call deadline, and neither that deadline
/// nor dropping the waiting future cancels it: a token refresh shared with
/// other callers runs on, bounded by its own timeout. Without a Tokio
/// runtime it runs inline.
pub(crate) async fn detached<T, F>(work: F) -> std::result::Result<T, Abandoned>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return Ok(work.await);
    };
    let deadline = effective_deadline();
    if deadline.is_some_and(|at| Instant::now() >= at) {
        return Err(Abandoned::Expired);
    }
    let task = runtime.spawn(work);
    let joined = match deadline {
        Some(at) => match tokio::time::timeout_at(at.into(), task).await {
            Ok(joined) => joined,
            Err(_) => return Err(Abandoned::Expired),
        },
        None => task.await,
    };
    joined.map_err(|_| Abandoned::Stopped)
}

pub(crate) fn check_poll_deadline(operation: &str) -> crate::prepare::Step<()> {
    if call_deadline_expired() {
        return Err(deadline_exceeded(
            operation,
            "before the request could be sent",
        ));
    }
    if poll_deadline().is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(crate::prepare::fail(
            crate::envelope::Diag::new(operation, crate::types::Category::TransportFailed)
                .retryable(crate::types::Retryable::Never)
                .remediation("The polling budget expired before the request was sent.".to_owned())
                .build(),
        ));
    }
    Ok(())
}

/// Stands in for a verification reference that did not resolve; equal to
/// nothing a response can hold.
fn unresolved() -> Value {
    let mut map = Map::new();
    map.insert("$tungsten_unresolved".to_owned(), Value::Bool(true));
    Value::Object(map)
}

/// `node` with every `$` reference resolved against `scope`; a reference
/// that does not resolve never matches (it is not dropped, which would make
/// the predicate hold vacuously).
fn resolve_refs(node: &Value, scope: &Map<String, Value>, depth: usize) -> Value {
    if depth > 64 {
        return unresolved();
    }
    match node {
        Value::String(text) if text.starts_with('$') => {
            match resolve_ref(text, scope).filter(|v| !v.is_null()) {
                Some(found) => found,
                None => unresolved(),
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| resolve_refs(i, scope, depth + 1))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), resolve_refs(v, scope, depth + 1)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// When a poll succeeds and how long it may take.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PollSpec<'a> {
    pub until: &'a Predicate,
    pub interval: Duration,
    pub budget: Duration,
}

fn millis(ms: f64) -> Duration {
    crate::util::duration_from_ms(ms)
}

impl ClientCore {
    pub(crate) async fn poll_with(
        &self,
        op: &OperationDescriptor,
        args: &Value,
        spec: &PollSpec<'_>,
        opts: &CallOptions,
        step: Option<&MacroClaim>,
    ) -> Result<Polled> {
        let PollSpec {
            until,
            interval,
            budget,
        } = *spec;
        let started = Instant::now();
        let mut once = opts.clone();
        once.verify = false;
        let mut last: Option<Response<Polled>> = None;
        loop {
            let call = self.call_with(op, args, &once, None, step);
            let called = if budget.is_zero() {
                call.await
            } else {
                match tokio::time::timeout(
                    budget.saturating_sub(started.elapsed()),
                    POLL_DEADLINE.scope(started.checked_add(budget), call),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        if !crate::classify::is_mutation(op)
                            && let Some(mut response) = last.take()
                        {
                            response.value.timed_out = true;
                            return Ok(response);
                        }
                        return Err(crate::prepare::fail(
                            crate::envelope::Diag::new(
                                op.id.clone(),
                                if crate::classify::is_mutation(op) {
                                    crate::types::Category::OutcomeUnknown
                                } else {
                                    crate::types::Category::TransportFailed
                                },
                            )
                            .remediation("The polling budget expired during the call.".to_owned())
                            .build(),
                        ));
                    }
                }
            };
            if called.is_err()
                && !budget.is_zero()
                && started.elapsed() >= budget
                && !crate::classify::is_mutation(op)
                && let Some(mut response) = last.take()
            {
                response.value.timed_out = true;
                return Ok(response);
            }
            if called.is_err()
                && !budget.is_zero()
                && started.elapsed() >= budget
                && !crate::classify::is_mutation(op)
            {
                return Err(crate::prepare::fail(
                    crate::envelope::Diag::new(
                        op.id.clone(),
                        crate::types::Category::TransportFailed,
                    )
                    .remediation(
                        "The polling budget expired before any observation completed.".to_owned(),
                    )
                    .build(),
                ));
            }
            let response = called?;
            let done = evaluate_predicate(until, response.value.as_ref());
            let out_of_time = (!budget.is_zero() && started.elapsed() >= budget)
                || (!done
                    && (budget.is_zero() || started.elapsed().saturating_add(interval) > budget));
            if done || out_of_time {
                return Ok(Response {
                    value: Polled {
                        body: response.value,
                        timed_out: out_of_time,
                    },
                    meta: response.meta,
                    verification: None,
                });
            }
            last = Some(Response {
                value: Polled {
                    body: response.value,
                    timed_out: false,
                },
                meta: response.meta,
                verification: None,
            });
            tokio::time::sleep(interval).await;
            if !budget.is_zero() && started.elapsed() >= budget {
                let mut response = last.take().expect("a completed observation");
                response.value.timed_out = true;
                return Ok(response);
            }
        }
    }

    pub(crate) async fn verify(
        &self,
        op: &OperationDescriptor,
        args: &Map<String, Value>,
        value: Option<&Value>,
        opts: &CallOptions,
    ) -> Verification {
        let unchecked = Verification {
            checked: false,
            passed: false,
            observed: None,
            timed_out: false,
            error: None,
        };
        let Some(hook) = &op.agent.verify else {
            return unchecked;
        };
        let Some(target) = self.operation(&hook.operation) else {
            return unchecked;
        };
        // The hook is written against the API: argument keys and `$args`
        // references use wire names, and predicates may reference the call.
        let mut scope = Map::new();
        if let Some(value) = value {
            scope.insert("response".to_owned(), value.clone());
        }
        scope.insert("args".to_owned(), Value::Object(with_wire_names(op, args)));
        let hook_args = if hook.args.is_null() {
            Value::Object(Map::new())
        } else {
            hook.args.clone()
        };
        let Some(Value::Object(evaluated)) = evaluate_expr(&hook_args, &scope, 0) else {
            return unchecked;
        };
        let mut mapped = Map::new();
        for (key, arg) in evaluated {
            mapped.insert(arg_name(&target, &key), arg);
        }
        let resolve = |predicate: &Value| match predicate {
            Value::Object(map) if !map.is_empty() => Some(resolve_refs(predicate, &scope, 0)),
            _ => None,
        };
        let terminal = resolve(&hook.terminal);
        let expect = resolve(&hook.expect).unwrap_or_else(|| Value::Object(Map::new()));
        let interval = hook
            .poll_interval
            .unwrap_or_else(|| millis(DEFAULT_POLL_INTERVAL_MS));
        let budget = hook.poll_budget.unwrap_or_else(|| {
            millis(if terminal.is_some() {
                DEFAULT_VERIFY_BUDGET_MS
            } else {
                0.0
            })
        });
        let mut rest = opts.clone();
        rest.confirm = None;
        rest.idempotency_key = None;
        rest.verify = false;
        let polled = self
            .poll_with(
                &target,
                &Value::Object(mapped),
                &PollSpec {
                    until: terminal.as_ref().unwrap_or(&expect),
                    interval,
                    budget,
                },
                &rest,
                None,
            )
            .await;
        match polled {
            Err(error) => Verification {
                error: Some(error.diagnostic),
                ..unchecked
            },
            Ok(response) => {
                let body = response.value.body;
                Verification {
                    checked: true,
                    passed: evaluate_predicate(&expect, body.as_ref()),
                    observed: body
                        .as_ref()
                        .map(|b| redact_paths(b, &target.agent.sensitive_response_fields)),
                    timed_out: !budget.is_zero() && response.value.timed_out,
                    error: None,
                }
            }
        }
    }
}
