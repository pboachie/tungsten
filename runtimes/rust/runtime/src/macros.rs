// SPDX-License-Identifier: Apache-2.0
//! Compiled macros (canonical form of `tungsten_ir::Macro`): preview and run.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Map, Value};

use crate::classify::is_mutation;
use crate::client::ClientCore;
use crate::confirm::{CONFIRMATION_TTL_MS, issue_token};
use crate::envelope::Diag;
use crate::expr::{contains_placeholder, describe_predicate, evaluate_dry, evaluate_expr};
use crate::helpers::{MAX_SAFE_INTEGER, bounded, interpolate, safety_name, unescape_placeholders};
use crate::pages::PageState;
use crate::prepare::{ConfirmTarget, MacroClaim, Purpose, fail};
use crate::preview::local_meta;
use crate::types::{
    BodyEncoding, CallOptions, Category, Confirm, Error, IdempotencyKind, MacroDescriptor,
    MacroStep, MacroStepKind, MacroStepPreview, OperationDescriptor, Outcome, ParamRole,
    PreviewResult, RenderedRequest, Response, ResponseMeta, Result, Retryable, Safety,
};
use crate::util::{MAX_ARG_DEPTH, duration_from_ms, js_number, nests_deeper_than};
use crate::verify::PollSpec;

const DEFAULT_POLL_INTERVAL_MS: f64 = 1000.0;
const DEFAULT_MACRO_BUDGET_MS: f64 = 30_000.0;
const MACRO_PAGE_LIMIT: f64 = 100.0;

fn safety_rank(safety: Safety) -> u8 {
    match safety {
        Safety::ReadOnly => 0,
        Safety::Mutating => 1,
        Safety::Destructive => 2,
        Safety::Irreversible => 3,
    }
}

fn uses_key(op: &OperationDescriptor) -> bool {
    matches!(
        op.agent.idempotency.policy,
        IdempotencyKind::CallerOwned | IdempotencyKind::Auto
    )
}

type Plan = Vec<(MacroStep, Arc<OperationDescriptor>)>;

impl ClientCore {
    fn macro_invalid(&self, name: &str, remediation: String) -> Error {
        fail(
            Diag::new(name, Category::ValidationFailed)
                .failed_parameter("macro")
                .expected("a valid macro descriptor")
                .remediation(remediation)
                .build(),
        )
    }

    /// The macro's input with the `add` defaults applied, or a failure.
    fn macro_input(
        &self,
        macro_: &MacroDescriptor,
        input: &Value,
    ) -> std::result::Result<Map<String, Value>, Error> {
        if nests_deeper_than(input, MAX_ARG_DEPTH) {
            return Err(fail(
                Diag::new(macro_.name.clone(), Category::ValidationFailed)
                    .failed_parameter("input")
                    .expected(format!(
                        "a JSON value nested at most {MAX_ARG_DEPTH} levels"
                    ))
                    .remediation(format!(
                        "Pass the input of {} without such deep nesting.",
                        macro_.name
                    ))
                    .build(),
            ));
        }
        let mut effective = match input {
            Value::Object(map) => map.clone(),
            Value::Null => Map::new(),
            _ => {
                return Err(fail(
                    Diag::new(macro_.name.clone(), Category::ValidationFailed)
                        .failed_parameter("input")
                        .expected("an object")
                        .remediation(format!("Pass the input of {} as one object.", macro_.name))
                        .build(),
                ));
            }
        };
        for (key, schema) in &macro_.input.add {
            if !effective.contains_key(key)
                && let Some(default) = schema.get("default")
            {
                effective.insert(key.clone(), default.clone());
            }
        }
        Ok(effective)
    }

    /// The macro's steps resolved against the registry, and its effective
    /// tier: the strictest of the declared one and every step's.
    fn macro_plan(&self, macro_: &MacroDescriptor) -> std::result::Result<(Plan, Safety), Error> {
        let name = &macro_.name;
        let mut steps = Vec::new();
        let mut safety = macro_.safety;
        for (index, step) in macro_.steps.iter().enumerate() {
            let Some(op) = self.operation(&step.operation) else {
                return Err(self.macro_invalid(
                    name,
                    format!(
                        "Step {} of {name} names {}, which is not registered with the client.",
                        index + 1,
                        step.operation
                    ),
                ));
            };
            if safety_rank(op.agent.safety) > safety_rank(safety) {
                safety = op.agent.safety;
            }
            steps.push((step.clone(), op));
        }
        Ok((steps, safety))
    }

    /// Step arguments evaluated against `scope` (a dry evaluation when
    /// `pending` names results not produced yet); for the step whose args the
    /// input extends, the fields the macro adds are removed.
    fn step_args(
        &self,
        macro_: &MacroDescriptor,
        step: &MacroStep,
        op: &OperationDescriptor,
        scope: &Map<String, Value>,
        pending: Option<&BTreeSet<String>>,
    ) -> Value {
        let source = if step.args.is_null() {
            Value::Object(Map::new())
        } else {
            step.args.clone()
        };
        let evaluated = match pending {
            Some(pending) => evaluate_dry(&source, scope, pending, 0),
            None => evaluate_expr(&source, scope, 0),
        };
        let mut args = match evaluated {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(value) => value,
        };
        let extends = macro_.input.extends.as_deref();
        if let Value::Object(map) = &mut args
            && Some(op.id.as_str()) == extends
        {
            for key in macro_.input.add.keys() {
                map.remove(key);
            }
        }
        args
    }

    pub(crate) async fn preview_macro_inner(
        &self,
        macro_: &MacroDescriptor,
        input: &Value,
        opts: &CallOptions,
    ) -> Result<PreviewResult> {
        let name = macro_.name.clone();
        let (steps, safety) = self.macro_plan(macro_)?;
        let effective = self.macro_input(macro_, input)?;
        if steps.is_empty() {
            return Err(
                self.macro_invalid(&name, format!("{name} has no steps; regenerate the SDK."))
            );
        }
        let mut rest = opts.clone();
        rest.confirm = None;
        rest.verify = false;
        // A dry evaluation: the input is known, earlier steps' results are
        // placeholders (`<from step NAME: path>`).
        let mut scope = Map::new();
        scope.insert("input".to_owned(), Value::Object(effective.clone()));
        let mut pending: BTreeSet<String> = BTreeSet::new();
        let mut previews: Vec<MacroStepPreview> = Vec::new();
        let mut effects: Vec<String> = Vec::new();
        if !macro_.summary.is_empty() {
            effects.push(macro_.summary.clone());
        }
        let mut key_used = false;
        for (index, (step, op)) in steps.iter().enumerate() {
            let where_ = format!("step {} of {name}: {}", index + 1, op.id);
            let args = self.step_args(macro_, step, op, &scope, Some(&pending));
            let mut step_opts = rest.clone();
            if !uses_key(op) || key_used {
                step_opts.idempotency_key = None;
            } else if opts.idempotency_key.is_some() {
                key_used = true;
            }
            let mut request: Option<RenderedRequest> = None;
            if args.is_object() {
                let encoding = op.body.as_ref().map(|b| b.encoding);
                // Bytes and multipart bodies cannot be encoded from a value not
                // known yet.
                let unrenderable = matches!(
                    encoding,
                    Some(BodyEncoding::Bytes | BodyEncoding::Multipart)
                ) && contains_placeholder(&args);
                if !unrenderable {
                    let prepared = match self
                        .prepare(op, &args, &step_opts, Purpose::MacroPreview, None, None)
                        .await
                    {
                        Ok(prepared) => prepared,
                        Err(mut error) => {
                            error.diagnostic.operation = name.clone();
                            error.diagnostic.remediation =
                                format!("{} ({where_})", error.diagnostic.remediation);
                            return Err(error);
                        }
                    };
                    request = Some(RenderedRequest {
                        method: prepared.method,
                        url: unescape_placeholders(&prepared.display_url, &args),
                        headers: prepared.headers.to_map(true),
                        body: prepared.display_body.clone(),
                    });
                }
            } else if !contains_placeholder(&args) {
                return Err(fail(
                    Diag::new(name.clone(), Category::ValidationFailed)
                        .failed_parameter("input")
                        .expected("an object")
                        .remediation(format!(
                            "Step {} of {name} does not evaluate to an argument object.",
                            index + 1
                        ))
                        .build(),
                ));
            }
            let mut own: Vec<String> = Vec::new();
            if let Some(message) = op
                .agent
                .confirmation
                .as_ref()
                .and_then(|c| c.message.as_deref())
                .filter(|m| !m.is_empty())
            {
                let empty = Map::new();
                own.push(interpolate(message, args.as_object().unwrap_or(&empty), op));
            }
            match step.kind {
                MacroStepKind::Poll => {
                    let until = step
                        .until
                        .as_ref()
                        .map(describe_predicate)
                        .unwrap_or_default();
                    let budget = bounded(
                        evaluate_expr(&step.budget_ms, &scope, 0).as_ref(),
                        DEFAULT_MACRO_BUDGET_MS,
                        0.0,
                        MAX_SAFE_INTEGER,
                    );
                    let interval = bounded(
                        step.interval_ms.map(Value::from).as_ref(),
                        DEFAULT_POLL_INTERVAL_MS,
                        0.0,
                        MAX_SAFE_INTEGER,
                    );
                    own.push(format!(
                        "Repeats {} every {} ms{}, for at most {} ms.",
                        op.id,
                        js_number(interval),
                        if until.is_empty() {
                            String::new()
                        } else {
                            format!(" until {until}")
                        },
                        js_number(budget)
                    ));
                }
                MacroStepKind::Paginate => {
                    let pages = page_limit(step);
                    own.push(format!("Reads up to {pages} pages of {}.", op.id));
                }
                MacroStepKind::Call => {}
            }
            if let Some(note) = op
                .agent
                .remediation_note
                .as_deref()
                .filter(|n| !n.is_empty())
            {
                own.push(note.to_owned());
            }
            let as_name = step.r#as.clone().filter(|a| !a.is_empty());
            effects.push(format!(
                "Step {}: {} {} ({}).",
                index + 1,
                kind_name(step.kind),
                op.id,
                safety_name(op.agent.safety)
            ));
            effects.extend(own.iter().cloned());
            previews.push(MacroStepPreview {
                step: u32::try_from(index + 1).unwrap_or(u32::MAX),
                kind: step.kind,
                operation: op.id.clone(),
                r#as: as_name.clone(),
                safety: op.agent.safety,
                request,
                effects: own,
            });
            if let Some(as_name) = as_name {
                pending.insert(as_name);
            }
        }
        if macro_.shown_once {
            effects.push(
                "The result contains values the API shows only once; store them immediately."
                    .to_owned(),
            );
        }
        // Step 1 has no earlier results, so it is always rendered.
        let Some(first) = previews.first().and_then(|p| p.request.clone()) else {
            return Err(fail(
                Diag::new(name.clone(), Category::ValidationFailed)
                    .failed_parameter("input")
                    .expected("an object")
                    .remediation(format!("Step 1 of {name} cannot be rendered."))
                    .build(),
            ));
        };
        let token = (safety != Safety::ReadOnly).then(|| {
            issue_token(
                &self.inner.confirmation_key,
                &format!("macro:{name}"),
                &Value::Object(effective.clone()),
                self.now(),
            )
        });
        Ok(Response {
            value: PreviewResult {
                operation: name,
                safety,
                request: first,
                effects,
                expires_in_ms: token.as_ref().map(|_| CONFIRMATION_TTL_MS),
                confirmation_token: token,
                server_preview: None,
                steps: Some(previews),
            },
            meta: local_meta(),
            verification: None,
        })
    }

    /// Whether running this step again (by rerunning its macro with the same
    /// input and options) answers the first result instead of applying the
    /// effect twice: an identity or content-hash body, a caller's key, or an
    /// automatic key (the store maps identical arguments to one key).
    fn rerun_protected(op: &OperationDescriptor, opts: &CallOptions) -> bool {
        match op.agent.idempotency.policy {
            IdempotencyKind::ContentIdentity
            | IdempotencyKind::ContentHash
            | IdempotencyKind::Auto => true,
            policy => {
                opts.idempotency_key.is_some()
                    && (policy == IdempotencyKind::CallerOwned
                        || op
                            .params
                            .iter()
                            .any(|p| p.role == ParamRole::IdempotencyKey))
            }
        }
    }

    /// The replay protection a macro's confirmation token is bound to: the
    /// run's idempotency key when a step sends it and every mutating step is
    /// protected against a rerun; otherwise none, so the token is used once (a
    /// key no step sends protects nothing).
    fn macro_bind(plan: &Plan, opts: &CallOptions) -> Option<String> {
        let key = opts.idempotency_key.as_ref()?;
        let mut sent = false;
        for (_, op) in plan {
            let mut step_opts = opts.clone();
            if !uses_key(op) || sent {
                step_opts.idempotency_key = None;
            } else {
                sent = true;
            }
            if is_mutation(op) && !Self::rerun_protected(op, &step_opts) {
                return None;
            }
        }
        sent.then(|| key.clone())
    }

    pub(crate) async fn run_macro_inner(
        &self,
        macro_: &MacroDescriptor,
        input: &Value,
        opts: &CallOptions,
    ) -> Outcome {
        let name = macro_.name.clone();
        let (steps, safety) = self.macro_plan(macro_)?;
        let effective = self.macro_input(macro_, input)?;
        self.confirmed(
            &ConfirmTarget {
                id: &name,
                subject: &format!("macro:{name}"),
                preview_call: "the macro's preview(...)",
            },
            safety,
            &Value::Object(effective.clone()),
            opts,
        )?;
        // The token is spent by the first step that sends (as an operation's
        // token is): a step failing pre-flight, before anything was sent,
        // leaves it valid for the corrected run.
        let token = match &opts.confirm {
            Some(Confirm::Token(token)) => Some(token.clone()),
            _ => None,
        };
        let claim = MacroClaim {
            name: name.clone(),
            token,
            bind: Self::macro_bind(&steps, opts),
            claimed: AtomicBool::new(false),
        };
        let mut scope = Map::new();
        scope.insert("input".to_owned(), Value::Object(effective));
        let mut completed: Vec<String> = Vec::new();
        // Results of completed steps by `as` name: returned as `partial` when a
        // later step fails.
        let mut produced = Map::new();
        // Completed mutating steps that a rerun of the macro would apply again.
        let mut unprotected: Vec<String> = Vec::new();
        let mut key_used = false;
        let mut meta = ResponseMeta {
            status: 0,
            headers: Default::default(),
            request_id: None,
            attempts: 0,
        };
        for (index, (step, op)) in steps.iter().enumerate() {
            let args = self.step_args(macro_, step, op, &scope, None);
            if !args.is_object() {
                return Err(fail(
                    Diag::new(name.clone(), Category::ValidationFailed)
                        .failed_parameter("input")
                        .expected("an object")
                        .remediation(format!(
                            "Step {} of {name} does not evaluate to an argument object.",
                            index + 1
                        ))
                        .build(),
                ));
            }
            let mut step_opts = opts.clone();
            step_opts.confirm = None;
            step_opts.verify = false;
            if !uses_key(op) || key_used {
                step_opts.idempotency_key = None;
            } else if opts.idempotency_key.is_some() {
                key_used = true;
            }
            let mut value: Option<Value> = None;
            let mut failure: Option<Error> = None;
            match step.kind {
                MacroStepKind::Poll => {
                    let budget = bounded(
                        evaluate_expr(&step.budget_ms, &scope, 0).as_ref(),
                        DEFAULT_MACRO_BUDGET_MS,
                        0.0,
                        MAX_SAFE_INTEGER,
                    );
                    let interval = bounded(
                        step.interval_ms.map(Value::from).as_ref(),
                        DEFAULT_POLL_INTERVAL_MS,
                        0.0,
                        MAX_SAFE_INTEGER,
                    );
                    let until = match &step.until {
                        Some(until @ Value::Object(_)) => until.clone(),
                        _ => Value::Object(Map::new()),
                    };
                    let spec = PollSpec {
                        until: &until,
                        interval: duration_from_ms(interval),
                        budget: duration_from_ms(budget),
                    };
                    match self
                        .poll_with(op, &args, &spec, &step_opts, Some(&claim))
                        .await
                    {
                        Ok(polled) => {
                            value = if polled.value.timed_out {
                                Some(Value::Null)
                            } else {
                                polled.value.body
                            };
                            meta = polled.meta;
                        }
                        Err(error) => failure = Some(error),
                    }
                }
                MacroStepKind::Paginate => {
                    let mut items: Vec<Value> = Vec::new();
                    let limit = page_limit(step);
                    let mut pages: u64 = 0;
                    let mut state = PageState::new(args.clone());
                    while let Some(page) = self
                        .page_step(op, &mut state, &step_opts, Some(&claim))
                        .await
                    {
                        match page {
                            Err(error) => {
                                failure = Some(error);
                                break;
                            }
                            Ok(page) => {
                                items.extend(page.value.items);
                                meta = page.meta;
                                pages += 1;
                                if pages >= limit {
                                    break;
                                }
                            }
                        }
                    }
                    value = Some(Value::Array(items));
                }
                MacroStepKind::Call => {
                    match self
                        .call_with(op, &args, &step_opts, None, Some(&claim))
                        .await
                    {
                        Ok(response) => {
                            value = response.value;
                            meta = response.meta;
                        }
                        Err(error) => failure = Some(error),
                    }
                }
            }
            let as_name = step.r#as.as_deref().filter(|a| !a.is_empty());
            if let Some(error) = failure {
                let Error {
                    mut diagnostic,
                    partial,
                } = error;
                if let (Some(partial), Some(as_name)) = (partial, as_name) {
                    produced.insert(as_name.to_owned(), *partial);
                }
                let done = if completed.is_empty() {
                    "no step completed before it".to_owned()
                } else {
                    format!("completed before it: {}", completed.join(", "))
                };
                let mut remediation = format!(
                    "{} Macro {name} stopped at step {} of {} ({}); {done}.",
                    diagnostic.remediation,
                    index + 1,
                    steps.len(),
                    op.id
                );
                let mut retryable = diagnostic.retryable;
                let mut next_action = diagnostic.next_action.take();
                let kept: Vec<&str> = produced.keys().map(String::as_str).collect();
                if !kept.is_empty() {
                    remediation.push_str(&format!(
                        " The result's partial holds the completed steps' results ({}){}.",
                        kept.join(", "),
                        if macro_.shown_once {
                            ", including values the API shows only once: store them now"
                        } else {
                            ""
                        }
                    ));
                }
                if !unprotected.is_empty() {
                    // Rerunning the macro would apply these steps again (a
                    // second endpoint, a lost one-time secret): finish from here
                    // instead.
                    remediation.push_str(&format!(
                        " Do not run {name} again: {} already took effect and has no replay protection.",
                        unprotected.join(", ")
                    ));
                    if matches!(retryable, Retryable::AfterDelay | Retryable::SameKeyOnly) {
                        retryable = Retryable::AfterRemediation;
                    }
                    let finish = format!(
                        "Finish {name} without rerunning it: call {} yourself with the values from the result's partial.",
                        op.id
                    );
                    next_action = Some(match next_action {
                        None => finish,
                        Some(existing) => format!("{existing} {finish}"),
                    });
                }
                // The envelope names what was called (the macro); the
                // remediation names the step that failed.
                diagnostic.operation = name.clone();
                diagnostic.remediation = remediation;
                diagnostic.retryable = retryable;
                diagnostic.next_action = next_action;
                let has_kept = !produced.is_empty();
                return Err(if has_kept {
                    Error {
                        diagnostic,
                        partial: Some(Box::new(Value::Object(produced))),
                    }
                } else {
                    Error {
                        diagnostic,
                        partial: None,
                    }
                });
            }
            completed.push(op.id.clone());
            if let Some(as_name) = as_name {
                match &value {
                    Some(value) => {
                        scope.insert(as_name.to_owned(), value.clone());
                        produced.insert(as_name.to_owned(), value.clone());
                    }
                    None => {
                        scope.remove(as_name);
                        produced.insert(as_name.to_owned(), Value::Null);
                    }
                }
            }
            if is_mutation(op) && !Self::rerun_protected(op, &step_opts) {
                unprotected.push(op.id.clone());
            }
        }
        Ok(Response {
            value: evaluate_expr(&macro_.output, &scope, 0),
            meta,
            verification: None,
        })
    }
}

fn kind_name(kind: MacroStepKind) -> &'static str {
    match kind {
        MacroStepKind::Call => "call",
        MacroStepKind::Poll => "poll",
        MacroStepKind::Paginate => "paginate",
    }
}

/// Pages a `paginate` step reads at most.
fn page_limit(step: &MacroStep) -> u64 {
    bounded(
        step.max_pages.map(Value::from).as_ref(),
        MACRO_PAGE_LIMIT,
        1.0,
        10_000.0,
    )
    .floor() as u64
}
