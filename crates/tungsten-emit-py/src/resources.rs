// SPDX-License-Identifier: AGPL-3.0-only
//! Resource modules (`<module>/resources/<name>.py`, a sync and an async
//! class each, written from one model of the tree), the clients
//! (`<module>/client.py`) and the package entry point (`__init__.py`).
//!
//! Each operation is a method with keyword-only arguments and a trailing
//! `opts: CallOptions | None = None`, returning `Result[T]`; operations
//! with a preview get `preview_<method>` and paginated ones
//! `<method>_pages`. Previews are separate methods rather than an attribute
//! of the method so every signature is a plain, fully typed `def`.

use tungsten_emit::Writer;
use tungsten_ir::{IdempotencyKind, OperationStatus, Safety};

use crate::ops::{ArgField, OpShape, arg_doc_line, idempotency_str, method_str, safety_str};
use crate::plan::{MemberKind, OpInfo, Plan, ResInfo};
use crate::py::{
    LINE_LENGTH, Py, PyImports, doc_text, docstring, dunder_all, dunder_all_cmp, paragraphs,
    two_blank,
};
use crate::types::Uses;

/// Whether the method is the sync or the async twin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Sync,
    Async,
}

/// The docstring of an operation method: its description, the agent facts
/// a caller must know before calling it, and its arguments.
pub(crate) fn op_doc(
    plan: &Plan<'_>,
    info: &OpInfo<'_>,
    shape: &OpShape<'_>,
    names: &OpNames,
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
    let preview = names
        .preview
        .as_deref()
        .map(|p| format!("`{p}()`"))
        .unwrap_or_else(|| "a preview".to_string());
    match a.safety {
        Safety::Destructive => parts.push(format!(
            "Requires confirmation: pass `opts={{\"confirm\": True}}`, or the `confirmation_token` of {preview} as `confirm`."
        )),
        Safety::Irreversible => parts.push(format!(
            "Cannot be undone: call {preview} first and pass its `confirmation_token` as `opts={{\"confirm\": token}}`."
        )),
        Safety::ReadOnly | Safety::Mutating => {}
    }
    let idem = &a.idempotency;
    match idem.policy {
        IdempotencyKind::CallerOwned => parts.push(format!(
            "Idempotency (`caller_owned`): generate a key{}, persist it with your intent, pass it as `opts={{\"idempotency_key\": key}}`, and reuse it on every retry.{}",
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
    if let Some(p) = &names.pages {
        parts.push(format!("`{p}()` iterates every page."));
    }
    if shape.bodiless {
        parts.push("A success response without a body yields `value=None`.".into());
    }
    if let Some(cluster) = &a.cluster {
        parts.push(format!("Cluster: `{cluster}`."));
    }
    if op.deprecated {
        parts.push("Deprecated.".into());
    }
    parts.push(args_section(&shape.fields));
    paragraphs(parts)
}

/// The `Args:` section of a method docstring.
pub(crate) fn args_section(fields: &[ArgField]) -> String {
    let mut lines = vec!["Args:".to_string()];
    lines.extend(fields.iter().map(arg_doc_line));
    lines.push(
        "    opts: Call options: `idempotency_key`, `confirm`, `timeout_ms`, `verify`, `headers`."
            .into(),
    );
    lines.join("\n")
}

/// The method names of one operation in its resource.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpNames {
    pub call: String,
    pub preview: Option<String>,
    pub pages: Option<String>,
}

fn op_names(r: &ResInfo<'_>, o: usize) -> OpNames {
    let mut names = OpNames::default();
    for m in &r.members {
        match m.kind {
            MemberKind::Op(i) if i == o => names.call = m.name.clone(),
            MemberKind::Preview(i) if i == o => names.preview = Some(m.name.clone()),
            MemberKind::Pages(i) if i == o => names.pages = Some(m.name.clone()),
            _ => {}
        }
    }
    names
}

/// `def name(self, *, a: T, opts: ...) -> R:` on one line when it fits in
/// [`LINE_LENGTH`] at `indent`, else one parameter per line.
pub(crate) fn signature(
    mode: Mode,
    name: &str,
    params: &[String],
    ret: &str,
    indent: usize,
) -> Vec<String> {
    let kw = if mode == Mode::Async {
        "async def"
    } else {
        "def"
    };
    let flat = format!("{kw} {name}({}) -> {ret}:", params.join(", "));
    if indent + flat.len() <= LINE_LENGTH {
        return vec![flat];
    }
    let mut lines = vec![format!("{kw} {name}(")];
    lines.extend(params.iter().map(|p| format!("    {p},")));
    lines.push(format!(") -> {ret}:"));
    lines
}

/// The parameters of an operation method: `self`, `*`, every argument, and
/// the call options.
pub(crate) fn method_params(fields: &[ArgField]) -> Vec<String> {
    let mut params = vec!["self".to_string(), "*".to_string()];
    for f in fields {
        if f.optional {
            params.push(format!("{}: {} = UNSET", f.key, f.hint));
        } else {
            params.push(format!("{}: {}", f.key, f.hint));
        }
    }
    params.push("opts: CallOptions | None = None".into());
    params
}

/// The arguments mapping a method hands to the runtime, on a line indented
/// by `indent` and starting at column `col`.
pub(crate) fn args_expr(fields: &[ArgField], indent: usize, col: usize) -> String {
    if fields.is_empty() {
        return "{}".into();
    }
    let dict = Py::Dict(
        fields
            .iter()
            .map(|f| (f.key.clone(), Py::Raw(f.key.clone())))
            .collect(),
    );
    if fields.iter().any(|f| f.optional) {
        format!(
            "_internal.args({})",
            dict.render_in(indent, col + "_internal.args(".len())
        )
    } else {
        dict.render_in(indent, col)
    }
}

/// `<prefix>self._core.<method>(<target>, <args>, opts)` on a line indented
/// by `indent`, one argument per line when it does not fit.
fn core_call(
    prefix: &str,
    method: &str,
    target: &str,
    fields: &[ArgField],
    indent: usize,
) -> Vec<String> {
    let head = format!("{prefix}self._core.{method}(");
    let args = args_expr(fields, indent, indent + head.len() + target.len() + 2);
    let flat = format!("{head}{target}, {args}, opts)");
    if indent + flat.len() <= LINE_LENGTH && !flat.contains('\n') {
        return vec![flat];
    }
    let args = args_expr(fields, indent + 4, indent + 4);
    let mut lines = vec![head];
    lines.push(format!("    {target},"));
    lines.push(format!("    {},", crate::py::indent_rest(&args, "    ")));
    lines.push("    opts,".into());
    lines.push(")".into());
    lines
}

/// `return [await ]self._core.<method>(<target>, <args>, opts)`, wrapped
/// when long.
pub(crate) fn call_lines(
    mode: Mode,
    method: &str,
    target: &str,
    fields: &[ArgField],
    indent: usize,
) -> Vec<String> {
    let prefix = if mode == Mode::Async {
        "return await "
    } else {
        "return "
    };
    core_call(prefix, method, target, fields, indent)
}

/// `return <wrap>(self._core.pages(...), _d.PAGE_ITEMS["<id>"])`: pages
/// whose items are turned into their type.
fn typed_pages_lines(
    wrap: &str,
    target: &str,
    id: &str,
    fields: &[ArgField],
    indent: usize,
) -> Vec<String> {
    let inner = core_call("", "pages", target, fields, indent + 4);
    let mut lines = vec![format!("return {wrap}(")];
    let last = inner.len() - 1;
    for (i, l) in inner.into_iter().enumerate() {
        lines.push(if i == last {
            format!("    {l},")
        } else {
            format!("    {l}")
        });
    }
    lines.push(format!("    _d.PAGE_ITEMS[{}],", crate::py::string_lit(id)));
    lines.push(")".into());
    lines
}

/// Doc of a resource class or attribute.
pub(crate) fn resource_doc(r: &ResInfo<'_>) -> String {
    let text = doc_text(r.res.doc.as_ref());
    if text.is_empty() {
        format!("Operations under `{}`.", r.res.path_prefix)
    } else {
        text
    }
}

/// One method (an operation, its preview or its pages) of a resource class.
fn write_method(
    w: &mut Writer,
    mode: Mode,
    plan: &Plan<'_>,
    info: &OpInfo<'_>,
    shape: &OpShape<'_>,
    names: &OpNames,
    kind: MemberKind,
) {
    let params = method_params(&shape.fields);
    let target = format!("_d.{}", info.key);
    let (name, ret, method, doc) = match kind {
        MemberKind::Preview(_) => (
            names.preview.clone().unwrap_or_default(),
            "Result[PreviewResult]".to_string(),
            "preview",
            paragraphs([
                format!(
                    "Preview `{}` without sending it: the request, its effects and, when a confirmation is required, a `confirmation_token` to pass to `{}()` as `opts={{\"confirm\": token}}`.",
                    info.op.id.0, names.call
                ),
                args_section(&shape.fields),
            ]),
        ),
        MemberKind::Pages(_) => {
            let item = shape
                .page_item
                .as_ref()
                .map_or_else(|| "Any".to_string(), |t| t.text());
            let iter = if mode == Mode::Async {
                "AsyncIterator"
            } else {
                "Iterator"
            };
            (
                names.pages.clone().unwrap_or_default(),
                format!("{iter}[Result[Page[{item}]]]"),
                "pages",
                paragraphs([
                    format!(
                        "Every page of `{}`, following the cursor until the last page. A failed page is yielded as its error result and ends the iteration. Items are validated into their type; an item that does not match stays as decoded.",
                        info.op.id.0
                    ),
                    args_section(&shape.fields),
                ]),
            )
        }
        MemberKind::Op(_) | MemberKind::Child(_) => (
            names.call.clone(),
            format!("Result[{}]", shape.success.text()),
            "call",
            op_doc(plan, info, shape, names),
        ),
    };
    // Pages are iterated, never awaited, in both modes.
    let method_mode = if method == "pages" { Mode::Sync } else { mode };
    for l in signature(method_mode, &name, &params, &ret, 4) {
        w.line(l);
    }
    w.indent();
    docstring(w, &doc);
    let typed_items = method == "pages" && shape.page_item_schema.is_some();
    if typed_items {
        // The runtime reads the items from the page body; they become the
        // item type here (`_internal.page_items`).
        let wrap = if mode == Mode::Async {
            "_internal.apage_items"
        } else {
            "_internal.page_items"
        };
        for l in typed_pages_lines(wrap, &target, &info.op.id.0, &shape.fields, 8) {
            w.line(l);
        }
    } else {
        for l in call_lines(method_mode, method, &target, &shape.fields, 8) {
            w.line(l);
        }
    }
    w.dedent();
}

/// The source of one resource module.
pub(crate) fn resource_file(
    plan: &Plan<'_>,
    shapes: &[OpShape<'_>],
    idx: usize,
    header: &str,
) -> String {
    let r = &plan.resources[idx];
    let mut uses = Uses::default();
    let mut imports = PyImports::default();
    imports.add("__future__", "annotations");
    imports.add("tungsten_runtime", "AsyncClientCore");
    imports.add("tungsten_runtime", "ClientCore");
    let mut has_ops = false;
    for m in &r.members {
        match m.kind {
            MemberKind::Op(o) => {
                has_ops = true;
                let shape = &shapes[o];
                uses.merge(&shape.hint_uses);
                uses.merge(&shape.result_uses);
                imports.add("tungsten_runtime", "CallOptions");
                imports.add("tungsten_runtime", "Result");
                if shape.fields.iter().any(|f| f.optional) {
                    imports.add("tungsten_runtime", "UNSET");
                    imports.add("tungsten_runtime", "Unset");
                    imports.add("..", "_internal");
                }
            }
            MemberKind::Preview(_) => imports.add("tungsten_runtime", "PreviewResult"),
            MemberKind::Pages(o) => {
                imports.add("collections.abc", "AsyncIterator");
                imports.add("collections.abc", "Iterator");
                imports.add("tungsten_runtime", "Page");
                if shapes[o].page_item_schema.is_some() {
                    imports.add("..", "_internal");
                }
                if shapes[o].page_item.is_none() {
                    imports.add("typing", "Any");
                }
            }
            MemberKind::Child(c) => {
                let child = &plan.resources[c];
                imports.add(&format!(".{}", child.module), &child.class);
                imports.add(&format!(".{}", child.module), &child.async_class);
            }
        }
    }
    if has_ops {
        imports.add_as("..", "_descriptors", "_d");
    }
    uses.add_to(plan, &mut imports, "..", "..models");

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    docstring(&mut w, &format!("Resource `{}`.", r.res.path_prefix));
    w.blank();
    w.line(imports.render());
    for mode in [Mode::Sync, Mode::Async] {
        two_blank(&mut w);
        let (class, core) = match mode {
            Mode::Sync => (&r.class, "ClientCore"),
            Mode::Async => (&r.async_class, "AsyncClientCore"),
        };
        w.line(format!("class {class}:"));
        w.indent();
        docstring(&mut w, &resource_doc(r));
        for m in &r.members {
            if let MemberKind::Child(c) = m.kind {
                let child = &plan.resources[c];
                let child_class = match mode {
                    Mode::Sync => &child.class,
                    Mode::Async => &child.async_class,
                };
                w.blank();
                w.line(format!("{}: {child_class}", m.name));
                docstring(&mut w, &resource_doc(child));
            }
        }
        w.blank();
        w.line(format!("def __init__(self, core: {core}) -> None:"));
        w.indent();
        let mut wrote = false;
        if has_ops {
            w.line("self._core = core");
            wrote = true;
        }
        for m in &r.members {
            if let MemberKind::Child(c) = m.kind {
                let child = &plan.resources[c];
                let child_class = match mode {
                    Mode::Sync => &child.class,
                    Mode::Async => &child.async_class,
                };
                w.line(format!("self.{} = {child_class}(core)", m.name));
                wrote = true;
            }
        }
        if !wrote {
            w.line("del core");
        }
        w.dedent();
        for m in &r.members {
            let o = match m.kind {
                MemberKind::Op(o) | MemberKind::Preview(o) | MemberKind::Pages(o) => o,
                MemberKind::Child(_) => continue,
            };
            w.blank();
            let names = op_names(r, o);
            write_method(&mut w, mode, plan, &plan.ops[o], &shapes[o], &names, m.kind);
        }
        w.dedent();
    }
    w.finish()
}

/// The constructor parameters of a client after `options`.
const CLIENT_SHORTCUTS: &[(&str, &str)] = &[
    ("base_url", "str | None"),
    ("auth", "AuthConfig | None"),
    ("timeout_ms", "int | None"),
    (
        "validate_responses",
        "Literal[\"off\", \"warn\", \"strict\"] | None",
    ),
    ("headers", "Mapping[str, str] | None"),
];

/// The source of `<module>/client.py`.
pub(crate) fn client_file(plan: &Plan<'_>, has_macros: bool, header: &str) -> String {
    let ir = plan.ir;
    let mut imports = PyImports::default();
    imports.add("__future__", "annotations");
    imports.add("collections.abc", "Mapping");
    imports.add("typing", "Literal");
    imports.add("typing", "Self");
    for n in [
        "AsyncClientCore",
        "AuthConfig",
        "ClientCore",
        "ClientOptions",
    ] {
        imports.add("tungsten_runtime", n);
    }
    imports.add(".", "_internal");
    imports.add("._descriptors", "API");
    imports.add("._descriptors", "OPERATIONS");
    if has_macros {
        imports.add(".macros", "MACROS");
        imports.add(".macros", "AsyncMacros");
        imports.add(".macros", "Macros");
    }
    let tops: Vec<usize> = if plan.multi() {
        plan.client_namespaces
            .iter()
            .flat_map(|n| n.members.iter().map(|(_, i)| *i))
            .collect()
    } else {
        plan.client_resources.iter().map(|(_, i)| *i).collect()
    };
    for i in tops {
        let r = &plan.resources[i];
        imports.add(&format!(".resources.{}", r.module), &r.class);
        imports.add(&format!(".resources.{}", r.module), &r.async_class);
    }

    // (attribute, sync class, async class, doc)
    let members: Vec<(String, String, String, String)> = if plan.multi() {
        plan.client_namespaces
            .iter()
            .map(|n| {
                (
                    n.member.clone(),
                    n.class.clone(),
                    n.async_class.clone(),
                    format!("{} (`{}` namespace).", n.ns.title, n.ns.name.wire),
                )
            })
            .collect()
    } else {
        plan.client_resources
            .iter()
            .map(|(name, i)| {
                let r = &plan.resources[*i];
                (
                    name.clone(),
                    r.class.clone(),
                    r.async_class.clone(),
                    resource_doc(r),
                )
            })
            .collect()
    };

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    docstring(&mut w, &format!("Clients of the {} API.", ir.api.title));
    w.blank();
    w.line(imports.render());
    for mode in [Mode::Sync, Mode::Async] {
        let (class, core, macros) = match mode {
            Mode::Sync => (&plan.client_class, "ClientCore", "Macros"),
            Mode::Async => (&plan.async_client_class, "AsyncClientCore", "AsyncMacros"),
        };
        two_blank(&mut w);
        w.line(format!("class {class}:"));
        w.indent();
        let flavor = match mode {
            Mode::Sync => "Synchronous client",
            Mode::Async => "Asynchronous client (every call is awaited)",
        };
        docstring(
            &mut w,
            &paragraphs([
                format!("{flavor} for the {} API.", ir.api.title),
                ir.api.description.clone().unwrap_or_default(),
                "Every call returns a `Result`: `Ok(value, meta)` or `Err(error)` with the diagnostic envelope; API and transport errors never raise. Arguments are validated before any request is sent.".to_string(),
            ]),
        );
        w.blank();
        w.line(format!("core: {core}"));
        docstring(&mut w, "The runtime core every call goes through.");
        if has_macros {
            w.blank();
            w.line(format!("macros: {macros}"));
            docstring(
                &mut w,
                "Multi-step workflows compiled from the agent manifest.",
            );
        }
        for (name, sync, async_, doc) in &members {
            w.blank();
            let c = if mode == Mode::Sync { sync } else { async_ };
            w.line(format!("{name}: {c}"));
            docstring(&mut w, doc);
        }
        w.blank();
        let mut params = vec![
            "self".to_string(),
            "options: ClientOptions | None = None".into(),
            "*".into(),
        ];
        params.extend(
            CLIENT_SHORTCUTS
                .iter()
                .map(|(n, t)| format!("{n}: {t} = None")),
        );
        for l in signature(Mode::Sync, "__init__", &params, "None", 4) {
            w.line(l);
        }
        w.indent();
        docstring(
            &mut w,
            "Create a client from `options`; the keyword arguments override the fields of the same name.\n\nEvery operation and macro descriptor of the API is registered with the runtime, so it resolves them by id (verification hooks, endpoint previews, macro steps).",
        );
        let macros_arg = if has_macros { "MACROS" } else { "()" };
        w.line(format!("self.core = {core}("));
        w.line("    API,");
        w.line("    _internal.client_options(");
        w.line("        options,");
        w.line("        OPERATIONS,");
        w.line(format!("        {macros_arg},"));
        for (n, _) in CLIENT_SHORTCUTS {
            w.line(format!("        {n}={n},"));
        }
        w.line("    ),");
        w.line(")");
        if has_macros {
            w.line(format!("self.macros = {macros}(self.core)"));
        }
        for (name, sync, async_, _) in &members {
            let c = if mode == Mode::Sync { sync } else { async_ };
            w.line(format!("self.{name} = {c}(self.core)"));
        }
        w.dedent();
        w.blank();
        match mode {
            Mode::Sync => {
                w.line("def close(self) -> None:");
                w.indent();
                docstring(&mut w, "Release the HTTP connection pool.");
                w.line("self.core.close()");
                w.dedent();
                w.blank();
                w.line("def __enter__(self) -> Self:");
                w.indent();
                w.line("return self");
                w.dedent();
                w.blank();
                w.line("def __exit__(self, *exc_info: object) -> None:");
                w.indent();
                w.line("self.close()");
                w.dedent();
            }
            Mode::Async => {
                w.line("async def aclose(self) -> None:");
                w.indent();
                docstring(&mut w, "Release the HTTP connection pool.");
                w.line("await self.core.aclose()");
                w.dedent();
                w.blank();
                w.line("async def __aenter__(self) -> Self:");
                w.indent();
                w.line("return self");
                w.dedent();
                w.blank();
                w.line("async def __aexit__(self, *exc_info: object) -> None:");
                w.indent();
                w.line("await self.aclose()");
                w.dedent();
            }
        }
        w.dedent();
    }
    for n in &plan.client_namespaces {
        for mode in [Mode::Sync, Mode::Async] {
            let (class, core) = match mode {
                Mode::Sync => (&n.class, "ClientCore"),
                Mode::Async => (&n.async_class, "AsyncClientCore"),
            };
            two_blank(&mut w);
            w.line(format!("class {class}:"));
            w.indent();
            docstring(
                &mut w,
                &format!("The `{}` namespace: {}.", n.ns.name.wire, n.ns.title),
            );
            for (name, i) in &n.members {
                let r = &plan.resources[*i];
                w.blank();
                let c = if mode == Mode::Sync {
                    &r.class
                } else {
                    &r.async_class
                };
                w.line(format!("{name}: {c}"));
                docstring(&mut w, &resource_doc(r));
            }
            w.blank();
            w.line(format!("def __init__(self, core: {core}) -> None:"));
            w.indent();
            if n.members.is_empty() {
                w.line("del core");
            }
            for (name, i) in &n.members {
                let r = &plan.resources[*i];
                let c = if mode == Mode::Sync {
                    &r.class
                } else {
                    &r.async_class
                };
                w.line(format!("self.{name} = {c}(core)"));
            }
            w.dedent();
            w.dedent();
        }
    }
    w.finish()
}

/// The source of the package's `__init__.py`.
pub(crate) fn init_file(plan: &Plan<'_>, has_macros: bool, header: &str) -> String {
    const RUNTIME: &[&str] = &[
        "UNSET",
        "CallOptions",
        "ClientOptions",
        "Diagnostic",
        "Err",
        "Ok",
        "Page",
        "PreviewResult",
        "Result",
        "TungstenError",
        "Unset",
        "unwrap",
    ];
    let mut imports = PyImports::default();
    imports.add("__future__", "annotations");
    for n in RUNTIME {
        imports.add("tungsten_runtime", n);
    }
    imports.add(".", "custom");
    imports.add(".", "models");
    imports.add(".client", &plan.client_class);
    imports.add(".client", &plan.async_client_class);
    let mut exported: Vec<String> = RUNTIME.iter().map(|s| s.to_string()).collect();
    exported.extend([
        "custom".to_string(),
        "models".to_string(),
        plan.client_class.clone(),
        plan.async_client_class.clone(),
    ]);
    if has_macros {
        imports.add(".", "macros");
        exported.push("macros".into());
    }
    exported.sort_by(|a, b| dunder_all_cmp(a, b));
    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    docstring(
        &mut w,
        &format!(
            "Python SDK for the {} API ({}), generated by tungsten.",
            plan.ir.api.title, plan.ir.api.version
        ),
    );
    w.blank();
    w.line(imports.render());
    w.blank();
    w.line(dunder_all(&exported));
    w.finish()
}
