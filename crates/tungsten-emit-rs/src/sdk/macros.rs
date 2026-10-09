// SPDX-License-Identifier: AGPL-3.0-only
//! `macros.rs`: each IR macro as a `MacroDescriptor` (data) and a method of
//! `client.macros()` that hands it to `ClientCore::run_macro`, with
//! `preview_<macro>` (`ClientCore::preview_macro`) for macros that are not
//! read-only. The runtime is the single implementation of macro semantics.
//!
//! Macros arrive in the canonical form documented on `tungsten_ir::Macro`.
//! The emitter checks the form, takes the input as a struct (the extended
//! operation's arguments plus the added fields), and rewrites the keys of
//! step argument objects and the first segment of `$input.<key>`
//! references from wire names to the struct's field names. A macro that
//! does not fit (unknown operation, reference to a later step, malformed
//! expression) is not emitted; [`plan_macros`] reports it as TG0742.

use serde_json::{Map, Value};
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::Writer;
use tungsten_ir::naming::{self, Role};
use tungsten_ir::{IdempotencyKind, Macro, Presence, Safety, Shape, TypeRef};

use super::ops::{
    ArgField, BodyShape, Lets, OpShape, fn_sig, is_arg, safety_str, write_args_struct,
    write_redacted_debug,
};
use super::plan::{Plan, RS, unique};
use super::resources::write_holder;
use super::rs::{Rx, doc, imports_for, paragraphs, put, reserved_names};
use super::types::{Cx, Slot, serde_attr};

static NULL: Value = Value::Null;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Call,
    Poll,
    Paginate,
}

impl StepKind {
    fn as_str(self) -> &'static str {
        match self {
            StepKind::Call => "call",
            StepKind::Poll => "poll",
            StepKind::Paginate => "paginate",
        }
    }

    fn rs(self) -> &'static str {
        match self {
            StepKind::Call => "MacroStepKind::Call",
            StepKind::Poll => "MacroStepKind::Poll",
            StepKind::Paginate => "MacroStepKind::Paginate",
        }
    }
}

#[derive(Debug, Clone)]
struct Step<'v> {
    kind: StepKind,
    /// Index into `Plan::ops`.
    op: usize,
    args: &'v Value,
    as_name: Option<&'v str>,
    until: Option<&'v Map<String, Value>>,
    interval_ms: Option<u64>,
    budget: &'v Value,
    max_pages: Option<u64>,
}

/// One emittable macro.
#[derive(Debug, Clone)]
pub(crate) struct MacroPlan<'v> {
    m: &'v Macro,
    /// Method name on `client.macros()`.
    pub member: String,
    /// `preview_<macro>` method name.
    preview: String,
    /// `MACRO_<NAME>` index constant.
    pub konst: String,
    /// The input struct.
    pub input: String,
    /// The output struct, when the output is a record.
    output: String,
    /// What the output is typed as; `None` when it stays `serde_json::Value`.
    out: Option<Output>,
    /// The operation whose arguments the input extends.
    base: Option<usize>,
    /// Fields added to the input: (name, JSON Schema, field name).
    add: Vec<(&'v str, &'v Value, String)>,
    steps: Vec<Step<'v>>,
}

impl MacroPlan<'_> {
    /// The macro's name (its id).
    pub(crate) fn name(&self) -> String {
        self.m.name.0.clone()
    }

    /// The `preview_<macro>` method name.
    pub(crate) fn preview_name(&self) -> &str {
        &self.preview
    }

    /// The output struct, when the output is a record.
    pub(crate) fn output_struct(&self) -> Option<&str> {
        matches!(self.out, Some(Output::Record(_))).then_some(self.output.as_str())
    }

    /// The Rust type of the macro's output value.
    pub(crate) fn output_type(&self) -> String {
        match &self.out {
            Some(Output::Single(ty)) => ty.clone(),
            Some(Output::Record(_)) => self.output.clone(),
            None => "Value".to_string(),
        }
    }
}

/// The emittable macros of `plan`, and a TG0742 warning for each macro that
/// is not emitted.
pub(crate) fn plan_macros<'v>(
    plan: &Plan<'v>,
    shapes: &[OpShape<'v>],
) -> (Vec<MacroPlan<'v>>, Diagnostics) {
    let mut diags = Diagnostics::new();
    let mut parsed = vec![];
    for m in &plan.ir.agent.macros {
        match parse(plan, shapes, m) {
            Ok(p) => parsed.push(p),
            Err(reason) => diags.push(
                Diagnostic::warning(
                    "TG0742",
                    format!(
                        "macro `{}` is not emitted in the Rust SDK: {reason}",
                        m.name.0
                    ),
                )
                .with_help("Macros use the canonical form documented on tungsten_ir::Macro."),
            ),
        }
    }
    let member_words: Vec<Vec<String>> = parsed
        .iter()
        .map(|p: &MacroPlan<'_>| {
            let id = p.m.name.0.as_str();
            naming::split_words(id.split_once('.').map_or(id, |(_, rest)| rest))
        })
        .collect();
    let mut words = member_words.clone();
    for (p, w) in parsed.iter().zip(&member_words) {
        if has_preview(p) {
            let mut pw = vec!["preview".to_string()];
            pw.extend(w.iter().cloned());
            words.push(pw);
        }
    }
    let names = unique(&["client", "new"], &words, Role::Method);
    let konsts = super::plan::unique_macro_konsts(&member_words);
    let both: Vec<Vec<String>> = ["input", "output"]
        .iter()
        .flat_map(|extra| {
            member_words
                .iter()
                .map(|w| super::plan::with_word(w, extra))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut reserved = reserved_names();
    reserved.push("Macros");
    let typed_names = fix_reserved(unique(&[], &both, Role::Type), &reserved);
    let (inputs, outputs) = typed_names.split_at(parsed.len());
    let mut previews = names[parsed.len()..].iter();
    for (i, p) in parsed.iter_mut().enumerate() {
        p.member = names[i].clone();
        p.konst = konsts[i].clone();
        p.input = inputs[i].clone();
        p.output = outputs[i].clone();
        if has_preview(p) {
            p.preview = previews.next().cloned().unwrap_or_default();
        }
    }
    for p in &mut parsed {
        match plan_output(plan, shapes, p) {
            Ok(out) => p.out = Some(out),
            Err(reason) => diags.push(Diagnostic::info(
                "TG0746",
                format!(
                    "the output of macro `{}` is typed `serde_json::Value` in the Rust SDK: {reason}",
                    p.m.name.0
                ),
            )),
        }
    }
    (parsed, diags)
}

/// Names that would shadow an imported token get a suffix.
fn fix_reserved(names: Vec<String>, reserved: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for n in names {
        let mut name = n.clone();
        let mut k = 2;
        while reserved.contains(&name.as_str()) || out.contains(&name) {
            name = format!("{n}{k}");
            k += 1;
        }
        out.push(name);
    }
    out
}

/// The typed output of a macro.
#[derive(Debug, Clone)]
enum Output {
    /// The output is one value of this Rust type.
    Single(String),
    /// The output is an object: the fields of the output struct.
    Record(Vec<OutField>),
}

/// One member of the output struct.
#[derive(Debug, Clone)]
struct OutField {
    /// Field name.
    key: String,
    /// The member's name in the output object.
    wire: String,
    slot: Slot,
    doc: String,
    sensitive: bool,
}

/// What an output expression evaluates to: the Rust slot, and whether the
/// value is a field the API shows only once.
struct OutValue {
    slot: Slot,
    sensitive: bool,
}

/// The presence of a value that may be missing (`undef`) or `null`.
fn presence_of(undef: bool, null: bool) -> Presence {
    match (undef, null) {
        (false, false) => Presence::Required,
        (false, true) => Presence::RequiredNullable,
        (true, false) => Presence::Optional,
        (true, true) => Presence::OptionalNullable,
    }
}

/// The type reached by reading `path` (wire names, map keys, array indexes)
/// from a value of type `ty`, its presence (`undef`: an earlier segment may
/// be missing) and whether it is a sensitive field; `None` when the path
/// does not resolve to a type.
fn walk_type(
    plan: &Plan<'_>,
    ty: &TypeRef,
    path: &[&str],
    mut undef: bool,
) -> Option<(TypeRef, Presence, bool)> {
    let mut cur = ty.clone();
    let mut null = false;
    let mut sensitive = false;
    for seg in path {
        null = false;
        sensitive = false;
        let unwrapped = match plan.resolve(&cur)? {
            Shape::Nullable { inner } => Some(inner.clone()),
            _ => None,
        };
        if let Some(inner) = unwrapped {
            undef = true;
            cur = inner;
        }
        let next = match plan.resolve(&cur)? {
            Shape::Record { fields, .. } => {
                let f = fields.iter().find(|f| f.wire_name == *seg)?;
                match f.presence {
                    Presence::Required => {}
                    Presence::RequiredNullable => null = true,
                    Presence::Optional => undef = true,
                    Presence::OptionalNullable => {
                        undef = true;
                        null = true;
                    }
                }
                sensitive = f.sensitive;
                f.ty.clone()
            }
            Shape::Map { values } => {
                undef = true;
                values.clone()
            }
            Shape::Array { items, .. } if seg.chars().all(|c| c.is_ascii_digit()) => {
                undef = true;
                items.clone()
            }
            _ => return None,
        };
        cur = next;
    }
    let unwrapped = match plan.resolve(&cur)? {
        Shape::Nullable { inner } => Some(inner.clone()),
        _ => None,
    };
    if let Some(inner) = unwrapped {
        null = true;
        cur = inner;
    }
    Some((cur, presence_of(undef, null), sensitive))
}

fn scalar(ty: &str) -> OutValue {
    OutValue {
        slot: Slot {
            ty: ty.to_string(),
            attrs: vec![],
        },
        sensitive: false,
    }
}

/// The Rust slot of the reference `r` (`$input.x`, `$step`, `$step.a.b`).
fn out_ref(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    mp: &MacroPlan<'_>,
    fields: &[ArgField<'_>],
    r: &str,
) -> Result<OutValue, String> {
    let cx = Cx::new(plan, None);
    let body = r.strip_prefix('$').unwrap_or(r);
    let (head, rest) = body.split_once('.').map_or((body, ""), |(h, t)| (h, t));
    let path: Vec<&str> = rest.split('.').filter(|p| !p.is_empty()).collect();
    if head == "input" {
        let renamed = rename_input_ref(r, mp, mp.base.map(|b| &shapes[b]));
        let key = renamed.strip_prefix("$input.").unwrap_or("");
        if key.contains('.') || key.is_empty() {
            return Err(format!("`{r}` reads inside the input"));
        }
        let f = fields
            .iter()
            .find(|f| f.key == key)
            .ok_or(format!("`{r}` names no input field"))?;
        return Ok(OutValue {
            slot: f.slot.clone(),
            sensitive: f.sensitive,
        });
    }
    let step = mp
        .steps
        .iter()
        .find(|s| s.as_name == Some(head))
        .ok_or(format!("`{r}` names no step"))?;
    let shape = &shapes[step.op];
    let op = &plan.ops[step.op].op.id.0;
    if step.kind == StepKind::Paginate {
        if !path.is_empty() {
            return Err(format!("`{r}` reads inside the items of a paginate step"));
        }
        let item = shape
            .item_ref
            .map_or_else(|| "Value".to_string(), |i| cx.ty(i));
        return Ok(scalar(&format!("Vec<{item}>")));
    }
    let body_ty = shape
        .success_ref
        .ok_or(format!("`{op}` has no single JSON response type"))?;
    let polled = step.kind == StepKind::Poll;
    if path.is_empty() {
        // A poll step is `null` when its budget ran out; a call without a
        // response body is missing.
        let presence = if polled {
            Presence::RequiredNullable
        } else if shape.bodiless {
            Presence::Optional
        } else {
            Presence::Required
        };
        return Ok(OutValue {
            slot: cx.slot(body_ty, presence, false),
            sensitive: false,
        });
    }
    let (leaf, presence, sensitive) = walk_type(plan, body_ty, &path, polled || shape.bodiless)
        .ok_or(format!(
            "`{r}` does not resolve to a type of `{op}`'s response"
        ))?;
    let shown_once = path
        .last()
        .is_some_and(|last| mp.m.sensitive_response_fields.iter().any(|f| f == last));
    Ok(OutValue {
        slot: cx.slot(&leaf, presence, false),
        sensitive: sensitive || shown_once,
    })
}

/// The typed output of a macro, or why it stays `serde_json::Value`.
fn plan_output(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    mp: &MacroPlan<'_>,
) -> Result<Output, String> {
    let fields = input_fields(shapes, mp);
    let value = |v: &Value| -> Result<OutValue, String> {
        if bool_expr(v).is_some() {
            return Ok(scalar("bool"));
        }
        match v {
            Value::String(s) if s.starts_with('$') => out_ref(plan, shapes, mp, &fields, s),
            Value::String(_) => Ok(scalar("String")),
            Value::Bool(_) => Ok(scalar("bool")),
            Value::Number(n) if n.is_i64() => Ok(scalar("i64")),
            Value::Number(n) if n.is_f64() => Ok(scalar("f64")),
            _ => {
                Err("an output member is a null, a large number, a list or a nested object".into())
            }
        }
    };
    let output = &mp.m.output;
    let Value::Object(map) = output else {
        let one = value(output)?;
        return if one.slot.ty.starts_with("Patch<") {
            Err("the output may be missing or null".into())
        } else {
            Ok(Output::Single(one.slot.ty))
        };
    };
    if bool_expr(output).is_some() {
        return Ok(Output::Single("bool".into()));
    }
    if map.is_empty() {
        return Err("the output is an empty object".into());
    }
    let words: Vec<Vec<String>> = map.keys().map(|k| naming::split_words(k)).collect();
    let keys = unique(&[], &words, Role::Field);
    let mut out = vec![];
    for ((wire, expr), key) in map.iter().zip(keys) {
        let v = value(expr)?;
        let shown = match expr {
            Value::String(s) if s.starts_with('$') => format!("The value of `{s}`."),
            other => match bool_expr(other) {
                Some(e) => format!("Whether `{e}`."),
                None => "A constant of the macro.".to_string(),
            },
        };
        out.push(OutField {
            key,
            wire: wire.clone(),
            slot: v.slot,
            doc: shown,
            sensitive: v.sensitive || mp.m.sensitive_response_fields.contains(wire),
        });
    }
    Ok(Output::Record(out))
}

fn parse<'v>(
    plan: &Plan<'v>,
    shapes: &[OpShape<'v>],
    m: &'v Macro,
) -> Result<MacroPlan<'v>, String> {
    let raw_steps = m
        .steps
        .as_array()
        .filter(|s| !s.is_empty())
        .ok_or("`steps` must be a non-empty array")?;
    let mut steps: Vec<Step<'v>> = vec![];
    let mut names: Vec<Option<&'v str>> = vec![];
    for (i, s) in raw_steps.iter().enumerate() {
        let obj = s.as_object().ok_or(format!("step {i} is not an object"))?;
        let kind = match obj.get("kind").and_then(Value::as_str) {
            Some("call") => StepKind::Call,
            Some("poll") => StepKind::Poll,
            Some("paginate") => StepKind::Paginate,
            other => return Err(format!("step {i} has unknown kind {other:?}")),
        };
        let op_id = obj
            .get("operation")
            .and_then(Value::as_str)
            .ok_or(format!("step {i} has no operation"))?;
        let op = *plan.op_by_id.get(op_id).ok_or(format!(
            "step {i} calls `{op_id}`, which is not a callable operation"
        ))?;
        let as_name = match obj.get("as") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() && !s.contains('.') && s != "input" => {
                if names.contains(&Some(s.as_str())) {
                    return Err(format!("step {i} reuses the name `{s}`"));
                }
                Some(s.as_str())
            }
            Some(v) => return Err(format!("step {i} has an invalid `as` {v}")),
        };
        let args = obj.get("args").unwrap_or(&NULL);
        check_expr(args, &names, i)?;
        let budget = obj.get("budget_ms").unwrap_or(&NULL);
        if !(budget.is_null()
            || budget.is_u64()
            || budget.as_str().is_some_and(|b| b.starts_with('$')))
        {
            return Err(format!("step {i} has an invalid `budget_ms`"));
        }
        check_expr(budget, &names, i)?;
        let until = match (kind, obj.get("until")) {
            (StepKind::Poll, Some(Value::Object(u))) => Some(u),
            (StepKind::Poll, _) => return Err(format!("poll step {i} has no `until` predicate")),
            _ => None,
        };
        let uint = |key: &str| -> Result<Option<u64>, String> {
            match obj.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v
                    .as_u64()
                    .map(Some)
                    .ok_or(format!("step {i} has an invalid `{key}`")),
            }
        };
        steps.push(Step {
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
    check_expr(&m.output, &names, steps.len())?;

    let input = m.input.as_object();
    let base = match input.and_then(|i| i.get("extends")) {
        None | Some(Value::Null) => steps
            .iter()
            .find(|s| s.args.as_str() == Some("$input"))
            .map(|s| s.op),
        Some(Value::String(id)) => Some(*plan.op_by_id.get(id.as_str()).ok_or(format!(
            "input extends `{id}`, which is not a callable operation"
        ))?),
        Some(v) => return Err(format!("input `extends` is invalid: {v}")),
    };
    let base_keys: Vec<String> = base
        .map(|b| shapes[b].fields.iter().map(|f| f.key.clone()).collect())
        .unwrap_or_default();
    let add = match input.and_then(|i| i.get("add")) {
        None | Some(Value::Null) => vec![],
        Some(Value::Object(fields)) => {
            let words: Vec<Vec<String>> = fields.keys().map(|k| naming::split_words(k)).collect();
            let keys = unique(
                &base_keys.iter().map(String::as_str).collect::<Vec<_>>(),
                &words,
                Role::Field,
            );
            fields
                .iter()
                .zip(keys)
                .map(|((name, schema), key)| (name.as_str(), schema, key))
                .collect()
        }
        Some(v) => return Err(format!("input `add` is invalid: {v}")),
    };
    Ok(MacroPlan {
        m,
        member: String::new(),
        preview: String::new(),
        konst: String::new(),
        input: String::new(),
        output: String::new(),
        out: None,
        base,
        add,
        steps,
    })
}

/// Parse `$input.a.b` / `$name.a.b` against the step names visible at step
/// `upto` (steps before it); `Ok(true)` for an `$input` reference.
fn check_ref(s: &str, names: &[Option<&str>], upto: usize) -> Result<bool, String> {
    let body = s
        .strip_prefix('$')
        .ok_or(format!("`{s}` is not a reference"))?;
    let mut parts = body.split('.');
    let head = parts.next().unwrap_or("");
    if parts.any(str::is_empty) {
        return Err(format!("reference `{s}` has an empty path segment"));
    }
    if head == "input" {
        return Ok(true);
    }
    names[..upto.min(names.len())]
        .contains(&Some(head))
        .then_some(false)
        .ok_or(format!("reference `{s}` names no earlier step"))
}

/// A boolean `{expr}`: (reference, operator, right-hand side text).
fn parse_bool(e: &str) -> Option<(&str, &'static str, &str)> {
    let (left, rest) = e.trim().split_once(' ')?;
    let (op, right) = [("in", "in "), ("==", "== "), ("!=", "!= ")]
        .into_iter()
        .find_map(|(op, p)| rest.trim_start().strip_prefix(p).map(|r| (op, r)))?;
    let value: Value = serde_json::from_str(right.trim()).ok()?;
    if op == "in" && !value.is_array() {
        return None;
    }
    Some((left, op, right))
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

/// Validate every reference in an expression.
fn check_expr(v: &Value, names: &[Option<&str>], upto: usize) -> Result<(), String> {
    if let Some(e) = bool_expr(v) {
        let (left, _, _) = parse_bool(e).ok_or(format!(
            "expression `{e}` is not `<ref> in [..]`, `<ref> == x` or `<ref> != x`"
        ))?;
        return check_ref(left, names, upto).map(|_| ());
    }
    match v {
        Value::String(s) if s.starts_with('$') => check_ref(s, names, upto).map(|_| ()),
        Value::Array(items) => items.iter().try_for_each(|i| check_expr(i, names, upto)),
        Value::Object(map) => map.values().try_for_each(|i| check_expr(i, names, upto)),
        _ => Ok(()),
    }
}

/// The argument name of the wire name `wire` among an operation's
/// arguments (parameters and merged body fields).
fn arg_key<'s>(shape: &'s OpShape<'_>, wire: &str) -> Option<&'s str> {
    shape
        .params
        .iter()
        .filter(|p| is_arg(p.param))
        .find(|p| p.param.wire_name == wire)
        .map(|p| p.name.as_str())
        .or_else(|| match &shape.body {
            Some(super::ops::BodyPlan {
                shape: BodyShape::Merged(pairs),
                ..
            }) => pairs
                .iter()
                .find(|(_, w)| w == wire)
                .map(|(a, _)| a.as_str()),
            _ => None,
        })
}

/// `$input.<name>...` with the first segment renamed to the input struct's
/// field name.
fn rename_input_ref(s: &str, mp: &MacroPlan<'_>, base: Option<&OpShape<'_>>) -> String {
    let Some(rest) = s.strip_prefix("$input.") else {
        return s.to_string();
    };
    let (first, tail) = rest
        .split_once('.')
        .map_or((rest, None), |(f, t)| (f, Some(t)));
    let renamed = mp
        .add
        .iter()
        .find(|(name, _, _)| *name == first)
        .map(|(_, _, key)| key.as_str())
        .or_else(|| base.and_then(|b| arg_key(b, first)))
        .unwrap_or(first);
    match tail {
        Some(t) => format!("$input.{renamed}.{t}"),
        None => format!("$input.{renamed}"),
    }
}

/// An expression with every `$input.<name>` reference renamed.
fn rename_expr(v: &Value, mp: &MacroPlan<'_>, base: Option<&OpShape<'_>>) -> Value {
    if let Some(e) = bool_expr(v)
        && let Some((left, op, right)) = parse_bool(e)
    {
        let mut map = Map::new();
        map.insert(
            "expr".into(),
            Value::String(format!(
                "{} {op} {}",
                rename_input_ref(left, mp, base),
                right.trim()
            )),
        );
        return Value::Object(map);
    }
    match v {
        Value::String(s) if s.starts_with("$input.") => {
            Value::String(rename_input_ref(s, mp, base))
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|i| rename_expr(i, mp, base)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, i)| (k.clone(), rename_expr(i, mp, base)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Step arguments for the descriptor: keys that are an argument's wire name
/// become its field name, and `$input` references are renamed.
fn step_args(
    shape: &OpShape<'_>,
    args: &Value,
    mp: &MacroPlan<'_>,
    base: Option<&OpShape<'_>>,
) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = arg_key(shape, k).map_or_else(|| k.clone(), str::to_string);
                    (key, rename_expr(v, mp, base))
                })
                .collect::<Map<String, Value>>(),
        ),
        Value::Null => Value::Object(Map::new()),
        other => rename_expr(other, mp, base),
    }
}

fn opt_u(n: Option<u64>) -> Rx {
    n.map_or_else(Rx::none, |n| Rx::some(Rx::atom(n.to_string())))
}

/// The `MacroDescriptor` literal of a macro.
fn descriptor_rx(plan: &Plan<'_>, shapes: &[OpShape<'_>], mp: &MacroPlan<'_>) -> (Vec<String>, Rx) {
    let m = mp.m;
    let base = mp.base.map(|b| &shapes[b]);
    let steps = Rx::list(
        mp.steps
            .iter()
            .map(|s| {
                Rx::record(
                    "MacroStep",
                    vec![
                        ("kind", Rx::atom(s.kind.rs())),
                        ("operation", Rx::string(&plan.ops[s.op].op.id.0)),
                        (
                            "args",
                            Rx::json(&step_args(&shapes[s.op], s.args, mp, base)),
                        ),
                        (
                            "r#as",
                            s.as_name.map_or_else(Rx::none, |n| Rx::some(Rx::string(n))),
                        ),
                        (
                            "until",
                            s.until.map_or_else(Rx::none, |u| {
                                Rx::some(Rx::json(&Value::Object(u.clone())))
                            }),
                        ),
                        ("interval_ms", opt_u(s.interval_ms)),
                        ("budget_ms", Rx::json(&rename_expr(s.budget, mp, base))),
                        ("max_pages", opt_u(s.max_pages)),
                    ],
                )
            })
            .collect(),
    );
    let mut lets = Lets::default();
    let add = lets.map(
        "add",
        mp.add
            .iter()
            .map(|(name, schema, _)| (name.to_string(), Rx::json(schema)))
            .collect(),
    );
    let value = Rx::record(
        "MacroDescriptor",
        vec![
            ("name", Rx::string(&m.name.0)),
            ("summary", Rx::string(&m.summary)),
            ("safety", Rx::atom(safety_rs(m.safety))),
            ("steps", steps),
            ("output", Rx::json(&rename_expr(&m.output, mp, base))),
            (
                "input",
                Rx::record(
                    "MacroInput",
                    vec![
                        (
                            "extends",
                            Rx::opt_string(mp.base.map(|b| plan.ops[b].op.id.0.as_str())),
                        ),
                        ("add", add),
                    ],
                ),
            ),
            (
                "sensitive_response_fields",
                Rx::strings(&m.sensitive_response_fields),
            ),
            ("shown_once", Rx::boolean(m.shown_once)),
            ("cluster", Rx::opt_string(m.cluster.as_deref())),
        ],
    );
    (lets.stmts, value)
}

/// The builder function of a macro descriptor.
fn macro_fn(konst: &str) -> String {
    format!(
        "macro_{}",
        konst.trim_start_matches("MACRO_").to_lowercase()
    )
}

fn safety_rs(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "Safety::ReadOnly",
        Safety::Mutating => "Safety::Mutating",
        Safety::Destructive => "Safety::Destructive",
        Safety::Irreversible => "Safety::Irreversible",
    }
}

/// Whether a macro gets `preview_<macro>`: every macro that is not
/// read-only.
pub(crate) fn has_preview(mp: &MacroPlan<'_>) -> bool {
    mp.m.safety != Safety::ReadOnly
}

/// A JSON Schema of an added input field as a Rust type.
fn schema_rs(schema: &Value) -> String {
    let Some(obj) = schema.as_object() else {
        return "Value".into();
    };
    if obj.contains_key("const") || obj.contains_key("enum") {
        return "Value".into();
    }
    let one = |t: &str| -> String {
        match t {
            "string" => "String".into(),
            "integer" => "i64".into(),
            "number" => "f64".into(),
            "boolean" => "bool".into(),
            "array" => format!(
                "Vec<{}>",
                obj.get("items").map_or_else(|| "Value".into(), schema_rs)
            ),
            _ => "Value".into(),
        }
    };
    match obj.get("type") {
        Some(Value::String(t)) => one(t),
        _ => "Value".into(),
    }
}

/// The fields of a macro's input struct: the extended operation's
/// arguments, then the added fields (optional when they have a default,
/// which the runtime applies).
fn input_fields<'a>(shapes: &[OpShape<'a>], mp: &MacroPlan<'_>) -> Vec<ArgField<'a>> {
    let mut fields: Vec<ArgField<'a>> = mp
        .base
        .map(|b| {
            shapes[b]
                .fields
                .iter()
                .map(|f| ArgField {
                    check: None,
                    ..f.clone()
                })
                .collect()
        })
        .unwrap_or_default();
    for (_, schema, key) in &mp.add {
        let ty = schema_rs(schema);
        let default = schema.get("default");
        let optional = default.is_some();
        let slot = if optional {
            Slot {
                ty: format!("Option<{ty}>"),
                attrs: vec![vec![
                    "default".into(),
                    r#"skip_serializing_if = "Option::is_none""#.into(),
                    r#"with = "s::opt""#.into(),
                ]],
            }
        } else {
            Slot { ty, attrs: vec![] }
        };
        let doc = paragraphs([
            schema
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            default
                .map(|d| {
                    format!(
                        "Default (applied by the runtime): `{}`.",
                        serde_json::to_string(d).unwrap_or_default()
                    )
                })
                .unwrap_or_default(),
        ]);
        fields.push(ArgField {
            key: key.clone(),
            slot,
            optional,
            sensitive: false,
            doc,
            check: None,
        });
    }
    fields
}

/// The documentation of a macro method: summary, tier and confirmation
/// rule, steps, keys, one-time secrets and cluster.
fn macro_doc(plan: &Plan<'_>, mp: &MacroPlan<'_>) -> String {
    let m = mp.m;
    let steps: Vec<String> = mp
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let op = &plan.ops[s.op].op;
            format!(
                "{}. `{}` `{}` ({})",
                i + 1,
                s.kind.as_str(),
                op.id.0,
                safety_str(op.agent.safety)
            )
        })
        .collect();
    let confirmation = match m.safety {
        Safety::Irreversible => format!(
            "Cannot be undone: call `{}()` with the same input first and pass its `confirmation_token` as `Confirm::Token` in the call options. The one confirmation covers every step of the run.",
            mp.preview
        ),
        Safety::Destructive => format!(
            "Requires confirmation for the whole run: pass `confirm: Some(Confirm::Yes)`, or the `confirmation_token` of `{}()` as `Confirm::Token`.",
            mp.preview
        ),
        Safety::ReadOnly | Safety::Mutating => String::new(),
    };
    let keyed: Vec<String> = mp
        .steps
        .iter()
        .map(|s| plan.ops[s.op].op)
        .filter(|op| op.agent.idempotency.policy == IdempotencyKind::CallerOwned)
        .map(|op| format!("`{}`", op.id.0))
        .collect();
    let key = if keyed.is_empty() {
        String::new()
    } else {
        format!(
            "Pass `idempotency_key` in the call options for {}: generate it once, persist it with your intent and reuse it on every retry. Only the first step that takes a key receives it.",
            keyed.join(", ")
        )
    };
    let shown_once = if m.shown_once {
        format!(
            "The output includes values shown only once{}: store them immediately.",
            if m.sensitive_response_fields.is_empty() {
                String::new()
            } else {
                format!(" (`{}`)", m.sensitive_response_fields.join("`, `"))
            }
        )
    } else {
        String::new()
    };
    paragraphs([
        m.summary.clone(),
        format!("`{}`. Safety: `{}`.", m.name.0, safety_str(m.safety)),
        confirmation,
        format!("Steps:\n{}", steps.join("\n")),
        key,
        shown_once,
        m.cluster
            .as_deref()
            .map(|c| format!("Cluster: `{c}`."))
            .unwrap_or_default(),
        match &mp.out {
            Some(Output::Record(_)) => format!(
                "The value is the macro's output, typed as `{}`. The first failing step's envelope is returned; its remediation names the steps already completed.",
                mp.output
            ),
            Some(Output::Single(_)) => "The value is the macro's output, typed from the steps it reads. The first failing step's envelope is returned; its remediation names the steps already completed.".to_string(),
            None => "The value is the macro's output expression evaluated by the runtime (its shape is not typed; see TG0746). The first failing step's envelope is returned; its remediation names the steps already completed.".to_string(),
        },
    ])
}

/// The output struct of a macro: one field per member of the output object.
fn write_output_struct(w: &mut Writer, name: &str, macro_name: &str, fields: &[OutField]) {
    doc(w, &format!("Output of the macro `{macro_name}`."));
    let sensitive = fields.iter().any(|f| f.sensitive);
    let derives = if sensitive {
        "Clone, PartialEq, Serialize, Deserialize"
    } else {
        "Debug, Clone, PartialEq, Serialize, Deserialize"
    };
    w.line(format!("#[derive({derives})]"));
    w.line(format!("pub struct {name} {{"));
    w.indent();
    for f in fields {
        doc(w, &f.doc);
        if f.key.trim_start_matches("r#") != f.wire {
            serde_attr(w, &[format!("rename = {}", super::rs::string_lit(&f.wire))]);
        }
        for a in &f.slot.attrs {
            serde_attr(w, a);
        }
        w.line(format!("pub {}: {},", f.key, f.slot.ty));
    }
    w.dedent();
    w.line("}");
    if sensitive {
        w.blank();
        let shown: Vec<(&str, bool)> = fields
            .iter()
            .map(|f| (f.key.as_str(), f.sensitive))
            .collect();
        write_redacted_debug(w, name, &shown);
    }
}

/// The source of `macros.rs`.
pub(crate) fn macros_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    macros: &[MacroPlan<'_>],
    header: &str,
) -> String {
    let client = &plan.client_class;
    let cx = Cx::new(plan, None);
    let mut code = Writer::new("    ");
    for (i, mp) in macros.iter().enumerate() {
        code.line(format!("pub const {}: usize = {i};", mp.konst));
    }
    code.blank();
    doc(
        &mut code,
        "The descriptors of every macro of the API, in IR order.",
    );
    code.line("pub fn descriptors() -> Vec<MacroDescriptor> {");
    code.indent();
    let list = Rx::list(
        macros
            .iter()
            .map(|m| Rx::call(macro_fn(&m.konst), vec![]))
            .collect(),
    );
    put(&mut code, 4, "", &list, "");
    code.dedent();
    code.line("}");
    for mp in macros {
        code.blank();
        let f = macro_fn(&mp.konst);
        code.line(format!("fn {f}() -> MacroDescriptor {{"));
        code.indent();
        let (lets, value) = descriptor_rx(plan, shapes, mp);
        for l in &lets {
            code.line(l);
        }
        put(&mut code, 4, "", &value, "");
        code.dedent();
        code.line("}");
    }
    code.blank();
    write_holder(
        &mut code,
        client,
        "Macros",
        "Multi-step workflows compiled from the agent manifest, run by the runtime.",
    );
    for mp in macros {
        code.blank();
        doc(&mut code, &macro_doc(plan, mp));
        let params = vec![
            "&self".to_string(),
            format!("input: {}", mp.input),
            "opts: &CallOptions".to_string(),
        ];
        fn_sig(
            &mut code,
            4,
            &format!("pub async fn {}", mp.member),
            &params,
            &format!("tungsten_runtime::Result<{}>", mp.output_type()),
        );
        code.indent();
        code.line("let client = self.client;");
        code.line(format!(
            "let descriptor = &client.descriptors.macros[{}];",
            mp.konst
        ));
        code.line("let input = s::args(&input);");
        code.line("let outcome = client.core.run_macro(descriptor, input, opts).await;");
        code.line("decode(&descriptor.name, outcome)");
        code.dedent();
        code.line("}");
        if has_preview(mp) {
            code.blank();
            doc(
                &mut code,
                &format!(
                    "Preview `{}` without sending anything: every step's request and effects, and the `confirmation_token` to pass to `{}()` when the run needs one.",
                    mp.m.name.0, mp.member
                ),
            );
            fn_sig(
                &mut code,
                4,
                &format!("pub async fn {}", mp.preview),
                &params,
                "tungsten_runtime::Result<PreviewResult>",
            );
            code.indent();
            code.line("let client = self.client;");
            code.line(format!(
                "let descriptor = &client.descriptors.macros[{}];",
                mp.konst
            ));
            code.line("let input = s::args(&input);");
            code.line("client.core.preview_macro(descriptor, input, opts).await");
            code.dedent();
            code.line("}");
        }
    }
    code.dedent();
    code.line("}");
    for mp in macros {
        code.blank();
        let fields = input_fields(shapes, mp);
        write_args_struct(
            &mut code,
            &cx,
            &mp.input,
            &format!(
                "Input of the macro `{}`: {}",
                mp.m.name.0,
                mp.base.map_or_else(
                    || "the fields it adds.".to_string(),
                    |b| format!(
                        "the arguments of `{}` and the fields it adds.",
                        plan.ops[b].op.id.0
                    )
                )
            ),
            &fields,
            None,
        );
    }
    for mp in macros {
        if let Some(Output::Record(fields)) = &mp.out {
            code.blank();
            write_output_struct(&mut code, &mp.output, &mp.m.name.0, fields);
        }
    }
    let code = code.finish();

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line("//! Macros: compiled workflows as data, run by the runtime.");
    let extra = vec![("crate::client".to_string(), client.clone())];
    let uses = imports_for(&code, &[], None, &extra);
    if !uses.is_empty() {
        w.blank();
        for u in &uses {
            w.line(u);
        }
    }
    let mut out = w.finish();
    out.push('\n');
    out.push_str(&code);
    out
}

#[allow(dead_code)]
fn _keep() -> naming::Target {
    RS
}
