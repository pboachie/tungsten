// SPDX-License-Identifier: AGPL-3.0-only
//! The gem's code besides the models: the entry file, the resources, the
//! client, the macros, the dispatch table and the descriptor document.

use tungsten_emit::Writer;
use tungsten_emit::sdk::descriptors::{op_summary, safety_str};
use tungsten_emit::sdk::{
    ArgPlan, ArgSource, ClientPlan, DocumentOptions, MemberKind, MemberPlan, OpPlan, SuccessPlan,
    document_value,
};
use tungsten_ir::{Presence, Safety};

use crate::context::Gen;
use crate::rb::{comment, one_line, str_lit};

/// Every file of this module.
pub fn files(g: &Gen<'_, '_>) -> Vec<(String, String)> {
    let lib = g.lib();
    let mut out = vec![(format!("lib/{lib}.rb"), entry(g))];
    for i in 0..g.plan.resources.len() {
        out.push((
            format!("lib/{lib}/resources/{}.rb", g.resource_file(i)),
            resource(g, i),
        ));
    }
    out.push((format!("lib/{lib}/client.rb"), client(g)));
    out.push((format!("lib/{lib}/macros.rb"), macros(g)));
    out.push((format!("lib/{lib}/dispatch.rb"), dispatch(g)));
    out.push((
        format!("lib/{lib}/tungsten-descriptors.json"),
        descriptors(g),
    ));
    out.push((format!("lib/{lib}/custom/extensions.rb"), custom(g)));
    out
}

/// The descriptor document, pretty-printed.
fn descriptors(g: &Gen<'_, '_>) -> String {
    let doc = document_value(
        g.plan,
        &g.names,
        &DocumentOptions {
            version: g.opts.version.clone(),
        },
    );
    let mut text = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    text
}

fn entry(g: &Gen<'_, '_>) -> String {
    let lib = g.lib();
    let mut w = Writer::new("  ");
    g.file_start(&mut w);
    w.line("require \"tungsten/runtime\"");
    w.blank();
    let title = &g.plan.ir.api.title;
    comment(
        &mut w,
        &format!(
            "The Ruby SDK of {}. Create a client with `{}::{}.new(base_url: ..., auth: {{ ... }})`.",
            if title.trim().is_empty() {
                g.plan.ir.api.name.wire.as_str()
            } else {
                title.trim()
            },
            g.module(),
            g.names.client_class
        ),
    );
    g.in_module(&mut w, |w| {
        comment(w, "The gem version.");
        w.line(format!("VERSION = {}", str_lit(&g.opts.version)));
        comment(
            w,
            "The SDK descriptor document: every operation and macro the runtime calls.",
        );
        if g.plan.ir.agent.redirects == tungsten_ir::RedirectPolicy::None {
            w.line("::Tungsten::Runtime::ClientCore.require_redirect_policy_v1");
        }
        if g
            .plan
            .operations
            .iter()
            .any(|p| p.op.agent.import_policy.is_some())
        {
            w.line("::Tungsten::Runtime::ClientCore.require_import_policy_v1");
        }
        w.line(format!(
            "DESCRIPTORS = ::Tungsten::Runtime::Descriptors.load(File.join(__dir__, {}, \"tungsten-descriptors.json\"))",
            str_lit(&lib)
        ));
    });
    w.blank();
    for ns in &g.plan.namespaces {
        if !ns.types.is_empty() {
            w.line(format!(
                "require_relative {}",
                str_lit(&format!("{lib}/models/{}", g.ns_file(&ns.name)))
            ));
        }
    }
    for i in 0..g.plan.resources.len() {
        w.line(format!(
            "require_relative {}",
            str_lit(&format!("{lib}/resources/{}", g.resource_file(i)))
        ));
    }
    for file in ["macros", "client", "dispatch", "custom/extensions"] {
        w.line(format!(
            "require_relative {}",
            str_lit(&format!("{lib}/{file}"))
        ));
    }
    w.finish()
}

/// The keyword parameters of an argument list: required ones first, then
/// the optional ones (default `UNSET`), then `opts: nil`.
fn signature(args: &[&ArgPlan<'_>], names: &[String]) -> String {
    let mut required = vec![];
    let mut optional = vec![];
    for (a, n) in args.iter().zip(names) {
        match a.presence {
            Presence::Required | Presence::RequiredNullable => required.push(format!("{n}:")),
            Presence::Optional | Presence::OptionalNullable => {
                optional.push(format!("{n}: ::Tungsten::Runtime::UNSET"))
            }
        }
    }
    required.extend(optional);
    required.push("opts: nil".to_string());
    required.join(", ")
}

/// The arguments object: `{ "name" => name, ... }`.
fn args_hash(names: &[String]) -> String {
    if names.is_empty() {
        return "{}".to_string();
    }
    let pairs: Vec<String> = names
        .iter()
        .map(|n| format!("{} => {n}", str_lit(n)))
        .collect();
    format!("{{ {} }}", pairs.join(", "))
}

fn arg_doc(g: &Gen<'_, '_>, a: &ArgPlan<'_>) -> String {
    let ty = match (&a.source, &a.ty) {
        (_, Some(ty)) => g.type_doc(ty),
        (ArgSource::Body { .. }, None) => "Tungsten::Runtime::Binary, String".to_string(),
        _ => "Object".to_string(),
    };
    let doc = match &a.source {
        ArgSource::Param { param, .. } => param.doc.as_ref(),
        ArgSource::Field(f) => f.doc.as_ref(),
        ArgSource::Body { .. } => None,
    };
    let text = doc
        .and_then(|d| d.description.as_deref().or(d.summary.as_deref()))
        .map(one_line)
        .unwrap_or_default();
    let place = match &a.source {
        ArgSource::Param { location, .. } => {
            format!("{} parameter `{}`", location.as_str(), a.wire)
        }
        ArgSource::Field(_) => format!("body field `{}`", a.wire),
        ArgSource::Body { media_type, .. } => format!("the request body ({media_type})"),
    };
    let optional = match a.presence {
        Presence::Optional | Presence::OptionalNullable => ", optional",
        _ => "",
    };
    if text.is_empty() {
        format!("[{ty}] {place}{optional}")
    } else {
        format!("[{ty}] {place}{optional}: {text}")
    }
}

/// The documentation of an operation's method.
fn op_doc(g: &Gen<'_, '_>, index: usize, op: &OpPlan<'_>, member: &str) -> String {
    let o = op.op;
    let mut parts: Vec<String> = vec![];
    let summary = op_summary(o);
    if !summary.is_empty() {
        parts.push(summary.clone());
    }
    if let Some(d) = o
        .doc
        .as_ref()
        .and_then(|d| d.description.as_deref())
        .map(str::trim)
        .filter(|d| !d.is_empty() && one_line(d) != summary)
    {
        parts.push(d.to_string());
    }
    parts.push(format!(
        "`{} {}` (`{}`). Safety: `{}`.",
        o.wire_method(),
        o.path.raw,
        o.id.0,
        safety_str(op.safety)
    ));
    match op.safety {
        Safety::Destructive => parts.push(format!(
            "Requires confirmation: pass `opts: {{ confirm: true }}`, or the `confirmation_token` of `preview_{member}` as `confirm`."
        )),
        Safety::Irreversible => parts.push(format!(
            "Requires confirmation: pass the `confirmation_token` of `preview_{member}` as `confirm`."
        )),
        _ => {}
    }
    if o.agent.shown_once {
        let fields = o.agent.sensitive_response_fields.join("`, `");
        if fields.is_empty() {
            parts.push(
                "The response includes values shown only once: store them immediately.".into(),
            );
        } else {
            parts.push(format!(
                "The response includes values shown only once (`{fields}`): store them immediately."
            ));
        }
    }
    let names = &g.names.operations[index].arguments;
    let mut tags: Vec<String> = op
        .arguments
        .iter()
        .zip(names)
        .map(|(a, n)| format!("@param {n} {}", arg_doc(g, a)))
        .collect();
    tags.push(
        "@param opts [Tungsten::Runtime::CallOptions, Hash, nil] call options: `idempotency_key`, `confirm`, `timeout_ms`, `verify`, `headers`."
            .to_string(),
    );
    let value = match &op.success {
        SuccessPlan::None => "nil (no body)".to_string(),
        SuccessPlan::Json(ty) => g.type_doc(ty),
        SuccessPlan::Bytes => "Tungsten::Runtime::Binary".to_string(),
        SuccessPlan::Text => "String".to_string(),
        SuccessPlan::ByStatus(_) | SuccessPlan::Raw(_) => "the JSON value".to_string(),
    };
    tags.push(format!(
        "@return [Tungsten::Runtime::Ok, Tungsten::Runtime::Err] `Ok#value` is {value}."
    ));
    parts.push(tags.join("\n"));
    parts.join("\n\n")
}

/// The decode expression of an operation's success value.
fn success_decode(g: &Gen<'_, '_>, op: &OpPlan<'_>) -> Option<String> {
    match &op.success {
        SuccessPlan::Json(ty) => g.decode(ty, "raw", 1),
        _ => None,
    }
}

fn resource(g: &Gen<'_, '_>, index: usize) -> String {
    let r = &g.plan.resources[index];
    let rn = &g.names.resources[index];
    let mut w = Writer::new("  ");
    g.file_start(&mut w);
    g.in_module(&mut w, |w| {
        w.line("module Resources");
        w.indent();
        let path = r.resource.path_prefix.as_str();
        let what = if path.is_empty() {
            format!("Operations of the `{}` resource.", r.path_words.join(" "))
        } else {
            format!("Operations under `{path}`.")
        };
        comment(w, &what);
        w.line(format!("class {}", rn.class));
        w.indent();
        let children: Vec<usize> = r
            .members
            .iter()
            .filter_map(|m| match m {
                MemberPlan::Child(c) => Some(*c),
                MemberPlan::Operation(_) => None,
            })
            .collect();
        for &c in &children {
            let name = g.member(index, MemberKind::Child(c)).unwrap_or_default();
            let child_path = g.plan.resources[c].resource.path_prefix.as_str();
            comment(
                w,
                &format!(
                    "@return [{}] operations under `{child_path}`.",
                    g.names.resources[c].class
                ),
            );
            w.line(format!("attr_reader :{name}"));
            w.blank();
        }
        comment(w, "@param core [Tungsten::Runtime::ClientCore]");
        w.line("def initialize(core)");
        w.indent();
        w.line("@core = core");
        for &c in &children {
            let name = g.member(index, MemberKind::Child(c)).unwrap_or_default();
            w.line(format!("@{name} = {}.new(core)", g.names.resources[c].class));
        }
        w.dedent();
        w.line("end");
        for m in &rn.members {
            let o = match m.kind {
                MemberKind::Operation(o)
                | MemberKind::Preview(o)
                | MemberKind::Pages(o)
                | MemberKind::Stream(o) => o,
                MemberKind::Child(_) => continue,
            };
            let op = &g.plan.operations[o];
            let id = str_lit(&op.op.id.0);
            let names = &g.names.operations[o].arguments;
            let args: Vec<&ArgPlan<'_>> = op.arguments.iter().collect();
            let sig = signature(&args, names);
            let hash = args_hash(names);
            let base = g.member(index, MemberKind::Operation(o)).unwrap_or_default();
            w.blank();
            match m.kind {
                MemberKind::Operation(_) => {
                    comment(w, &op_doc(g, o, op, base));
                    w.line(format!("def {}({sig})", m.name));
                    w.indent();
                    let call = format!("@core.call({id}, {hash}, opts)");
                    match success_decode(g, op) {
                        Some(expr) => {
                            w.line(format!(
                                "::Tungsten::Runtime::Typed.result({call}) {{ |raw| {expr} }}"
                            ));
                        }
                        None => {
                            w.line(call);
                        }
                    }
                }
                MemberKind::Preview(_) => {
                    comment(
                        w,
                        &format!(
                            "Preview of `{base}`: the request it would send and its effects, without sending it, and a `confirmation_token` bound to these exact arguments.\n\n@return [Tungsten::Runtime::Ok, Tungsten::Runtime::Err] `Ok#value` is a `Tungsten::Runtime::PreviewResult`."
                        ),
                    );
                    w.line(format!("def {}({sig})", m.name));
                    w.indent();
                    w.line(format!("@core.preview({id}, {hash}, opts)"));
                }
                MemberKind::Pages(_) => {
                    comment(
                        w,
                        &format!(
                            "Every page of `{base}`, fetched as the enumeration proceeds.\n\n@return [Enumerator<Tungsten::Runtime::Ok, Tungsten::Runtime::Err>] each `Ok#value` is a `Tungsten::Runtime::Page`; an `Err` ends the enumeration."
                        ),
                    );
                    w.line(format!("def {}({sig})", m.name));
                    w.indent();
                    let pages = format!("@core.pages({id}, {hash}, opts)");
                    match op
                        .page
                        .as_ref()
                        .and_then(|p| p.item.as_ref())
                        .and_then(|t| g.decode(t, "item", 1))
                    {
                        Some(expr) => w.line(format!(
                            "::Tungsten::Runtime::Typed.pages({pages}) {{ |item| {expr} }}"
                        )),
                        None => w.line(pages),
                    };
                }
                MemberKind::Stream(_) => {
                    comment(
                        w,
                        &format!(
                            "The server-sent events of `{base}`; the request is sent when the enumeration starts.\n\n@return [Tungsten::Runtime::EventStream] events with `on`, `collect`, `reduce`, `first` and `cancel`."
                        ),
                    );
                    w.line(format!("def {}({sig})", m.name));
                    w.indent();
                    match op
                        .stream
                        .as_ref()
                        .and_then(|s| g.decode(&s.event, "data", 1))
                    {
                        Some(expr) => w.line(format!(
                            "@core.stream({id}, {hash}, opts, decode: ->(data) {{ {expr} }})"
                        )),
                        None => w.line(format!("@core.stream({id}, {hash}, opts)")),
                    };
                }
                MemberKind::Child(_) => {}
            }
            w.dedent();
            w.line("end");
        }
        w.dedent();
        w.line("end");
        w.dedent();
        w.line("end");
    });
    w.finish()
}

fn client(g: &Gen<'_, '_>) -> String {
    let mut w = Writer::new("  ");
    g.file_start(&mut w);
    let api = &g.plan.ir.api;
    g.in_module(&mut w, |w| {
        // Namespace classes of a multi-namespace API.
        if let ClientPlan::Namespaces(list) = &g.plan.client {
            for (n, ns) in list.iter().enumerate() {
                comment(w, &format!("The resources of the `{}` namespace.", ns.name));
                w.line(format!("class {}", g.names.namespace_classes[n]));
                w.indent();
                for (i, &r) in ns.resources.iter().enumerate() {
                    comment(
                        w,
                        &format!("@return [Resources::{}]", g.names.resources[r].class),
                    );
                    w.line(format!("attr_reader :{}", g.names.namespace_members[n][i]));
                    w.blank();
                }
                comment(w, "@param core [Tungsten::Runtime::ClientCore]");
                w.line("def initialize(core)");
                w.indent();
                for (i, &r) in ns.resources.iter().enumerate() {
                    w.line(format!(
                        "@{} = Resources::{}.new(core)",
                        g.names.namespace_members[n][i], g.names.resources[r].class
                    ));
                }
                w.dedent();
                w.line("end");
                w.dedent();
                w.line("end");
                w.blank();
            }
        }
        let title = if api.title.trim().is_empty() {
            api.name.wire.clone()
        } else {
            api.title.trim().to_string()
        };
        let mut doc = format!("The {title} client.");
        if let Some(d) = api.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            doc.push_str("\n\n");
            doc.push_str(d);
        }
        doc.push_str(
            "\n\nEvery method returns `Tungsten::Runtime::Ok` or `Tungsten::Runtime::Err` (whose `error` is the diagnostic envelope); nothing raises for an API or transport failure.",
        );
        comment(w, &doc);
        w.line(format!("class {}", g.names.client_class));
        w.indent();
        comment(w, "@return [Tungsten::Runtime::ClientCore] the engine behind every method.");
        w.line("attr_reader :core");
        w.blank();
        comment(w, "@return [Macros] the API's macros.");
        w.line("attr_reader :macros");
        let members: Vec<(String, String)> = match &g.plan.client {
            ClientPlan::Resources(list) => list
                .iter()
                .enumerate()
                .map(|(i, &r)| {
                    (
                        g.names.client_members[i].clone(),
                        format!("Resources::{}", g.names.resources[r].class),
                    )
                })
                .collect(),
            ClientPlan::Namespaces(list) => (0..list.len())
                .map(|n| {
                    (
                        g.names.client_members[n].clone(),
                        g.names.namespace_classes[n].clone(),
                    )
                })
                .collect(),
        };
        for (name, class) in &members {
            w.blank();
            comment(w, &format!("@return [{class}]"));
            w.line(format!("attr_reader :{name}"));
        }
        w.blank();
        comment(
            w,
            "@param options [Tungsten::Runtime::ClientOptions, Hash, nil] client options; keywords are merged over them (`base_url:`, `auth:`, `timeout_ms:`, `retries:`, `middleware:`, ...).",
        );
        w.line("def initialize(options = nil, **keywords)");
        w.indent();
        if g.plan.ir.agent.redirects == tungsten_ir::RedirectPolicy::None {
            w.line("::Tungsten::Runtime::ClientCore.require_redirect_policy_v1");
        }
        w.line("@core = ::Tungsten::Runtime::ClientCore.new(DESCRIPTORS, ::Tungsten::Runtime::ClientOptions.from(options, **keywords))");
        w.line("@macros = Macros.new(@core)");
        for (name, class) in &members {
            w.line(format!("@{name} = {class}.new(@core)"));
        }
        w.dedent();
        w.line("end");
        w.blank();
        comment(
            w,
            "Calls by operation id or macro name with JSON arguments, through the typed methods.\n\n@return [Dispatch]",
        );
        w.line("def dispatch = Dispatch.new(self)");
        w.blank();
        comment(
            w,
            "Managed OAuth2 authorization: `authorization_url` owns state/S256 and returns an attempt; `exchange_code` requires its handle, full callback URL and owning session binding. `refresh`, `cancel_authorization` and `logout` use fenced session state. Explicit primitive escapes are `low_level_authorization_url`, `low_level_exchange_code`, `low_level_refresh`; `pkce` is a primitive utility.\n\n@return [Tungsten::Runtime::OAuthFlow]",
        );
        w.line("def oauth(scheme) = @core.oauth(scheme)");
        w.dedent();
        w.line("end");
    });
    w.finish()
}

fn macros(g: &Gen<'_, '_>) -> String {
    let mut w = Writer::new("  ");
    g.file_start(&mut w);
    g.in_module(&mut w, |w| {
        comment(
            w,
            "The API's macros: multi-step workflows the runtime runs (`client.macros`). Each returns `Tungsten::Runtime::Ok` with the macro's output, or `Err` with the failing step's envelope and the completed steps' results as `partial`.",
        );
        w.line("class Macros");
        w.indent();
        comment(w, "@param core [Tungsten::Runtime::ClientCore]");
        w.line("def initialize(core)");
        w.indent();
        w.line("@core = core");
        w.dedent();
        w.line("end");
        for (i, m) in g.plan.macros.iter().enumerate() {
            let names = &g.names.macros[i];
            let required: Vec<String> = m
                .input
                .iter()
                .zip(&names.input)
                .filter(|(f, _)| matches!(f.presence, Presence::Required | Presence::RequiredNullable))
                .map(|(_, n)| format!("{n}:"))
                .collect();
            let optional: Vec<String> = m
                .input
                .iter()
                .zip(&names.input)
                .filter(|(f, _)| matches!(f.presence, Presence::Optional | Presence::OptionalNullable))
                .map(|(_, n)| format!("{n}: ::Tungsten::Runtime::UNSET"))
                .collect();
            let mut params = required;
            params.extend(optional);
            params.push("opts: nil".to_string());
            w.blank();
            let src = m.source;
            let mut doc = one_line(&src.summary);
            if !doc.is_empty() {
                doc.push_str("\n\n");
            }
            doc.push_str(&format!(
                "Macro `{}`. Safety: `{}`.",
                src.name.0,
                safety_str(src.safety)
            ));
            if src.shown_once {
                doc.push_str(
                    "\n\nThe output includes values the API shows only once: store them immediately.",
                );
            }
            doc.push_str("\n\n@return [Tungsten::Runtime::Ok, Tungsten::Runtime::Err]");
            comment(w, &doc);
            w.line(format!("def {}({})", names.member, params.join(", ")));
            w.indent();
            w.line(format!(
                "@core.run_macro({}, {}, opts)",
                str_lit(m.name()),
                args_hash(&names.input)
            ));
            w.dedent();
            w.line("end");
        }
        w.dedent();
        w.line("end");
    });
    w.finish()
}

/// One dispatch table entry: `"id" => [required, every, ->(c, a, o) { ... }]`.
fn entry_line(id: &str, required: &[String], every: &[String], call: &str) -> String {
    let list = |names: &[String]| -> String {
        if names.is_empty() {
            "[].freeze".to_string()
        } else {
            format!("%w[{}].freeze", names.join(" "))
        }
    };
    format!(
        "{} => [{}, {}, ->(c, a, o) {{ {call}(**a, opts: o) }}],",
        str_lit(id),
        list(required),
        list(every)
    )
}

fn table(w: &mut Writer, name: &str, doc: &str, lines: &[String]) {
    comment(w, doc);
    if lines.is_empty() {
        w.line(format!("{name} = {{}}.freeze"));
        return;
    }
    w.line(format!("{name} = {{"));
    w.indent();
    for l in lines {
        w.line(l);
    }
    w.dedent();
    w.line("}.freeze");
}

fn dispatch(g: &Gen<'_, '_>) -> String {
    let mut ops = vec![];
    let mut previews = vec![];
    let mut pages = vec![];
    let mut streams = vec![];
    for (o, op) in g.plan.operations.iter().enumerate() {
        let names = &g.names.operations[o].arguments;
        let required: Vec<String> = op
            .arguments
            .iter()
            .zip(names)
            .filter(|(a, _)| matches!(a.presence, Presence::Required | Presence::RequiredNullable))
            .map(|(_, n)| n.clone())
            .collect();
        let chain = g.resource_chain(op.resource).join(".");
        let prefix = if chain.is_empty() {
            "c".to_string()
        } else {
            format!("c.{chain}")
        };
        let id = &op.op.id.0;
        let add = |kind: MemberKind, out: &mut Vec<String>| {
            if let Some(member) = g.member(op.resource, kind) {
                out.push(entry_line(
                    id,
                    &required,
                    names,
                    &format!("{prefix}.{member}"),
                ));
            }
        };
        add(MemberKind::Operation(o), &mut ops);
        add(MemberKind::Preview(o), &mut previews);
        add(MemberKind::Pages(o), &mut pages);
        add(MemberKind::Stream(o), &mut streams);
    }
    let mut macros = vec![];
    for (i, m) in g.plan.macros.iter().enumerate() {
        let names = &g.names.macros[i];
        let required: Vec<String> = m
            .input
            .iter()
            .zip(&names.input)
            .filter(|(f, _)| matches!(f.presence, Presence::Required | Presence::RequiredNullable))
            .map(|(_, n)| n.clone())
            .collect();
        macros.push(entry_line(
            m.name(),
            &required,
            &names.input,
            &format!("c.macros.{}", names.member),
        ));
    }
    let mut w = Writer::new("  ");
    g.file_start(&mut w);
    g.in_module(&mut w, |w| {
        comment(
            w,
            "Calls by operation id or macro name with JSON arguments keyed by the Ruby argument names, through the typed methods (`Tungsten::Runtime::Dispatcher`). Arguments that do not fit a method go to the core as they are, so pre-flight validation reports them.",
        );
        w.line("class Dispatch < ::Tungsten::Runtime::Dispatcher");
        w.indent();
        let doc = "Operation id => [required argument names, every argument name, the typed call].";
        table(w, "OPERATIONS", doc, &ops);
        w.blank();
        table(w, "PREVIEWS", "Previews, as OPERATIONS.", &previews);
        w.blank();
        table(w, "PAGES", "Page enumerations, as OPERATIONS.", &pages);
        w.blank();
        table(w, "STREAMS", "Event streams, as OPERATIONS.", &streams);
        w.blank();
        table(
            w,
            "MACROS",
            "Macro name => [required input names, every input name, the macro call].",
            &macros,
        );
        w.blank();
        let client_class = &g.names.client_class;
        comment(w, &format!("@param client [{client_class}]"));
        w.line("def initialize(client)");
        w.indent();
        w.line("super(client, operations: OPERATIONS, previews: PREVIEWS, pages: PAGES, streams: STREAMS, macros: MACROS)");
        w.dedent();
        w.line("end");
        w.dedent();
        w.line("end");
    });
    w.finish()
}

fn custom(g: &Gen<'_, '_>) -> String {
    let mut w = Writer::new("  ");
    w.line("# frozen_string_literal: true");
    w.blank();
    comment(
        &mut w,
        "Hand-written additions to the SDK. tungsten creates this file once and never\noverwrites or removes it; the gem requires it last.",
    );
    g.in_module(&mut w, |w| {
        comment(w, "Your own helpers.");
        w.line("module Custom");
        w.line("end");
    });
    w.finish()
}
