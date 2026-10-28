// SPDX-License-Identifier: AGPL-3.0-only
//! The client (`Client.fs`), `Dispatch.fs` (the runtime's `IDispatch`) and
//! `Macros.fs`.
//!
//! F# compiles in file order and has no partial classes, so the client
//! comes last: `Macros.fs` declares the macro inputs and the macros class,
//! `Dispatch.fs` an internal class implementing `IDispatch` over the
//! client's top-level members (operations by id and macros by name with
//! JSON arguments, through the internal twins of the typed methods, so the
//! value of a result is the decoded wire body and its metadata is kept),
//! and `Client.fs` the client, which implements `IDispatch` by delegating
//! to it and exposes it as `Dispatch`.

use tungsten_emit::Writer;
use tungsten_emit::sdk::{ClientPlan, InputSource};
use tungsten_ir::AuthScheme;

use crate::Ctx;
use crate::fs::{self, lit};
use crate::resources::{ArgField, OpInfo, Request, internal};
use crate::typeref::Ft;

/// The methods of one macro.
#[derive(Debug)]
pub struct MacroInfo {
    /// The member name: methods are `<Member>Async` and
    /// `Preview<Member>Async`.
    pub member: String,
    pub input: Request,
}

impl Ctx<'_> {
    /// The macros class name: `<Client>Macros`, unique among the classes.
    pub fn macros_class(&self) -> String {
        let mut taken = self.root_taken();
        fs::unique(format!("{}Macros", self.names.client_class), &mut taken)
    }

    /// The dispatch class name: `<Client>Dispatch`, unique among the
    /// classes.
    pub fn dispatch_class(&self) -> String {
        let mut taken = self.root_taken();
        taken.push(self.macros_class());
        fs::unique(format!("{}Dispatch", self.names.client_class), &mut taken)
    }

    fn has_code_flow(&self) -> bool {
        self.ir
            .auth
            .iter()
            .any(|s| matches!(s, AuthScheme::OAuth2 { .. }) && s.authorization_code().is_some())
    }

    /// The client's top-level members: (member, class).
    fn top_members(&self) -> Vec<(String, String)> {
        match &self.plan.client {
            ClientPlan::Resources(top) => top
                .iter()
                .zip(&self.names.client_members)
                .map(|(r, m)| (m.clone(), self.names.resources[*r].class.clone()))
                .collect(),
            ClientPlan::Namespaces(_) => self
                .names
                .client_members
                .iter()
                .zip(&self.names.namespace_classes)
                .map(|(m, c)| (m.clone(), c.clone()))
                .collect(),
        }
    }

    /// The methods of every macro, in plan order.
    pub fn macro_infos(&self, infos: &[OpInfo]) -> Vec<MacroInfo> {
        let mut taken = self.root_taken();
        taken.push(self.macros_class());
        taken.push(self.dispatch_class());
        taken.extend(infos.iter().filter_map(|i| i.request.record.clone()));
        let mut out = vec![];
        for (k, m) in self.plan.macros.iter().enumerate() {
            let member = self.names.macros[k].member.clone();
            let base = m.base.map(|b| &self.plan.operations[b]);
            let words: Vec<Vec<String>> = m.input.iter().map(|f| f.words.clone()).collect();
            let props = Self::arg_props(&words);
            let fields: Vec<ArgField> = m
                .input
                .iter()
                .zip(props)
                .map(|(f, prop)| {
                    let ft = match (&f.source, base) {
                        (InputSource::Argument(i), Some(op)) => op
                            .arguments
                            .get(*i)
                            .and_then(|a| a.ty.as_ref())
                            .map_or_else(Ft::any, |t| self.ft(t, None)),
                        _ => Ft::any(),
                    };
                    ArgField {
                        prop,
                        key: f.key.clone(),
                        ft,
                        presence: f.presence,
                        doc: String::new(),
                    }
                })
                .collect();
            let input = Self::request(fields, format!("{member}MacroInput"), &mut taken);
            out.push(MacroInfo { member, input });
        }
        out
    }

    /// `Macros.fs` (with macros), `Dispatch.fs` and `Client.fs`.
    pub fn client_files(&self) -> Vec<(String, String)> {
        let infos = self.op_infos();
        let macros = self.macro_infos(&infos);
        let mut out = vec![];
        if !macros.is_empty() {
            out.push(("Macros.fs".into(), self.macros_file(&macros)));
        }
        out.push(("Dispatch.fs".into(), self.dispatch_file(&infos, &macros)));
        out.push(("Client.fs".into(), self.client_file(!macros.is_empty())));
        out
    }

    /// `Client.fs`.
    pub fn client_file(&self, macros: bool) -> String {
        let client = &self.names.client_class;
        let mut w = self.file_writer();
        w.line(format!("namespace {}", self.opts.namespace));
        w.blank();
        Self::root_opens(&mut w);
        let members = self.top_members();
        let macros_class = self.macros_class();
        fs::doc_line(
            &mut w,
            &format!(
                "The client of the {} API. Throws ClientConfigException when the options are invalid (a base URL that is not an absolute http(s) URL).",
                self.ir.api.title
            ),
        );
        w.line("[<Sealed>]");
        w.line(format!("type {client}(?options: ClientOptions) ="));
        w.indent();
        if self.ir.agent.redirects == tungsten_ir::RedirectPolicy::None {
            w.line("do ClientCore.RequireRedirectPolicyV1()");
        }
        if self
            .plan
            .operations
            .iter()
            .any(|p| p.op.agent.import_policy.is_some())
        {
            w.line("do ClientCore.RequireImportPolicyV1()");
        }
        w.line("let core = Identity.core Ops.Document options");
        let mut dispatch_args = vec!["core".to_string()];
        for (k, (_, class)) in members.iter().enumerate() {
            w.line(format!("let member{k} = {class}(core)"));
            dispatch_args.push(format!("member{k}"));
        }
        if macros {
            w.line(format!("let macros = {macros_class}(core)"));
            dispatch_args.push("macros".into());
        }
        w.line(format!(
            "let dispatch = {}({}) :> IDispatch",
            self.dispatch_class(),
            dispatch_args.join(", ")
        ));
        w.blank();
        fs::doc_line(&mut w, "The runtime engine every method calls through.");
        w.line("member _.Core: ClientCore = core");
        for (k, (m, class)) in members.iter().enumerate() {
            w.blank();
            w.line(format!("member _.{m}: {class} = member{k}"));
        }
        if macros {
            w.blank();
            fs::doc_line(&mut w, "The macros of the API: several calls as one step.");
            w.line(format!("member _.Macros: {macros_class} = macros"));
        }
        w.blank();
        fs::doc_line(
            &mut w,
            "Operations by id and macros by name with JSON arguments, through the typed methods.",
        );
        w.line("member _.Dispatch: IDispatch = dispatch");
        if self.has_code_flow() {
            w.blank();
            fs::doc_line(
                &mut w,
                "The C# core's managed OAuth2 attempts, callback validation, refresh and local logout for a scheme.",
            );
            w.line("member _.OAuth(scheme: string) : OAuthFlow = core.OAuth(scheme)");
        }
        w.blank();
        w.line("interface IDispatch with");
        w.indent();
        w.line("member _.Core = dispatch.Core");
        w.line("member _.Operations = dispatch.Operations");
        w.line("member _.Macros = dispatch.Macros");
        for (name, params) in [
            ("InvokeAsync", "operation, args, options, cancellationToken"),
            (
                "PreviewOperationAsync",
                "operation, args, options, cancellationToken",
            ),
            (
                "PaginateAsync",
                "operation, args, options, cancellationToken",
            ),
            ("StreamEvents", "operation, args, options"),
            ("RunMacroAsync", "name, input, options, cancellationToken"),
            (
                "PreviewMacroAsync",
                "name, input, options, cancellationToken",
            ),
        ] {
            w.line(format!(
                "member _.{name}({params}) = dispatch.{name}({params})"
            ));
        }
        w.dedent();
        w.dedent();
        w.finish()
    }

    /// `Macros.fs`.
    pub fn macros_file(&self, macros: &[MacroInfo]) -> String {
        let mut w = self.file_writer();
        w.line(format!("namespace {}", self.opts.namespace));
        w.blank();
        Self::root_opens(&mut w);
        let class = self.macros_class();
        for (m, info) in self.plan.macros.iter().zip(macros) {
            Self::args_record(
                &mut w,
                &format!("The input of the macro {}.", m.name()),
                &info.input,
            );
            if info.input.record.is_some() {
                w.blank();
            }
        }
        fs::doc_line(
            &mut w,
            "The macros of the API: several calls as one step, with previews and confirmation.",
        );
        w.line("[<Sealed>]");
        w.line(format!("type {class} internal (core: ClientCore) ="));
        w.indent();
        let tail = "?options: CallOptions, ?cancellationToken: CancellationToken";
        let internal_tail =
            "options: CallOptions option, cancellationToken: CancellationToken option";
        for (k, (m, info)) in self.plan.macros.iter().zip(macros).enumerate() {
            let member = &info.member;
            let req = &info.input;
            let param = req.param("input");
            let internal_param = req.internal_param("input");
            let pass = req.pass("input");
            let args = req.args("input");
            let field = format!("Ops.Macro{}", self.names.macros[k].member);
            let run = internal(member);
            let preview = internal(&format!("Preview{member}"));
            let summary = m.source.summary.trim();
            if k > 0 {
                w.blank();
            }
            w.line(format!(
                "member internal _.{run}({internal_param}{internal_tail}) : Task<Result<JsonNode>> ="
            ));
            w.line(format!(
                "    Call.macro core {field} {args} options cancellationToken"
            ));
            w.blank();
            w.line(format!(
                "member internal _.{preview}({internal_param}{internal_tail}) : Task<Result<PreviewResult>> ="
            ));
            w.line(format!(
                "    Call.previewMacro core {field} {args} options cancellationToken"
            ));
            w.blank();
            fs::doc_line(
                &mut w,
                if summary.is_empty() {
                    "Runs the macro."
                } else {
                    summary
                },
            );
            w.line(format!(
                "member this.{member}Async({param}{tail}) : Task<Result<JsonNode, Diagnostic>> ="
            ));
            w.line(format!(
                "    this.{run}({pass}options, cancellationToken) |> TaskResult.toResult"
            ));
            w.blank();
            fs::doc_line(
                &mut w,
                "Renders every step of the macro without sending anything, with a confirmation token for this input.",
            );
            w.line(format!(
                "member this.Preview{member}Async({param}{tail}) : Task<Result<PreviewResult, Diagnostic>> ="
            ));
            w.line(format!(
                "    this.{preview}({pass}options, cancellationToken) |> TaskResult.toResult"
            ));
        }
        w.dedent();
        w.finish()
    }

    /// `Dispatch.fs`.
    pub fn dispatch_file(&self, infos: &[OpInfo], macros: &[MacroInfo]) -> String {
        let class = self.dispatch_class();
        let members = self.top_members();
        let mut w = self.file_writer();
        w.line(format!("namespace {}", self.opts.namespace));
        w.blank();
        Self::root_opens(&mut w);
        // The expression of each resource from the dispatch class: the
        // client member's constructor argument, then the path below it.
        let paths: Vec<String> = self
            .resource_paths()
            .into_iter()
            .map(|p| {
                let (head, rest) = p.split_once('.').unwrap_or((p.as_str(), ""));
                let k = members.iter().position(|(m, _)| m == head).unwrap_or(0);
                if rest.is_empty() {
                    format!("member{k}")
                } else {
                    format!("member{k}.{rest}")
                }
            })
            .collect();
        let mut params = vec!["core: ClientCore".to_string()];
        for (k, (_, c)) in members.iter().enumerate() {
            params.push(format!("member{k}: {c}"));
        }
        if !macros.is_empty() {
            params.push(format!("macros: {}", self.macros_class()));
        }
        fs::doc_line(
            &mut w,
            "Operations by id and macros by name with JSON arguments, through the typed methods (the runtime's IDispatch).",
        );
        w.line("[<Sealed>]");
        w.line(format!("type internal {class}({}) =", params.join(", ")));
        w.indent();
        let ops: Vec<String> = infos.iter().map(|i| format!("Ops.{}", i.field)).collect();
        w.line(format!(
            "static let operations: OperationDescriptor[] = {}",
            array(&ops)
        ));
        let macro_ops: Vec<String> = self
            .names
            .macros
            .iter()
            .map(|m| format!("Ops.Macro{}", m.member))
            .collect();
        w.line(format!(
            "static let macroDescriptors: MacroDescriptor[] = {}",
            array(&macro_ops)
        ));
        w.blank();
        w.line("interface IDispatch with");
        w.indent();
        w.line("member _.Core = core");
        w.blank();
        w.line("member _.Operations = operations :> IReadOnlyList<OperationDescriptor>");
        w.blank();
        w.line("member _.Macros = macroDescriptors :> IReadOnlyList<MacroDescriptor>");
        let ops_tail = "options, cancellationToken";
        let decode = |w: &mut Writer, req: &Request, args: &str| {
            w.line(format!(
                "let ok, request = Typed.TryDecodeArgs<{}>({args}, {})",
                req.decoded_ty(),
                req.decoder()
            ));
        };
        let passed_tail = "Interop.ofObj options, Some cancellationToken";
        // InvokeAsync
        w.blank();
        w.line("member _.InvokeAsync(operation, args, options, cancellationToken) =");
        w.indent();
        w.line("match operation with");
        for (p, info) in self.plan.operations.iter().zip(infos) {
            w.line(format!("| {} ->", lit(&p.op.id.0)));
            w.indent();
            decode(&mut w, &info.request, "args");
            w.line("if ok then");
            w.indent();
            w.line("task {");
            w.indent();
            w.line(format!(
                "let! result = {}.{}({}{passed_tail})",
                paths[p.resource],
                internal(&info.member),
                info.request.passed("request")
            ));
            w.line("return Typed.Untyped result");
            w.dedent();
            w.line("}");
            w.dedent();
            w.line("else");
            w.line(format!(
                "    core.CallAsync(Ops.{}, args, {ops_tail})",
                info.field
            ));
            w.dedent();
        }
        w.line("| _ -> Task.FromResult(Typed.UnknownOperation<JsonNode>(operation))");
        w.dedent();
        // PreviewOperationAsync
        w.blank();
        w.line("member _.PreviewOperationAsync(operation, args, options, cancellationToken) =");
        w.indent();
        w.line("match operation with");
        for (p, info) in self.plan.operations.iter().zip(infos) {
            w.line(format!("| {} ->", lit(&p.op.id.0)));
            w.indent();
            match &info.preview {
                Some(preview) => {
                    decode(&mut w, &info.request, "args");
                    w.line("if ok then");
                    w.line(format!(
                        "    {}.{}({}{passed_tail})",
                        paths[p.resource],
                        internal(preview),
                        info.request.passed("request")
                    ));
                    w.line("else");
                    w.line(format!(
                        "    core.PreviewAsync(Ops.{}, args, {ops_tail})",
                        info.field
                    ));
                }
                None => {
                    w.line(format!(
                        "core.PreviewAsync(Ops.{}, args, {ops_tail})",
                        info.field
                    ));
                }
            }
            w.dedent();
        }
        w.line("| _ -> Task.FromResult(Typed.UnknownOperation<PreviewResult>(operation))");
        w.dedent();
        // PaginateAsync
        w.blank();
        w.line("member _.PaginateAsync(operation, args, options, cancellationToken) =");
        w.indent();
        w.line("match operation with");
        for (p, info) in self.plan.operations.iter().zip(infos) {
            let Some(pages) = &info.pages else {
                continue;
            };
            w.line(format!("| {} ->", lit(&p.op.id.0)));
            w.indent();
            decode(&mut w, &info.request, "args");
            w.line("if ok then");
            w.line(format!(
                "    Typed.UntypedPagesAsync({}.{}({}{passed_tail}), cancellationToken)",
                paths[p.resource],
                internal(pages),
                info.request.passed("request")
            ));
            w.line("else");
            w.line(format!(
                "    core.PagesAsync(Ops.{}, args, {ops_tail})",
                info.field
            ));
            w.dedent();
        }
        w.line("| _ ->");
        w.indent();
        w.line("Typed.Once(");
        w.line("    if operations |> Array.exists (fun op -> op.Id = operation) then");
        w.line("        Typed.NotPaginated<Page<JsonNode>>(operation)");
        w.line("    else");
        w.line("        Typed.UnknownOperation<Page<JsonNode>>(operation)");
        w.line(")");
        w.dedent();
        w.dedent();
        // StreamEvents
        w.blank();
        w.line("member _.StreamEvents(operation, args, options) =");
        w.indent();
        w.line("match operation with");
        for (p, info) in self.plan.operations.iter().zip(infos) {
            let Some(stream) = &info.stream else {
                continue;
            };
            w.line(format!("| {} ->", lit(&p.op.id.0)));
            w.indent();
            decode(&mut w, &info.request, "args");
            w.line("if ok then");
            w.line(format!(
                "    {}.{}({}Interop.ofObj options).Untyped()",
                paths[p.resource],
                internal(stream),
                info.request.passed("request")
            ));
            w.line("else");
            w.line(format!(
                "    core.Stream(Ops.{}, args, options)",
                info.field
            ));
            w.dedent();
        }
        w.line("| _ -> null");
        w.dedent();
        // Macros
        for (preview, ret, run) in [
            (false, "JsonNode", "RunMacroAsync"),
            (true, "PreviewResult", "PreviewMacroAsync"),
        ] {
            w.blank();
            w.line(format!(
                "member _.{run}(name, input, options, cancellationToken) ="
            ));
            w.indent();
            w.line("match name with");
            for ((k, m), info) in self.plan.macros.iter().enumerate().zip(macros) {
                let member = &info.member;
                w.line(format!("| {} ->", lit(m.name())));
                w.indent();
                decode(&mut w, &info.input, "input");
                let method = if preview {
                    internal(&format!("Preview{member}"))
                } else {
                    internal(member)
                };
                w.line("if ok then");
                w.line(format!(
                    "    macros.{method}({}{passed_tail})",
                    info.input.passed("request")
                ));
                w.line("else");
                w.line(format!(
                    "    core.{run}(Ops.Macro{}, input, {ops_tail})",
                    self.names.macros[k].member
                ));
                w.dedent();
            }
            w.line(format!(
                "| _ -> Task.FromResult(Typed.UnknownMacro<{ret}>(name))"
            ));
            w.dedent();
        }
        w.dedent();
        w.dedent();
        w.finish()
    }
}

/// An F# array literal: `[| a; b |]`, or `[||]`.
fn array(items: &[String]) -> String {
    if items.is_empty() {
        "[||]".into()
    } else {
        format!("[| {} |]", items.join("; "))
    }
}
