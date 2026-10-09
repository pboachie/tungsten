// SPDX-License-Identifier: AGPL-3.0-only
//! Resource classes (`src/resources/...`), the client (`src/client.ts`)
//! and the package entry point (`src/index.ts`).
//!
//! Each operation is a callable property, `(args, opts?) =>
//! Promise<Result<T>>`, with `.descriptor` and `.safety` attached, plus
//! `.preview()` for operations with a preview mode and `.pages()` for
//! paginated ones, built with `Object.assign` so the property's type is
//! exact.

use std::collections::BTreeSet;

use tungsten_emit::{CommentStyle, Imports, Writer};
use tungsten_ir::naming::Role;
use tungsten_ir::{IdempotencyKind, OperationStatus, PreviewMode, Safety};

use crate::models::write_imports;
use crate::ops::{OpShape, idempotency_str, method_str, safety_str};
use crate::plan::{MemberKind, OpInfo, Plan, ResInfo, unique, with_word};
use crate::ts::{doc_text, paragraphs, string_lit};

/// Relative import path from package file `from` to package file `to`
/// (both like `src/a/b.ts`), with the `.js` extension NodeNext requires.
pub(crate) fn rel_import(from: &str, to: &str) -> String {
    let from_dir: Vec<&str> = from
        .rsplit_once('/')
        .map_or_else(Vec::new, |(dir, _)| dir.split('/').collect());
    let to_parts: Vec<&str> = to.split('/').collect();
    let common = from_dir
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_dir.len() - common;
    let mut parts: Vec<String> = if ups == 0 {
        vec![".".into()]
    } else {
        vec!["..".into(); ups]
    };
    parts.extend(to_parts[common..].iter().map(|s| s.to_string()));
    let joined = parts.join("/");
    match joined.strip_suffix(".ts") {
        Some(stem) => format!("{stem}.js"),
        None => joined,
    }
}

/// Whether the operation gets a `.preview()`.
pub(crate) fn has_preview(info: &OpInfo<'_>) -> bool {
    info.op.agent.safety != Safety::ReadOnly && info.op.agent.preview != PreviewMode::None
}

/// The TSDoc of an operation property: its description and the agent
/// facts a caller must know before calling it.
pub(crate) fn op_doc(plan: &Plan<'_>, info: &OpInfo<'_>, shape: &OpShape<'_>) -> String {
    let op = info.op;
    let a = &op.agent;
    let mut parts = vec![doc_text(op.doc.as_ref())];
    parts.push(format!(
        "`{} {}` (`{}`). Safety: `{}`.",
        method_str(op.method),
        op.path.raw,
        op.id.0,
        safety_str(a.safety)
    ));
    match a.safety {
        Safety::Destructive => parts.push(
            "Requires confirmation: pass `{ confirm: true }`, or the `confirmation_token` of `.preview()` as `confirm`.".into(),
        ),
        Safety::Irreversible => parts.push(
            "Cannot be undone: call `.preview()` first and pass its `confirmation_token` as `{ confirm }`.".into(),
        ),
        Safety::ReadOnly | Safety::Mutating => {}
    }
    let idem = &a.idempotency;
    match idem.policy {
        IdempotencyKind::CallerOwned => parts.push(format!(
            "Idempotency (`caller_owned`): generate a key{}, persist it with your intent, pass it as `idempotencyKey`, and reuse it on every retry.{}",
            idem.format.as_deref().map(|f| format!(" ({f})")).unwrap_or_default(),
            if idem.persist_required { " Calls without a key are refused." } else { "" }
        )),
        IdempotencyKind::None => {}
        k => parts.push(format!("Idempotency: `{}`.", idempotency_str(k))),
    }
    if let Some(note) = &idem.note {
        parts.push(note.clone());
    }
    if let OperationStatus::Gated { gate } = &op.status {
        let text = plan
            .ir
            .agent
            .gates
            .get(&gate.env_var)
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "Enabled only when the server sets `{}`; otherwise it answers {} and the call returns `GATE_DISABLED`.",
                    gate.env_var, gate.disabled_status
                )
            });
        parts.push(text);
    }
    if let Some(note) = &a.remediation_note {
        parts.push(note.clone());
    }
    if a.shown_once {
        parts.push(format!(
            "The response includes values shown only once{}: store them immediately.",
            if a.sensitive_response_fields.is_empty() {
                String::new()
            } else {
                format!(" (`{}`)", a.sensitive_response_fields.join("`, `"))
            }
        ));
    }
    if shape.page_item.is_some() {
        parts.push("`.pages()` iterates every page.".into());
    }
    if matches!(shape.success.text.as_str(), "void") || shape.success.text.ends_with(" | undefined")
    {
        parts.push("A success response without a body yields `value: undefined`.".into());
    }
    if let Some(cluster) = &a.cluster {
        parts.push(format!("Cluster: `{cluster}`."));
    }
    if op.deprecated {
        parts.push("@deprecated".into());
    }
    paragraphs(parts)
}

/// The declared type of an operation property.
fn op_property_type(info: &OpInfo<'_>, shape: &OpShape<'_>) -> Vec<String> {
    let args = format!(
        "args{}: ops.{}",
        if shape.all_optional { "?" } else { "" },
        info.args_type
    );
    let mut lines = vec![format!(
        "(({args}, opts?: CallOptions) => Promise<Result<{}>>) & {{",
        shape.success.text
    )];
    lines.push("  readonly descriptor: OperationDescriptor;".into());
    lines.push(format!(
        "  readonly safety: {};",
        string_lit(safety_str(info.op.agent.safety))
    ));
    if has_preview(info) {
        lines.push(format!(
            "  preview({args}, opts?: CallOptions): Promise<Result<PreviewResult>>;"
        ));
    }
    if let Some(item) = &shape.page_item {
        lines.push(format!(
            "  pages({args}, opts?: CallOptions): AsyncIterable<Result<Page<{}>>>;",
            item.text
        ));
    }
    lines.push("}".into());
    lines
}

/// The constructor assignment of an operation property.
fn op_assignment(member: &str, info: &OpInfo<'_>, shape: &OpShape<'_>) -> Vec<String> {
    let param = if shape.all_optional {
        format!("args: ops.{} = {{}}", info.args_type)
    } else {
        format!("args: ops.{}", info.args_type)
    };
    let d = format!("ops.{}", info.key);
    let mut lines = vec![
        format!("this.{member} = Object.assign("),
        format!(
            "  ({param}, opts?: CallOptions) => core.call<{}>({d}, args, opts),",
            shape.success.text
        ),
        "  {".into(),
        format!("    descriptor: {d},"),
        format!(
            "    safety: {} as const,",
            string_lit(safety_str(info.op.agent.safety))
        ),
    ];
    if has_preview(info) {
        lines.push(format!(
            "    preview: ({param}, opts?: CallOptions) => core.preview({d}, args, opts),"
        ));
    }
    if let Some(item) = &shape.page_item {
        lines.push(format!(
            "    pages: ({param}, opts?: CallOptions) => core.pages<{}>({d}, args, opts),",
            item.text
        ));
    }
    lines.push("  },".into());
    lines.push(");".into());
    lines
}

/// Doc of a resource class or property.
fn resource_doc(r: &ResInfo<'_>) -> String {
    let text = doc_text(r.res.doc.as_ref());
    if text.is_empty() {
        format!("Operations under `{}`.", r.res.path_prefix)
    } else {
        text
    }
}

/// The source of one resource class file.
pub(crate) fn resource_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    idx: usize,
    header: &str,
) -> String {
    let r = &plan.resources[idx];
    let mut imports = Imports::new();
    imports.add_type("@tungsten/runtime", "ClientCoreApi");
    let mut namespaces: BTreeSet<String> = BTreeSet::new();
    let mut has_ops = false;
    for m in &r.members {
        match m.kind {
            MemberKind::Op(o) => {
                has_ops = true;
                let info = &plan.ops[o];
                let shape = &shapes[o];
                for t in ["CallOptions", "OperationDescriptor", "Result"] {
                    imports.add_type("@tungsten/runtime", t);
                }
                if has_preview(info) {
                    imports.add_type("@tungsten/runtime", "PreviewResult");
                }
                if shape.page_item.is_some() {
                    imports.add_type("@tungsten/runtime", "Page");
                }
                namespaces.extend(shape.result_namespaces.iter().cloned());
            }
            MemberKind::Child(c) => {
                let child = &plan.resources[c];
                imports.add(&rel_import(&r.file, &child.file), &child.class);
            }
        }
    }
    let mut w = Writer::new("  ");
    w.line(header);
    w.blank();
    write_imports(&mut w, &imports);
    if has_ops {
        w.line(format!(
            "import * as ops from {};",
            string_lit(&rel_import(&r.file, "src/descriptors.ts"))
        ));
    }
    for ns in &namespaces {
        if let Some(m) = plan.model_ns(ns) {
            w.line(format!(
                "import type * as {} from {};",
                m.alias,
                string_lit(&rel_import(&r.file, &format!("src/models/{}.ts", m.file)))
            ));
        }
    }
    w.blank();
    w.doc(CommentStyle::JsDoc, &resource_doc(r));
    w.line(format!("export class {} {{", r.class));
    w.indent();
    for m in &r.members {
        match m.kind {
            MemberKind::Op(o) => {
                let info = &plan.ops[o];
                w.doc(CommentStyle::JsDoc, &op_doc(plan, info, &shapes[o]));
                let lines = op_property_type(info, &shapes[o]);
                w.line(format!("readonly {}: {}", m.name, lines[0]));
                for l in &lines[1..lines.len() - 1] {
                    w.line(l);
                }
                w.line("};");
            }
            MemberKind::Child(c) => {
                let child = &plan.resources[c];
                w.doc(CommentStyle::JsDoc, &resource_doc(child));
                w.line(format!("readonly {}: {};", m.name, child.class));
            }
        }
    }
    w.blank();
    w.line("constructor(core: ClientCoreApi) {");
    w.indent();
    for m in &r.members {
        match m.kind {
            MemberKind::Op(o) => {
                for l in op_assignment(&m.name, &plan.ops[o], &shapes[o]) {
                    w.line(l);
                }
            }
            MemberKind::Child(c) => {
                w.line(format!(
                    "this.{} = new {}(core);",
                    m.name, plan.resources[c].class
                ));
            }
        }
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.finish()
}

/// The source of `src/client.ts`.
pub(crate) fn client_file(plan: &Plan<'_>, has_macros: bool, header: &str) -> String {
    let ir = plan.ir;
    let mut imports = Imports::new();
    imports.add("@tungsten/runtime", "ClientCore");
    imports.add_type("@tungsten/runtime", "ClientOptions");
    imports.add("./descriptors.js", "api");
    imports.add("./descriptors.js", "operations");
    if has_macros {
        imports.add("./macros.js", "Macros");
    }
    let tops: Vec<usize> = if plan.multi() {
        imports.add_type("@tungsten/runtime", "ClientCoreApi");
        plan.client_namespaces
            .iter()
            .flat_map(|n| n.members.iter().map(|(_, i)| *i))
            .collect()
    } else {
        plan.client_resources.iter().map(|(_, i)| *i).collect()
    };
    for i in tops {
        let r = &plan.resources[i];
        imports.add(&rel_import("src/client.ts", &r.file), &r.class);
    }
    let mut w = Writer::new("  ");
    w.line(header);
    w.blank();
    write_imports(&mut w, &imports);
    w.blank();
    let title = tungsten_ir::title_stem(&ir.api.title);
    w.doc(
        CommentStyle::JsDoc,
        &paragraphs([
            format!("Client for the {title} API."),
            ir.api.description.clone().unwrap_or_default(),
            "Every call returns a `Result`: `{ ok: true, value }` or `{ ok: false, error }` with the diagnostic envelope; API and transport errors never throw.".to_string(),
        ]),
    );
    w.line(format!("export class {} {{", plan.client_class));
    w.indent();
    w.doc(
        CommentStyle::JsDoc,
        "The runtime core every call goes through.",
    );
    w.line("readonly core: ClientCore;");
    if has_macros {
        w.doc(
            CommentStyle::JsDoc,
            "Multi-step workflows compiled from the agent manifest.",
        );
        w.line("readonly macros: Macros;");
    }
    let members: Vec<(String, String, String)> = if plan.multi() {
        plan.client_namespaces
            .iter()
            .map(|n| {
                let doc = paragraphs([format!("{} (`{}` namespace).", n.ns.title, n.ns.name.wire)]);
                (n.member.clone(), n.class.clone(), doc)
            })
            .collect()
    } else {
        plan.client_resources
            .iter()
            .map(|(name, i)| {
                let r = &plan.resources[*i];
                (name.clone(), r.class.clone(), resource_doc(r))
            })
            .collect()
    };
    for (name, class, doc) in &members {
        w.doc(CommentStyle::JsDoc, doc);
        w.line(format!("readonly {name}: {class};"));
    }
    w.blank();
    w.line("constructor(options: ClientOptions = {}) {");
    w.indent();
    w.line("// Every operation is registered so the runtime resolves them by id");
    w.line("// (verification hooks, endpoint previews, macro steps).");
    w.line("this.core = new ClientCore(api, { ...options, operations: [...operations, ...(options.operations ?? [])] });");
    if has_macros {
        w.line("this.macros = new Macros(this.core);");
    }
    for (name, class, _) in &members {
        w.line(format!("this.{name} = new {class}(this.core);"));
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    for n in &plan.client_namespaces {
        w.blank();
        w.doc(
            CommentStyle::JsDoc,
            &format!("The `{}` namespace: {}.", n.ns.name.wire, n.ns.title),
        );
        w.line(format!("export class {} {{", n.class));
        w.indent();
        for (name, i) in &n.members {
            let r = &plan.resources[*i];
            w.doc(CommentStyle::JsDoc, &resource_doc(r));
            w.line(format!("readonly {name}: {};", r.class));
        }
        w.blank();
        w.line("constructor(core: ClientCoreApi) {");
        w.indent();
        for (name, i) in &n.members {
            w.line(format!(
                "this.{name} = new {}(core);",
                plan.resources[*i].class
            ));
        }
        w.dedent();
        w.line("}");
        w.dedent();
        w.line("}");
    }
    w.finish()
}

/// The source of `src/index.ts`.
pub(crate) fn index_file(plan: &Plan<'_>, has_macros: bool, header: &str) -> String {
    let mut w = Writer::new("  ");
    w.line(header);
    w.blank();
    w.line(format!(
        "export {{ {} }} from \"./client.js\";",
        plan.client_class
    ));
    w.line("export type { CallOptions, ClientOptions, Diagnostic, Page, PreviewResult, Result } from \"@tungsten/runtime\";");
    // With one namespace its models are exported directly; otherwise each
    // namespace's models are a namespace export (`publicModels.Error`).
    let star = (!plan.multi())
        .then(|| plan.ir.namespaces.first().map(|n| n.name.wire.clone()))
        .flatten();
    let grouped: Vec<&crate::plan::ModelNs> = plan
        .models
        .iter()
        .filter(|m| Some(&m.name) != star.as_ref())
        .collect();
    let names = unique(
        &["descriptors", "macros", "custom"],
        &grouped
            .iter()
            .map(|m| with_word(&tungsten_ir::Ident::new(m.name.as_str()).words, "models"))
            .collect::<Vec<_>>(),
        Role::Module,
    );
    if let Some(m) = star.as_deref().and_then(|s| plan.model_ns(s)) {
        w.line(format!("export * from \"./models/{}.js\";", m.file));
    }
    for (m, name) in grouped.iter().zip(names) {
        w.line(format!(
            "export * as {name} from \"./models/{}.js\";",
            m.file
        ));
    }
    w.line("export * as descriptors from \"./descriptors.js\";");
    if has_macros {
        w.line("export * as macros from \"./macros.js\";");
    }
    w.line("export * as custom from \"./custom/index.js\";");
    w.finish()
}
