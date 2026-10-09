// SPDX-License-Identifier: AGPL-3.0-only
//! `blocking.rs`: the blocking client, `<Api>BlockingClient`. It has the
//! resources, methods, arguments and results of the async client, and runs
//! each call to completion on the current-thread runtime that its
//! `tungsten_runtime::blocking::Runtime` owns (the runtime is the single
//! implementation; this file only forwards). Page iterators are blocking
//! `Iterator`s of pages.

use tungsten_emit::Writer;

use super::dispatch::client_paths;
use super::macros::{MacroPlan, has_preview as macro_has_preview};
use super::ops::{OpShape, fn_sig};
use super::plan::{MemberKind, Plan};
use super::resources::{op_names, resource_doc, write_holder};
use super::rs::{Rx, chain_stmt, doc, doc_text, imports_for, paragraphs, put, string_lit};

/// The name of the blocking client: the async client's name with
/// `Blocking` before its `Client` suffix.
pub(crate) fn blocking_client_name(plan: &Plan<'_>) -> String {
    let client = &plan.client_class;
    match client.strip_suffix("Client") {
        Some(api) => format!("{api}BlockingClient"),
        None => format!("{client}Blocking"),
    }
}

/// `let resource = client.a();` then one rebinding per further accessor,
/// the way `dispatch.rs` walks to a resource (one call per statement keeps
/// every line what rustfmt prints).
fn resource_lines(w: &mut Writer, path: &[String], root: &str) {
    for (i, name) in path.iter().enumerate() {
        let from = if i == 0 { root } else { "resource" };
        w.line(format!("let resource = {from}.{name}();"));
    }
}

/// The task a call runs: the call on the resource (or on `macros`), awaited.
fn task_lines(w: &mut Writer, root: &str, setup: &dyn Fn(&mut Writer), call: &str) {
    w.line("let task = async move {");
    w.indent();
    setup(w);
    chain_stmt(w, 12, "", root, &[call, ".await"], "");
    w.dedent();
    w.line("};");
}

/// The first paragraph of a documentation text.
fn summary(text: &str) -> String {
    text.split("\n\n").next().unwrap_or("").trim().to_string()
}

/// A call forwarded to the runtime.
#[allow(clippy::too_many_arguments)]
fn write_call(
    w: &mut Writer,
    text: &str,
    name: &str,
    param: &str,
    ret: &str,
    operation: &str,
    path: &[String],
    method: &str,
) {
    w.blank();
    doc(w, text);
    let params = vec![
        "&self".to_string(),
        param.to_string(),
        "opts: &CallOptions".to_string(),
    ];
    fn_sig(w, 4, &format!("pub fn {name}"), &params, ret);
    w.indent();
    w.line("let client = self.client.inner.clone();");
    w.line("let opts = opts.clone();");
    let call = format!(".{method}(request, &opts)");
    task_lines(w, "resource", &|w| resource_lines(w, path, "client"), &call);
    w.line(format!("let operation = {};", string_lit(operation)));
    w.line("self.client.runtime.run(operation, task)");
    w.dedent();
    w.line("}");
}

/// The source of `blocking.rs`.
pub(crate) fn blocking_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    macros: &[MacroPlan<'_>],
    header: &str,
) -> String {
    let async_client = &plan.client_class;
    let client = blocking_client_name(plan);
    let paths = client_paths(plan);
    let mut extra: Vec<(String, String)> = vec![
        ("crate::client".into(), async_client.clone()),
        ("tungsten_runtime::blocking".into(), "Runtime".into()),
    ];
    let mut code = Writer::new("    ");
    doc(
        &mut code,
        &paragraphs([
            format!(
                "Blocking client for the {} API.",
                tungsten_ir::title_stem(&plan.ir.api.title)
            ),
            format!(
                "It has the resources, methods, arguments and results of `{async_client}`, and each call returns when the request is done. Calls run on a thread of their own that this client owns, so they work from any thread, including threads of an async runtime, which they block while they wait: from async code, prefer `{async_client}` or run them with `spawn_blocking`."
            ),
        ]),
    );
    code.line("#[derive(Debug, Clone)]");
    code.line(format!("pub struct {client} {{"));
    code.indent();
    code.line(format!("pub(crate) inner: {async_client},"));
    code.line("pub(crate) runtime: Runtime,");
    code.dedent();
    code.line("}");
    code.blank();
    code.line(format!("impl {client} {{"));
    code.indent();
    doc(
        &mut code,
        &format!("Create a blocking client from `options` (see [`{async_client}::new`])."),
    );
    code.line("pub fn new(options: ClientOptions) -> Result<Self, ConfigError> {");
    code.indent();
    code.line(format!("Self::from_client({async_client}::new(options)?)"));
    code.dedent();
    code.line("}");
    code.blank();
    doc(
        &mut code,
        "Wrap an async client. The blocking client shares its connection pool, idempotency store and confirmation key.",
    );
    code.line(format!(
        "pub fn from_client(inner: {async_client}) -> Result<Self, ConfigError> {{"
    ));
    code.indent();
    code.line("let runtime = Runtime::new()?;");
    code.line("Ok(Self { inner, runtime })");
    code.dedent();
    code.line("}");
    code.blank();
    doc(&mut code, "The async client behind this one.");
    code.line(format!("pub fn async_client(&self) -> &{async_client} {{"));
    code.indent();
    code.line("&self.inner");
    code.dedent();
    code.line("}");
    if !macros.is_empty() {
        code.blank();
        doc(
            &mut code,
            "Multi-step workflows compiled from the agent manifest.",
        );
        code.line("pub fn macros(&self) -> Macros<'_> {");
        code.indent();
        code.line("Macros::new(self)");
        code.dedent();
        code.line("}");
    }
    if plan.multi() {
        for n in &plan.client_namespaces {
            code.blank();
            doc(
                &mut code,
                &format!("The `{}` namespace: {}.", n.ns.name.wire, n.ns.title),
            );
            code.line(format!("pub fn {}(&self) -> {}<'_> {{", n.member, n.strukt));
            code.indent();
            code.line(format!("{}::new(self)", n.strukt));
            code.dedent();
            code.line("}");
        }
    } else {
        for (name, i) in &plan.client_resources {
            let r = &plan.resources[*i];
            code.blank();
            doc(&mut code, &resource_doc(r));
            code.line(format!("pub fn {name}(&self) -> {}<'_> {{", r.strukt));
            code.indent();
            code.line(format!("{}::new(self)", r.strukt));
            code.dedent();
            code.line("}");
        }
    }
    code.dedent();
    code.line("}");

    for n in &plan.client_namespaces {
        code.blank();
        write_holder(
            &mut code,
            &client,
            &n.strukt,
            &format!("The `{}` namespace: {}.", n.ns.name.wire, n.ns.title),
        );
        for (name, i) in &n.members {
            let r = &plan.resources[*i];
            code.blank();
            doc(&mut code, &resource_doc(r));
            code.line(format!("pub fn {name}(&self) -> {}<'a> {{", r.strukt));
            code.indent();
            code.line(format!("{}::new(self.client)", r.strukt));
            code.dedent();
            code.line("}");
        }
        code.dedent();
        code.line("}");
    }

    for r in &plan.resources {
        code.blank();
        write_holder(&mut code, &client, &r.strukt, &resource_doc(r));
        for m in &r.members {
            if let MemberKind::Child(c) = m.kind {
                let child = &plan.resources[c];
                code.blank();
                doc(&mut code, &resource_doc(child));
                fn_sig(
                    &mut code,
                    4,
                    &format!("pub fn {}", m.name),
                    &["&self".to_string()],
                    &format!("{}<'a>", child.strukt),
                );
                code.indent();
                code.line(format!("{}::new(self.client)", child.strukt));
                code.dedent();
                code.line("}");
            }
        }
        for m in &r.members {
            let o = match m.kind {
                MemberKind::Op(o) | MemberKind::Preview(o) | MemberKind::Pages(o) => o,
                MemberKind::Child(_) => continue,
            };
            let info = &plan.ops[o];
            let names = op_names(r, o);
            let req = &info.request;
            let module = &plan.resources[info.res].module;
            extra.push((format!("crate::resources::{module}"), req.clone()));
            let param = format!("request: {req}");
            let id = &info.op.id.0;
            let facts = format!(
                "`{} {}` (`{}`). Safety: `{}`.",
                super::ops::method_str(info.op.method),
                info.op.path.raw,
                id,
                super::ops::safety_str(info.op.agent.safety)
            );
            let brief = summary(&doc_text(info.op.doc.as_ref()));
            match m.kind {
                MemberKind::Op(_) => write_call(
                    &mut code,
                    &paragraphs([
                        brief,
                        format!("Blocking form of `{async_client}`'s `{}()`.", names.call),
                        facts,
                    ]),
                    &m.name,
                    &param,
                    &format!("tungsten_runtime::Result<{}>", shapes[o].success),
                    id,
                    &paths[o],
                    &names.call,
                ),
                MemberKind::Preview(_) => write_call(
                    &mut code,
                    &format!(
                        "Blocking form of `{}()`: preview `{id}` without sending it.",
                        m.name
                    ),
                    &m.name,
                    &param,
                    "tungsten_runtime::Result<PreviewResult>",
                    id,
                    &paths[o],
                    &m.name,
                ),
                MemberKind::Pages(_) => {
                    let item = shapes[o]
                        .page_item
                        .clone()
                        .unwrap_or_else(|| "Value".into());
                    code.blank();
                    doc(
                        &mut code,
                        &format!(
                            "Blocking form of `{}()`: an iterator over every page of `{id}`. A failed page is yielded as its error and ends the iteration.",
                            m.name
                        ),
                    );
                    let params = vec![
                        "&self".to_string(),
                        param.clone(),
                        "opts: &CallOptions".to_string(),
                    ];
                    fn_sig(
                        &mut code,
                        4,
                        &format!("pub fn {}", m.name),
                        &params,
                        &format!("Pages<{item}>"),
                    );
                    code.indent();
                    resource_lines(&mut code, &paths[o], "self.client.inner");
                    let call = Rx::call(
                        format!("resource.{}", m.name),
                        vec![Rx::atom("request"), Rx::atom("opts")],
                    );
                    put(&mut code, 8, "let pages = ", &call, ";");
                    code.line(format!("let operation = {};", string_lit(id)));
                    code.line("self.client.runtime.pages(operation, pages)");
                    code.dedent();
                    code.line("}");
                    extra.push(("tungsten_runtime::blocking".into(), "Pages".into()));
                }
                MemberKind::Child(_) => {}
            }
        }
        code.dedent();
        code.line("}");
    }

    if !macros.is_empty() {
        code.blank();
        write_holder(
            &mut code,
            &client,
            "Macros",
            "Multi-step workflows compiled from the agent manifest, run by the runtime.",
        );
        for mp in macros {
            let name = mp.name();
            let input = &mp.input;
            code.blank();
            doc(
                &mut code,
                &format!("Blocking form of `Macros::{}()`: run `{name}`.", mp.member),
            );
            let params = vec![
                "&self".to_string(),
                format!("input: {input}"),
                "opts: &CallOptions".to_string(),
            ];
            fn_sig(
                &mut code,
                4,
                &format!("pub fn {}", mp.member),
                &params,
                &format!("tungsten_runtime::Result<{}>", mp.output_type()),
            );
            code.indent();
            code.line("let client = self.client.inner.clone();");
            code.line("let opts = opts.clone();");
            let call = format!(".{}(input, &opts)", mp.member);
            task_lines(
                &mut code,
                "macros",
                &|w| {
                    w.line("let macros = client.macros();");
                },
                &call,
            );
            code.line(format!("let operation = {};", string_lit(&name)));
            code.line("self.client.runtime.run(operation, task)");
            code.dedent();
            code.line("}");
            if macro_has_preview(mp) {
                code.blank();
                doc(
                    &mut code,
                    &format!(
                        "Blocking form of `Macros::{}()`: preview `{name}` without sending anything.",
                        mp.preview_name()
                    ),
                );
                fn_sig(
                    &mut code,
                    4,
                    &format!("pub fn {}", mp.preview_name()),
                    &params,
                    "tungsten_runtime::Result<PreviewResult>",
                );
                code.indent();
                code.line("let client = self.client.inner.clone();");
                code.line("let opts = opts.clone();");
                let call = format!(".{}(input, &opts)", mp.preview_name());
                task_lines(
                    &mut code,
                    "macros",
                    &|w| {
                        w.line("let macros = client.macros();");
                    },
                    &call,
                );
                code.line(format!("let operation = {};", string_lit(&name)));
                code.line("self.client.runtime.run(operation, task)");
                code.dedent();
                code.line("}");
            }
            extra.push(("crate::macros".to_string(), mp.input.clone()));
            if let Some(output) = mp.output_struct() {
                extra.push(("crate::macros".to_string(), output.to_string()));
            }
        }
        code.dedent();
        code.line("}");
    }
    let code = code.finish();

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line("//! The blocking client: the same calls, run to completion.");
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
