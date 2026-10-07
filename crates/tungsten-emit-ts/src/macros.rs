// SPDX-License-Identifier: AGPL-3.0-only
//! `src/macros.ts`: one async method per IR macro, exposed as
//! `client.macros.<name>`.
//!
//! Macros arrive in the canonical form documented on `tungsten_ir::Macro`:
//! steps (`call`, `poll`, `paginate`) over operation ids, expressions in
//! which `$input`, `$<as>` and their dotted paths are references, and
//! `{expr: "<ref> in [..]" | "<ref> == x" | "<ref> != x"}` booleans. A
//! macro that does not fit the form (unknown operation, reference to a
//! later step, malformed expression) is not emitted; [`plan_macros`]
//! reports it as TG0710.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{CommentStyle, Imports, Writer};
use tungsten_ir::naming::{self, Role};
use tungsten_ir::{Macro, Presence, Shape, TypeRef};

use crate::models::{TypeCx, Uses, resolve, write_imports};
use crate::ops::{OpShape, is_arg, safety_str};
use crate::plan::{Plan, unique, with_word};
use crate::ts::{json_lit, paragraphs, prop_key, string_lit};

/// Poll interval when a poll step gives none.
const DEFAULT_INTERVAL_MS: u64 = 1000;
/// Poll budget when a poll step gives none.
const DEFAULT_BUDGET_MS: u64 = 60000;

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
    for name in &members {
        let words = naming::split_words(name);
        type_words.push(with_word(&words, "input"));
        type_words.push(with_word(&words, "output"));
    }
    let types = unique(&[], &type_words, Role::Type);
    for (i, (p, member)) in parsed.iter_mut().zip(members).enumerate() {
        p.member = member;
        p.input_type = types[2 * i].clone();
        p.output_type = types[2 * i + 1].clone();
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

/// Code generation state for one macro.
struct Gen<'g, 'v> {
    plan: &'g Plan<'v>,
    shapes: &'g [OpShape<'v>],
    mp: &'g MacroPlan<'v>,
    input_var: &'static str,
    helpers: &'g mut BTreeSet<&'static str>,
    /// Model namespaces the macro's types name.
    uses: &'g mut Uses,
}

impl Gen<'_, '_> {
    fn names(&self) -> Vec<Option<&str>> {
        self.mp.steps.iter().map(|s| s.as_name).collect()
    }

    /// The JavaScript value of a reference (typed when it has no path).
    fn ref_js(&mut self, r: &Ref<'_>) -> String {
        let (base, path) = match r {
            Ref::Input(p) => (self.input_var.to_string(), p),
            Ref::Step(i, p) => {
                let base = if self.mp.steps[*i].kind == StepKind::Paginate {
                    format!("step{i}")
                } else {
                    format!("step{i}.value")
                };
                (base, p)
            }
        };
        if path.is_empty() {
            base
        } else {
            self.helpers.insert("get");
            format!(
                "get({base}, [{}])",
                path.iter()
                    .map(|p| string_lit(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }

    fn parse<'s>(&self, s: &'s str, upto: usize) -> Option<Ref<'s>> {
        parse_ref(s, &self.names(), upto).ok()
    }

    /// A boolean expression.
    fn bool_js(&mut self, e: &str, upto: usize) -> String {
        let Some((left, op, right)) = parse_bool(e) else {
            return "false".into();
        };
        let Some(r) = self.parse(left, upto) else {
            return "false".into();
        };
        let value = self.ref_js(&r);
        match op {
            "in" => {
                self.helpers.insert("isIn");
                format!("isIn({value}, {})", json_lit(&right))
            }
            "==" => {
                self.helpers.insert("jsonEqual");
                format!("jsonEqual({value}, {})", json_lit(&right))
            }
            _ => {
                self.helpers.insert("jsonEqual");
                format!("!jsonEqual({value}, {})", json_lit(&right))
            }
        }
    }

    /// An expression as JavaScript; with `typed`, leaves read through a
    /// path are cast to their resolved type (for the output).
    fn expr_js(&mut self, v: &Value, upto: usize, typed: bool) -> String {
        if let Some(e) = bool_expr(v) {
            return self.bool_js(e, upto);
        }
        match v {
            Value::String(s) if s.starts_with('$') => {
                let Some(r) = self.parse(s, upto) else {
                    return "undefined".into();
                };
                let js = self.ref_js(&r);
                let has_path = matches!(&r, Ref::Input(p) | Ref::Step(_, p) if !p.is_empty());
                if typed && has_path {
                    format!("({js} as {})", self.ref_type(&r))
                } else {
                    js
                }
            }
            Value::Array(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(|i| self.expr_js(i, upto, typed))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Object(map) if map.is_empty() => "{}".into(),
            Value::Object(map) => format!(
                "{{ {} }}",
                map.iter()
                    .map(|(k, i)| format!("{}: {}", prop_key(k), self.expr_js(i, upto, typed)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => json_lit(v),
        }
    }

    /// The args object of step `i`. Object keys that are a parameter's wire
    /// name become that parameter's args key.
    fn args_js(&mut self, i: usize) -> String {
        let step = &self.mp.steps[i];
        let shape = &self.shapes[step.op];
        match step.args {
            Value::String(s) if s == "$input" => {
                if self.mp.add.is_empty() {
                    self.input_var.to_string()
                } else {
                    self.helpers.insert("omit");
                    format!(
                        "omit({}, [{}])",
                        self.input_var,
                        self.mp
                            .add
                            .iter()
                            .map(|a| string_lit(a.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Value::String(s) if s.starts_with('$') => {
                self.helpers.insert("asArgs");
                let inner = self.expr_js(step.args, i, false);
                format!("asArgs({inner})")
            }
            Value::Object(map) if !map.is_empty() => {
                let parts: Vec<String> = map
                    .iter()
                    .map(|(k, v)| {
                        let key = shape
                            .params
                            .iter()
                            .find(|p| is_arg(p.param) && p.param.wire_name == *k)
                            .map_or(k.as_str(), |p| p.name.as_str());
                        format!("{}: {}", prop_key(key), self.expr_js(v, i, false))
                    })
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            _ => "{}".into(),
        }
    }

    /// The TypeScript type of a reference.
    fn ref_type(&mut self, r: &Ref<'_>) -> String {
        let cx = TypeCx::outside(self.plan);
        match r {
            Ref::Input(path) => {
                let Some((first, rest)) = path.split_first() else {
                    return self.mp.input_type.clone();
                };
                if let Some(a) = self.mp.add.iter().find(|a| a.name == *first) {
                    return if rest.is_empty() {
                        format!("{} | undefined", a.ts)
                    } else {
                        "unknown".into()
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
                if path.is_empty() {
                    return shape.success.text.clone();
                }
                match &shape.success_ref {
                    Some(ty) => self.walk(&cx, ty, path, false),
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

    /// The TypeScript type of the output expression.
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
                    .map(|(k, i)| format!("{}: {}", prop_key(k), self.out_type(i, upto)))
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

/// The source of `src/macros.ts`, and the `internal.ts` helpers it uses.
pub(crate) fn macros_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    macros: &[MacroPlan<'_>],
    header: &str,
) -> (String, BTreeSet<&'static str>) {
    let mut helpers: BTreeSet<&'static str> = BTreeSet::new();
    let mut namespaces: BTreeSet<String> = BTreeSet::new();
    let mut types = Writer::new("  ");
    let mut class = Writer::new("  ");
    class.doc(
        CommentStyle::JsDoc,
        "Multi-step workflows compiled from the agent manifest.",
    );
    class.line("export class Macros {");
    class.indent();
    class.line("readonly #core: ClientCoreApi;");
    class.blank();
    class.line("constructor(core: ClientCoreApi) {");
    class.line("  this.#core = core;");
    class.line("}");
    for mp in macros {
        for s in &mp.steps {
            namespaces.extend(shapes[s.op].result_namespaces.iter().cloned());
        }
        if let Some(b) = mp.base {
            namespaces.extend(shapes[b].uses.namespaces.iter().cloned());
        }
        let mut uses = Uses::default();
        write_macro(
            plan,
            shapes,
            mp,
            &mut types,
            &mut class,
            &mut helpers,
            &mut uses,
        );
        namespaces.extend(uses.namespaces);
    }
    class.dedent();
    class.line("}");

    let mut imports = Imports::new();
    for t in ["CallOptions", "ClientCoreApi", "Result"] {
        imports.add_type("@tungsten/runtime", t);
    }
    for h in &helpers {
        imports.add("./internal.js", h);
    }
    let mut w = Writer::new("  ");
    w.line(header);
    w.blank();
    write_imports(&mut w, &imports);
    w.line("import * as ops from \"./descriptors.js\";");
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
    out.push_str(&class.finish());
    (out, helpers)
}

fn write_macro(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    mp: &MacroPlan<'_>,
    types: &mut Writer,
    class: &mut Writer,
    helpers: &mut BTreeSet<&'static str>,
    uses: &mut Uses,
) {
    let has_defaults = mp.add.iter().any(|a| a.default.is_some());
    let mut g = Gen {
        plan,
        shapes,
        mp,
        input_var: if has_defaults { "inputs" } else { "input" },
        helpers,
        uses,
    };

    // Input type.
    types.doc(CommentStyle::JsDoc, &format!("Input of `{}`.", mp.m.name.0));
    let base = mp.base.map(|b| format!("ops.{}", plan.ops[b].args_type));
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
                    &format!("@defaultValue `{}`", json_lit(d)),
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
    types.line(format!(
        "export type {} = {};",
        mp.output_type,
        g.out_type(&mp.m.output, n)
    ));
    types.blank();

    // Method.
    let steps_doc: Vec<String> = mp
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "{}. `{}` `{}`",
                i + 1,
                s.kind.as_str(),
                plan.ops[s.op].op.id.0
            )
        })
        .collect();
    class.blank();
    class.doc(
        CommentStyle::JsDoc,
        &paragraphs([
            mp.m.summary.clone(),
            format!("Safety: `{}`.", safety_str(mp.m.safety)),
            format!("Steps:\n{}", steps_doc.join("\n")),
            if n > 1 {
                "`opts` apply to the first step; later steps receive only its `signal`, `timeoutMs` and `headers`. The first failing step's result is returned.".to_string()
            } else {
                "The step's failure result is returned as is.".to_string()
            },
        ]),
    );
    let input_optional = mp.base.is_none_or(|b| shapes[b].all_optional)
        && mp.add.iter().all(|a| a.default.is_some());
    class.line(format!(
        "async {}(input: {}{}, opts?: CallOptions): Promise<Result<{}>> {{",
        mp.member,
        mp.input_type,
        if input_optional { " = {}" } else { "" },
        mp.output_type
    ));
    class.indent();
    class.line("const core = this.#core;");
    if has_defaults {
        let defaults: Vec<String> = mp
            .add
            .iter()
            .filter_map(|a| {
                a.default
                    .map(|d| format!("{}: {}", prop_key(a.name), json_lit(d)))
            })
            .collect();
        class.line(format!(
            "const inputs: {} = {{ {}, ...input }};",
            mp.input_type,
            defaults.join(", ")
        ));
    }
    if n > 1 {
        g.helpers.insert("laterOptions");
        class.line("const later = laterOptions(opts);");
    }
    let mut last_meta = String::new();
    for i in 0..n {
        let step = &mp.steps[i];
        let info = &plan.ops[step.op];
        let shape = &shapes[step.op];
        let d = format!("ops.{}", info.key);
        let o = if i == 0 { "opts" } else { "later" };
        let args = g.args_js(i);
        match step.kind {
            StepKind::Call => {
                class.line(format!(
                    "const step{i} = await core.call<{}>({d}, {args}, {o});",
                    shape.success.text
                ));
                class.line(format!("if (!step{i}.ok) return step{i};"));
                last_meta = format!("step{i}.meta");
            }
            StepKind::Poll => {
                let budget = match step.budget {
                    Value::Number(b) => b.to_string(),
                    Value::String(_) => {
                        let expr = g.expr_js(step.budget, i, false);
                        g.helpers.insert("invalidInput");
                        class.line(format!("const budget{i} = {expr};"));
                        class.line(format!("if (typeof budget{i} !== \"number\") {{"));
                        class.line(format!(
                            "  return invalidInput({}, {}, budget{i}, \"a number of milliseconds\");",
                            string_lit(&mp.m.name.0),
                            string_lit(step.budget.as_str().unwrap_or("budget_ms").trim_start_matches("$input."))
                        ));
                        class.line("}");
                        format!("budget{i}")
                    }
                    _ => DEFAULT_BUDGET_MS.to_string(),
                };
                let until = step
                    .until
                    .map_or_else(|| "{}".to_string(), |u| json_lit(&Value::Object(u.clone())));
                class.line(format!(
                    "const step{i} = await core.poll<{}>({d}, {args}, {until}, {}, {budget}, {o});",
                    shape.success.text,
                    step.interval_ms.unwrap_or(DEFAULT_INTERVAL_MS)
                ));
                class.line(format!("if (!step{i}.ok) return step{i};"));
                last_meta = format!("step{i}.meta");
            }
            StepKind::Paginate => {
                let item = shape
                    .page_item
                    .as_ref()
                    .map_or_else(|| "unknown".to_string(), |t| t.text.clone());
                g.helpers.insert("noMeta");
                class.line(format!("const step{i}: Array<{item}> = [];"));
                class.line(format!("let meta{i} = noMeta();"));
                if step.max_pages.is_some() {
                    class.line(format!("let pages{i} = 0;"));
                }
                class.line(format!(
                    "for await (const page of core.pages<{item}>({d}, {args}, {o})) {{"
                ));
                class.line("  if (!page.ok) return page;");
                class.line(format!("  step{i}.push(...page.value.items);"));
                class.line(format!("  meta{i} = page.meta;"));
                if let Some(max) = step.max_pages {
                    class.line(format!("  pages{i} += 1;"));
                    class.line(format!("  if (pages{i} >= {max}) break;"));
                }
                class.line("}");
                last_meta = format!("meta{i}");
            }
        }
    }
    let output = g.expr_js(&mp.m.output, n, true);
    class.line(format!(
        "return {{ ok: true, value: {output}, meta: {last_meta} }};"
    ));
    class.dedent();
    class.line("}");
}
