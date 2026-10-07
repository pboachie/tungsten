// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten explain`: a diagnostic code, an operation or a type.

use std::fmt::Write as _;

use serde::Serialize;
use tungsten_core::diagnostic::codes::REGISTRY;
use tungsten_ir::{
    Additional, Ir, NamedType, Operation, OperationStatus, Param, Primitive, Shape, StatusMatch,
    StringFormat, TypeRef, UnionStrategy,
};

use crate::args::ExplainArgs;
use crate::explain_table::{area, explanation};
use crate::output::{CliError, CommandName, CommandResult, ErrorKind, ExplainResult};
use crate::{Report, exit, input};

/// Width of the key column in human output.
const KEY_WIDTH: usize = 11;
/// Wrap width for explanation prose.
const WRAP: usize = 78;
/// Enum values shown before eliding.
const MAX_ENUM_VALUES: usize = 8;
/// Suggestions offered for an unknown id.
const MAX_SUGGESTIONS: usize = 3;

pub(crate) fn run(args: &ExplainArgs) -> Report {
    let report = Report::new(CommandName::Explain);
    if is_code(&args.target) {
        return finish(report, explain_code(&args.target.to_ascii_uppercase()));
    }
    let compiled = input::compile(&args.project);
    let mut report = report;
    report.diagnostics = compiled.diagnostics.0;
    report.sources = compiled.workspace.sources;
    let Some(ir) = compiled.ir else {
        report.exit = exit::FAILED;
        return report;
    };
    // The project's own warnings are not what was asked about.
    report.diagnostics.clear();
    finish(report, explain_in_ir(&ir, &args.target))
}

fn finish(mut report: Report, outcome: Result<(ExplainResult, String), CliError>) -> Report {
    match outcome {
        Ok((result, human)) => {
            report.result = Some(CommandResult::Explain(result));
            report.human = human;
            report
        }
        Err(err) => report.failed(exit::FAILED, err),
    }
}

/// `TG` followed by four digits, in any letter case.
fn is_code(target: &str) -> bool {
    target.len() == 6
        && target.is_ascii()
        && target[..2].eq_ignore_ascii_case("TG")
        && target[2..].bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn explain_code(code: &str) -> Result<(ExplainResult, String), CliError> {
    let Some(&(_, summary)) = REGISTRY.iter().find(|(c, _)| *c == code) else {
        let prefix = &code[..4];
        let siblings: Vec<&str> = REGISTRY
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| c.starts_with(prefix))
            .collect();
        let mut err = CliError::new(
            ErrorKind::NotFound,
            format!("unknown diagnostic code {code}"),
        );
        if !siblings.is_empty() {
            err = err.with_help(format!(
                "known codes in this range: {}",
                siblings.join(", ")
            ));
        }
        return Err(err);
    };
    let (meaning, fix) = match explanation(code) {
        Some(e) => (e.meaning, e.fix),
        None => (
            summary,
            "No extended explanation is available for this code.",
        ),
    };
    let area = area(code);
    let mut human = format!("{code}: {summary}\n");
    row(&mut human, "area", area);
    human.push_str("\nmeaning\n");
    human.push_str(&wrap(meaning, 2, WRAP));
    human.push_str("\nfix\n");
    human.push_str(&wrap(fix, 2, WRAP));
    Ok((
        ExplainResult::DiagnosticCode {
            code: code.to_string(),
            summary: summary.to_string(),
            area: area.to_string(),
            meaning: meaning.to_string(),
            fix: fix.to_string(),
        },
        human,
    ))
}

/// An operation found in the IR with where it lives.
struct FoundOp<'a> {
    op: &'a Operation,
    /// `namespace.resource.child`; `None` for planned operations.
    resource: Option<String>,
}

impl FoundOp<'_> {
    /// Alternative id `namespace.resource.method`.
    fn path_id(&self) -> Option<String> {
        self.resource
            .as_ref()
            .map(|r| format!("{r}.{}", self.op.name.wire))
    }
}

fn all_operations(ir: &Ir) -> Vec<FoundOp<'_>> {
    fn walk<'a>(r: &'a tungsten_ir::Resource, prefix: &str, out: &mut Vec<FoundOp<'a>>) {
        let path = format!("{prefix}.{}", r.name.wire);
        out.extend(r.operations.iter().map(|op| FoundOp {
            op,
            resource: Some(path.clone()),
        }));
        r.children.iter().for_each(|c| walk(c, &path, out));
    }
    let mut out = vec![];
    for ns in &ir.namespaces {
        ns.resources
            .iter()
            .for_each(|r| walk(r, &ns.name.wire, &mut out));
        out.extend(ns.planned.iter().map(|op| FoundOp { op, resource: None }));
    }
    out
}

enum Hit<'a> {
    Op(FoundOp<'a>),
    Type(&'a NamedType),
}

impl Hit<'_> {
    fn id(&self) -> &str {
        match self {
            Hit::Op(f) => &f.op.id.0,
            Hit::Type(t) => &t.id.0,
        }
    }
}

/// Look up an operation or type id in an IR and explain it.
pub(crate) fn explain_in_ir(ir: &Ir, target: &str) -> Result<(ExplainResult, String), CliError> {
    let hit = find(ir, target)?;
    match hit {
        Hit::Op(found) => {
            let fragment = to_value(found.op)?;
            let human = describe_operation(&found);
            Ok((
                ExplainResult::Operation {
                    id: found.op.id.0.clone(),
                    planned: found.resource.is_none(),
                    resource: found.resource,
                    fragment,
                },
                human,
            ))
        }
        Hit::Type(t) => Ok((
            ExplainResult::Type {
                id: t.id.0.clone(),
                fragment: to_value(t)?,
            },
            describe_type(t),
        )),
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<serde_json::Value, CliError> {
    serde_json::to_value(v).map_err(|err| {
        CliError::new(
            ErrorKind::Internal,
            format!("IR fragment could not be serialized: {err}"),
        )
    })
}

/// Exact ids first (operation id, `namespace.resource.method`, type id);
/// then a bare operationId or type name when exactly one item has it.
fn find<'a>(ir: &'a Ir, target: &str) -> Result<Hit<'a>, CliError> {
    let mut ops = all_operations(ir);
    if let Some(i) = ops
        .iter()
        .position(|f| f.op.id.0 == target || f.path_id().as_deref() == Some(target))
    {
        return Ok(Hit::Op(ops.swap_remove(i)));
    }
    if let Some(t) = ir.types.types.iter().find(|t| t.id.0 == target) {
        return Ok(Hit::Type(t));
    }
    let suffix = format!(".{target}");
    let mut hits: Vec<Hit<'a>> = vec![];
    let all_ids: Vec<String> = ops
        .iter()
        .map(|f| f.op.id.0.clone())
        .chain(ir.types.types.iter().map(|t| t.id.0.clone()))
        .collect();
    for found in ops {
        if found.op.operation_id.as_deref() == Some(target) || found.op.id.0.ends_with(&suffix) {
            hits.push(Hit::Op(found));
        }
    }
    hits.extend(
        ir.types
            .types
            .iter()
            .filter(|t| t.name.wire == target || t.id.0.ends_with(&suffix))
            .map(Hit::Type),
    );
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => {
            let mut err = CliError::new(
                ErrorKind::NotFound,
                format!("no diagnostic code, operation or type named `{target}`"),
            );
            let close = suggestions(target, &all_ids);
            if !close.is_empty() {
                err = err.with_help(format!("did you mean {}?", close.join(", ")));
            }
            Err(err)
        }
        _ => {
            let mut ids: Vec<&str> = hits.iter().map(Hit::id).collect();
            ids.sort_unstable();
            Err(
                CliError::new(ErrorKind::NotFound, format!("`{target}` is ambiguous"))
                    .with_help(format!("use a full id: {}", ids.join(", "))),
            )
        }
    }
}

/// Ids within a small edit distance of `target`, closest first.
fn suggestions(target: &str, ids: &[String]) -> Vec<String> {
    let limit = (target.chars().count() / 4).max(2);
    let mut scored: Vec<(usize, &String)> = ids
        .iter()
        .map(|id| (edit_distance(target, id), id))
        .filter(|(d, _)| *d <= limit)
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.1 == b.1);
    scored
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, id)| id.clone())
        .collect()
}

/// Levenshtein distance over characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, &cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != cb);
            cur[j + 1] = substitute.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

fn describe_operation(found: &FoundOp<'_>) -> String {
    let op = found.op;
    let mut out = format!("operation {}\n", op.id.0);
    row(
        &mut out,
        "method",
        &format!("{} {}", tag(&op.method), op.path.raw),
    );
    match &found.resource {
        Some(r) => row(
            &mut out,
            "resource",
            &format!("{r} · method {}", op.name.wire),
        ),
        None => row(&mut out, "resource", "none (planned, not callable)"),
    }
    let status = match &op.status {
        OperationStatus::Implemented => "implemented".to_string(),
        OperationStatus::Planned { reason } => format!("planned: {reason}"),
        OperationStatus::Gated { gate } => format!(
            "gated by {} (default {}, {} when off)",
            gate.env_var,
            if gate.default_on { "on" } else { "off" },
            gate.disabled_status
        ),
    };
    row(&mut out, "status", &status);
    row(&mut out, "safety", &tag(&op.agent.safety));
    if op.deprecated {
        row(&mut out, "deprecated", "yes");
    }
    if let Some(summary) = op.doc.as_ref().and_then(|d| d.summary.as_deref()) {
        row(&mut out, "summary", summary);
    }
    for (key, params) in [
        ("path", &op.params.path),
        ("query", &op.params.query),
        ("header", &op.params.header),
        ("cookie", &op.params.cookie),
    ] {
        if !params.is_empty() {
            let lines: Vec<String> = params.iter().map(param_line).collect();
            row(&mut out, key, &lines.join("\n"));
        }
    }
    if let Some(body) = &op.body {
        let mut lines: Vec<String> = body
            .content
            .iter()
            .map(|c| format!("{}: {}", c.media_type, type_label(&c.ty)))
            .collect();
        if lines.is_empty() {
            lines.push("no content".into());
        }
        if body.required {
            lines[0].push_str(" (required)");
        }
        row(&mut out, "body", &lines.join("\n"));
    }
    if !op.responses.is_empty() {
        let lines: Vec<String> = op
            .responses
            .iter()
            .map(|r| {
                let status = status_label(r.status);
                if r.content.is_empty() {
                    format!("{status} no body")
                } else {
                    let content: Vec<String> = r
                        .content
                        .iter()
                        .map(|c| format!("{}: {}", c.media_type, type_label(&c.ty)))
                        .collect();
                    format!("{status} {}", content.join(", "))
                }
            })
            .collect();
        row(&mut out, "responses", &lines.join("\n"));
    }
    let security = if op.security.is_empty() {
        "none".to_string()
    } else {
        op.security
            .iter()
            .map(|req| {
                req.all_of
                    .iter()
                    .map(|s| s.scheme.as_str())
                    .collect::<Vec<_>>()
                    .join(" + ")
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    row(&mut out, "security", &security);
    if let Some(p) = &op.pagination {
        let how = if p.inferred { "inferred" } else { "declared" };
        row(
            &mut out,
            "pagination",
            &format!("{} over `{}` ({how})", tag_kind(&p.style), p.items_field),
        );
    }
    row(&mut out, "source", &source_label(&op.source));
    out
}

fn param_line(p: &Param) -> String {
    let required = if p.required { "required" } else { "optional" };
    format!("{}: {} ({required})", p.wire_name, type_label(&p.ty))
}

fn describe_type(t: &NamedType) -> String {
    let mut out = format!("type {}\n", t.id.0);
    row(&mut out, "name", &t.name.wire);
    row(&mut out, "kind", &shape_kind(&t.shape));
    match &t.shape {
        Shape::Record { fields, .. } if !fields.is_empty() => {
            let lines: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}: {} ({})",
                        f.wire_name,
                        type_label(&f.ty),
                        tag(&f.presence).replace('_', " ")
                    )
                })
                .collect();
            row(&mut out, "fields", &lines.join("\n"));
        }
        Shape::Enum { values, .. } => {
            row(
                &mut out,
                "values",
                &enum_values(values.iter().map(|v| &v.value)),
            );
        }
        Shape::Union(u) => {
            let lines: Vec<String> = u
                .variants
                .iter()
                .map(|v| match &v.tag {
                    Some(tag) => format!("{} = {}: {}", tag, v.name.wire, type_label(&v.ty)),
                    None => format!("{}: {}", v.name.wire, type_label(&v.ty)),
                })
                .collect();
            row(&mut out, "variants", &lines.join("\n"));
        }
        Shape::Record { .. } => {}
        other => row(&mut out, "shape", &shape_label(other)),
    }
    if t.recursive {
        row(&mut out, "recursive", "yes");
    }
    if let Some(summary) = t.doc.as_ref().and_then(|d| d.summary.as_deref()) {
        row(&mut out, "summary", summary);
    }
    row(&mut out, "source", &source_label(&t.origin));
    out
}

fn shape_kind(shape: &Shape) -> String {
    match shape {
        Shape::Record { additional, .. } => match additional {
            Additional::Closed => "record (closed)".into(),
            Additional::Open => "record (open)".into(),
            Additional::Typed { values } => {
                format!("record (extra values: {})", type_label(values))
            }
        },
        Shape::Enum { base, .. } => format!("enum of {}", primitive_label(base)),
        Shape::Union(u) => match (&u.strategy, &u.discriminator) {
            (UnionStrategy::Tagged, Some(d)) => format!("union tagged on `{}`", d.property),
            (strategy, _) => format!("union ({})", tag(strategy)),
        },
        Shape::Primitive { .. } => "primitive".into(),
        Shape::Const { .. } => "const".into(),
        Shape::Array { .. } => "array".into(),
        Shape::Map { .. } => "map".into(),
        Shape::Intersection { .. } => "intersection".into(),
        Shape::Nullable { .. } => "nullable".into(),
        Shape::Any => "any".into(),
        Shape::Never => "never".into(),
    }
}

/// Compact rendering of a type reference: named types by id, inline
/// shapes structurally.
fn type_label(r: &TypeRef) -> String {
    match r {
        TypeRef::Named(id) => id.0.clone(),
        TypeRef::Inline(shape) => shape_label(shape),
    }
}

fn shape_label(shape: &Shape) -> String {
    match shape {
        Shape::Primitive { primitive, .. } => primitive_label(primitive),
        Shape::Enum { values, .. } => {
            format!("enum({})", enum_values(values.iter().map(|v| &v.value)))
        }
        Shape::Const { value } => format!("const {value}"),
        Shape::Array { items, .. } => format!("array<{}>", type_label(items)),
        Shape::Map { values } => format!("map<{}>", type_label(values)),
        Shape::Record { fields, .. } => format!(
            "record{{{}}}",
            fields
                .iter()
                .map(|f| f.wire_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Shape::Union(u) => u
            .variants
            .iter()
            .map(|v| type_label(&v.ty))
            .collect::<Vec<_>>()
            .join(" | "),
        Shape::Intersection { members } => members
            .iter()
            .map(type_label)
            .collect::<Vec<_>>()
            .join(" & "),
        Shape::Nullable { inner } => format!("{}?", type_label(inner)),
        Shape::Any => "any".into(),
        Shape::Never => "never".into(),
    }
}

fn primitive_label(p: &Primitive) -> String {
    match p {
        Primitive::String { format: None } => "string".into(),
        Primitive::String {
            format: Some(StringFormat::Other(f)),
        } => format!("string({f})"),
        Primitive::String { format: Some(f) } => format!("string({})", tag(f)),
        other => tag_kind(other),
    }
}

fn enum_values<'a>(values: impl ExactSizeIterator<Item = &'a serde_json::Value>) -> String {
    let n = values.len();
    let mut shown: Vec<String> = values
        .take(MAX_ENUM_VALUES)
        .map(|v| v.to_string())
        .collect();
    if n > MAX_ENUM_VALUES {
        shown.push(format!("… {} more", n - MAX_ENUM_VALUES));
    }
    shown.join(", ")
}

fn status_label(s: StatusMatch) -> String {
    match s {
        StatusMatch::Exact(code) => code.to_string(),
        StatusMatch::Range(class) => format!("{class}XX"),
        StatusMatch::Default => "default".into(),
    }
}

fn source_label(s: &tungsten_ir::SourceRef) -> String {
    if s.pointer.is_empty() {
        s.file.clone()
    } else {
        format!("{}#{}", s.file, s.pointer)
    }
}

/// The serialized name of a unit enum value (`read_only`, `GET`).
fn tag<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// The `kind` of an internally tagged enum value (`cursor`, `int64`).
fn tag_kind<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::Object(map)) => map
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or_default()
            .to_string(),
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// `  key        value`, with continuation lines aligned under the value.
fn row(out: &mut String, key: &str, value: &str) {
    let indent = " ".repeat(KEY_WIDTH + 2);
    for (i, line) in value.lines().enumerate() {
        if i == 0 {
            let _ = writeln!(out, "  {key:<KEY_WIDTH$}{line}");
        } else {
            let _ = writeln!(out, "{indent}{line}");
        }
    }
    if value.is_empty() {
        let _ = writeln!(out, "  {key}");
    }
}

/// Greedy word wrap with a fixed indent.
fn wrap(text: &str, indent: usize, width: usize) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && indent + line.len() + 1 + word.len() > width {
            let _ = writeln!(out, "{pad}{line}");
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        let _ = writeln!(out, "{pad}{line}");
    }
    out
}
