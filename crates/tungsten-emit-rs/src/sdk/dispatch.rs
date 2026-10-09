// SPDX-License-Identifier: AGPL-3.0-only
//! `dispatch.rs`: `impl Dispatch for <Api>Client`, the dynamic entry point
//! that runs the typed methods from an operation id and JSON arguments
//! (arguments -> request struct -> typed method -> response -> JSON). When
//! the arguments do not decode, the call goes to `ClientCore` with them
//! as given, so the pre-flight envelope is the runtime's.

use tungsten_emit::Writer;

use super::macros::{MacroPlan, has_preview as macro_has_preview};
use super::ops::fn_sig;
use super::plan::{MemberKind, Plan};
use super::rs::{MAX_WIDTH, Rx, chain_stmt, imports_for, put, string_lit};

/// For each operation (by index into `Plan::ops`), the accessor names that
/// lead from the client to its resource.
pub(crate) fn client_paths(plan: &Plan<'_>) -> Vec<Vec<String>> {
    fn walk(plan: &Plan<'_>, idx: usize, prefix: &[String], out: &mut Vec<Vec<String>>) {
        for m in &plan.resources[idx].members {
            match m.kind {
                MemberKind::Op(o) => out[o] = prefix.to_vec(),
                MemberKind::Child(c) => {
                    let mut p = prefix.to_vec();
                    p.push(m.name.clone());
                    walk(plan, c, &p, out);
                }
                MemberKind::Preview(_) | MemberKind::Pages(_) | MemberKind::Stream(_) => {}
            }
        }
    }
    let mut out = vec![vec![]; plan.ops.len()];
    if plan.multi() {
        for n in &plan.client_namespaces {
            for (name, i) in &n.members {
                walk(plan, *i, &[n.member.clone(), name.clone()], &mut out);
            }
        }
    } else {
        for (name, i) in &plan.client_resources {
            walk(plan, *i, std::slice::from_ref(name), &mut out);
        }
    }
    out
}

/// `let resource = client.a();` then one rebinding per further accessor.
fn resource_lines(w: &mut Writer, path: &[String], root: &str) {
    for (i, name) in path.iter().enumerate() {
        let from = if i == 0 { root } else { "resource" };
        w.line(format!("let resource = {from}.{name}();"));
    }
}

/// The source of `dispatch.rs`.
pub(crate) fn dispatch_file(plan: &Plan<'_>, macros: &[MacroPlan<'_>], header: &str) -> String {
    let client = &plan.client_class;
    let paths = client_paths(plan);
    let mut code = Writer::new("    ");
    let method_of = |o: usize, kind: fn(&MemberKind) -> Option<usize>| -> Option<String> {
        plan.resources[plan.ops[o].res]
            .members
            .iter()
            .find(|m| kind(&m.kind) == Some(o))
            .map(|m| m.name.clone())
    };
    let preview_member = |k: &MemberKind| match k {
        MemberKind::Preview(o) => Some(*o),
        _ => None,
    };
    let pages_member = |k: &MemberKind| match k {
        MemberKind::Pages(o) => Some(*o),
        _ => None,
    };
    let stream_member = |k: &MemberKind| match k {
        MemberKind::Stream(o) => Some(*o),
        _ => None,
    };
    let streamed: Vec<usize> = (0..plan.ops.len())
        .filter(|&o| method_of(o, stream_member).is_some())
        .collect();
    let previews: Vec<usize> = (0..plan.ops.len())
        .filter(|&o| method_of(o, preview_member).is_some())
        .collect();
    let paged: Vec<usize> = (0..plan.ops.len())
        .filter(|&o| method_of(o, pages_member).is_some())
        .collect();
    let macro_previews: Vec<usize> = macros
        .iter()
        .enumerate()
        .filter(|(_, m)| macro_has_preview(m))
        .map(|(i, _)| i)
        .collect();

    code.line(format!("impl Dispatch for {client} {{"));
    code.indent();
    code.line("fn core(&self) -> &ClientCore {");
    code.indent();
    code.line("&self.core");
    code.dedent();
    code.line("}");
    code.blank();
    code.line("fn operations(&self) -> &[OperationDescriptor] {");
    code.indent();
    code.line("&self.descriptors.list");
    code.dedent();
    code.line("}");
    code.blank();
    code.line("fn macros(&self) -> &[MacroDescriptor] {");
    code.indent();
    code.line("&self.descriptors.macros");
    code.dedent();
    code.line("}");

    // invoke
    code.blank();
    fn_sig(
        &mut code,
        4,
        "async fn invoke",
        &[
            "&self".into(),
            "operation: &str".into(),
            "args: Value".into(),
            "opts: CallOptions".into(),
        ],
        "Outcome",
    );
    code.indent();
    if plan.ops.is_empty() {
        code.line("let _ = (args, opts);");
        code.line("Err(s::unknown_operation(operation))");
    } else {
        code.line("match operation {");
        code.indent();
        for info in &plan.ops {
            await_arm(
                &mut code,
                &string_lit(&info.op.id.0),
                format!("call_{}(self, args, &opts)", suffix(&info.builder)),
            );
        }
        code.line("_ => Err(s::unknown_operation(operation)),");
        code.dedent();
        code.line("}");
    }
    code.dedent();
    code.line("}");

    // preview
    code.blank();
    fn_sig(
        &mut code,
        4,
        "async fn preview",
        &[
            "&self".into(),
            "operation: &str".into(),
            "args: Value".into(),
            "opts: CallOptions".into(),
        ],
        "tungsten_runtime::Result<PreviewResult>",
    );
    code.indent();
    if !previews.is_empty() {
        code.line("match operation {");
        code.indent();
        for &o in &previews {
            await_arm(
                &mut code,
                &string_lit(&plan.ops[o].op.id.0),
                format!(
                    "preview_{}(self, args, &opts)",
                    suffix(&plan.ops[o].builder)
                ),
            );
        }
        code.line("_ => {");
        code.indent();
    }
    code.line("let Some(op) = s::find(&self.descriptors.operations, operation) else {");
    code.indent();
    code.line("return Err(s::unknown_operation(operation));");
    code.dedent();
    code.line("};");
    code.line("self.core.preview(op, args, &opts).await");
    if !previews.is_empty() {
        code.dedent();
        code.line("}");
        code.dedent();
        code.line("}");
    }
    code.dedent();
    code.line("}");

    // pages
    code.blank();
    fn_sig(
        &mut code,
        4,
        "async fn pages",
        &[
            "&self".into(),
            "operation: &str".into(),
            "args: Value".into(),
            "opts: CallOptions".into(),
        ],
        "Vec<tungsten_runtime::Result<Page<Value>>>",
    );
    code.indent();
    if paged.is_empty() {
        code.line("let _ = (args, opts);");
        code.line("vec![Err(s::not_paginated(operation))]");
    } else {
        code.line("match operation {");
        code.indent();
        for &o in &paged {
            await_arm(
                &mut code,
                &string_lit(&plan.ops[o].op.id.0),
                format!("pages_{}(self, args, opts)", suffix(&plan.ops[o].builder)),
            );
        }
        code.line("_ => vec![Err(s::not_paginated(operation))],");
        code.dedent();
        code.line("}");
    }
    code.dedent();
    code.line("}");

    // stream
    if !streamed.is_empty() {
        code.blank();
        fn_sig(
            &mut code,
            4,
            "async fn stream",
            &[
                "&self".into(),
                "operation: &str".into(),
                "args: Value".into(),
                "opts: CallOptions".into(),
            ],
            "Vec<StreamResult<Value>>",
        );
        code.indent();
        code.line("match operation {");
        code.indent();
        for &o in &streamed {
            await_arm(
                &mut code,
                &string_lit(&plan.ops[o].op.id.0),
                format!("stream_{}(self, args, opts)", suffix(&plan.ops[o].builder)),
            );
        }
        code.line("_ => vec![Err(s::not_streamed(operation))],");
        code.dedent();
        code.line("}");
        code.dedent();
        code.line("}");
    }

    // run_macro
    code.blank();
    fn_sig(
        &mut code,
        4,
        "async fn run_macro",
        &[
            "&self".into(),
            "name: &str".into(),
            "input: Value".into(),
            "opts: CallOptions".into(),
        ],
        "Outcome",
    );
    code.indent();
    if macros.is_empty() {
        code.line("let _ = (input, opts);");
        code.line("Err(s::unknown_macro(name))");
    } else {
        code.line("match name {");
        code.indent();
        for mp in macros {
            await_arm(
                &mut code,
                &string_lit(&mp.name()),
                format!("macro_{}(self, input, &opts)", suffix_macro(&mp.konst)),
            );
        }
        code.line("_ => Err(s::unknown_macro(name)),");
        code.dedent();
        code.line("}");
    }
    code.dedent();
    code.line("}");

    // preview_macro
    code.blank();
    fn_sig(
        &mut code,
        4,
        "async fn preview_macro",
        &[
            "&self".into(),
            "name: &str".into(),
            "input: Value".into(),
            "opts: CallOptions".into(),
        ],
        "tungsten_runtime::Result<PreviewResult>",
    );
    code.indent();
    if macro_previews.is_empty() {
        code.line("let _ = (input, opts);");
        code.line("Err(s::unknown_macro(name))");
    } else {
        code.line("match name {");
        code.indent();
        for &i in &macro_previews {
            await_arm(
                &mut code,
                &string_lit(&macros[i].name()),
                format!(
                    "preview_macro_{}(self, input, &opts)",
                    suffix_macro(&macros[i].konst)
                ),
            );
        }
        code.line("_ => Err(s::unknown_macro(name)),");
        code.dedent();
        code.line("}");
    }
    code.dedent();
    code.line("}");
    code.dedent();
    code.line("}");

    // Per-operation helpers.
    for (o, info) in plan.ops.iter().enumerate() {
        let name = suffix(&info.builder);
        let konst = format!("descriptors::{}", info.konst);
        let names = super::resources::op_names(&plan.resources[info.res], o);
        code.blank();
        fn_sig(
            &mut code,
            0,
            &format!("async fn call_{name}"),
            &[
                format!("client: &{client}"),
                "args: Value".into(),
                "opts: &CallOptions".into(),
            ],
            "Outcome",
        );
        code.indent();
        code.line(format!("let op = client.operation({konst});"));
        code.line("let Some(request) = s::decode_args(&args) else {");
        code.indent();
        code.line("return client.core.call(op, args, opts).await;");
        code.dedent();
        code.line("};");
        resource_lines(&mut code, &paths[o], "client");
        let call = format!(".{}(request, opts)", names.call);
        chain_stmt(
            &mut code,
            4,
            "let outcome = ",
            "resource",
            &[&call, ".await"],
            ";",
        );
        code.line("s::encode(outcome)");
        code.dedent();
        code.line("}");
        if let Some(preview) = &names.preview {
            code.blank();
            fn_sig(
                &mut code,
                0,
                &format!("async fn preview_{name}"),
                &[
                    format!("client: &{client}"),
                    "args: Value".into(),
                    "opts: &CallOptions".into(),
                ],
                "tungsten_runtime::Result<PreviewResult>",
            );
            code.indent();
            code.line(format!("let op = client.operation({konst});"));
            code.line("let Some(request) = s::decode_args(&args) else {");
            code.indent();
            code.line("return client.core.preview(op, args, opts).await;");
            code.dedent();
            code.line("};");
            resource_lines(&mut code, &paths[o], "client");
            let call = format!(".{preview}(request, opts)");
            chain_stmt(&mut code, 4, "", "resource", &[&call, ".await"], "");
            code.dedent();
            code.line("}");
        }
        if let Some(pages) = &names.pages {
            code.blank();
            fn_sig(
                &mut code,
                0,
                &format!("async fn pages_{name}"),
                &[
                    format!("client: &{client}"),
                    "args: Value".into(),
                    "opts: CallOptions".into(),
                ],
                "Vec<tungsten_runtime::Result<Page<Value>>>",
            );
            code.indent();
            code.line(format!("let op = client.operation({konst});"));
            code.line("let Some(request) = s::decode_args(&args) else {");
            code.indent();
            code.line("let pages = client.core.pages(op.clone(), args, opts);");
            code.line("return s::collect_raw(pages).await;");
            code.dedent();
            code.line("};");
            resource_lines(&mut code, &paths[o], "client");
            let call = Rx::call(
                format!("resource.{pages}"),
                vec![Rx::atom("request"), Rx::atom("&opts")],
            );
            put(&mut code, 4, "let pages = ", &call, ";");
            code.line("s::collect_typed(pages).await");
            code.dedent();
            code.line("}");
        }
        if let Some(stream) = &names.stream {
            code.blank();
            fn_sig(
                &mut code,
                0,
                &format!("async fn stream_{name}"),
                &[
                    format!("client: &{client}"),
                    "args: Value".into(),
                    "opts: CallOptions".into(),
                ],
                "Vec<StreamResult<Value>>",
            );
            code.indent();
            code.line(format!("let op = client.operation({konst});"));
            code.line("let Some(request) = s::decode_args(&args) else {");
            code.indent();
            let spec = Rx::call("client.stream_descriptor", vec![Rx::atom(konst.clone())]);
            put(&mut code, 8, "let spec = ", &spec, ";");
            code.line("let events = client.core.stream(op.clone(), spec, args, opts);");
            code.line("return s::collect_raw_events(events).await;");
            code.dedent();
            code.line("};");
            resource_lines(&mut code, &paths[o], "client");
            let call = Rx::call(
                format!("resource.{stream}"),
                vec![Rx::atom("request"), Rx::atom("&opts")],
            );
            put(&mut code, 4, "let events = ", &call, ";");
            code.line("s::collect_events(events).await");
            code.dedent();
            code.line("}");
        }
    }

    // Per-macro helpers.
    for mp in macros {
        let name = suffix_macro(&mp.konst);
        code.blank();
        fn_sig(
            &mut code,
            0,
            &format!("async fn macro_{name}"),
            &[
                format!("client: &{client}"),
                "input: Value".into(),
                "opts: &CallOptions".into(),
            ],
            "Outcome",
        );
        code.indent();
        code.line(format!(
            "let descriptor = &client.descriptors.macros[macros::{}];",
            mp.konst
        ));
        code.line("let Some(typed) = s::decode_args(&input) else {");
        code.indent();
        code.line("return client.core.run_macro(descriptor, input, opts).await;");
        code.dedent();
        code.line("};");
        code.line("let macros = client.macros();");
        let call = format!(".{}(typed, opts)", mp.member);
        chain_stmt(
            &mut code,
            4,
            "let outcome = ",
            "macros",
            &[&call, ".await"],
            ";",
        );
        code.line("s::encode(outcome)");
        code.dedent();
        code.line("}");
        if macro_has_preview(mp) {
            code.blank();
            fn_sig(
                &mut code,
                0,
                &format!("async fn preview_macro_{name}"),
                &[
                    format!("client: &{client}"),
                    "input: Value".into(),
                    "opts: &CallOptions".into(),
                ],
                "tungsten_runtime::Result<PreviewResult>",
            );
            code.indent();
            code.line(format!(
                "let descriptor = &client.descriptors.macros[macros::{}];",
                mp.konst
            ));
            code.line("let Some(typed) = s::decode_args(&input) else {");
            code.indent();
            code.line("return client.core.preview_macro(descriptor, input, opts).await;");
            code.dedent();
            code.line("};");
            code.line("let macros = client.macros();");
            let call = format!(".{}(typed, opts)", mp.preview_name());
            chain_stmt(&mut code, 4, "", "macros", &[&call, ".await"], "");
            code.dedent();
            code.line("}");
        }
    }
    let code = code.finish();

    let mut extra = vec![("crate::client".to_string(), client.clone())];
    if !plan.ops.is_empty() {
        extra.push(("crate".to_string(), "descriptors".to_string()));
    }
    if !macros.is_empty() {
        extra.push(("crate".to_string(), "macros".to_string()));
    }
    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line("//! The dynamic entry point: operations and macros by id and JSON.");
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

/// A match arm whose body is `call(..).await`: one line when it fits, else
/// a block, with `.await` on its own line when the call fills the line.
fn await_arm(w: &mut Writer, pattern: &str, call: String) {
    let flat = format!("{pattern} => {call}.await,");
    if 12 + flat.len() <= MAX_WIDTH {
        w.line(flat);
        return;
    }
    w.line(format!("{pattern} => {{"));
    w.indent();
    if 16 + call.len() + ".await".len() <= MAX_WIDTH {
        w.line(format!("{call}.await"));
    } else {
        w.line(call);
        w.indent();
        w.line(".await");
        w.dedent();
    }
    w.dedent();
    w.line("}");
}

/// The part of a builder name (`op_list_pets`) after its prefix.
fn suffix(name: &str) -> &str {
    name.strip_prefix("op_").unwrap_or(name)
}

/// The part of a macro constant (`MACRO_X`) after its prefix, lower case.
fn suffix_macro(konst: &str) -> String {
    konst.trim_start_matches("MACRO_").to_lowercase()
}
