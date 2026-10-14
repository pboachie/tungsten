// SPDX-License-Identifier: AGPL-3.0-only
//! `lib/src/client.dart`: the client class over `ClientCore`, and in a
//! multi-namespace API one class per namespace.

use tungsten_emit::sdk::ClientPlan;
use tungsten_ir::RedirectPolicy;

use crate::dart::Code;
use crate::{Gen, Library};

use super::resources::{doc_text, late_member};
use super::{DISPATCH_CLASS, LibraryText, has_code_flow, macros_class, resource_file};

/// The client library.
pub(crate) fn library(g: &Gen<'_, '_>) -> Library {
    let client = &g.names.client_class;
    let mut locals: Vec<String> = vec!["descriptors.dart".into(), "dispatch.dart".into()];
    let macros = !g.plan.macros.is_empty();
    if macros {
        locals.push("macros.dart".into());
    }
    let title = {
        let t = g.plan.ir.api.title.trim();
        if t.is_empty() {
            g.plan.ir.api.name.wire.clone()
        } else {
            crate::dart::one_line(t)
        }
    };
    let mut w = Code::new();
    // The client's members: (name, class, doc).
    let mut members: Vec<(String, String, String)> = vec![];
    let mut namespaces: Vec<(String, Vec<(String, String, String)>)> = vec![];
    let resource_member = |r: usize, name: &str| -> (String, String, String) {
        let res = &g.plan.resources[r];
        let doc = doc_text(res.resource.doc.as_ref());
        let doc = if doc.is_empty() {
            format!("Operations under `{}`.", res.resource.path_prefix)
        } else {
            doc
        };
        (name.to_string(), g.names.resources[r].class.clone(), doc)
    };
    let mut resource_files: Vec<String> = vec![];
    match &g.plan.client {
        ClientPlan::Resources(top) => {
            for (r, m) in top.iter().zip(&g.names.client_members) {
                members.push(resource_member(*r, m));
                resource_files.push(resource_file(&g.names.resources[*r].class));
            }
        }
        ClientPlan::Namespaces(list) => {
            for (k, ns) in list.iter().enumerate() {
                let class = g.names.namespace_classes[k].clone();
                members.push((
                    g.names.client_members[k].clone(),
                    class.clone(),
                    format!("The `{}` namespace of the API.", ns.name),
                ));
                let inner: Vec<(String, String, String)> = ns
                    .resources
                    .iter()
                    .zip(&g.names.namespace_members[k])
                    .map(|(r, m)| {
                        resource_files.push(resource_file(&g.names.resources[*r].class));
                        resource_member(*r, m)
                    })
                    .collect();
                namespaces.push((class, inner));
            }
        }
    }
    resource_files.sort();
    resource_files.dedup();
    locals.extend(resource_files.into_iter().map(|f| format!("resources/{f}")));

    w.doc(&format!(
        "The client of the {title} API: every operation is a method of a resource member returning a `Future<Result<T>>`; API and transport failures are `Err` values, never exceptions."
    ));
    w.line(format!("final class {client} {{"));
    w.indent();
    w.doc("A client; throws `ClientConfigException` when the options are invalid (a base URL that is not an absolute http(s) URL without credentials).");
    let ctor = format!("{client}([ClientOptions options = const ClientOptions()])");
    let redirects_none = g.plan.ir.agent.redirects == RedirectPolicy::None;
    let init = "core = ClientCore(descriptors, options)";
    let end = if redirects_none { " {" } else { ";" };
    let flat = format!("{ctor} : {init}{end}");
    if w.fits(&flat) {
        w.line(flat);
    } else if w.fits(&ctor) {
        w.line(&ctor);
        w.indent();
        w.indent();
        w.line(format!(": {init}{end}"));
        w.dedent();
        w.dedent();
    } else {
        w.line(format!("{client}(["));
        w.indent();
        w.line("ClientOptions options = const ClientOptions(),");
        w.dedent();
        w.line(format!("]) : {init}{end}"));
    }
    if redirects_none {
        w.indent();
        w.line("ClientCore.requireRedirectPolicyV1();");
        w.dedent();
        w.line("}");
    }
    w.blank();
    w.doc("The runtime engine every method calls through.");
    w.line("final ClientCore core;");
    for (name, class, doc) in &members {
        w.blank();
        w.doc(doc);
        late_member(&mut w, class, name, &format!("{class}(core)"));
    }
    if macros {
        let class = macros_class(g);
        w.blank();
        w.doc("The macros of the API: several calls as one step, with previews.");
        late_member(&mut w, &class, "macros", &format!("{class}(core)"));
    }
    w.blank();
    w.doc("Operations by id and macros by name with JSON arguments, through the typed methods (for tools and drivers that only know ids and JSON).");
    late_member(&mut w, DISPATCH_CLASS, "dispatch", &format!("{DISPATCH_CLASS}(this)"));
    if has_code_flow(g) {
        w.blank();
        w.doc("The OAuth2 authorization-code helpers (PKCE, authorization URL, code exchange, refresh) of the scheme named [scheme].");
        w.line("OAuthFlow oauth(String scheme) => core.oauth(scheme);");
    }
    w.dedent();
    w.line("}");
    for (class, inner) in &namespaces {
        w.blank();
        w.doc("One namespace of the API: its top-level resources.");
        w.line(format!("final class {class} {{"));
        w.indent();
        w.doc("The namespace over the client's runtime engine.");
        w.line(format!("{class}(this.core);"));
        w.blank();
        w.doc("The runtime engine every method calls through.");
        w.line("final ClientCore core;");
        for (name, rclass, doc) in inner {
            w.blank();
            w.doc(doc);
            late_member(&mut w, rclass, name, &format!("{rclass}(core)"));
        }
        w.dedent();
        w.line("}");
    }
    let body = w.finish();
    let mut exports = vec![client.clone()];
    exports.extend(namespaces.iter().map(|(c, _)| c.clone()));
    let lib = LibraryText {
        doc: None,
        locals,
        wire: "wire.dart",
        models: "models/",
        runtime: None,
    };
    Library {
        path: "src/client.dart".into(),
        text: lib.finish(g, &body),
        exports: Some(exports),
    }
}
