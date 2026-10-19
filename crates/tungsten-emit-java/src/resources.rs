// SPDX-License-Identifier: AGPL-3.0-only
//! The client, the resources with their request records and typed methods,
//! the namespace classes, `Dispatch`, `Macros` and `Ops` (the descriptor
//! document and one constant per operation).

use tungsten_emit::Writer;
use tungsten_emit::sdk::{ArgSource, ClientPlan, MemberKind, OpPlan, SuccessPlan};
use tungsten_ir::BodyEncoding;

use crate::Ctx;
use crate::java::{self, capitalize, javadoc, lit};
use crate::types::{Component, Extra, From, Jt, used_imports, write_record};

/// The typed shape of one operation.
#[derive(Debug, Clone)]
pub struct OpInfo {
    /// Index into the plan's operations.
    pub index: usize,
    /// The resource it belongs to.
    pub resource: usize,
    /// The resource member calling it.
    pub member: String,
    /// The request record (nested in the resource class).
    pub request: String,
    pub comps: Vec<Component>,
    pub success: Jt,
    pub preview: Option<String>,
    pub pages: Option<(String, Jt)>,
    pub stream: Option<(String, Jt)>,
}

fn runtime(name: &str) -> String {
    format!("{}.{name}", java::RUNTIME)
}

impl Ctx<'_> {
    fn success_jt(&self, p: &OpPlan<'_>, ns: &str) -> Jt {
        match &p.success {
            SuccessPlan::None => Jt {
                java: "Void".into(),
                dec: "v -> null".into(),
                enc: "v -> null".into(),
            },
            SuccessPlan::Json(t) => self.jt(t, From::Resources(ns)),
            SuccessPlan::Bytes => Jt {
                java: "Binary".into(),
                dec: "Decode::binary".into(),
                enc: "Binary::toJson".into(),
            },
            SuccessPlan::Text => Jt {
                java: "String".into(),
                dec: "Decode::string".into(),
                enc: "v -> v".into(),
            },
            SuccessPlan::ByStatus(_) | SuccessPlan::Raw(_) => Jt::json(),
        }
    }

    /// The typed shape of every operation, in plan order.
    pub fn op_infos(&self) -> Vec<OpInfo> {
        let mut out = vec![];
        for (i, p) in self.plan.operations.iter().enumerate() {
            let ns = p.namespace.as_str();
            let rn = &self.names.resources[p.resource];
            let member = |kind: MemberKind| {
                rn.members
                    .iter()
                    .find(|m| m.kind == kind)
                    .map(|m| m.name.clone())
            };
            let name = member(MemberKind::Operation(i)).unwrap_or_else(|| format!("op{i}"));
            let mut request = format!("{}Request", capitalize(&name));
            if request == rn.class {
                request.push_str("Args");
            }
            let arg_names = &self.names.operations[i].arguments;
            let comps = p
                .arguments
                .iter()
                .enumerate()
                .map(|(j, a)| {
                    let jt = match (&a.ty, &a.source) {
                        (Some(t), _) => self.jt(t, From::Resources(ns)),
                        (
                            None,
                            ArgSource::Body {
                                encoding: BodyEncoding::Text,
                                ..
                            },
                        ) => Jt {
                            java: "String".into(),
                            dec: "Decode::string".into(),
                            enc: "v -> v".into(),
                        },
                        (None, _) => Jt {
                            java: "Binary".into(),
                            dec: "Decode::binary".into(),
                            enc: "Binary::toJson".into(),
                        },
                    };
                    let doc = match &a.source {
                        ArgSource::Param { location, .. } => {
                            format!(
                                "Sent as the {} {} parameter (argument {}).",
                                a.wire,
                                location.as_str(),
                                a.key
                            )
                        }
                        ArgSource::Field(f) => f
                            .doc
                            .as_ref()
                            .and_then(|d| d.description.clone().or_else(|| d.summary.clone()))
                            .unwrap_or_else(|| {
                                format!("The {} member of the request body.", a.wire)
                            }),
                        ArgSource::Body { media_type, .. } => {
                            format!("The request body ({media_type}).")
                        }
                    };
                    Component {
                        name: arg_names
                            .get(j)
                            .cloned()
                            .unwrap_or_else(|| format!("arg{j}")),
                        key: a.key.clone(),
                        presence: a.presence,
                        jt,
                        doc,
                    }
                })
                .collect();
            let pages = p.page.as_ref().and_then(|pg| {
                let item = pg
                    .item
                    .as_ref()
                    .map_or_else(Jt::json, |t| self.jt(t, From::Resources(ns)));
                member(MemberKind::Pages(i)).map(|m| (m, item))
            });
            let stream = p.stream.as_ref().and_then(|s| {
                let event = if s.typed {
                    self.jt(&s.event, From::Resources(ns))
                } else {
                    Jt::json()
                };
                member(MemberKind::Stream(i)).map(|m| (m, event))
            });
            out.push(OpInfo {
                index: i,
                resource: p.resource,
                member: name,
                request,
                comps,
                success: self.success_jt(p, ns),
                preview: member(MemberKind::Preview(i)),
                pages,
                stream,
            });
        }
        out
    }

    fn op_const(&self, index: usize) -> String {
        format!("Ops.{}", self.names.operations[index].name)
    }

    fn op_label(&self, index: usize) -> String {
        let op = self.plan.operations[index].op;
        format!(
            "{} ({} {})",
            op.id.0,
            tungsten_emit::sdk::descriptors::method_str(op.method),
            op.path.raw
        )
    }

    fn summary(&self, index: usize) -> String {
        tungsten_emit::sdk::descriptors::op_summary(self.plan.operations[index].op)
    }

    fn resources_package(&self) -> String {
        format!("{}.resources", self.opts.package)
    }

    /// The resource classes, the namespace classes: (path, contents).
    pub fn resources(&self, infos: &[OpInfo]) -> Vec<(String, String)> {
        let mut out = vec![];
        let package = self.resources_package();
        let dir = format!("src/main/java/{}", package.replace('.', "/"));
        for (r, plan) in self.plan.resources.iter().enumerate() {
            let rn = &self.names.resources[r];
            let ns = plan.namespace.as_str();
            let mut w = Writer::new("    ");
            let prefix = &plan.resource.path_prefix;
            let path = if prefix.is_empty() {
                String::new()
            } else {
                format!(" ({prefix})")
            };
            javadoc(
                &mut w,
                &format!("The {} resource{path}.", plan.path_words.join(" ")),
            );
            w.line(format!("public final class {} {{", rn.class));
            w.indent();
            w.line("private final ClientCore core;");
            w.blank();
            w.line("/** The resource of a client's runtime core. */");
            w.block(
                format!("public {}(ClientCore core) {{", rn.class),
                "}",
                |w| {
                    w.line("this.core = core;");
                },
            );
            for m in &rn.members {
                w.blank();
                match m.kind {
                    MemberKind::Child(c) => {
                        let child = &self.names.resources[c].class;
                        w.line(format!(
                            "/** The {} resource. */",
                            self.plan.resources[c].path_words.join(" ")
                        ));
                        w.block(format!("public {child} {}() {{", m.name), "}", |w| {
                            w.line(format!("return new {child}(core);"));
                        });
                    }
                    MemberKind::Operation(o) => self.write_operation(&mut w, &infos[o]),
                    MemberKind::Preview(o) => {
                        let info = &infos[o];
                        javadoc(
                            &mut w,
                            &format!(
                                "Renders the request of {} without sending it and issues a confirmation token bound to these exact arguments.",
                                self.op_label(o)
                            ),
                        );
                        w.block(
                            format!(
                                "public Result<PreviewResult> {}({} request, CallOptions opts) {{",
                                m.name, info.request
                            ),
                            "}",
                            |w| {
                                w.line(format!(
                                    "return Typed.preview(core, {}, request, opts);",
                                    self.op_const(o)
                                ));
                            },
                        );
                    }
                    MemberKind::Pages(o) => {
                        let (name, item) = infos[o]
                            .pages
                            .clone()
                            .unwrap_or_else(|| (m.name.clone(), Jt::json()));
                        javadoc(
                            &mut w,
                            &format!(
                                "The pages of {}, fetched as the iteration advances.",
                                self.op_label(o)
                            ),
                        );
                        w.block(
                            format!(
                                "public Typed.TypedPages<{}> {name}({} request, CallOptions opts) {{",
                                item.java, infos[o].request
                            ),
                            "}",
                            |w| {
                                w.line(format!("return Typed.pages(core, {}, request, opts, {});", self.op_const(o), item.dec));
                            },
                        );
                    }
                    MemberKind::Stream(o) => {
                        let (name, event) = infos[o]
                            .stream
                            .clone()
                            .unwrap_or_else(|| (m.name.clone(), Jt::json()));
                        javadoc(
                            &mut w,
                            &format!(
                                "The server-sent events of {}; close the stream when leaving early.",
                                self.op_label(o)
                            ),
                        );
                        w.block(
                            format!(
                                "public EventStream<{}> {name}({} request, CallOptions opts) {{",
                                event.java, infos[o].request
                            ),
                            "}",
                            |w| {
                                w.line(format!(
                                    "return Typed.stream(core, {}, request, opts, {});",
                                    self.op_const(o),
                                    event.dec
                                ));
                            },
                        );
                    }
                }
            }
            w.dedent();
            w.line("}");
            let body = w.finish();
            let mut imports = vec![
                runtime("Binary"),
                runtime("CallOptions"),
                runtime("ClientCore"),
                runtime("Decode"),
                runtime("EventStream"),
                runtime("Json"),
                runtime("JsonObject"),
                runtime("Patch"),
                runtime("PreviewResult"),
                runtime("Result"),
                runtime("Typed"),
                format!("{}.Ops", self.opts.package),
                "java.util.List".into(),
                "java.util.Map".into(),
            ];
            imports = used_imports(imports, &body);
            if !self.model_names(ns).is_empty() {
                imports.push(format!("{}.*", self.model_package(ns)));
            }
            let mut f = java::file(self.ir, &package, &imports);
            f.line(body.trim_end());
            out.push((format!("{dir}/{}.java", rn.class), f.finish()));
        }
        if let ClientPlan::Namespaces(list) = &self.plan.client {
            for (k, ns) in list.iter().enumerate() {
                let class = &self.names.namespace_classes[k];
                let mut w = Writer::new("    ");
                javadoc(&mut w, &format!("The {} namespace of the API.", ns.name));
                w.line(format!("public final class {class} {{"));
                w.indent();
                w.line("private final ClientCore core;");
                w.blank();
                w.line("/** The namespace of a client's runtime core. */");
                w.block(format!("public {class}(ClientCore core) {{"), "}", |w| {
                    w.line("this.core = core;");
                });
                for (j, &r) in ns.resources.iter().enumerate() {
                    let child = &self.names.resources[r].class;
                    let member = &self.names.namespace_members[k][j];
                    w.blank();
                    w.line(format!(
                        "/** The {} resource. */",
                        self.plan.resources[r].path_words.join(" ")
                    ));
                    w.block(format!("public {child} {member}() {{"), "}", |w| {
                        w.line(format!("return new {child}(core);"));
                    });
                }
                w.dedent();
                w.line("}");
                let mut f = java::file(self.ir, &package, &[runtime("ClientCore")]);
                f.line(w.finish().trim_end());
                out.push((format!("{dir}/{class}.java"), f.finish()));
            }
        }
        out
    }

    fn write_operation(&self, w: &mut Writer, info: &OpInfo) {
        let o = info.index;
        let mut doc = format!("The arguments of {}.", self.op_label(o));
        let summary = self.summary(o);
        write_record(w, &info.request, &doc, &info.comps, &Extra::Refused);
        w.blank();
        doc = format!("Calls {}.", self.op_label(o));
        if !summary.is_empty() {
            doc.push_str("\n\n");
            doc.push_str(&summary);
        }
        javadoc(w, &doc);
        w.block(
            format!(
                "public Result<{}> {}({} request, CallOptions opts) {{",
                info.success.java, info.member, info.request
            ),
            "}",
            |w| {
                w.line(format!(
                    "return Typed.call(core, {}, request, opts, {});",
                    self.op_const(o),
                    info.success.dec
                ));
            },
        );
        w.blank();
        w.line(
            format!(
                "/** Calls {} with the default call options. */",
                self.op_label(o)
            )
            .replace("*/ with", "with"),
        );
        w.block(
            format!(
                "public Result<{}> {}({} request) {{",
                info.success.java, info.member, info.request
            ),
            "}",
            |w| {
                w.line(format!(
                    "return {}(request, CallOptions.defaults());",
                    info.member
                ));
            },
        );
    }

    /// The accessor chain of a resource from the client (`client.publicNs().webhooks()`).
    pub fn accessor(&self, r: usize) -> String {
        let plan = &self.plan.resources[r];
        if let Some(parent) = plan.parent {
            let rn = &self.names.resources[parent];
            let name = rn
                .members
                .iter()
                .find(|m| m.kind == MemberKind::Child(r))
                .map(|m| m.name.clone())
                .unwrap_or_default();
            return format!("{}.{name}()", self.accessor(parent));
        }
        match &self.plan.client {
            ClientPlan::Resources(list) => {
                let at = list.iter().position(|&x| x == r).unwrap_or(0);
                format!("client.{}()", self.names.client_members[at])
            }
            ClientPlan::Namespaces(list) => {
                for (k, ns) in list.iter().enumerate() {
                    if let Some(j) = ns.resources.iter().position(|&x| x == r) {
                        return format!(
                            "client.{}().{}()",
                            self.names.client_members[k], self.names.namespace_members[k][j]
                        );
                    }
                }
                "client".into()
            }
        }
    }

    /// The schemes with an authorization-code flow.
    fn code_schemes(&self) -> Vec<String> {
        self.ir
            .auth
            .iter()
            .filter(|s| s.authorization_code().is_some())
            .map(|s| s.name().to_string())
            .collect()
    }

    /// The client, `Ops`, `Dispatch` and `Macros`: (path, contents).
    pub fn root(&self, infos: &[OpInfo]) -> Vec<(String, String)> {
        let package = &self.opts.package;
        let dir = format!("src/main/java/{}", package.replace('.', "/"));
        let resources = self.resources_package();
        let has_macros = !self.plan.macros.is_empty();
        let mut out = vec![];

        // Ops
        let mut w = Writer::new("    ");
        javadoc(
            &mut w,
            "The SDK descriptor document (the resource tungsten-descriptors.json) and every callable operation as data for the runtime.",
        );
        w.line("public final class Ops {");
        w.indent();
        if self.ir.agent.redirects == tungsten_ir::RedirectPolicy::None {
            w.line("static { io.github.pboachie.tungsten.runtime.ClientCore.requireRedirectPolicyV1(); }");
        }
        w.line("/** The descriptor document, loaded once. */");
        w.line("public static final Descriptors DOCUMENT = Descriptors.load(Ops.class.getResourceAsStream(\"tungsten-descriptors.json\"));");
        for (i, p) in self.plan.operations.iter().enumerate() {
            w.blank();
            w.line(format!("/** {} */", java::doc_text(&self.op_label(i))));
            w.line(format!(
                "public static final OperationDescriptor {} = DOCUMENT.operation({});",
                self.names.operations[i].name,
                lit(&p.op.id.0)
            ));
        }
        w.blank();
        w.line("/** Every callable operation, in IR order. */");
        let ops: Vec<String> = self
            .names
            .operations
            .iter()
            .map(|o| o.name.clone())
            .collect();
        w.line(format!(
            "public static final List<OperationDescriptor> OPERATIONS = List.of({});",
            ops.join(", ")
        ));
        w.blank();
        w.line("/** Every macro, in IR order. */");
        let macros: Vec<String> = self
            .plan
            .macros
            .iter()
            .map(|m| format!("DOCUMENT.macro({})", lit(m.name())))
            .collect();
        w.line(format!(
            "public static final List<MacroDescriptor> MACROS = List.of({});",
            macros.join(", ")
        ));
        w.blank();
        w.line("private Ops() {}");
        w.dedent();
        w.line("}");
        let mut f = java::file(
            self.ir,
            package,
            &[
                runtime("Descriptors"),
                runtime("MacroDescriptor"),
                runtime("OperationDescriptor"),
                "java.util.List".into(),
            ],
        );
        f.line(w.finish().trim_end());
        out.push((format!("{dir}/Ops.java"), f.finish()));

        // Client
        let client = &self.names.client_class;
        let mut w = Writer::new("    ");
        javadoc(
            &mut w,
            &format!(
                "The {} API client. It is safe for concurrent use; one client shares one connection pool, one confirmation key and one idempotency store.",
                if self.ir.api.title.is_empty() {
                    &self.ir.api.name.wire
                } else {
                    &self.ir.api.title
                }
            ),
        );
        w.line(format!("public final class {client} {{"));
        w.indent();
        w.line("private final ClientCore core;");
        w.blank();
        javadoc(
            &mut w,
            "A client; without a base URL in the options it calls the API's first server.",
        );
        w.block(
            format!("public {client}(ClientOptions options) {{"),
            "}",
            |w| {
                if self.ir.agent.redirects == tungsten_ir::RedirectPolicy::None {
                    w.line("ClientCore.requireRedirectPolicyV1();");
                }
                w.line("this.core = new ClientCore(Ops.DOCUMENT.api(), Ops.OPERATIONS, options);");
            },
        );
        w.blank();
        w.line("/** The runtime client the SDK runs on. */");
        w.block("public ClientCore core() {", "}", |w| {
            w.line("return core;");
        });
        w.blank();
        w.line("/** Operations by id and macros by name with JSON arguments, through the typed methods. */");
        w.block("public Dispatch dispatch() {", "}", |w| {
            w.line("return new Dispatch(this);");
        });
        if has_macros {
            w.blank();
            w.line("/** The macros. */");
            w.block("public Macros macros() {", "}", |w| {
                w.line("return new Macros(core);");
            });
        }
        let schemes = self.code_schemes();
        if !schemes.is_empty() {
            w.blank();
            w.line("/** Managed S256/state attempts of an OAuth2 scheme; completion requires its handle, full callback and owning session binding. */");
            w.block("public OAuthFlow oauth(String scheme) {", "}", |w| {
                w.line("return core.oauth(scheme);");
            });
            if schemes.len() == 1 {
                w.blank();
                w.line(format!(
                    "/** The authorization-code helpers of {}. */",
                    java::doc_text(&schemes[0])
                ));
                w.block("public OAuthFlow oauth() {", "}", |w| {
                    w.line(format!("return core.oauth({});", lit(&schemes[0])));
                });
            }
        }
        match &self.plan.client {
            ClientPlan::Resources(list) => {
                for (j, &r) in list.iter().enumerate() {
                    let class = &self.names.resources[r].class;
                    w.blank();
                    w.line(format!(
                        "/** The {} resource. */",
                        self.plan.resources[r].path_words.join(" ")
                    ));
                    w.block(
                        format!(
                            "public {resources}.{class} {}() {{",
                            self.names.client_members[j]
                        ),
                        "}",
                        |w| {
                            w.line(format!("return new {resources}.{class}(core);"));
                        },
                    );
                }
            }
            ClientPlan::Namespaces(list) => {
                for (k, ns) in list.iter().enumerate() {
                    let class = &self.names.namespace_classes[k];
                    w.blank();
                    w.line(format!(
                        "/** The {} namespace. */",
                        java::doc_text(&ns.name)
                    ));
                    w.block(
                        format!(
                            "public {resources}.{class} {}() {{",
                            self.names.client_members[k]
                        ),
                        "}",
                        |w| {
                            w.line(format!("return new {resources}.{class}(core);"));
                        },
                    );
                }
            }
        }
        w.dedent();
        w.line("}");
        let body = w.finish();
        let imports = used_imports(
            vec![
                runtime("ClientCore"),
                runtime("ClientOptions"),
                runtime("OAuthFlow"),
            ],
            &body,
        );
        let mut f = java::file(self.ir, package, &imports);
        f.line(body.trim_end());
        out.push((format!("{dir}/{client}.java"), f.finish()));

        out.push((format!("{dir}/Dispatch.java"), self.dispatch(infos)));
        if has_macros {
            out.push((format!("{dir}/Macros.java"), self.macros_file()));
        }
        out
    }

    fn dispatch(&self, infos: &[OpInfo]) -> String {
        let client = &self.names.client_class;
        let mut w = Writer::new("    ");
        javadoc(
            &mut w,
            "Runs operations by id and macros by name with an arguments object keyed by the arguments layout keys, through the typed methods; arguments that do not decode go to the runtime as they are, so pre-flight validation reports them. Results carry the decoded wire body.",
        );
        w.line(
            "public final class Dispatch implements io.github.pboachie.tungsten.runtime.Dispatch {",
        );
        w.indent();
        w.line(format!("private final {client} client;"));
        w.blank();
        w.line("/** The dispatch of a client. */");
        w.block(format!("public Dispatch({client} client) {{"), "}", |w| {
            w.line("this.client = client;");
        });
        w.blank();
        w.line("@Override");
        w.block("public ClientCore core() {", "}", |w| {
            w.line("return client.core();");
        });
        w.blank();
        w.line("@Override");
        w.block(
            "public List<OperationDescriptor> operations() {",
            "}",
            |w| {
                w.line("return Ops.OPERATIONS;");
            },
        );
        w.blank();
        w.line("@Override");
        w.block("public List<MacroDescriptor> macros() {", "}", |w| {
            w.line("return Ops.MACROS;");
        });
        let request = |info: &OpInfo| -> String {
            format!(
                "{}.{}.{}",
                self.resources_package(),
                self.names.resources[info.resource].class,
                info.request
            )
        };
        let cases = |w: &mut Writer,
                     pick: &dyn Fn(&OpInfo) -> Option<String>,
                     fallback: &dyn Fn(&OpInfo) -> String| {
            for info in infos {
                let Some(call) = pick(info) else {
                    continue;
                };
                let id = &self.plan.operations[info.index].op.id.0;
                w.block(format!("case {}: {{", lit(id)), "}", |w| {
                    w.line(format!(
                        "var request = Typed.decodeArgs(args, {}::fromJson);",
                        request(info)
                    ));
                    w.line(format!(
                        "return request == null ? {} : {call};",
                        fallback(info)
                    ));
                });
            }
        };
        w.blank();
        w.line("@Override");
        w.block("public Result<Object> invoke(String operation, Object args, CallOptions opts) {", "}", |w| {
            w.block("switch (operation) {", "}", |w| {
                cases(
                    w,
                    &|i| Some(format!("Typed.untyped({}.{}(request, opts))", self.accessor(i.resource), i.member)),
                    &|i| format!("client.core().call({}, args, opts)", self.op_const(i.index)),
                );
                w.line("default:");
                w.line("    return io.github.pboachie.tungsten.runtime.Dispatch.unknownOperation(client.core(), operation);");
            });
        });
        w.blank();
        w.line("@Override");
        w.block(
            "public Result<PreviewResult> previewOperation(String operation, Object args, CallOptions opts) {",
            "}",
            |w| {
                w.block("switch (operation) {", "}", |w| {
                    cases(
                        w,
                        &|i| i.preview.as_ref().map(|m| format!("{}.{m}(request, opts)", self.accessor(i.resource))),
                        &|i| format!("client.core().preview({}, args, opts)", self.op_const(i.index)),
                    );
                    w.line("default:");
                    w.line("    break;");
                });
                w.line("OperationDescriptor op = io.github.pboachie.tungsten.runtime.Dispatch.find(Ops.OPERATIONS, operation);");
                w.line("return op == null ? io.github.pboachie.tungsten.runtime.Dispatch.unknownOperation(client.core(), operation) : client.core().preview(op, args, opts);");
            },
        );
        w.blank();
        w.line("@Override");
        w.block(
            "public List<Result<Page<Object>>> paginate(String operation, Object args, CallOptions opts) {",
            "}",
            |w| {
                w.block("switch (operation) {", "}", |w| {
                    cases(
                        w,
                        &|i| {
                            i.pages
                                .as_ref()
                                .map(|(m, _)| format!("{}.{m}(request, opts).allRaw()", self.accessor(i.resource)))
                        },
                        &|i| format!("client.core().pages({}, args, opts).all()", self.op_const(i.index)),
                    );
                    w.line("default:");
                    w.line("    return List.of(io.github.pboachie.tungsten.runtime.Dispatch.notPaginated(client.core(), operation));");
                });
            },
        );
        w.blank();
        w.line("@Override");
        w.block("public EventStream<?> streamEvents(String operation, Object args, CallOptions opts) {", "}", |w| {
            w.block("switch (operation) {", "}", |w| {
                cases(
                    w,
                    &|i| i.stream.as_ref().map(|(m, _)| format!("{}.{m}(request, opts)", self.accessor(i.resource))),
                    &|i| format!("client.core().stream({}, args, opts)", self.op_const(i.index)),
                );
                w.line("default:");
                w.line("    return io.github.pboachie.tungsten.runtime.Dispatch.noStream(client.core(), io.github.pboachie.tungsten.runtime.Dispatch.find(Ops.OPERATIONS, operation), operation);");
            });
        });
        w.blank();
        w.line("@Override");
        w.block("public Result<Object> runMacro(String name, Object input, CallOptions opts) {", "}", |w| {
            w.line("MacroDescriptor m = io.github.pboachie.tungsten.runtime.Dispatch.findMacro(Ops.MACROS, name);");
            w.line("return m == null ? io.github.pboachie.tungsten.runtime.Dispatch.unknownMacro(client.core(), name) : client.core().runMacro(m, input, opts);");
        });
        w.blank();
        w.line("@Override");
        w.block("public Result<PreviewResult> previewMacro(String name, Object input, CallOptions opts) {", "}", |w| {
            w.line("MacroDescriptor m = io.github.pboachie.tungsten.runtime.Dispatch.findMacro(Ops.MACROS, name);");
            w.line("return m == null ? io.github.pboachie.tungsten.runtime.Dispatch.unknownMacro(client.core(), name) : client.core().previewMacro(m, input, opts);");
        });
        w.dedent();
        w.line("}");
        let body = w.finish();
        let imports = used_imports(
            vec![
                runtime("CallOptions"),
                runtime("ClientCore"),
                runtime("EventStream"),
                runtime("MacroDescriptor"),
                runtime("OperationDescriptor"),
                runtime("Page"),
                runtime("PreviewResult"),
                runtime("Result"),
                runtime("Typed"),
                "java.util.List".into(),
            ],
            &body,
        );
        let mut f = java::file(self.ir, &self.opts.package, &imports);
        f.line(body.trim_end());
        f.finish()
    }

    fn macros_file(&self) -> String {
        let mut w = Writer::new("    ");
        javadoc(
            &mut w,
            "The macros of the API: compiled multi-step workflows the runtime runs.",
        );
        w.line("public final class Macros {");
        w.indent();
        w.line("private final ClientCore core;");
        w.blank();
        w.line("/** The macros of a client's runtime core. */");
        w.block("public Macros(ClientCore core) {", "}", |w| {
            w.line("this.core = core;");
        });
        let mut taken: Vec<String> = self.names.macros.iter().map(|m| m.member.clone()).collect();
        for (k, m) in self.plan.macros.iter().enumerate() {
            let member = &self.names.macros[k].member;
            let mut preview = format!("preview{}", capitalize(member));
            while taken.contains(&preview) {
                preview.push_str("Macro");
            }
            taken.push(preview.clone());
            let summary = m.source.summary.clone();
            let extends = m
                .base
                .map(|b| {
                    format!(
                        " Its input is the arguments object of {} plus the macro's own fields.",
                        self.plan.operations[b].op.id.0
                    )
                })
                .unwrap_or_default();
            w.blank();
            javadoc(
                &mut w,
                &format!("Runs the macro {}: {summary}{extends}", m.name()),
            );
            w.block(
                format!("public Result<Object> {member}(Object input, CallOptions opts) {{"),
                "}",
                |w| {
                    w.line(format!(
                        "return core.runMacro(Ops.DOCUMENT.macro({}), input, opts);",
                        lit(m.name())
                    ));
                },
            );
            w.blank();
            javadoc(
                &mut w,
                &format!(
                    "Previews the macro {} without sending anything and issues its confirmation token.",
                    m.name()
                ),
            );
            w.block(
                format!(
                    "public Result<PreviewResult> {preview}(Object input, CallOptions opts) {{"
                ),
                "}",
                |w| {
                    w.line(format!(
                        "return core.previewMacro(Ops.DOCUMENT.macro({}), input, opts);",
                        lit(m.name())
                    ));
                },
            );
        }
        w.dedent();
        w.line("}");
        let mut f = java::file(
            self.ir,
            &self.opts.package,
            &[
                runtime("CallOptions"),
                runtime("ClientCore"),
                runtime("PreviewResult"),
                runtime("Result"),
            ],
        );
        f.line(w.finish().trim_end());
        f.finish()
    }
}
