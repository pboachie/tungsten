// SPDX-License-Identifier: AGPL-3.0-only
//! `lib/src/resources/<resource>.dart`: one final class per resource over
//! the client's `ClientCore`, with a method per operation, its preview,
//! pages and stream members, and a getter per child resource.

use tungsten_emit::sdk::MemberKind;
use tungsten_emit::sdk::descriptors::{method_str, safety_str};
use tungsten_ir::Doc;

use crate::dart::{Code, Expr, one_line, str_lit};
use crate::models::Dt;
use crate::{Gen, Library};

use super::{
    Arg, LibraryText, args_doc, args_map, decoder_fn, method_head, op_args, resource_file,
    result_type, success_dt, upper_first,
};

/// The text of a doc: its summary, then its description when it says more.
pub(crate) fn doc_text(doc: Option<&Doc>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    let summary = doc.summary.as_deref().map(str::trim).unwrap_or("");
    let description = doc.description.as_deref().map(str::trim).unwrap_or("");
    let mut text = one_line(summary);
    if !description.is_empty() && description != summary {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(description);
    }
    text
}

/// The library of every resource, in plan order.
pub(crate) fn libraries(g: &Gen<'_, '_>) -> Vec<Library> {
    (0..g.plan.resources.len()).map(|r| library(g, r)).collect()
}

/// The `late final` declaration of a member `name` of class `class`
/// built from `core`.
pub(crate) fn late_member(w: &mut Code, class: &str, name: &str, init: &str) {
    let flat = format!("late final {class} {name} = {init};");
    if w.fits(&flat) {
        w.line(flat);
    } else {
        w.line(format!("late final {class} {name} ="));
        w.indent();
        w.indent();
        w.line(format!("{init};"));
        w.dedent();
        w.dedent();
    }
}

/// The receiver of the core in a method whose parameters are `args`:
/// `this.core` when a parameter is named `core`.
fn core_of(args: &[Arg]) -> &'static str {
    if args.iter().any(|a| a.name == "core") {
        "this.core"
    } else {
        "core"
    }
}

fn library(g: &Gen<'_, '_>, r: usize) -> Library {
    let plan = &g.plan.resources[r];
    let names = &g.names.resources[r];
    let class = &names.class;
    let mut w = Code::new();
    let mut decoders = Code::new();
    let mut locals: Vec<String> = vec![];
    let doc = doc_text(plan.resource.doc.as_ref());
    if doc.is_empty() {
        w.doc(&format!(
            "Operations under `{}`.",
            plan.resource.path_prefix
        ));
    } else {
        w.doc(&doc);
    }
    w.line(format!("final class {class} {{"));
    w.indent();
    w.doc("The resource over the client's runtime engine.");
    w.line(format!("{class}(this.core);"));
    w.blank();
    w.doc("The runtime engine every method calls through.");
    w.line("final ClientCore core;");
    for m in &names.members {
        w.blank();
        match m.kind {
            MemberKind::Child(c) => {
                let child = &g.names.resources[c].class;
                let cdoc = doc_text(g.plan.resources[c].resource.doc.as_ref());
                if cdoc.is_empty() {
                    w.doc(&format!(
                        "Operations under `{}`.",
                        g.plan.resources[c].resource.path_prefix
                    ));
                } else {
                    w.doc(&cdoc);
                }
                late_member(&mut w, child, &m.name, &format!("{child}(core)"));
                locals.push(resource_file(child));
            }
            MemberKind::Operation(o) => operation(g, &mut w, &mut decoders, o, &m.name),
            MemberKind::Preview(o) => preview(g, &mut w, o, &m.name),
            MemberKind::Pages(o) => pages(g, &mut w, &mut decoders, o, &m.name),
            MemberKind::Stream(o) => stream(g, &mut w, &mut decoders, o, &m.name),
        }
    }
    w.dedent();
    w.line("}");
    let decoders = decoders.finish();
    let mut body = w.finish();
    if !decoders.trim().is_empty() {
        body.push('\n');
        body.push_str(&decoders);
    }
    locals.sort();
    locals.dedup();
    let lib = LibraryText {
        doc: None,
        locals,
        wire: "../wire.dart",
        models: "../models/",
        runtime: None,
    };
    Library {
        path: format!("src/resources/{}", resource_file(class)),
        text: lib.finish(g, &body),
        exports: Some(vec![class.clone()]),
    }
}

/// The documentation of an operation's method.
fn op_doc(g: &Gen<'_, '_>, o: usize, args: &[Arg], tail: &str) -> String {
    let op = g.plan.operations[o].op;
    let mut text = doc_text(op.doc.as_ref());
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(&format!(
        "`{} {}` (operation `{}`, {}).",
        method_str(op.method),
        op.path.raw,
        op.id.0,
        safety_str(op.agent.safety).replace('_', "-")
    ));
    if op.deprecated {
        text.push_str(" Deprecated.");
    }
    if !tail.is_empty() {
        text.push(' ');
        text.push_str(tail);
    }
    let params = args_doc(args);
    if !params.is_empty() {
        text.push_str("\n\n");
        text.push_str(&params);
    }
    text
}

/// The operation's id as a Dart string literal.
fn id_lit(g: &Gen<'_, '_>, o: usize) -> String {
    str_lit(&g.plan.operations[o].op.id.0)
}

fn operation(g: &Gen<'_, '_>, w: &mut Code, decoders: &mut Code, o: usize, name: &str) {
    let op = &g.plan.operations[o];
    let args = op_args(g, o);
    let dt = success_dt(g, o);
    let ty = result_type(g, dt.as_ref(), op.bodiless_success);
    let tail = "Completes with `Ok` and the typed value, or `Err` with the diagnostic envelope.";
    w.doc(&op_doc(g, o, &args, tail));
    method_head(w, g, &format!("Future<Result<{ty}>> {name}"), &args);
    w.indent();
    args_map(w, &args);
    let mut call = vec![
        Expr::raw(id_lit(g, o)),
        Expr::raw("$args"),
        Expr::named("options", Expr::raw("options")),
    ];
    if let Some(dt) = &dt {
        let f = format!("_decode{}", upper_first(name));
        decoders.blank();
        decoders.doc(&format!("The typed success value of `{}`.", op.op.id.0));
        decoder_fn(decoders, g, &f, dt);
        call.push(Expr::named("decode", Expr::raw(f)));
    }
    let core = core_of(&args);
    w.expr(
        "return ",
        &Expr::call(format!("{core}.call<{ty}>"), call),
        ";",
    );
    w.dedent();
    w.line("}");
}

fn preview(g: &Gen<'_, '_>, w: &mut Code, o: usize, name: &str) {
    let args = op_args(g, o);
    let tail = "Renders the request without sending it; a mutating operation's result carries a confirmation token bound to these arguments, passed back as `CallOptions(confirm: Confirm.token(...))`.";
    w.doc(&op_doc(g, o, &args, tail));
    method_head(
        w,
        g,
        &format!("Future<Result<PreviewResult>> {name}"),
        &args,
    );
    w.indent();
    args_map(w, &args);
    let core = core_of(&args);
    w.expr(
        "return ",
        &Expr::call(
            format!("{core}.preview"),
            vec![
                Expr::raw(id_lit(g, o)),
                Expr::raw("$args"),
                Expr::named("options", Expr::raw("options")),
            ],
        ),
        ";",
    );
    w.dedent();
    w.line("}");
}

fn pages(g: &Gen<'_, '_>, w: &mut Code, decoders: &mut Code, o: usize, name: &str) {
    let op = &g.plan.operations[o];
    let args = op_args(g, o);
    let item: Option<Dt> = op
        .page
        .as_ref()
        .and_then(|p| p.item.as_ref())
        .map(|t| g.dt(t))
        .filter(|d| *d != Dt::Json);
    let ty = result_type(g, item.as_ref(), false);
    let tail = "Every page from the one these arguments select, as a stream of results: one `Ok` per page, or a final `Err`.";
    w.doc(&op_doc(g, o, &args, tail));
    method_head(w, g, &format!("Stream<Result<Page<{ty}>>> {name}"), &args);
    w.indent();
    args_map(w, &args);
    let core = core_of(&args);
    let raw = Expr::call(
        format!("{core}.pages"),
        vec![
            Expr::raw(id_lit(g, o)),
            Expr::raw("$args"),
            Expr::named("options", Expr::raw("options")),
        ],
    );
    match &item {
        Some(dt) => {
            let f = format!("_decode{}", upper_first(name));
            decoders.blank();
            decoders.doc(&format!("One typed item of a page of `{}`.", op.op.id.0));
            decoder_fn(decoders, g, &f, dt);
            w.expr(
                "return ",
                &Expr::call(format!("Typed.pages<{ty}>"), vec![raw, Expr::raw(f)]),
                ";",
            );
        }
        None => w.expr("return ", &raw, ";"),
    }
    w.dedent();
    w.line("}");
}

fn stream(g: &Gen<'_, '_>, w: &mut Code, decoders: &mut Code, o: usize, name: &str) {
    let op = &g.plan.operations[o];
    let args = op_args(g, o);
    let event: Option<Dt> = op
        .stream
        .as_ref()
        .filter(|s| s.typed)
        .map(|s| g.dt(&s.event))
        .filter(|d| *d != Dt::Json);
    let ty = result_type(g, event.as_ref(), false);
    let mut tail = "The server-sent events of the operation as an `EventStream`: one event per message, then the end of the stream or one final failure; it also has `on`, `collect`, `reduce`, `first` and `cancel`, and a dropped stream of a call that is safe to repeat is resumed with `Last-Event-ID`.".to_string();
    if let Some(s) = &op.stream {
        if let Some(done) = &s.done {
            tail.push_str(&format!(
                " The stream ends at the event whose data is `{done}`, which is not delivered."
            ));
        }
        if let Some(flag) = &s.flag {
            tail.push_str(&format!(" Sets `{flag}` in the request body."));
        }
    }
    w.doc(&op_doc(g, o, &args, &tail));
    method_head(w, g, &format!("EventStream<{ty}> {name}"), &args);
    w.indent();
    args_map(w, &args);
    let mut call = vec![
        Expr::raw(id_lit(g, o)),
        Expr::raw("$args"),
        Expr::named("options", Expr::raw("options")),
    ];
    if let Some(dt) = &event {
        let f = format!("_decode{}", upper_first(name));
        decoders.blank();
        decoders.doc(&format!("The typed data of one event of `{}`.", op.op.id.0));
        decoder_fn(decoders, g, &f, dt);
        call.push(Expr::named("decode", Expr::raw(f)));
    }
    let core = core_of(&args);
    w.expr(
        "return ",
        &Expr::call(format!("{core}.stream<{ty}>"), call),
        ";",
    );
    w.dedent();
    w.line("}");
}
