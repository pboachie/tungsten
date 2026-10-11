// SPDX-License-Identifier: AGPL-3.0-only
//! Macros in the canonical form (`tungsten_ir::Macro`), parsed once.
//!
//! A macro is a list of steps (`call`, `poll`, `paginate`) over operation
//! ids, an output expression and an input (`extends`: an operation whose
//! arguments the input takes, `add`: further fields as JSON Schemas).
//! Expressions are JSON in which a string starting with `$` is a reference
//! (`$input`, `$input.a.b`, `$<as>`, `$<as>.a.b`), an object `{"expr": "<ref>
//! in [..]" | "<ref> == x" | "<ref> != x"}` is a boolean and anything else a
//! literal.
//!
//! [`plan_macros`] checks the form exactly as the TypeScript emitter does
//! (so the set of macros a language leaves out is the set of TG0710), and
//! rewrites names into the arguments layout of [`crate::args`]: the keys of
//! a step's argument object that are the wire name of one of the step
//! operation's arguments become its args key, and the first segment of an
//! `$input.<name>` reference that is the wire name of an argument of the
//! extended operation becomes its args key (added fields keep their names).
//! A macro that does not fit the form is a [`MacroIssue`].

use serde_json::{Map, Value};
use tungsten_ir::{Macro, Presence};

use super::plan::{ArgSource, OpPlan, SdkPlan};

static NULL: Value = Value::Null;

/// A step's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Call,
    Poll,
    Paginate,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Call => "call",
            StepKind::Poll => "poll",
            StepKind::Paginate => "paginate",
        }
    }
}

/// A macro that is not in the canonical form, and why. Each emitter reports
/// it under its own code and leaves the macro out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroIssue {
    /// The macro's name (`public.submitAlphaMessageAndAwait`).
    pub name: String,
    /// What is wrong, as one sentence fragment (`step 1 calls
    /// `public.nope`, which is not a callable operation`).
    pub reason: String,
    /// JSON Pointer of the offending member inside the macro
    /// (`/steps/1/operation`, `/input/extends`, `/output`).
    pub pointer: String,
}

/// One step of a macro.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroStep {
    pub kind: StepKind,
    /// Index into [`SdkPlan::operations`].
    pub operation: usize,
    /// The argument expression, keys and `$input` references rewritten to
    /// args keys; a missing or `null` argument object is `{}`.
    pub args: Value,
    /// The name the step's result is bound to.
    pub as_name: Option<String>,
    /// `poll` only: the predicate on the step's result.
    pub until: Option<Map<String, Value>>,
    pub interval_ms: Option<u64>,
    /// The budget expression: `null`, a number of milliseconds or a
    /// reference.
    pub budget: Value,
    /// `paginate` only: the page limit.
    pub max_pages: Option<u64>,
}

/// Where a reference reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefRoot {
    /// The macro input.
    Input,
    /// The result of the step at this index.
    Step(usize),
}

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub enum MacroExpr {
    /// `$input.a.b` or `$<as>.a.b`, with the path segments after the root.
    Ref {
        root: RefRoot,
        path: Vec<String>,
    },
    /// `{"expr": "<ref> <op> <json>"}`; `op` is `in`, `==` or `!=`.
    Bool {
        root: RefRoot,
        path: Vec<String>,
        op: &'static str,
        right: Value,
    },
    Literal(Value),
    Array(Vec<MacroExpr>),
    Object(Vec<(String, MacroExpr)>),
}

/// One field of a macro's input.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroInputField {
    /// The key in the input object (an args key of the extended operation,
    /// or the added field's name).
    pub key: String,
    /// The words a target renders the name from.
    pub words: Vec<String>,
    pub source: InputSource,
    pub presence: Presence,
}

/// Where an input field comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum InputSource {
    /// An argument of the extended operation: index into its
    /// [`super::OpPlan::arguments`].
    Argument(usize),
    /// An added field: its JSON Schema and default.
    Added {
        schema: Value,
        default: Option<Value>,
    },
}

/// A macro in the canonical form.
#[derive(Debug, Clone)]
pub struct MacroPlan<'a> {
    pub source: &'a Macro,
    /// Words of the macro's member name (the name without its namespace
    /// prefix: `submit alpha message and await`).
    pub words: Vec<String>,
    pub steps: Vec<MacroStep>,
    /// The operation whose arguments the input extends (index into
    /// [`SdkPlan::operations`]): `input.extends`, else the first step whose
    /// whole argument is `$input`.
    pub base: Option<usize>,
    /// The input: the extended operation's arguments, then the added
    /// fields (optional when they have a default).
    pub input: Vec<MacroInputField>,
    /// The added fields as written (`input.add`), for descriptors.
    pub add: Map<String, Value>,
    /// The output expression with `$input` references rewritten.
    pub output: Value,
    /// The output expression, parsed.
    pub output_shape: MacroExpr,
}

impl MacroPlan<'_> {
    /// The macro's name.
    pub fn name(&self) -> &str {
        &self.source.name.0
    }
}

/// The macros of `plan` in the canonical form, and an issue for every other
/// one, both in IR order.
pub(crate) fn plan_macros<'a>(plan: &SdkPlan<'a>) -> (Vec<MacroPlan<'a>>, Vec<MacroIssue>) {
    let mut out = vec![];
    let mut issues = vec![];
    for m in &plan.ir.agent.macros {
        match parse(plan, m) {
            Ok(p) => out.push(p),
            Err((reason, pointer)) => issues.push(MacroIssue {
                name: m.name.0.clone(),
                reason,
                pointer,
            }),
        }
    }
    (out, issues)
}

type Problem = (String, String);

fn parse<'a>(plan: &SdkPlan<'a>, m: &'a Macro) -> Result<MacroPlan<'a>, Problem> {
    let raw_steps = m.steps.as_array().filter(|s| !s.is_empty()).ok_or((
        "`steps` must be a non-empty array".to_string(),
        "/steps".to_string(),
    ))?;
    struct Raw<'v> {
        kind: StepKind,
        op: usize,
        args: &'v Value,
        as_name: Option<&'v str>,
        until: Option<&'v Map<String, Value>>,
        interval_ms: Option<u64>,
        budget: &'v Value,
        max_pages: Option<u64>,
    }
    let mut raw: Vec<Raw<'_>> = vec![];
    let mut names: Vec<Option<&str>> = vec![];
    for (i, s) in raw_steps.iter().enumerate() {
        let at = |member: &str| -> String {
            if member.is_empty() {
                format!("/steps/{i}")
            } else {
                format!("/steps/{i}/{member}")
            }
        };
        let obj = s
            .as_object()
            .ok_or((format!("step {i} is not an object"), at("")))?;
        let kind = match obj.get("kind").and_then(Value::as_str) {
            Some("call") => StepKind::Call,
            Some("poll") => StepKind::Poll,
            Some("paginate") => StepKind::Paginate,
            other => return Err((format!("step {i} has unknown kind {other:?}"), at("kind"))),
        };
        let op_id = obj
            .get("operation")
            .and_then(Value::as_str)
            .ok_or((format!("step {i} has no operation"), at("operation")))?;
        let op = *plan.op_by_id.get(op_id).ok_or((
            format!("step {i} calls `{op_id}`, which is not a callable operation"),
            at("operation"),
        ))?;
        let as_name = match obj.get("as") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() && !s.contains('.') && s != "input" => {
                if names.contains(&Some(s.as_str())) {
                    return Err((format!("step {i} reuses the name `{s}`"), at("as")));
                }
                Some(s.as_str())
            }
            Some(v) => return Err((format!("step {i} has an invalid `as` {v}"), at("as"))),
        };
        let args = obj.get("args").unwrap_or(&NULL);
        check_expr(args, &names, i).map_err(|r| (r, at("args")))?;
        let budget = obj.get("budget_ms").unwrap_or(&NULL);
        if !(budget.is_null()
            || budget.is_u64()
            || budget.as_str().is_some_and(|b| b.starts_with('$')))
        {
            return Err((
                format!("step {i} has an invalid `budget_ms`"),
                at("budget_ms"),
            ));
        }
        check_expr(budget, &names, i).map_err(|r| (r, at("budget_ms")))?;
        let until = match (kind, obj.get("until")) {
            (StepKind::Poll, Some(Value::Object(u))) => Some(u),
            (StepKind::Poll, _) => {
                return Err((
                    format!("poll step {i} has no `until` predicate"),
                    at("until"),
                ));
            }
            _ => None,
        };
        let uint = |key: &str| -> Result<Option<u64>, Problem> {
            match obj.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v
                    .as_u64()
                    .map(Some)
                    .ok_or((format!("step {i} has an invalid `{key}`"), at(key))),
            }
        };
        raw.push(Raw {
            kind,
            op,
            args,
            as_name,
            until,
            interval_ms: uint("interval_ms")?,
            budget,
            max_pages: uint("max_pages")?,
        });
        names.push(as_name);
    }
    check_expr(&m.output, &names, raw.len()).map_err(|r| (r, "/output".to_string()))?;

    let input = m.input.as_object();
    let base = match input.and_then(|i| i.get("extends")) {
        None | Some(Value::Null) => raw
            .iter()
            .find(|s| s.args.as_str() == Some("$input"))
            .map(|s| s.op),
        Some(Value::String(id)) => Some(*plan.op_by_id.get(id.as_str()).ok_or((
            format!("input extends `{id}`, which is not a callable operation"),
            "/input/extends".to_string(),
        ))?),
        Some(v) => {
            return Err((
                format!("input `extends` is invalid: {v}"),
                "/input/extends".to_string(),
            ));
        }
    };
    let add = match input.and_then(|i| i.get("add")) {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(fields)) => fields.clone(),
        Some(v) => {
            return Err((
                format!("input `add` is invalid: {v}"),
                "/input/add".to_string(),
            ));
        }
    };

    let base_op = base.map(|b| &plan.operations[b]);
    let mut fields: Vec<MacroInputField> = vec![];
    if let Some(op) = base_op {
        for (i, a) in op.arguments.iter().enumerate() {
            fields.push(MacroInputField {
                key: a.key.clone(),
                words: a.words.clone(),
                source: InputSource::Argument(i),
                presence: a.presence,
            });
        }
    }
    for (name, schema) in &add {
        let default = schema.get("default").cloned();
        fields.push(MacroInputField {
            key: name.clone(),
            words: tungsten_ir::naming::split_words(name),
            presence: if default.is_some() {
                Presence::Optional
            } else {
                Presence::Required
            },
            source: InputSource::Added {
                schema: schema.clone(),
                default,
            },
        });
    }

    let rename = |v: &Value| rename_expr(v, &add, base_op);
    let steps = raw
        .iter()
        .map(|s| MacroStep {
            kind: s.kind,
            operation: s.op,
            args: step_args(&plan.operations[s.op], s.args, &add, base_op),
            as_name: s.as_name.map(str::to_string),
            until: s.until.cloned(),
            interval_ms: s.interval_ms,
            budget: rename(s.budget),
            max_pages: s.max_pages,
        })
        .collect::<Vec<_>>();
    let output = rename(&m.output);
    let output_shape = parse_expr(&output, &names, raw.len());
    let id = m.name.0.as_str();
    Ok(MacroPlan {
        source: m,
        words: tungsten_ir::naming::split_words(id.split_once('.').map_or(id, |(_, rest)| rest)),
        steps,
        base,
        input: fields,
        add,
        output,
        output_shape,
    })
}

/// Parse `$input.a.b` / `$name.a.b` against the step names visible at step
/// `upto` (the steps before it).
fn parse_ref(
    s: &str,
    names: &[Option<&str>],
    upto: usize,
) -> Result<(RefRoot, Vec<String>), String> {
    let body = s
        .strip_prefix('$')
        .ok_or(format!("`{s}` is not a reference"))?;
    let mut parts = body.split('.');
    let head = parts.next().unwrap_or("");
    let path: Vec<String> = parts.map(str::to_string).collect();
    if path.iter().any(String::is_empty) {
        return Err(format!("reference `{s}` has an empty path segment"));
    }
    if head == "input" {
        return Ok((RefRoot::Input, path));
    }
    names[..upto.min(names.len())]
        .iter()
        .position(|n| *n == Some(head))
        .map(|i| (RefRoot::Step(i), path))
        .ok_or(format!("reference `{s}` names no earlier step"))
}

/// A boolean `{expr}`: (reference, operator, right-hand side JSON).
fn parse_bool(e: &str) -> Option<(&str, &'static str, Value)> {
    let (left, rest) = e.trim().split_once(' ')?;
    let (op, right) = [("in", "in "), ("==", "== "), ("!=", "!= ")]
        .into_iter()
        .find_map(|(op, p)| rest.trim_start().strip_prefix(p).map(|r| (op, r)))?;
    let value: Value = serde_json::from_str(right.trim()).ok()?;
    if op == "in" && !value.is_array() {
        return None;
    }
    Some((left, op, value))
}

/// The `{expr}` string of a boolean expression object.
fn bool_expr(v: &Value) -> Option<&str> {
    let obj = v.as_object()?;
    if obj.len() == 1 {
        obj.get("expr")?.as_str()
    } else {
        None
    }
}

/// Check every reference in an expression.
fn check_expr(v: &Value, names: &[Option<&str>], upto: usize) -> Result<(), String> {
    if let Some(e) = bool_expr(v) {
        let (left, _, _) = parse_bool(e).ok_or(format!(
            "expression `{e}` is not `<ref> in [..]`, `<ref> == x` or `<ref> != x`"
        ))?;
        return parse_ref(left, names, upto).map(|_| ());
    }
    match v {
        Value::String(s) if s.starts_with('$') => parse_ref(s, names, upto).map(|_| ()),
        Value::Array(items) => items.iter().try_for_each(|i| check_expr(i, names, upto)),
        Value::Object(map) => map.values().try_for_each(|i| check_expr(i, names, upto)),
        _ => Ok(()),
    }
}

/// An expression already checked by [`check_expr`], as a tree.
fn parse_expr(v: &Value, names: &[Option<&str>], upto: usize) -> MacroExpr {
    if let Some((left, op, right)) = bool_expr(v).and_then(parse_bool)
        && let Ok((root, path)) = parse_ref(left, names, upto)
    {
        return MacroExpr::Bool {
            root,
            path,
            op,
            right,
        };
    }
    match v {
        Value::String(s) if s.starts_with('$') => match parse_ref(s, names, upto) {
            Ok((root, path)) => MacroExpr::Ref { root, path },
            Err(_) => MacroExpr::Literal(v.clone()),
        },
        Value::Array(items) => {
            MacroExpr::Array(items.iter().map(|i| parse_expr(i, names, upto)).collect())
        }
        Value::Object(map) => MacroExpr::Object(
            map.iter()
                .map(|(k, i)| (k.clone(), parse_expr(i, names, upto)))
                .collect(),
        ),
        other => MacroExpr::Literal(other.clone()),
    }
}

/// The args key of the argument with this wire name among an operation's
/// parameters (merged fields are keyed by their wire names already).
fn param_key<'o>(op: &'o OpPlan<'_>, wire: &str) -> Option<&'o str> {
    op.arguments
        .iter()
        .find(|a| matches!(a.source, ArgSource::Param { .. }) && a.wire == wire)
        .map(|a| a.key.as_str())
}

/// `$input.<name>...` with the first segment renamed to the extended
/// operation's args key; added fields keep their names.
fn rename_input_ref(s: &str, add: &Map<String, Value>, base: Option<&OpPlan<'_>>) -> String {
    let Some(rest) = s.strip_prefix("$input.") else {
        return s.to_string();
    };
    let (first, tail) = rest
        .split_once('.')
        .map_or((rest, None), |(f, t)| (f, Some(t)));
    let renamed = if add.contains_key(first) {
        first
    } else {
        base.and_then(|b| param_key(b, first)).unwrap_or(first)
    };
    match tail {
        Some(t) => format!("$input.{renamed}.{t}"),
        None => format!("$input.{renamed}"),
    }
}

/// An expression with every `$input.<name>` reference renamed.
fn rename_expr(v: &Value, add: &Map<String, Value>, base: Option<&OpPlan<'_>>) -> Value {
    if let Some(e) = bool_expr(v)
        && let Some((left, op, _)) = parse_bool(e)
    {
        let renamed = rename_input_ref(left, add, base);
        if renamed == left {
            return v.clone();
        }
        let right = e
            .trim()
            .split_once(' ')
            .and_then(|(_, rest)| rest.trim_start().strip_prefix(op))
            .unwrap_or("")
            .trim();
        let mut map = Map::new();
        map.insert(
            "expr".into(),
            Value::String(format!("{renamed} {op} {right}")),
        );
        return Value::Object(map);
    }
    match v {
        Value::String(s) if s.starts_with("$input.") => {
            Value::String(rename_input_ref(s, add, base))
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|i| rename_expr(i, add, base)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, i)| (k.clone(), rename_expr(i, add, base)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A step's argument object: keys that are a parameter's wire name become
/// its args key, `$input` references are renamed; `null` is `{}`.
fn step_args(
    op: &OpPlan<'_>,
    args: &Value,
    add: &Map<String, Value>,
    base: Option<&OpPlan<'_>>,
) -> Value {
    match args {
        Value::Object(map) if bool_expr(args).is_none() => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = param_key(op, k).map_or_else(|| k.clone(), str::to_string);
                    (key, rename_expr(v, add, base))
                })
                .collect(),
        ),
        Value::Null => Value::Object(Map::new()),
        other => rename_expr(other, add, base),
    }
}
