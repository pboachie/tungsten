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
        loop {
            let response = self.call_with(op, args, &once, None, step).await?;
            let done = evaluate_predicate(until, response.value.as_ref());
            let out_of_time = !done && started.elapsed().saturating_add(interval) > budget;
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
            tokio::time::sleep(interval).await;
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
