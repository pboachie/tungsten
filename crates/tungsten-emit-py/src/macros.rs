// SPDX-License-Identifier: AGPL-3.0-only
//! `<module>/macros.py`: each IR macro as a `MacroDescriptor` constant
//! (data) and a method of `client.macros` (`Macros`, `AsyncMacros`) that
//! hands it to `ClientCore.run_macro`, with `preview_<macro>`
//! (`ClientCore.preview_macro`) for macros that are not read-only. The
//! runtime is the single implementation of macro semantics.
//!
//! Macros arrive in the canonical form documented on `tungsten_ir::Macro`.
//! The emitter checks the form, takes the input as keyword arguments (the
//! extended operation's arguments plus the added fields), and rewrites the
//! keys of step argument objects and the first segment of `$input.<key>`
//! references from wire names to the operation's Python argument names. A
//! macro that does not fit (unknown operation, reference to a later step,
//! malformed expression, an added field that is not a free Python name) is
//! not emitted; [`plan_macros`] reports it as TG0730.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::Writer;
use tungsten_ir::naming::{self, Role};
use tungsten_ir::{IdempotencyKind, Macro, Safety};

use crate::ops::{ARG_RESERVED, ArgField, OpShape, is_arg, safety_str};
use crate::plan::{Plan, no_leading_digit, unique};
use crate::py::{
    Py, PyImports, after_imports, docstring, is_identifier, is_keyword, json_lit, paragraphs,
    two_blank,
};
use crate::resources::{Mode, args_section, call_lines, method_params, signature};

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

/// One emittable macro.
#[derive(Debug, Clone)]
pub(crate) struct MacroPlan<'v> {
    m: &'v Macro,
    pub member: String,
    preview: String,
    /// Name of the `MacroDescriptor` constant.
    descriptor: String,
    /// The operation whose arguments the input extends.
    base: Option<usize>,
    /// Fields added to the input: (name, JSON Schema).
    add: Vec<(&'v str, &'v Value)>,
    steps: Vec<Step<'v>>,
}

/// The emittable macros of `plan`, and a TG0730 warning for each macro that
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
                    "TG0730",
                    format!(
                        "macro `{}` is not emitted in the Python SDK: {reason}",
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
            no_leading_digit(
                &naming::split_words(id.split_once('.').map_or(id, |(_, rest)| rest)),
                "macro",
            )
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
    let names = unique(&["l"], &words, Role::Method);
    let consts = unique(&["MACROS"], &member_words, Role::EnumVariant);
    let mut previews = names[parsed.len()..].iter();
    for (i, p) in parsed.iter_mut().enumerate() {
        p.member = names[i].clone();
        p.descriptor = consts[i].clone();
        if has_preview(p) {
            p.preview = previews.next().cloned().unwrap_or_default();
        }
    }
    (parsed, diags)
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
    let base_keys: Vec<&str> = base
        .map(|b| shapes[b].fields.iter().map(|f| f.key.as_str()).collect())
        .unwrap_or_default();
    let add = match input.and_then(|i| i.get("add")) {
        None | Some(Value::Null) => vec![],
        Some(Value::Object(fields)) => {
            let mut out = vec![];
            for (name, schema) in fields {
                let free = is_identifier(name)
                    && !is_keyword(name)
                    && !name.starts_with('_')
                    && !ARG_RESERVED.contains(&name.as_str())
                    && !base_keys.contains(&name.as_str());
                if !free {
                    return Err(format!(
                        "the added input field `{name}` is not a free Python keyword argument name"
                    ));
                }
                out.push((name.as_str(), schema));
            }
            out
        }
        Some(v) => return Err(format!("input `add` is invalid: {v}")),
    };
    Ok(MacroPlan {
        m,
        member: String::new(),
        preview: String::new(),
        descriptor: String::new(),
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
            Some(crate::ops::BodyPlan {
                shape: crate::ops::BodyShape::Merged(pairs),
                ..
            }) => pairs
                .iter()
                .find(|(_, w)| w == wire)
                .map(|(a, _)| a.as_str()),
            _ => None,
        })
}

/// `$input.<wire>...` with the first segment renamed to the base
/// operation's argument name.
fn rename_input_ref(s: &str, base: Option<&OpShape<'_>>) -> String {
    let Some(rest) = s.strip_prefix("$input.") else {
        return s.to_string();
    };
    let (first, tail) = rest
        .split_once('.')
        .map_or((rest, None), |(f, t)| (f, Some(t)));
    let renamed = base.and_then(|b| arg_key(b, first)).unwrap_or(first);
    match tail {
        Some(t) => format!("$input.{renamed}.{t}"),
        None => format!("$input.{renamed}"),
    }
}

/// An expression with every `$input.<wire>` reference renamed.
fn rename_expr(v: &Value, base: Option<&OpShape<'_>>) -> Value {
    if let Some(e) = bool_expr(v)
        && let Some((left, op, right)) = parse_bool(e)
    {
        let mut map = Map::new();
        map.insert(
            "expr".into(),
            Value::String(format!(
                "{} {op} {}",
                rename_input_ref(left, base),
                right.trim()
            )),
        );
        return Value::Object(map);
    }
    match v {
        Value::String(s) if s.starts_with("$input.") => Value::String(rename_input_ref(s, base)),
        Value::Array(items) => Value::Array(items.iter().map(|i| rename_expr(i, base)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, i)| (k.clone(), rename_expr(i, base)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Step arguments for the descriptor: keys that are an argument's wire name
/// become its Python name, and `$input` references are renamed.
fn step_args(shape: &OpShape<'_>, args: &Value, base: Option<&OpShape<'_>>) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = arg_key(shape, k).map_or_else(|| k.clone(), str::to_string);
                    (key, rename_expr(v, base))
                })
                .collect::<Map<String, Value>>(),
        ),
        Value::Null => Value::Object(Map::new()),
        other => rename_expr(other, base),
    }
}

/// The `MacroDescriptor` literal of a macro.
fn descriptor_py(plan: &Plan<'_>, shapes: &[OpShape<'_>], mp: &MacroPlan<'_>) -> Py {
    let m = mp.m;
    let base = mp.base.map(|b| &shapes[b]);
    let opt_u64 = |n: Option<u64>| n.map_or_else(Py::none, Py::num);
    let steps = mp
        .steps
        .iter()
        .map(|s| {
            Py::dict(vec![
                ("kind", Py::str(s.kind.as_str())),
                ("operation", Py::str(&plan.ops[s.op].op.id.0)),
                ("args", Py::json(&step_args(&shapes[s.op], s.args, base))),
                ("as_", Py::opt_str(s.as_name)),
                (
                    "until",
                    s.until
                        .map_or_else(Py::none, |u| Py::json(&Value::Object(u.clone()))),
                ),
                ("interval_ms", opt_u64(s.interval_ms)),
                ("budget_ms", Py::json(&rename_expr(s.budget, base))),
                ("max_pages", opt_u64(s.max_pages)),
            ])
        })
        .collect();
    let add: Map<String, Value> = mp
        .add
        .iter()
        .map(|(k, v)| (k.to_string(), (*v).clone()))
        .collect();
    Py::dict(vec![
        ("name", Py::str(&m.name.0)),
        ("summary", Py::str(&m.summary)),
        ("safety", Py::str(safety_str(m.safety))),
        ("steps", Py::List(steps)),
        ("output", Py::json(&rename_expr(&m.output, base))),
        (
            "input",
            Py::dict(vec![
                (
                    "extends",
                    Py::opt_str(mp.base.map(|b| plan.ops[b].op.id.0.as_str())),
                ),
                ("add", Py::json(&Value::Object(add))),
            ]),
        ),
        (
            "sensitive_response_fields",
            Py::strs(&m.sensitive_response_fields),
        ),
        ("shown_once", Py::bool(m.shown_once)),
        ("cluster", Py::opt_str(m.cluster.as_deref())),
    ])
}

/// Whether a macro gets `preview_<macro>`: every macro that is not
/// read-only.
fn has_preview(mp: &MacroPlan<'_>) -> bool {
    mp.m.safety != Safety::ReadOnly
}

/// A JSON Schema of an added input field as a Python type.
fn schema_py(schema: &Value) -> String {
    let Some(obj) = schema.as_object() else {
        return "Any".into();
    };
    let literal = |v: &Value| match v {
        Value::Null => Some("None".to_string()),
        Value::String(_) | Value::Bool(_) => Some(format!("Literal[{}]", json_lit(v))),
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(format!("Literal[{n}]")),
        _ => None,
    };
    if let Some(c) = obj.get("const") {
        return literal(c).unwrap_or_else(|| "Any".into());
    }
    if let Some(Value::Array(values)) = obj.get("enum") {
        let parts: Option<Vec<String>> = values.iter().map(literal).collect();
        return match parts {
            Some(p) if !p.is_empty() => p.join(" | "),
            _ => "Any".into(),
        };
    }
    let one = |t: &str| -> String {
        match t {
            "string" => "str".into(),
            "integer" => "int".into(),
            "number" => "float".into(),
            "boolean" => "bool".into(),
            "null" => "None".into(),
            "array" => format!(
                "list[{}]",
                obj.get("items").map_or_else(|| "Any".into(), schema_py)
            ),
            "object" => "dict[str, Any]".into(),
            _ => "Any".into(),
        }
    };
    let text = match obj.get("type") {
        Some(Value::String(t)) => one(t),
        Some(Value::Array(ts)) => {
            let parts: Vec<String> = ts.iter().filter_map(Value::as_str).map(one).collect();
            if parts.is_empty() {
                "Any".into()
            } else {
                parts.join(" | ")
            }
        }
        _ => "Any".into(),
    };
    if text.split(" | ").any(|p| p == "Any") {
        "Any".into()
    } else {
        text
    }
}

/// The keyword arguments of a macro method: the extended operation's
/// arguments, then the added fields (optional when they have a default,
/// which the runtime applies).
fn input_fields(shapes: &[OpShape<'_>], mp: &MacroPlan<'_>) -> Vec<ArgField> {
    let mut fields: Vec<ArgField> = mp
        .base
        .map(|b| shapes[b].fields.clone())
        .unwrap_or_default();
    for (name, schema) in &mp.add {
        let t = schema_py(schema);
        let default = schema.get("default");
        let optional = default.is_some();
        let hint = if optional && t != "Any" {
            format!("{t} | Unset")
        } else {
            t.clone()
        };
        let doc = paragraphs([
            schema
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            default
                .map(|d| format!("Default (applied by the runtime): `{}`.", json_lit(d)))
                .unwrap_or_default(),
        ]);
        fields.push(ArgField {
            key: name.to_string(),
            hint,
            schema: t,
            optional,
            json: true,
            doc,
        });
    }
    fields
}

/// The docstring of a macro method: summary, tier and confirmation rule,
/// steps, keys, one-time secrets and cluster.
fn macro_doc(plan: &Plan<'_>, mp: &MacroPlan<'_>, fields: &[ArgField]) -> String {
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
            "Cannot be undone: call `{}()` with the same input first and pass its `confirmation_token` as `opts={{\"confirm\": token}}`. The one confirmation covers every step of the run.",
            mp.preview
        ),
        Safety::Destructive => format!(
            "Requires confirmation for the whole run: pass `opts={{\"confirm\": True}}`, or the `confirmation_token` of `{}()` as `confirm`.",
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
            "Pass `opts={{\"idempotency_key\": key}}` for {}: generate it once, persist it with your intent and reuse it on every retry. Only the first step that takes a key receives it.",
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
        "The value is the macro's output expression evaluated by the runtime. The first failing step's envelope is returned; its remediation names the steps already completed.".to_string(),
        args_section(fields),
    ])
}

/// The source of `<module>/macros.py`.
pub(crate) fn macros_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    macros: &[MacroPlan<'_>],
    header: &str,
) -> String {
    let mut imports = PyImports::default();
    imports.add("__future__", "annotations");
    imports.add("typing", "Any");
    for n in [
        "AsyncClientCore",
        "CallOptions",
        "ClientCore",
        "MacroDescriptor",
        "Result",
    ] {
        imports.add("tungsten_runtime", n);
    }
    let mut namespaces: BTreeSet<String> = BTreeSet::new();
    let mut typing: BTreeSet<&str> = BTreeSet::new();
    let mut inputs: Vec<Vec<ArgField>> = vec![];
    for mp in macros {
        if let Some(b) = mp.base {
            namespaces.extend(shapes[b].hint_uses.namespaces.iter().cloned());
            typing.extend(shapes[b].hint_uses.typing.iter().copied());
        }
        let fields = input_fields(shapes, mp);
        for f in &fields {
            if f.hint.contains("Literal[") {
                typing.insert("Literal");
            }
        }
        if fields.iter().any(|f| f.optional) {
            imports.add("tungsten_runtime", "UNSET");
            imports.add("tungsten_runtime", "Unset");
            imports.add(".", "_internal");
        }
        if has_preview(mp) {
            imports.add("tungsten_runtime", "PreviewResult");
        }
        inputs.push(fields);
    }
    for t in typing {
        imports.add("typing", t);
    }
    for ns in &namespaces {
        if let Some(m) = plan.model_ns(ns) {
            imports.add_as(".models", &m.file, &m.alias);
        }
    }

    let mut head = Writer::new("    ");
    head.line(header);
    head.blank();
    docstring(
        &mut head,
        "Macros: compiled workflows as data, run by the runtime.",
    );
    head.blank();
    head.line(imports.render());
    let mut w = Writer::new("    ");
    for mp in macros {
        two_blank(&mut w);
        let prefix = format!("{}: MacroDescriptor = ", mp.descriptor);
        w.line(format!(
            "{prefix}{}",
            descriptor_py(plan, shapes, mp).render(prefix.len())
        ));
        docstring(
            &mut w,
            &format!("`{}` as data for `ClientCore.run_macro`.", mp.m.name.0),
        );
    }
    two_blank(&mut w);
    let list = Py::List(
        macros
            .iter()
            .map(|m| Py::Raw(m.descriptor.clone()))
            .collect(),
    );
    let prefix = "MACROS: list[MacroDescriptor] = ";
    w.line(format!("{prefix}{}", list.render(prefix.len())));
    docstring(&mut w, "Every macro of the API.");

    for mode in [Mode::Sync, Mode::Async] {
        two_blank(&mut w);
        let (class, core) = match mode {
            Mode::Sync => ("Macros", "ClientCore"),
            Mode::Async => ("AsyncMacros", "AsyncClientCore"),
        };
        w.line(format!("class {class}:"));
        w.indent();
        docstring(
            &mut w,
            "Multi-step workflows compiled from the agent manifest, run by the runtime.",
        );
        w.blank();
        w.line(format!("def __init__(self, core: {core}) -> None:"));
        w.indent();
        w.line("self._core = core");
        w.dedent();
        for (mp, fields) in macros.iter().zip(&inputs) {
            let params = method_params(fields);
            w.blank();
            for l in signature(mode, &mp.member, &params, "Result[Any]", 4) {
                w.line(l);
            }
            w.indent();
            docstring(&mut w, &macro_doc(plan, mp, fields));
            for l in call_lines(mode, "run_macro", &mp.descriptor, fields, 8) {
                w.line(l);
            }
            w.dedent();
            if has_preview(mp) {
                w.blank();
                for l in signature(mode, &mp.preview, &params, "Result[PreviewResult]", 4) {
                    w.line(l);
                }
                w.indent();
                docstring(
                    &mut w,
                    &paragraphs([
                        format!(
                            "Preview `{}` without sending anything: every step's request and effects, and the `confirmation_token` to pass to `{}()` when the run needs one.",
                            mp.m.name.0, mp.member
                        ),
                        args_section(fields),
                    ]),
                );
                for l in call_lines(mode, "preview_macro", &mp.descriptor, fields, 8) {
                    w.line(l);
                }
                w.dedent();
            }
        }
        w.dedent();
    }
    let mut out = head.finish();
    let text = w.finish();
    let text = text.trim_start_matches('\n');
    out.push_str(after_imports(text));
    out.push_str(text);
    out
}
