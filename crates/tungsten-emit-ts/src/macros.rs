// SPDX-License-Identifier: AGPL-3.0-only
//! `src/macros.ts`: each IR macro as a `MacroDescriptor` constant (data)
//! and a typed member of `client.macros` that hands it to
//! `ClientCore.runMacro`, with `.preview()` (`ClientCore.previewMacro`)
//! for macros that are not read-only. The runtime is the single
//! implementation of macro semantics: step order, expressions, the
//! macro-level confirmation (a `destructive` or `irreversible` macro, or one
//! with such a step, is confirmed once for the whole run), input defaults
//! and poll timeouts.
//!
//! Macros arrive in the canonical form documented on `tungsten_ir::Macro`:
//! steps (`call`, `poll`, `paginate`) over operation ids, expressions in
//! which `$input`, `$<as>` and their dotted paths are references, and
//! `{expr: "<ref> in [..]" | "<ref> == x" | "<ref> != x"}` booleans. The
//! emitter checks the form (so the input and output types are exact) and
//! rewrites the keys of step argument objects from wire names to the
//! operation's args keys. A macro that does not fit the form (unknown
//! operation, reference to a later step, malformed expression) is not
//! emitted; [`plan_macros`] reports it as TG0710.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{CommentStyle, Imports, Writer};
use tungsten_ir::naming::{self, Role};
use tungsten_ir::{Macro, Presence, Shape, TypeRef};

use crate::models::{TypeCx, Uses, resolve, write_imports};
use crate::ops::{OpShape, is_arg, safety_str};
use crate::plan::{Plan, unique, with_word};
use crate::ts::{Js, json_lit, paragraphs, prop_key, string_lit};

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

/// A field added to the macro input (`input.add`).
#[derive(Debug, Clone)]
struct AddField<'v> {
    name: &'v str,
    ts: String,
    default: Option<&'v Value>,
}

/// One emittable macro.
#[derive(Debug, Clone)]
pub(crate) struct MacroPlan<'v> {
    m: &'v Macro,
    pub member: String,
    input_type: String,
    output_type: String,
    /// Name of the `MacroDescriptor` constant.
    descriptor: String,
    /// The operation whose args the input extends.
    base: Option<usize>,
    add: Vec<AddField<'v>>,
    steps: Vec<Step<'v>>,
}

/// A parsed reference: `$input...` or `$<step>...`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ref<'v> {
    Input(Vec<&'v str>),
    Step(usize, Vec<&'v str>),
}

/// The emittable macros of `plan`, and a TG0710 warning for each macro that
/// is not emitted.
pub(crate) fn plan_macros<'v>(plan: &Plan<'v>) -> (Vec<MacroPlan<'v>>, Diagnostics) {
    let mut diags = Diagnostics::new();
    let mut parsed = vec![];
    for m in &plan.ir.agent.macros {
        match parse(plan, m) {
            Ok(p) => parsed.push(p),
            Err(reason) => diags.push(
                Diagnostic::warning(
                    "TG0710",
                    format!(
                        "macro `{}` is not emitted in the TypeScript SDK: {reason}",
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
    let members = unique(&[], &member_words, Role::Method);
    let mut type_words = vec![];
    let mut const_words = vec![];
    for name in &members {
        let words = naming::split_words(name);
        type_words.push(with_word(&words, "input"));
        type_words.push(with_word(&words, "output"));
        const_words.push(with_word(&words, "macro"));
    }
    let types = unique(&["Macros"], &type_words, Role::Type);
    let consts = unique(&[], &const_words, Role::Method);
    for (i, (p, member)) in parsed.iter_mut().zip(members).enumerate() {
        p.member = member;
        p.input_type = types[2 * i].clone();
        p.output_type = types[2 * i + 1].clone();
        p.descriptor = consts[i].clone();
    }
    (parsed, diags)
}

fn parse<'v>(plan: &Plan<'v>, m: &'v Macro) -> Result<MacroPlan<'v>, String> {
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
    let add = match input.and_then(|i| i.get("add")) {
        None | Some(Value::Null) => vec![],
        Some(Value::Object(fields)) => fields
            .iter()
            .map(|(name, schema)| AddField {
                name,
                ts: schema_ts(schema),
                default: schema.get("default"),
            })
            .collect(),
        Some(v) => return Err(format!("input `add` is invalid: {v}")),
    };
    Ok(MacroPlan {
        m,
        member: String::new(),
        input_type: String::new(),
        output_type: String::new(),
        descriptor: String::new(),
        base,
        add,
        steps,
    })
}

/// Parse `$input.a.b` / `$name.a.b` against the step names visible at
/// step `upto` (steps before it).
fn parse_ref<'v>(s: &'v str, names: &[Option<&str>], upto: usize) -> Result<Ref<'v>, String> {
    let body = s
        .strip_prefix('$')
        .ok_or(format!("`{s}` is not a reference"))?;
    let mut parts = body.split('.');
    let head = parts.next().unwrap_or("");
    let path: Vec<&str> = parts.collect();
    if path.iter().any(|p| p.is_empty()) {
        return Err(format!("reference `{s}` has an empty path segment"));
    }
    if head == "input" {
        return Ok(Ref::Input(path));
    }
    names[..upto.min(names.len())]
        .iter()
        .position(|n| *n == Some(head))
        .map(|i| Ref::Step(i, path))
        .ok_or(format!("reference `{s}` names no earlier step"))
}

/// A boolean `{expr}`: (reference, operator, right-hand JSON).
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

/// Validate every reference in an expression.
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

/// A JSON Schema of an added input field as a TypeScript type.
fn schema_ts(schema: &Value) -> String {
    let Some(obj) = schema.as_object() else {
        return "unknown".into();
    };
    if let Some(c) = obj.get("const") {
        return crate::ts::json_type(c);
    }
    if let Some(Value::Array(values)) = obj.get("enum") {
        let parts: Vec<String> = values.iter().map(crate::ts::json_type).collect();
        return if parts.is_empty() {
            "never".into()
        } else {
            parts.join(" | ")
        };
    }
    let one = |t: &str| -> String {
        match t {
            "string" => "string".into(),
            "integer" | "number" => "number".into(),
            "boolean" => "boolean".into(),
            "null" => "null".into(),
            "array" => format!(
                "Array<{}>",
                obj.get("items").map_or_else(|| "unknown".into(), schema_ts)
            ),
            "object" => {
                let required: BTreeSet<&str> = obj
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|r| r.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                match obj.get("properties").and_then(Value::as_object) {
                    Some(props) if !props.is_empty() => format!(
                        "{{ {} }}",
                        props
                            .iter()
                            .map(|(k, s)| format!(
                                "{}{}: {}",
                                prop_key(k),
                                if required.contains(k.as_str()) {
                                    ""
                                } else {
                                    "?"
                                },
                                schema_ts(s)
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    _ => "{ [key: string]: unknown }".into(),
                }
            }
            _ => "unknown".into(),
        }
    };
    match obj.get("type") {
        Some(Value::String(t)) => one(t),
        Some(Value::Array(ts)) => {
            let parts: Vec<String> = ts.iter().filter_map(Value::as_str).map(one).collect();
            if parts.is_empty() {
                "unknown".into()
            } else {
                parts.join(" | ")
            }
        }
        _ => "unknown".into(),
    }
}

/// Type computation for one macro's input and output.
struct Gen<'g, 'v> {
    plan: &'g Plan<'v>,
    shapes: &'g [OpShape<'v>],
    mp: &'g MacroPlan<'v>,
    /// Model namespaces the macro's types name.
    uses: &'g mut Uses,
}

impl Gen<'_, '_> {
    fn names(&self) -> Vec<Option<&str>> {
        self.mp.steps.iter().map(|s| s.as_name).collect()
    }

    fn parse<'s>(&self, s: &'s str, upto: usize) -> Option<Ref<'s>> {
        parse_ref(s, &self.names(), upto).ok()
    }

    /// The TypeScript type of a reference. A poll step's value is `null`
    /// when its budget ran out (`MacroStep.until` in the runtime contract).
    fn ref_type(&mut self, r: &Ref<'_>) -> String {
        let cx = TypeCx::outside(self.plan);
        match r {
            Ref::Input(path) => {
                let Some((first, rest)) = path.split_first() else {
                    return self.mp.input_type.clone();
                };
                if let Some(a) = self.mp.add.iter().find(|a| a.name == *first) {
                    return if !rest.is_empty() {
                        "unknown".into()
                    } else if a.default.is_some() {
                        a.ts.clone()
                    } else {
                        format!("{} | undefined", a.ts)
                    };
                }
                let Some(base) = self.mp.base else {
                    return "unknown".into();
                };
                let Some(f) = self.shapes[base].fields.iter().find(|f| f.key == *first) else {
                    return "unknown".into();
                };
                match (rest.is_empty(), &f.ty) {
                    (true, _) => {
                        if f.optional {
                            format!("{} | undefined", f.ts)
                        } else {
                            f.ts.clone()
                        }
                    }
                    (false, Some(ty)) => self.walk(&cx, ty, rest, f.optional || f.nullable),
                    (false, None) => "unknown".into(),
                }
            }
            Ref::Step(i, path) => {
                let step = &self.mp.steps[*i];
                let shape = &self.shapes[step.op];
                if step.kind == StepKind::Paginate {
                    let item = shape
                        .page_item
                        .as_ref()
                        .map_or("unknown".to_string(), |t| t.text.clone());
                    return if path.is_empty() {
                        format!("Array<{item}>")
                    } else {
                        "unknown".into()
                    };
                }
                let polled = step.kind == StepKind::Poll;
                if path.is_empty() {
                    let t = &shape.success.text;
                    return match (polled, t.strip_suffix(" | undefined")) {
                        (false, _) => t.clone(),
                        // Keep `| undefined` last: object members read it.
                        (true, Some(defined)) => format!("{defined} | null | undefined"),
                        (true, None) => format!("{t} | null"),
                    };
                }
                match &shape.success_ref {
                    Some(ty) => self.walk(&cx, ty, path, polled),
                    None => "unknown".into(),
                }
            }
        }
    }

    /// [`walk_type`], recording the namespace of the type reached.
    fn walk(&mut self, cx: &TypeCx<'_, '_>, ty: &TypeRef, path: &[&str], undef: bool) -> String {
        let (text, reached) = walk_type(cx, ty, path, undef);
        if let Some(r) = reached {
            self.uses.add_ref(self.plan, None, &r);
        }
        text
    }

    /// The TypeScript type of the output expression. A reference with a
    /// path that does not resolve reads as `undefined` (dropped from
    /// objects), so it is typed with `| undefined` where it may be missing.
    fn out_type(&mut self, v: &Value, upto: usize) -> String {
        if bool_expr(v).is_some() {
            return "boolean".into();
        }
        match v {
            Value::String(s) if s.starts_with('$') => self
                .parse(s, upto)
                .map_or_else(|| "unknown".into(), |r| self.ref_type(&r)),
            Value::String(_) => "string".into(),
            Value::Number(_) => "number".into(),
            Value::Bool(_) => "boolean".into(),
            Value::Null => "null".into(),
            Value::Array(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(|i| self.out_type(i, upto))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Object(map) if map.is_empty() => "{ [key: string]: never }".into(),
            Value::Object(map) => format!(
                "{{ {} }}",
                map.iter()
                    .map(|(k, i)| {
                        let t = self.out_type(i, upto);
                        // A value that can be undefined is dropped from the
                        // object, so the key is optional.
                        match t.strip_suffix(" | undefined") {
                            Some(rest) => format!("{}?: {rest}", prop_key(k)),
                            None if t == "unknown" => format!("{}?: unknown", prop_key(k)),
                            None => format!("{}: {t}", prop_key(k)),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        }
    }
}

/// The type of the value `get(value, path)` reads from a value of type
/// `ty`: through record fields (wire names), map values and array indexes.
/// `undef` says whether an earlier segment may already be missing. Returns
/// the type text and the reference reached (none when unknown).
fn walk_type(
    cx: &TypeCx<'_, '_>,
    ty: &TypeRef,
    path: &[&str],
    mut undef: bool,
) -> (String, Option<TypeRef>) {
    let unknown = || ("unknown".to_string(), None);
    let mut cur = ty.clone();
    let mut nullable_last = false;
    for seg in path {
        nullable_last = false;
        let mut shape = resolve(cx.plan, &cur).cloned();
        if let Some(Shape::Nullable { inner }) = &shape {
            undef = true;
            cur = inner.clone();
            shape = resolve(cx.plan, &cur).cloned();
        }
        match shape {
            Some(Shape::Record { fields, .. }) => {
                let Some(f) = fields.iter().find(|f| f.wire_name == *seg) else {
                    return unknown();
                };
                match f.presence {
                    Presence::Required => {}
                    Presence::RequiredNullable => nullable_last = true,
                    Presence::Optional => undef = true,
                    Presence::OptionalNullable => {
                        undef = true;
                        nullable_last = true;
                    }
                }
                cur = f.ty.clone();
            }
            Some(Shape::Map { values }) => {
                undef = true;
                cur = values;
            }
            Some(Shape::Array { items, .. }) if seg.chars().all(|c| c.is_ascii_digit()) => {
                undef = true;
                cur = items;
            }
            _ => return unknown(),
        }
    }
    let mut t = cx.ts_ref(&cur).text;
    if nullable_last {
        t.push_str(" | null");
    }
    if undef {
        t.push_str(" | undefined");
    }
    (t, Some(cur))
}

/// Step arguments for the descriptor: keys of an argument object that are
/// a parameter's wire name become that parameter's args key.
fn step_args(shape: &OpShape<'_>, args: &Value) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = shape
                        .params
                        .iter()
                        .find(|p| is_arg(p.param) && p.param.wire_name == *k)
                        .map_or_else(|| k.clone(), |p| p.name.clone());
                    (key, v.clone())
                })
                .collect::<Map<String, Value>>(),
        ),
        Value::Null => Value::Object(Map::new()),
        other => other.clone(),
    }
}

/// The `MacroDescriptor` literal of a macro.
fn descriptor_js(plan: &Plan<'_>, shapes: &[OpShape<'_>], mp: &MacroPlan<'_>) -> Js {
    let m = mp.m;
    let null = || Js::Raw("null".into());
    let opt_u64 = |n: Option<u64>| n.map_or_else(null, Js::num);
    let steps = mp
        .steps
        .iter()
        .map(|s| {
            Js::obj(vec![
                ("kind", Js::str(s.kind.as_str())),
                ("operation", Js::str(&plan.ops[s.op].op.id.0)),
                ("args", Js::json(&step_args(&shapes[s.op], s.args))),
                ("as", Js::opt_str(s.as_name)),
                (
                    "until",
                    s.until
                        .map_or_else(null, |u| Js::json(&Value::Object(u.clone()))),
                ),
                ("interval_ms", opt_u64(s.interval_ms)),
                ("budget_ms", Js::json(s.budget)),
                ("max_pages", opt_u64(s.max_pages)),
            ])
        })
        .collect();
    let add: Map<String, Value> = m
        .input
        .get("add")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    Js::obj(vec![
        ("name", Js::str(&m.name.0)),
        ("summary", Js::str(&m.summary)),
        ("safety", Js::str(safety_str(m.safety))),
        ("steps", Js::Array(steps)),
        ("output", Js::json(&m.output)),
        (
            "input",
            Js::obj(vec![
                (
                    "extends",
                    Js::opt_str(mp.base.map(|b| plan.ops[b].op.id.0.as_str())),
                ),
                ("add", Js::json(&Value::Object(add))),
            ]),
        ),
        (
            "sensitiveResponseFields",
            Js::strs(&m.sensitive_response_fields),
        ),
        ("shownOnce", Js::bool(m.shown_once)),
        ("cluster", Js::opt_str(m.cluster.as_deref())),
    ])
}

/// Whether a macro gets `.preview()`: every macro that is not read-only.
fn has_preview(mp: &MacroPlan<'_>) -> bool {
    mp.m.safety != tungsten_ir::Safety::ReadOnly
}

/// The TSDoc of a macro member: summary, tier and confirmation rule,
/// steps, keys, one-time secrets and cluster.
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
        tungsten_ir::Safety::Irreversible => "Cannot be undone: call `.preview()` with the same input first and pass its `confirmation_token` as `{ confirm }`. The one confirmation covers every step of the run.".to_string(),
        tungsten_ir::Safety::Destructive => "Requires confirmation for the whole run: pass `{ confirm: true }`, or the `confirmation_token` of `.preview()` as `confirm`.".to_string(),
        _ => String::new(),
    };
    let keyed: Vec<String> = mp
        .steps
        .iter()
        .map(|s| plan.ops[s.op].op)
        .filter(|op| op.agent.idempotency.policy == tungsten_ir::IdempotencyKind::CallerOwned)
        .map(|op| format!("`{}`", op.id.0))
        .collect();
    let key = if keyed.is_empty() {
        String::new()
    } else {
        format!(
            "Pass `idempotencyKey` for {}: generate it once, persist it with your intent and reuse it on every retry. Only the first step that takes a key receives it.",
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
        "The first failing step's envelope is returned; its remediation names the steps already completed.".to_string(),
    ])
}

/// The source of `src/macros.ts`.
pub(crate) fn macros_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    macros: &[MacroPlan<'_>],
    header: &str,
) -> String {
    let mut namespaces: BTreeSet<String> = BTreeSet::new();
    let mut types = Writer::new("  ");
    let mut data = Writer::new("  ");
    let mut class = Writer::new("  ");
    let mut any_preview = false;
    let mut uses_input_types = false;
    for mp in macros {
        if let Some(b) = mp.base {
            namespaces.extend(shapes[b].uses.namespaces.iter().cloned());
            uses_input_types = true;
        }
        let mut uses = Uses::default();
        let mut g = Gen {
            plan,
            shapes,
            mp,
            uses: &mut uses,
        };
        write_types(&mut g, &mut types);
        namespaces.extend(uses.namespaces);

        data.blank();
        data.doc(
            CommentStyle::JsDoc,
            &format!("`{}` as data for `ClientCore.runMacro`.", mp.m.name.0),
        );
        let prefix = format!("export const {}: MacroDescriptor = ", mp.descriptor);
        data.line(format!(
            "{prefix}{};",
            descriptor_js(plan, shapes, mp).render("  ", prefix.len())
        ));
        any_preview |= has_preview(mp);
    }
    data.blank();
    data.doc(CommentStyle::JsDoc, "Every macro of the API.");
    let list = Js::Array(
        macros
            .iter()
            .map(|m| Js::Raw(m.descriptor.clone()))
            .collect(),
    );
    let prefix = "export const macroDescriptors: MacroDescriptor[] = ";
    data.line(format!("{prefix}{};", list.render("  ", prefix.len())));

    class.blank();
    class.doc(
        CommentStyle::JsDoc,
        "Multi-step workflows compiled from the agent manifest, run by `ClientCore.runMacro`.",
    );
    class.line("export class Macros {");
    class.indent();
    for mp in macros {
        class.doc(CommentStyle::JsDoc, &macro_doc(plan, mp));
        let input = format!(
            "input{}: {}",
            if input_optional(shapes, mp) { "?" } else { "" },
            mp.input_type
        );
        class.line(format!(
            "readonly {}: (({input}, opts?: CallOptions) => Promise<Result<{}>>) & {{",
            mp.member, mp.output_type
        ));
        class.line("  readonly descriptor: MacroDescriptor;");
        class.line(format!(
            "  readonly safety: {};",
            string_lit(safety_str(mp.m.safety))
        ));
        if has_preview(mp) {
            class.line(format!(
                "  preview({input}, opts?: CallOptions): Promise<Result<PreviewResult>>;"
            ));
        }
        class.line("};");
    }
    class.blank();
    class.line("constructor(core: ClientCoreApi & ClientCoreExtensions) {");
    class.indent();
    for mp in macros {
        let param = if input_optional(shapes, mp) {
            format!("input: {} = {{}}", mp.input_type)
        } else {
            format!("input: {}", mp.input_type)
        };
        let d = &mp.descriptor;
        class.line(format!("this.{} = Object.assign(", mp.member));
        class.line(format!(
            "  ({param}, opts?: CallOptions) => core.runMacro<{}>({d}, input, opts),",
            mp.output_type
        ));
        class.line("  {");
        class.line(format!("    descriptor: {d},"));
        class.line(format!(
            "    safety: {} as const,",
            string_lit(safety_str(mp.m.safety))
        ));
        if has_preview(mp) {
            class.line(format!(
                "    preview: ({param}, opts?: CallOptions) => core.previewMacro({d}, input, opts),"
            ));
        }
        class.line("  },");
        class.line(");");
    }
    class.dedent();
    class.line("}");
    class.dedent();
    class.line("}");

    let mut imports = Imports::new();
    for t in [
        "CallOptions",
        "ClientCoreApi",
        "ClientCoreExtensions",
        "MacroDescriptor",
        "Result",
    ] {
        imports.add_type("@tungsten/runtime", t);
    }
    if any_preview {
        imports.add_type("@tungsten/runtime", "PreviewResult");
    }
    let mut w = Writer::new("  ");
    w.line(header);
    w.blank();
    w.line("// Macros: compiled workflows as data, run by the runtime.");
    w.blank();
    write_imports(&mut w, &imports);
    if uses_input_types {
        w.line("import type * as ops from \"./descriptors.js\";");
    }
    for ns in &namespaces {
        if let Some(m) = plan.model_ns(ns) {
            w.line(format!(
                "import type * as {} from \"./models/{}.js\";",
                m.alias, m.file
            ));
        }
    }
    let mut out = w.finish();
    out.push('\n');
    out.push_str(&types.finish());
    out.push('\n');
    out.push_str(&data.finish());
    out.push('\n');
    out.push_str(&class.finish());
    out
}

/// Whether the input parameter can be omitted: every args key of the
/// extended operation is optional and every added field has a default.
fn input_optional(shapes: &[OpShape<'_>], mp: &MacroPlan<'_>) -> bool {
    mp.base.is_none_or(|b| shapes[b].all_optional) && mp.add.iter().all(|a| a.default.is_some())
}

/// The input and output types of a macro.
fn write_types(g: &mut Gen<'_, '_>, types: &mut Writer) {
    let mp = g.mp;
    types.blank();
    types.doc(CommentStyle::JsDoc, &format!("Input of `{}`.", mp.m.name.0));
    let base = mp.base.map(|b| format!("ops.{}", g.plan.ops[b].args_type));
    if mp.add.is_empty() {
        types.line(format!(
            "export type {} = {};",
            mp.input_type,
            base.as_deref().unwrap_or("{ [key: string]: never }")
        ));
    } else {
        let prefix = base.map(|b| format!("{b} & ")).unwrap_or_default();
        types.line(format!("export type {} = {prefix}{{", mp.input_type));
        types.indent();
        for a in &mp.add {
            if let Some(d) = a.default {
                types.doc(
                    CommentStyle::JsDoc,
                    &format!("@defaultValue `{}` (applied by the runtime)", json_lit(d)),
                );
            }
            types.line(format!(
                "{}{}: {};",
                prop_key(a.name),
                if a.default.is_some() { "?" } else { "" },
                a.ts
            ));
        }
        types.dedent();
        types.line("};");
    }
    types.blank();
    types.doc(
        CommentStyle::JsDoc,
        &format!("Output of `{}`.", mp.m.name.0),
    );
    let n = mp.steps.len();
    let out = g.out_type(&mp.m.output, n);
    types.line(format!("export type {} = {out};", mp.output_type));
}
