// SPDX-License-Identifier: AGPL-3.0-only
//! `lib/src/dispatch.dart`: `Dispatch`, the runtime's `Dispatcher` over
//! the typed methods.
//!
//! Each entry lists the arguments layout keys its typed method requires
//! and every key it takes; the runtime routes arguments that fit to the
//! entry and any others to the core as they are, so pre-flight validation
//! reports them. An entry reads each argument with the decoder of its
//! Dart type; a value that does not decode (a `FormatException`) also goes
//! to the core as it is.

use tungsten_emit::sdk::MemberKind;

use crate::dart::{Code, Expr, str_lit};
use crate::{Gen, Library};

use super::{
    Arg, DISPATCH_CLASS, LibraryText, arg_in, macro_args, member_name, op_args, resource_paths,
};

/// One typed entry: its key in the table, the arguments, the typed call
/// (`client.pets.list`) and the call of the core it falls back to.
struct Entry {
    key: String,
    args: Vec<Arg>,
    typed: String,
    fallback: Expr,
    doc: String,
}

/// One table of the dispatcher.
struct Table {
    /// The `Dispatcher` constructor parameter.
    param: &'static str,
    /// The entry result type (`DispatchEntry<R>`).
    result: &'static str,
    /// The prefix of the entry functions (`_call`).
    prefix: &'static str,
    entries: Vec<Entry>,
}

fn fallback(method: &str, ty: Option<&str>, key: &str) -> Expr {
    let callee = match ty {
        Some(t) => format!("client.core.{method}<{t}>"),
        None => format!("client.core.{method}"),
    };
    Expr::call(
        callee,
        vec![
            Expr::raw(str_lit(key)),
            Expr::raw("args"),
            Expr::named("options", Expr::raw("options")),
        ],
    )
}

fn tables(g: &Gen<'_, '_>) -> Vec<Table> {
    let paths = resource_paths(g);
    let mut calls = vec![];
    let mut previews = vec![];
    let mut pages = vec![];
    let mut streams = vec![];
    for (o, op) in g.plan.operations.iter().enumerate() {
        let id = op.op.id.0.clone();
        let base = &paths[op.resource];
        let typed = |kind: MemberKind| -> Option<String> {
            member_name(g, op.resource, kind).map(|m| format!("client.{base}.{m}"))
        };
        if let Some(t) = typed(MemberKind::Operation(o)) {
            calls.push(Entry {
                key: id.clone(),
                args: op_args(g, o),
                typed: t,
                fallback: fallback("call", Some("Object?"), &id),
                doc: format!("Calls `{id}`."),
            });
        }
        if let Some(t) = typed(MemberKind::Preview(o)) {
            previews.push(Entry {
                key: id.clone(),
                args: op_args(g, o),
                typed: t,
                fallback: fallback("preview", None, &id),
                doc: format!("Previews `{id}`."),
            });
        }
        if let Some(t) = typed(MemberKind::Pages(o)) {
            pages.push(Entry {
                key: id.clone(),
                args: op_args(g, o),
                typed: t,
                fallback: fallback("pages", None, &id),
                doc: format!("The pages of `{id}`."),
            });
        }
        if let Some(t) = typed(MemberKind::Stream(o)) {
            streams.push(Entry {
                key: id.clone(),
                args: op_args(g, o),
                typed: t,
                fallback: fallback("stream", Some("Object?"), &id),
                doc: format!("The events of `{id}`."),
            });
        }
    }
    let macros = g
        .plan
        .macros
        .iter()
        .enumerate()
        .map(|(k, m)| Entry {
            key: m.name().to_string(),
            args: macro_args(g, k),
            typed: format!("client.macros.{}", g.names.macros[k].member),
            fallback: fallback("runMacro", None, m.name()),
            doc: format!("Runs the macro `{}`.", m.name()),
        })
        .collect();
    vec![
        Table {
            param: "operationEntries",
            result: "Future<Result<Object?>>",
            prefix: "_call",
            entries: calls,
        },
        Table {
            param: "previewEntries",
            result: "Future<Result<PreviewResult>>",
            prefix: "_preview",
            entries: previews,
        },
        Table {
            param: "pageEntries",
            result: "Object",
            prefix: "_pages",
            entries: pages,
        },
        Table {
            param: "streamEntries",
            result: "Object",
            prefix: "_stream",
            entries: streams,
        },
        Table {
            param: "macroEntries",
            result: "Object",
            prefix: "_macro",
            entries: macros,
        },
    ]
}

/// A list literal of string keys after `prefix`: on one line when it
/// fits, else one key per line with a trailing comma.
fn keys(w: &mut Code, prefix: &str, keys: &[String], suffix: &str) {
    let items: Vec<String> = keys.iter().map(|k| str_lit(k)).collect();
    let flat = format!("{prefix}const [{}]{suffix}", items.join(", "));
    if w.fits(&flat) {
        w.line(flat);
        return;
    }
    w.line(format!("{prefix}const ["));
    w.indent();
    for i in &items {
        w.line(format!("{i},"));
    }
    w.dedent();
    w.line(format!("]{suffix}"));
}

/// The dispatcher library.
pub(crate) fn library(g: &Gen<'_, '_>) -> Library {
    let client = &g.names.client_class;
    let tables = tables(g);
    let mut w = Code::new();
    w.doc("Operations by id and macros by name with JSON arguments keyed by the arguments layout keys, through the typed methods of a client (the runtime's `Dispatcher`). Arguments that do not fit a typed method go to the core as they are, so pre-flight validation reports them.");
    w.line(format!(
        "final class {DISPATCH_CLASS} extends Dispatcher {{"
    ));
    w.indent();
    w.doc("The dispatcher of [client].");
    let used: Vec<&Table> = tables.iter().filter(|t| !t.entries.is_empty()).collect();
    if used.is_empty() {
        w.line(format!(
            "{DISPATCH_CLASS}({client} client) : super(client.core);"
        ));
    } else {
        w.line(format!("{DISPATCH_CLASS}({client} client)"));
        w.indent();
        w.indent();
        w.line(": super(");
        w.indent();
        w.indent();
        w.line("client.core,");
        for t in &used {
            w.line(format!("{}: {}(client),", t.param, table_builder(t.param)));
        }
        w.dedent();
        w.line(");");
        w.dedent();
        w.dedent();
        w.dedent();
    }
    for t in &used {
        let builder = table_builder(t.param);
        w.blank();
        let flat = format!(
            "static Map<String, DispatchEntry<{}>> {builder}({client} client) {{",
            t.result
        );
        if w.fits(&flat) {
            w.line(flat);
        } else {
            w.line(format!(
                "static Map<String, DispatchEntry<{}>> {builder}(",
                t.result
            ));
            w.indent();
            w.line(format!("{client} client,"));
            w.dedent();
            w.line(") {");
        }
        w.indent();
        w.line("return {");
        w.indent();
        for (i, e) in t.entries.iter().enumerate() {
            w.line(format!("{}: (", str_lit(&e.key)));
            w.indent();
            let required: Vec<String> = e
                .args
                .iter()
                .filter(|a| a.required())
                .map(|a| a.key.clone())
                .collect();
            let every: Vec<String> = e.args.iter().map(|a| a.key.clone()).collect();
            keys(&mut w, "required: ", &required, ",");
            keys(&mut w, "every: ", &every, ",");
            w.line(format!(
                "call: (args, options) => {}{i}(client, args, options),",
                t.prefix
            ));
            w.dedent();
            w.line("),");
        }
        w.dedent();
        w.line("};");
        w.dedent();
        w.line("}");
        for (i, e) in t.entries.iter().enumerate() {
            w.blank();
            w.doc(&e.doc);
            w.line(format!("static {} {}{i}(", t.result, t.prefix));
            w.indent();
            w.line(format!("{client} client,"));
            w.line("Map<String, Object?> args,");
            w.line("CallOptions options,");
            w.dedent();
            w.line(") {");
            w.indent();
            w.line("try {");
            w.indent();
            let mut named: Vec<Expr> = e
                .args
                .iter()
                .map(|a| Expr::named(a.name.clone(), arg_in(g, a)))
                .collect();
            named.push(Expr::named("options", Expr::raw("options")));
            super::expr(&mut w, "return ", &Expr::call(e.typed.clone(), named), ";");
            w.dedent();
            w.line("} on FormatException {");
            w.indent();
            w.expr("return ", &e.fallback, ";");
            w.dedent();
            w.line("}");
            w.dedent();
            w.line("}");
        }
    }
    w.dedent();
    w.line("}");
    let optional = tables
        .iter()
        .flat_map(|t| &t.entries)
        .flat_map(|e| &e.args)
        .any(|a| matches!(a.presence, tungsten_ir::Presence::Optional));
    if optional {
        w.blank();
        w.doc("An optional argument: null when absent; a value (null included) that its decoder refuses throws a `FormatException`.");
        w.line("T? _optional<T>(");
        w.indent();
        w.line("Map<String, Object?> args,");
        w.line("String key,");
        w.line("Decoder<T> decoder,");
        w.dedent();
        w.line(") {");
        w.indent();
        w.line("if (!args.containsKey(key)) return null;");
        w.line("return decoder(args[key], key);");
        w.dedent();
        w.line("}");
    }
    let body = w.finish();
    let lib = LibraryText {
        doc: None,
        locals: vec!["client.dart".to_string()],
        wire: "wire.dart",
        models: "models/",
        runtime: None,
    };
    Library {
        path: "src/dispatch.dart".into(),
        text: lib.finish(g, &body),
        exports: Some(vec![DISPATCH_CLASS.into()]),
    }
}

/// The static method that builds the table of a `Dispatcher` parameter.
fn table_builder(param: &str) -> &'static str {
    match param {
        "operationEntries" => "_operations",
        "previewEntries" => "_previews",
        "pageEntries" => "_pages",
        "streamEntries" => "_streams",
        _ => "_macros",
    }
}
