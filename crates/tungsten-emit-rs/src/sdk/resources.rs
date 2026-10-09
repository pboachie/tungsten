// SPDX-License-Identifier: AGPL-3.0-only
//! Resource modules (`resources/<name>.rs`: the resource struct, its
//! methods and the request structs of its operations), the client
//! (`client.rs`) and the crate entry point (`lib.rs`).
//!
//! Each operation is a method `(request, &CallOptions)` returning
//! `tungsten_runtime::Result<T>`; operations with a preview get
//! `preview_<method>` and paginated ones `<method>_pages`. Resources are
//! reached through accessor methods that borrow the client:
//! `client.webhooks().rotate(request, &opts).await`.

use tungsten_emit::Writer;
use tungsten_ir::{IdempotencyKind, OperationStatus, Safety};

use super::ops::{OpShape, fn_sig, idempotency_str, method_str, safety_str, write_request};
use super::plan::{MemberKind, OpInfo, Plan, ResInfo};
use super::rs::{Rx, doc, doc_text, imports_for, paragraphs, put};
use super::types::{Cx, write_patterns};

/// The documentation of an operation method.
pub(crate) fn op_doc(
    plan: &Plan<'_>,
    info: &OpInfo<'_>,
    shape: &OpShape<'_>,
    preview: Option<&str>,
    pages: Option<&str>,
    stream: Option<&str>,
) -> String {
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
    let preview_call = preview
        .map(|p| format!("`{p}()`"))
        .unwrap_or_else(|| "a preview".to_string());
    match a.safety {
        Safety::Destructive => parts.push(format!(
            "Requires confirmation: pass `confirm: Some(Confirm::Yes)` in the call options, or the `confirmation_token` of {preview_call} as `Confirm::Token`."
        )),
        Safety::Irreversible => parts.push(format!(
            "Cannot be undone: call {preview_call} first and pass its `confirmation_token` as `Confirm::Token` in the call options."
        )),
        Safety::ReadOnly | Safety::Mutating => {}
    }
    let idem = &a.idempotency;
    match idem.policy {
        IdempotencyKind::CallerOwned => parts.push(format!(
            "Idempotency (`caller_owned`): generate a key{}, persist it with your intent, pass it as `idempotency_key` in the call options, and reuse it on every retry.{}",
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
    if let Some(p) = pages {
        parts.push(format!("`{p}()` iterates every page."));
    }
    if let Some(p) = stream {
        parts.push(format!("`{p}()` iterates the events of its stream."));
    }
    if shape.bodiless {
        parts.push("A success response without a body yields `None` (or `()`).".into());
    }
    if shape.mixed_success {
        parts.push(
            "The operation answers with different bodies; the value is `serde_json::Value`.".into(),
        );
    }
    if let Some(cluster) = &a.cluster {
        parts.push(format!("Cluster: `{cluster}`."));
    }
    if op.deprecated {
        parts.push("Deprecated.".into());
    }
    paragraphs(parts)
}

/// The method names of one operation in its resource.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpNames {
    pub call: String,
    pub preview: Option<String>,
    pub pages: Option<String>,
    pub stream: Option<String>,
}

pub(crate) fn op_names(r: &ResInfo<'_>, o: usize) -> OpNames {
    let mut names = OpNames::default();
    for m in &r.members {
        match m.kind {
            MemberKind::Op(i) if i == o => names.call = m.name.clone(),
            MemberKind::Preview(i) if i == o => names.preview = Some(m.name.clone()),
            MemberKind::Pages(i) if i == o => names.pages = Some(m.name.clone()),
            MemberKind::Stream(i) if i == o => names.stream = Some(m.name.clone()),
            _ => {}
        }
    }
    names
}

/// Doc of a resource struct or accessor.
pub(crate) fn resource_doc(r: &ResInfo<'_>) -> String {
    let text = doc_text(r.res.doc.as_ref());
    if text.is_empty() {
        format!("Operations under `{}`.", r.res.path_prefix)
    } else {
        text
    }
}

/// The accessor-struct skeleton shared by resources, namespaces and
/// macros: a reference to the client with its constructor and getter.
pub(crate) fn write_holder(w: &mut Writer, client: &str, name: &str, text: &str) {
    doc(w, text);
    w.line("#[derive(Debug, Clone, Copy)]");
    w.line(format!("pub struct {name}<'a> {{"));
    w.indent();
    w.line(format!("client: &'a {client},"));
    w.dedent();
    w.line("}");
    w.blank();
    w.line(format!("impl<'a> {name}<'a> {{"));
    w.indent();
    w.line(format!(
        "pub(crate) fn new(client: &'a {client}) -> Self {{"
    ));
    w.indent();
    w.line("Self { client }");
    w.dedent();
    w.line("}");
    w.blank();
    doc(w, "The client these operations go through.");
    w.line(format!("pub fn client(&self) -> &'a {client} {{"));
    w.indent();
    w.line("self.client");
    w.dedent();
    w.line("}");
}

/// `let client = self.client;` and `let op = client.operation(CONST);`.
fn op_lines(w: &mut Writer, konst: &str) {
    w.line("let client = self.client;");
    let call = Rx::call("client.operation", vec![Rx::atom(konst)]);
    put(w, 8, "let op = ", &call, ";");
}

/// One method (an operation, its preview, its pages or its stream) of a resource.
fn write_method(
    w: &mut Writer,
    plan: &Plan<'_>,
    info: &OpInfo<'_>,
    shape: &OpShape<'_>,
    names: &OpNames,
    kind: MemberKind,
    name: &str,
) {
    let req = &info.request;
    let konst = format!("descriptors::{}", info.konst);
    w.blank();
    match kind {
        MemberKind::Preview(_) => {
            doc(
                w,
                &paragraphs([format!(
                    "Preview `{}` without sending it: the request, its effects and, when a confirmation is required, a `confirmation_token` to pass to `{}()` as `Confirm::Token`.",
                    info.op.id.0, names.call
                )]),
            );
            let params = vec![
                "&self".to_string(),
                format!("request: {req}"),
                "opts: &CallOptions".to_string(),
            ];
            fn_sig(
                w,
                4,
                &format!("pub async fn {name}"),
                &params,
                "tungsten_runtime::Result<PreviewResult>",
            );
            w.indent();
            op_lines(w, &konst);
            w.line("let args = s::args(&request);");
            w.line("client.core.preview(op, args, opts).await");
        }
        MemberKind::Pages(_) => {
            let item = shape.page_item.clone().unwrap_or_else(|| "Value".into());
            doc(
                w,
                &format!(
                    "Every page of `{}`, following the cursor until the last page. A failed page is yielded as its error and ends the iteration. With response validation on, the runtime validates each item; an item that does not decode as `{item}` is an `UNEXPECTED_RESPONSE` error.",
                    info.op.id.0
                ),
            );
            let params = vec![
                "&self".to_string(),
                format!("request: {req}"),
                "opts: &CallOptions".to_string(),
            ];
            fn_sig(
                w,
                4,
                &format!("pub fn {name}"),
                &params,
                &format!("TypedPages<{item}>"),
            );
            w.indent();
            op_lines(w, &konst);
            w.line("let args = s::args(&request);");
            w.line("let pages = client.core.pages(op.clone(), args, opts.clone());");
            w.line("pages.typed()");
        }
        MemberKind::Stream(_) => {
            let event = shape
                .stream
                .as_ref()
                .map_or_else(|| "Value".to_string(), |s| s.event.clone());
            let mut parts = vec![format!(
                "The events of `{}` (`{}`): one item per server-sent event, then either the end of the stream or one final error. An error before the stream starts is the only item. The request is sent by the first `next()`; dropping the stream closes the connection. An event that does not decode as `{event}` is an `UNEXPECTED_RESPONSE` error.",
                info.op.id.0,
                method_str(info.op.method)
            )];
            if let Some(spec) = &info.op.stream {
                if let Some(done) = &spec.done {
                    parts.push(format!(
                        "The stream ends at the event whose data is `{done}`, which is not delivered."
                    ));
                }
                if let Some(flag) = &spec.request_flag {
                    parts.push(format!("Sets `{flag}` in the request body."));
                }
            }
            doc(w, &paragraphs(parts));
            let params = vec![
                "&self".to_string(),
                format!("request: {req}"),
                "opts: &CallOptions".to_string(),
            ];
            fn_sig(
                w,
                4,
                &format!("pub fn {name}"),
                &params,
                &format!("TypedEvents<{event}>"),
            );
            w.indent();
            op_lines(w, &konst);
            w.line("let args = s::args(&request);");
            let spec = Rx::call("client.stream_descriptor", vec![Rx::atom(konst.clone())]);
            put(w, 8, "let spec = ", &spec, ";");
            w.line("let events = client.core.stream(op.clone(), spec, args, opts.clone());");
            w.line("events.typed()");
        }
        MemberKind::Op(_) | MemberKind::Child(_) => {
            doc(
                w,
                &op_doc(
                    plan,
                    info,
                    shape,
                    names.preview.as_deref(),
                    names.pages.as_deref(),
                    names.stream.as_deref(),
                ),
            );
            let params = vec![
                "&self".to_string(),
                format!("request: {req}"),
                "opts: &CallOptions".to_string(),
            ];
            fn_sig(
                w,
                4,
                &format!("pub async fn {name}"),
                &params,
                &format!("tungsten_runtime::Result<{}>", shape.success),
            );
            w.indent();
            op_lines(w, &konst);
            w.line("let args = s::args(&request);");
            w.line("let outcome = client.core.call(op, args, opts).await;");
            w.line("decode(&op.id, outcome)");
        }
    }
    w.dedent();
    w.line("}");
}

/// The source of one resource module.
pub(crate) fn resource_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    idx: usize,
    header: &str,
) -> String {
    let r = &plan.resources[idx];
    let client = &plan.client_class;
    let cx = Cx::new(plan, None);
    let mut code = Writer::new("    ");
    let mut extra: Vec<(String, String)> = vec![("crate::client".into(), client.clone())];
    if r.members
        .iter()
        .any(|m| !matches!(m.kind, MemberKind::Child(_)))
    {
        extra.push(("crate".into(), "descriptors".into()));
    }
    write_holder(&mut code, client, &r.strukt, &resource_doc(r));
    for m in &r.members {
        if let MemberKind::Child(c) = m.kind {
            let child = &plan.resources[c];
            extra.push((
                format!("crate::resources::{}", child.module),
                child.strukt.clone(),
            ));
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
            MemberKind::Op(o)
            | MemberKind::Preview(o)
            | MemberKind::Pages(o)
            | MemberKind::Stream(o) => o,
            MemberKind::Child(_) => continue,
        };
        let names = op_names(r, o);
        write_method(
            &mut code,
            plan,
            &plan.ops[o],
            &shapes[o],
            &names,
            m.kind,
            &m.name,
        );
    }
    code.dedent();
    code.line("}");
    // Request structs of the resource's operations.
    for m in &r.members {
        if let MemberKind::Op(o) = m.kind {
            code.blank();
            write_request(&mut code, &cx, &plan.ops[o], &shapes[o]);
        }
    }
    let patterns = cx.patterns.borrow().clone();
    write_patterns(&mut code, &patterns);
    let code = code.finish();

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line(format!(
        "//! Resource `{}`.",
        r.res.path_prefix.replace('\n', " ")
    ));
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

/// The source of `resources/mod.rs`.
pub(crate) fn resources_mod(plan: &Plan<'_>, header: &str) -> String {
    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line("//! Resources, one module each.");
    w.blank();
    let mut modules: Vec<&str> = plan.resources.iter().map(|r| r.module.as_str()).collect();
    modules.sort_unstable();
    for m in modules {
        w.line(format!("pub mod {m};"));
    }
    w.finish()
}

/// The source of `client.rs`.
pub(crate) fn client_file(plan: &Plan<'_>, has_macros: bool, header: &str) -> String {
    let ir = plan.ir;
    let client = &plan.client_class;
    let mut code = Writer::new("    ");
    let mut extra: Vec<(String, String)> =
        vec![("crate::descriptors".into(), "Descriptors".into())];
    doc(
        &mut code,
        &paragraphs([
            format!("Client for the {} API.", ir.api.title),
            ir.api.description.clone().unwrap_or_default(),
            "Every call returns `tungsten_runtime::Result<T>`: the value with its response metadata, or the diagnostic envelope. API and transport errors are values, never panics. Arguments are validated before any request is sent.".to_string(),
        ]),
    );
    code.line("#[derive(Debug, Clone)]");
    code.line(format!("pub struct {client} {{"));
    code.indent();
    code.line("pub(crate) core: ClientCore,");
    code.line("pub(crate) descriptors: Arc<Descriptors>,");
    code.dedent();
    code.line("}");
    code.blank();
    code.line(format!("impl {client} {{"));
    code.indent();
    doc(
        &mut code,
        "Create a client from `options`. Every operation descriptor of the API is registered with the runtime, so it resolves operations by id (verification hooks, endpoint previews, macro steps).",
    );
    code.line("pub fn new(mut options: ClientOptions) -> Result<Self, ConfigError> {");
    code.indent();
    code.line("let descriptors = crate::descriptors::get();");
    code.line("let own = descriptors.operations.iter().cloned();");
    code.line("options.operations = own.chain(options.operations).collect();");
    code.line("let core = ClientCore::new(descriptors.api.clone(), options)?;");
    put(
        &mut code,
        8,
        "Ok(",
        &Rx::record(
            "Self",
            vec![
                ("core", Rx::atom("core")),
                ("descriptors", Rx::atom("descriptors")),
            ],
        ),
        ")",
    );
    code.dedent();
    code.line("}");
    code.blank();
    doc(&mut code, "The runtime core every call goes through.");
    code.line("pub fn core(&self) -> &ClientCore {");
    code.indent();
    code.line("&self.core");
    code.dedent();
    code.line("}");
    code.blank();
    doc(&mut code, "The operation, API and macro descriptors.");
    code.line("pub fn descriptors(&self) -> &Descriptors {");
    code.indent();
    code.line("&self.descriptors");
    code.dedent();
    code.line("}");
    code.blank();
    code.line("#[allow(dead_code)]");
    code.line("pub(crate) fn operation(&self, index: usize) -> &Arc<OperationDescriptor> {");
    code.indent();
    code.line("&self.descriptors.operations[index]");
    code.dedent();
    code.line("}");
    if plan.ops.iter().any(|o| o.op.stream.is_some()) {
        code.blank();
        code.line("#[allow(dead_code)]");
        code.line(
            "pub(crate) fn stream_descriptor(&self, index: usize) -> Arc<StreamDescriptor> {",
        );
        code.indent();
        code.line("self.descriptors");
        code.indent();
        code.line(".streams");
        code.line(".iter()");
        code.line(".find(|(i, _)| *i == index)");
        code.line(".map(|(_, spec)| spec.clone())");
        code.line(".expect(\"a descriptor for every streaming operation\")");
        code.dedent();
        code.dedent();
        code.line("}");
    }
    if has_macros {
        extra.push(("crate::macros".into(), "Macros".into()));
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
            extra.push((format!("crate::resources::{}", r.module), r.strukt.clone()));
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
            client,
            &n.strukt,
            &format!("The `{}` namespace: {}.", n.ns.name.wire, n.ns.title),
        );
        for (name, i) in &n.members {
            let r = &plan.resources[*i];
            extra.push((format!("crate::resources::{}", r.module), r.strukt.clone()));
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
    let code = code.finish();

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line(format!(
        "//! Client of the {} API.",
        ir.api.title.replace('\n', " ")
    ));
    let uses = imports_for(&code, &["Descriptors"], None, &extra);
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
