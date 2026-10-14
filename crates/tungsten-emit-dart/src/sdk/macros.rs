// SPDX-License-Identifier: AGPL-3.0-only
//! `lib/src/macros.dart`: one method per macro in the canonical form
//! (`ClientCore.runMacro`) and one preview per macro
//! (`ClientCore.previewMacro`), with the macro's input as named
//! parameters.

use std::collections::BTreeSet;

use tungsten_emit::sdk::descriptors::safety_str;

use crate::dart::{Code, Expr, str_lit};
use crate::{Gen, Library};

use super::{
    LibraryText, args_doc, args_map, macro_args, macros_class, method_head, unique, upper_first,
};

/// The preview method names of the macros, by macro index: `preview<Macro>`,
/// unique among the macro members.
pub(crate) fn preview_names(g: &Gen<'_, '_>) -> Vec<String> {
    let mut taken: BTreeSet<String> = g.names.macros.iter().map(|m| m.member.clone()).collect();
    taken.insert("core".into());
    g.names
        .macros
        .iter()
        .map(|m| {
            let name = unique(format!("preview{}", upper_first(&m.member)), &taken);
            taken.insert(name.clone());
            name
        })
        .collect()
}

/// The macros library.
pub(crate) fn library(g: &Gen<'_, '_>) -> Library {
    let class = macros_class(g);
    let previews = preview_names(g);
    let mut w = Code::new();
    w.doc("The macros of the API: several calls as one step, with previews and confirmation.");
    w.line(format!("final class {class} {{"));
    w.indent();
    w.doc("The macros over the client's runtime engine.");
    w.line(format!("{class}(this._core);"));
    w.blank();
    w.line("final ClientCore _core;");
    for (k, m) in g.plan.macros.iter().enumerate() {
        let member = &g.names.macros[k].member;
        let args = macro_args(g, k);
        let summary = m.source.summary.trim();
        let mut doc = if summary.is_empty() {
            "Runs the macro.".to_string()
        } else {
            crate::dart::one_line(summary)
        };
        doc.push_str(&format!(
            "\n\nMacro `{}` ({}, {} step{}).",
            m.name(),
            safety_str(m.source.safety).replace('_', "-"),
            m.steps.len(),
            if m.steps.len() == 1 { "" } else { "s" }
        ));
        let params = args_doc(&args);
        if !params.is_empty() {
            doc.push_str("\n\n");
            doc.push_str(&params);
        }
        let name = Expr::raw(str_lit(m.name()));
        for (method, ret, run, text) in [
            (member.clone(), "Object?", "runMacro", doc.clone()),
            (
                previews[k].clone(),
                "PreviewResult",
                "previewMacro",
                format!(
                    "Renders every step of `{}` without sending anything, with a confirmation token bound to this input.",
                    m.name()
                ),
            ),
        ] {
            w.blank();
            w.doc(&text);
            method_head(&mut w, g, &format!("Future<Result<{ret}>> {method}"), &args);
            w.indent();
            args_map(&mut w, &args);
            w.expr(
                "return ",
                &Expr::call(
                    format!("_core.{run}"),
                    vec![
                        name.clone(),
                        Expr::raw("$args"),
                        Expr::named("options", Expr::raw("options")),
                    ],
                ),
                ";",
            );
            w.dedent();
            w.line("}");
        }
    }
    w.dedent();
    w.line("}");
    let body = w.finish();
    let lib = LibraryText {
        doc: None,
        locals: vec![],
        wire: "wire.dart",
        models: "models/",
        runtime: None,
    };
    Library {
        path: "src/macros.dart".into(),
        text: lib.finish(g, &body),
        exports: Some(vec![class]),
    }
}
