// SPDX-License-Identifier: AGPL-3.0-only
//! Macros: checked against the compiled API and normalized to the canonical
//! form of `tungsten_ir::Macro`.
//!
//! A macro whose namespace, operations or `input.extends` do not resolve,
//! whose name is an operation id, that polls an operation which is not
//! `read_only`, or that paginates an operation without pagination is
//! dropped with a warning. Its safety is the strictest step's (a poll
//! counts as `read_only`); a stricter declared tier is kept, a weaker one
//! is raised with TG0609. References into step results and the input are
//! checked against the operations' fields (TG0607).

use serde_json::{Map, Value, json};
use tungsten_ir::{Macro, Operation, OperationId, Safety};

use crate::apply::{Cx, Origin, safety_name};
use crate::expr;
use crate::fields::{self, Lookup};
use crate::model::{MacroConfig, MacroStepConfig};
use crate::report::Reporter;

pub(crate) fn build(cx: &Cx<'_>, r: &mut Reporter<'_>) -> Vec<Macro> {
    let mut out = vec![];
    for (i, m) in cx.cfg.macros.iter().enumerate() {
        if let Some(built) = one(cx, i, m, r) {
            out.push(built);
        }
    }
    out
}

/// One resolved step.
struct Step<'a> {
    kind: &'static str,
    op: &'a Operation,
    config: &'a MacroStepConfig,
}

fn one(cx: &Cx<'_>, i: usize, m: &MacroConfig, r: &mut Reporter<'_>) -> Option<Macro> {
    let at = format!("/macros/{i}");
    let origin = Origin::Manifest(at.clone());
    let ns = m.name.split('.').next().unwrap_or_default();
    if !cx.index.has_namespace(ns) {
        r.warning(
            "TG0604",
            &format!("{at}/name"),
            format!(
                "macro `{}` names unknown namespace `{ns}`; the macro is dropped",
                m.name
            ),
        );
        return None;
    }
    if cx.index.ops.contains_key(&m.name) {
        r.warning(
            "TG0609",
            &format!("{at}/name"),
            format!(
                "macro `{}` has the id of an operation; the macro is dropped",
                m.name
            ),
        );
        return None;
    }
    let input = m.input.clone().unwrap_or_default();
    let extends = match &input.extends {
        Some(reference) => {
            let id = cx.callable(reference, r, &origin, "input", "/extends")?;
            Some(*cx.ops.get(id.as_str())?)
        }
        None => None,
    };
    let mut steps = vec![];
    for (j, config) in m.steps.iter().enumerate() {
        let (kind, reference) = match (&config.call, &config.poll, &config.paginate) {
            (Some(op), None, None) => ("call", op),
            (None, Some(op), None) => ("poll", op),
            (None, None, Some(op)) => ("paginate", op),
            _ => return None,
        };
        let id = cx.callable(reference, r, &origin, "steps", &format!("/{j}/{kind}"))?;
        let op = *cx.ops.get(id.as_str())?;
        let safety = step_safety(cx, op);
        if kind == "poll" && safety != Safety::ReadOnly {
            r.warning(
                "TG0605",
                &format!("{at}/steps/{j}/poll"),
                format!(
                    "`{id}` is {}; polling would repeat its side effect, so the macro is dropped",
                    safety_name(safety)
                ),
            );
            return None;
        }
        if kind == "paginate" && !cx.index.ops.get(&id).is_some_and(|o| o.paginated) {
            r.warning(
                "TG0605",
                &format!("{at}/steps/{j}/paginate"),
                format!("`{id}` has no pagination; the macro is dropped"),
            );
            return None;
        }
        steps.push(Step { kind, op, config });
    }
    let strictest = steps
        .iter()
        .map(|s| {
            if s.kind == "poll" {
                Safety::ReadOnly
            } else {
                step_safety(cx, s.op)
            }
        })
        .max()
        .unwrap_or(Safety::ReadOnly);
    let safety = match m.safety {
        Some(declared) if declared < strictest => {
            r.warning(
                "TG0609",
                &format!("{at}/safety"),
                format!(
                    "macro `{}` declares {} but a step is {}; it is {}",
                    m.name,
                    safety_name(declared),
                    safety_name(strictest),
                    safety_name(strictest)
                ),
            );
            strictest
        }
        Some(declared) => declared,
        None => strictest,
    };
    check_references(cx, &at, m, extends, &steps, r);
    let sensitive = m.response.clone().unwrap_or_default();
    if let Value::Object(output) = &m.output {
        for (k, path) in sensitive.sensitive_fields.iter().enumerate() {
            let first = path.split('.').next().unwrap_or_default();
            if !output.contains_key(first) {
                r.warning(
                    "TG0607",
                    &format!("{at}/response/sensitive_fields/{k}"),
                    format!("`{path}` names no field of the output of `{}`", m.name),
                );
            }
        }
    }
    let mut sensitive_fields = sensitive.sensitive_fields.clone();
    sensitive_fields.sort();
    sensitive_fields.dedup();
    Some(Macro {
        name: OperationId(m.name.clone()),
        summary: m.summary.clone(),
        safety,
        steps: Value::Array(steps.iter().map(canonical_step).collect()),
        output: expr::canonical(&m.output),
        input: json!({
            "extends": extends.map(|o| o.id.0.clone()),
            "add": Value::Object(input.add.iter().map(|(k, v)| (k.clone(), Value::Object(v.clone()))).collect()),
        }),
        cluster: None,
        sensitive_response_fields: sensitive_fields,
        shown_once: sensitive.shown_once,
    })
}

fn step_safety(cx: &Cx<'_>, op: &Operation) -> Safety {
    cx.safety.get(&op.id.0).copied().unwrap_or(Safety::Mutating)
}

fn canonical_step(step: &Step<'_>) -> Value {
    let c = step.config;
    let mut out = Map::new();
    out.insert("kind".into(), json!(step.kind));
    out.insert("operation".into(), json!(step.op.id.0));
    out.insert(
        "args".into(),
        c.args.as_ref().map_or_else(|| json!({}), expr::canonical),
    );
    out.insert("as".into(), json!(c.as_name));
    out.insert(
        "until".into(),
        c.until.clone().map_or(Value::Null, Value::Object),
    );
    out.insert("interval_ms".into(), json!(c.interval_ms));
    out.insert(
        "budget_ms".into(),
        c.budget_ms.clone().unwrap_or(Value::Null),
    );
    out.insert("max_pages".into(), json!(c.max_pages));
    Value::Object(out)
}

/// `$input.<f>` must name an input field and `$<as>.<f>` a success-response
/// field of that step's operation (paginate results are arrays of items
/// and are not checked).
fn check_references(
    cx: &Cx<'_>,
    at: &str,
    m: &MacroConfig,
    extends: Option<&Operation>,
    steps: &[Step<'_>],
    r: &mut Reporter<'_>,
) {
    let added: Vec<&str> = m
        .input
        .as_ref()
        .map(|i| i.add.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let mut exprs: Vec<(String, &Value)> = vec![];
    let mut owned = vec![];
    for (j, s) in m.steps.iter().enumerate() {
        if let Some(args) = &s.args {
            exprs.push((format!("{at}/steps/{j}/args"), args));
        }
        if let Some(until) = &s.until {
            owned.push((
                format!("{at}/steps/{j}/until"),
                Value::Object(until.clone()),
            ));
        }
        if let Some(budget) = &s.budget_ms {
            exprs.push((format!("{at}/steps/{j}/budget_ms"), budget));
        }
    }
    exprs.push((format!("{at}/output"), &m.output));
    exprs.extend(owned.iter().map(|(p, v)| (p.clone(), v)));
    for (base, value) in exprs {
        let (refs, _) = expr::references(value);
        for (suffix, reference) in refs {
            let Some(first) = reference.path.first() else {
                continue;
            };
            let path = reference.path.join(".");
            let missing = if reference.root == "input" {
                if added.contains(&first.as_str()) {
                    None
                } else {
                    match extends {
                        Some(op) => match fields::request(cx.types, op, &path) {
                            Lookup::Missing(_) => Some(format!("the input of `{}`", m.name)),
                            _ => None,
                        },
                        None => Some(format!("the input of `{}` (it declares no input)", m.name)),
                    }
                }
            } else {
                steps
                    .iter()
                    .find(|s| {
                        s.config.as_name.as_deref() == Some(reference.root.as_str())
                            && s.kind != "paginate"
                    })
                    .and_then(|s| match fields::response(cx.types, s.op, &path) {
                        Lookup::Missing(_) => {
                            Some(format!("the success response of `{}`", s.op.id.0))
                        }
                        _ => None,
                    })
            };
            if let Some(what) = missing {
                r.warning(
                    "TG0607",
                    &format!("{base}{suffix}"),
                    format!("`${}.{path}` names no field of {what}", reference.root),
                );
            }
        }
    }
}
