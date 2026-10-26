// SPDX-License-Identifier: AGPL-3.0-only
//! The client, the resources with their request classes and typed
//! functions, the namespace classes, `Dispatch`, `Macros` and `Ops` (the
//! descriptor document and one constant per operation).

use tungsten_emit::Writer;
use tungsten_emit::sdk::{ArgSource, ClientPlan, MemberKind, OpPlan, SuccessPlan};
use tungsten_ir::{BodyEncoding, Presence};

use crate::Ctx;
use crate::kotlin::{self, capitalize, kdoc, kdoc_line, lit};
use crate::types::{Component, Extra, From, Kt, used_imports, write_class};

/// The typed shape of one operation.
#[derive(Debug, Clone)]
pub struct OpInfo {
    /// Index into the plan's operations.
    pub index: usize,
    /// The resource it belongs to.
    pub resource: usize,
    /// The resource member calling it.
    pub member: String,
    /// The request class (nested in the resource class).
    pub request: String,
    pub comps: Vec<Component>,
    /// The success value: its type is `full()`.
    pub success: Kt,
    pub preview: Option<String>,
    pub pages: Option<(String, Kt)>,
    pub stream: Option<(String, Kt)>,
}

impl OpInfo {
    /// The request parameter: with a default when no argument is required.
    fn request_param(&self) -> String {
        let required = self
            .comps
            .iter()
            .any(|c| matches!(c.presence, Presence::Required | Presence::RequiredNullable));
        if required {
            format!("request: {}", self.request)
        } else {
            format!("request: {} = {}()", self.request, self.request)
        }
    }
}

fn runtime(name: &str) -> String {
    format!("{}.{name}", kotlin::RUNTIME)
}

fn layer(name: &str) -> String {
    format!("{}.{name}", kotlin::KOTLIN_RUNTIME)
}

/// The runtime's `Dispatch` interface (the generated class has its name).
const DISPATCH: &str = "io.github.pboachie.tungsten.runtime.Dispatch";

/// The opt-in coroutines layer's package (functions are called by their
/// qualified name: a resource member of the same name cannot shadow them).
const COROUTINES: &str = "io.github.pboachie.tungsten.runtime.kotlin.coroutines";

/// The call options parameter of every typed function.
const OPTIONS: &str = "options: CallOptions = CallOptions.defaults()";

impl Ctx<'_> {
    fn success_kt(&self, p: &OpPlan<'_>, ns: &str) -> Kt {
        match &p.success {
            SuccessPlan::None => Kt {
                base: "Nothing".into(),
                dec: "{ _ -> null }".into(),
                enc: "Codec::same".into(),
                nullable: true,
            },
            SuccessPlan::Json(t) => self.kt(t, From::Resources(ns)),
            SuccessPlan::Bytes => crate::types::primitive_kt(&tungsten_ir::Primitive::Bytes),
            SuccessPlan::Text => Kt {
                base: "String".into(),
                dec: "Codec::string".into(),
                enc: "Codec::same".into(),
                nullable: false,
            },
            SuccessPlan::ByStatus(_) | SuccessPlan::Raw(_) => Kt::json(),
        }
    }

    /// The decoder of a success value: `(Any?) -> full`, the runtime never
    /// gives it null (a missing body is null without decoding).
    fn success_dec(kt: &Kt) -> String {
        if kt.base == "Nothing" {
            kt.dec.clone()
        } else {
            kt.full_dec()
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
            let arg_names = &self.names.operations[i].arguments;
            let comps = p
                .arguments
                .iter()
                .enumerate()
                .map(|(j, a)| {
                    let kt = match (&a.ty, &a.source) {
                        (Some(t), _) => self.kt(t, From::Resources(ns)),
                        (
                            None,
                            ArgSource::Body {
                                encoding: BodyEncoding::Text,
                                ..
                            },
                        ) => Kt {
                            base: "String".into(),
                            dec: "Codec::string".into(),
                            enc: "Codec::same".into(),
                            nullable: false,
                        },
                        (None, _) => crate::types::primitive_kt(&tungsten_ir::Primitive::Bytes),
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
                        kt,
                        doc,
                    }
                })
                .collect();
            let pages = p.page.as_ref().and_then(|pg| {
                let item = pg
                    .item
                    .as_ref()
                    .map_or_else(Kt::json, |t| self.kt(t, From::Resources(ns)));
                member(MemberKind::Pages(i)).map(|m| (m, item))
            });
            let stream = p.stream.as_ref().and_then(|s| {
                let event = if s.typed {
                    self.kt(&s.event, From::Resources(ns))
                } else {
                    Kt::json()
                };
                member(MemberKind::Stream(i)).map(|m| (m, event))
            });
            out.push(OpInfo {
                index: i,
                resource: p.resource,
                request: crate::request_class(&name, &rn.class),
                member: name,
                comps,
                success: self.success_kt(p, ns),
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

    /// The names of the opt-in coroutine members of resource `r`, indexed like
    /// its members (`None` for members without one): `<op>Suspend` for an
    /// operation, `<pages>Flow` and `<stream>Flow` for the pages and stream
    /// members. A name another member already has gets a number.
    fn coroutine_names(&self, r: usize) -> Vec<Option<String>> {
        let members = &self.names.resources[r].members;
        let mut taken: std::collections::BTreeSet<String> =
            members.iter().map(|m| m.name.clone()).collect();
        members
            .iter()
            .map(|m| {
                let suffix = match m.kind {
                    MemberKind::Operation(_) => "Suspend",
                    MemberKind::Pages(_) | MemberKind::Stream(_) => "Flow",
                    _ => return None,
                };
                let mut name = format!("{}{suffix}", m.name);
                let mut n = 2;
                while taken.contains(&name) {
                    name = format!("{}{suffix}{n}", m.name);
                    n += 1;
                }
                taken.insert(name.clone());
                Some(name)
            })
            .collect()
    }

    /// The resource classes, the namespace classes: (path, contents).
    pub fn resources(&self, infos: &[OpInfo]) -> Vec<(String, String)> {
        let mut out = vec![];
        let package = self.resources_package();
        let dir = format!("src/main/kotlin/{}", package.replace('.', "/"));
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
            kdoc(
                &mut w,
                &format!("The {} resource{path}.", plan.path_words.join(" ")),
            );
            w.line(format!(
                "class {}(private val core: ClientCore) {{",
                rn.class
            ));
            w.indent();
            let mut first = true;
            let suspend = if self.opts.coroutines {
                self.coroutine_names(r)
            } else {
                vec![]
            };
            for (mi, m) in rn.members.iter().enumerate() {
                if !first {
                    w.blank();
                }
                first = false;
                match m.kind {
                    MemberKind::Child(c) => {
                        let child = &self.names.resources[c].class;
                        kdoc_line(
                            &mut w,
                            &format!(
                                "The {} resource.",
                                self.plan.resources[c].path_words.join(" ")
                            ),
                        );
                        w.line(format!("val {}: {child} = {child}(core)", m.name));
                    }
                    MemberKind::Operation(o) => {
                        self.write_operation(&mut w, &infos[o]);
                        if let Some(Some(name)) = suspend.get(mi) {
                            w.blank();
                            self.write_suspend(&mut w, &infos[o], &m.name, name);
                        }
                    }
                    MemberKind::Preview(o) => {
                        let info = &infos[o];
                        kdoc(
                            &mut w,
                            &format!(
                                "Renders the request of {} without sending it and issues a confirmation token bound to these exact arguments.",
                                self.op_label(o)
                            ),
                        );
                        w.line(format!(
                            "fun {}({}, {OPTIONS}): Result<PreviewResult> =",
                            m.name,
                            info.request_param()
                        ));
                        w.line(format!(
                            "    Typed.preview(core, {}, request, options)",
                            self.op_const(o)
                        ));
                    }
                    MemberKind::Pages(o) => {
                        let (name, item) = infos[o]
                            .pages
                            .clone()
                            .unwrap_or_else(|| (m.name.clone(), Kt::json()));
                        kdoc(
                            &mut w,
                            &format!(
                                "The pages of {}, fetched as the sequence advances (once).",
                                self.op_label(o)
                            ),
                        );
                        w.line(format!(
                            "fun {name}({}, {OPTIONS}): PageSequence<{}> =",
                            infos[o].request_param(),
                            item.full()
                        ));
                        w.line(format!(
                            "    PageSequence(Typed.pages(core, {}, request, options, {}))",
                            self.op_const(o),
                            item.full_dec()
                        ));
                        if let Some(Some(flow)) = suspend.get(mi) {
                            w.blank();
                            kdoc(
                                &mut w,
                                &format!(
                                    "The pages of {} as a cold Flow: each collection starts a new pagination and a page is requested only when the collector takes the previous one (add buffer(n) to prefetch up to n pages). maxResponseBytes and pagination cycle detection apply to every page; an error is the last element. Cancelling the collector cancels the request in flight.",
                                    self.op_label(o)
                                ),
                            );
                            w.line(format!(
                                "fun {flow}({}, {OPTIONS}): kotlinx.coroutines.flow.Flow<Result<Page<{}>>> =",
                                infos[o].request_param(),
                                item.full()
                            ));
                            w.line(format!(
                                "    {COROUTINES}.pagesFlow {{ Typed.pages(core, {}, request, options, {}) }}",
                                self.op_const(o),
                                item.full_dec()
                            ));
                        }
                    }
                    MemberKind::Stream(o) => {
                        let (name, event) = infos[o]
                            .stream
                            .clone()
                            .unwrap_or_else(|| (m.name.clone(), Kt::json()));
                        kdoc(
                            &mut w,
                            &format!(
                                "The server-sent events of {}; close the stream (`use`) when leaving early.",
                                self.op_label(o)
                            ),
                        );
                        w.line(format!(
                            "fun {name}({}, {OPTIONS}): EventStream<{}> =",
                            infos[o].request_param(),
                            event.full()
                        ));
                        w.line(format!(
                            "    Typed.stream(core, {}, request, options, {})",
                            self.op_const(o),
                            event.full_dec()
                        ));
                        if let Some(Some(flow)) = suspend.get(mi) {
                            w.blank();
                            kdoc(
                                &mut w,
                                &format!(
                                    "The server-sent events of {} as a cold Flow: each collection opens a new stream, events are read only as the collector takes them (add buffer(n) to prefetch up to n events), and the stream is closed when the flow completes, fails or is cancelled. An error is the last element.",
                                    self.op_label(o)
                                ),
                            );
                            w.line(format!(
                                "fun {flow}({}, {OPTIONS}): kotlinx.coroutines.flow.Flow<Result<StreamEvent<{}>>> =",
                                infos[o].request_param(),
                                event.full()
                            ));
                            w.line(format!(
                                "    {COROUTINES}.eventsFlow {{ Typed.stream(core, {}, request, options, {}) }}",
                                self.op_const(o),
                                event.full_dec()
                            ));
                        }
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
                runtime("PreviewResult"),
                runtime("Result"),
                runtime("Typed"),
                layer("Codec"),
                layer("PageSequence"),
                layer("Patch"),
                format!("{}.Ops", self.opts.package),
            ];
            if self.opts.coroutines {
                // Only the opt-in Flow signatures name them.
                imports.extend([runtime("Page"), runtime("StreamEvent")]);
            }
            // Models are imported by name, not with a star import: a
            // default import (`kotlin.annotation.Target`, `java.lang.Error`)
            // would win over a star-imported model of the same name.
            let model_package = self.model_package(ns);
            imports.extend(
                self.model_names(ns)
                    .into_iter()
                    .filter(|m| !self.resources_taken.contains(m))
                    .map(|m| format!("{model_package}.{m}")),
            );
            imports = used_imports(imports, &body);
            let mut f = kotlin::file(self.ir, &package, &imports);
            f.line(body.trim_end());
            out.push((format!("{dir}/{}.kt", rn.class), f.finish()));
        }
        if let ClientPlan::Namespaces(list) = &self.plan.client {
            for (k, ns) in list.iter().enumerate() {
                let class = &self.names.namespace_classes[k];
                let mut w = Writer::new("    ");
                kdoc(&mut w, &format!("The {} namespace of the API.", ns.name));
                w.line(format!("class {class}(core: ClientCore) {{"));
                w.indent();
                for (j, &r) in ns.resources.iter().enumerate() {
                    let child = &self.names.resources[r].class;
                    let member = &self.names.namespace_members[k][j];
                    if j > 0 {
                        w.blank();
                    }
                    kdoc_line(
                        &mut w,
                        &format!(
                            "The {} resource.",
                            self.plan.resources[r].path_words.join(" ")
                        ),
                    );
                    w.line(format!("val {member}: {child} = {child}(core)"));
                }
                w.dedent();
                w.line("}");
                let mut f = kotlin::file(self.ir, &package, &[runtime("ClientCore")]);
                f.line(w.finish().trim_end());
                out.push((format!("{dir}/{class}.kt"), f.finish()));
            }
        }
        out
    }

    fn write_operation(&self, w: &mut Writer, info: &OpInfo) {
        let o = info.index;
        let mut doc = format!("The arguments of {}.", self.op_label(o));
        let summary = self.summary(o);
        write_class(w, &info.request, &doc, &info.comps, &Extra::Refused);
        w.blank();
        doc = format!("Calls {}.", self.op_label(o));
        if !summary.is_empty() {
            doc.push_str("\n\n");
            doc.push_str(&summary);
        }
        kdoc(w, &doc);
        w.line(format!(
            "fun {}({}, {OPTIONS}): Result<{}> =",
            info.member,
            info.request_param(),
            info.success.full()
        ));
        w.line(format!(
            "    Typed.call(core, {}, request, options, {})",
            self.op_const(o),
            Self::success_dec(&info.success)
        ));
    }

    /// The opt-in `suspend` variant of the blocking function `blocking`.
    fn write_suspend(&self, w: &mut Writer, info: &OpInfo, blocking: &str, name: &str) {
        kdoc(
            w,
            &format!(
                "Calls {} without blocking the caller's thread (on Dispatchers.IO) and returns the same Result as {blocking}. Cancelling the coroutine cancels the request in flight and ends the call with a CancellationException instead of a Result; a mutation cancelled after it was sent has an unknown outcome, so reuse its idempotency key to repeat it.",
                self.op_label(info.index)
            ),
        );
        w.line(format!(
            "suspend fun {name}({}, {OPTIONS}): Result<{}> =",
            info.request_param(),
            info.success.full()
        ));
        w.line(format!(
            "    {COROUTINES}.suspending {{ {blocking}(request, options) }}"
        ));
    }

    /// The accessor chain of a resource from the client
    /// (`client.public_.webhooks`).
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
            return format!("{}.{name}", self.accessor(parent));
        }
        match &self.plan.client {
            ClientPlan::Resources(list) => {
                let at = list.iter().position(|&x| x == r).unwrap_or(0);
                format!("client.{}", self.names.client_members[at])
            }
            ClientPlan::Namespaces(list) => {
                for (k, ns) in list.iter().enumerate() {
                    if let Some(j) = ns.resources.iter().position(|&x| x == r) {
                        return format!(
                            "client.{}.{}",
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
        let dir = format!("src/main/kotlin/{}", package.replace('.', "/"));
        let resources = self.resources_package();
        let has_macros = !self.plan.macros.is_empty();
        let mut out = vec![];

        // Ops
        let mut w = Writer::new("    ");
        kdoc(
            &mut w,
            "The SDK descriptor document (the resource tungsten-descriptors.json) and every callable operation as data for the runtime.",
        );
        let no_redirects = self.ir.agent.redirects == tungsten_ir::RedirectPolicy::None;
        let imported = self
            .plan
            .operations
            .iter()
            .any(|p| p.op.agent.import_policy.is_some());
        w.block("object Ops {", "}", |w| {
            if no_redirects {
                kdoc_line(w, "The manifest forbids redirects: the runtime must enforce redirect policy v1.");
                w.line("init { io.github.pboachie.tungsten.runtime.ClientCore.requireRedirectPolicyV1() }");
                w.blank();
            }
            if imported {
                kdoc_line(w, "Operations carry imported-operation policy: the runtime must enforce import policy v1.");
                w.line("init { io.github.pboachie.tungsten.runtime.ClientCore.requireImportPolicyV1() }");
                w.blank();
            }
            kdoc_line(w, "The descriptor document, loaded once.");
            w.line("@JvmField");
            w.line("val DOCUMENT: Descriptors = Descriptors.load(Ops::class.java.getResourceAsStream(\"tungsten-descriptors.json\"))");
            for (i, p) in self.plan.operations.iter().enumerate() {
                w.blank();
                kdoc_line(w, &self.op_label(i));
                w.line("@JvmField");
                w.line(format!(
                    "val {}: OperationDescriptor = DOCUMENT.operation({})",
                    self.names.operations[i].name,
                    lit(&p.op.id.0)
                ));
            }
            w.blank();
            kdoc_line(w, "Every callable operation, in IR order.");
            w.line("@JvmField");
            let ops: Vec<String> = self
                .names
                .operations
                .iter()
                .map(|o| o.name.clone())
                .collect();
            w.line(format!(
                "val OPERATIONS: List<OperationDescriptor> = listOf({})",
                ops.join(", ")
            ));
            w.blank();
            kdoc_line(w, "Every macro, in IR order.");
            w.line("@JvmField");
            let macros: Vec<String> = self
                .plan
                .macros
                .iter()
                .map(|m| format!("DOCUMENT.macro({})", lit(m.name())))
                .collect();
            w.line(format!(
                "val MACROS: List<MacroDescriptor> = listOf({})",
                macros.join(", ")
            ));
        });
        let mut f = kotlin::file(
            self.ir,
            package,
            &[
                runtime("Descriptors"),
                runtime("MacroDescriptor"),
                runtime("OperationDescriptor"),
            ],
        );
        f.line(w.finish().trim_end());
        out.push((format!("{dir}/Ops.kt"), f.finish()));

        // Client
        let client = &self.names.client_class;
        let mut w = Writer::new("    ");
        kdoc(
            &mut w,
            &format!(
                "The {} API client. It is safe for concurrent use; one client shares one confirmation key and one idempotency store. Without a base URL in the options it calls the API's first server.",
                if self.ir.api.title.is_empty() {
                    &self.ir.api.name.wire
                } else {
                    &self.ir.api.title
                }
            ),
        );
        w.line(format!(
            "class {client}(options: ClientOptions = ClientOptions.builder().build()) {{"
        ));
        w.indent();
        if no_redirects {
            w.line("init {");
            w.line("    ClientCore.requireRedirectPolicyV1()");
            w.line("}");
            w.blank();
        }
        kdoc_line(
            &mut w,
            "The runtime client the SDK runs on (it reports the Kotlin runtime's identity).",
        );
        w.line("val core: ClientCore = ClientCore(Ops.DOCUMENT.api(), Ops.OPERATIONS, KotlinRuntime.options(options))");
        w.blank();
        kdoc_line(
            &mut w,
            "Operations by id and macros by name with JSON arguments, through the typed functions.",
        );
        w.line("fun dispatch(): Dispatch = Dispatch(this)");
        if has_macros {
            w.blank();
            kdoc_line(&mut w, "The macros.");
            w.line("val macros: Macros = Macros(core)");
        }
        let schemes = self.code_schemes();
        if !schemes.is_empty() {
            w.blank();
            kdoc_line(
                &mut w,
                "Managed S256/state attempts of an OAuth2 scheme; completion requires its handle, full callback and owning session binding.",
            );
            w.line("fun oauth(scheme: String): OAuthFlow = core.oauth(scheme)");
            if schemes.len() == 1 {
                w.blank();
                kdoc_line(
                    &mut w,
                    &format!("The authorization-code helpers of {}.", schemes[0]),
                );
                w.line(format!(
                    "fun oauth(): OAuthFlow = core.oauth({})",
                    lit(&schemes[0])
                ));
            }
        }
        match &self.plan.client {
            ClientPlan::Resources(list) => {
                for (j, &r) in list.iter().enumerate() {
                    let class = &self.names.resources[r].class;
                    w.blank();
                    kdoc_line(
                        &mut w,
                        &format!(
                            "The {} resource.",
                            self.plan.resources[r].path_words.join(" ")
                        ),
                    );
                    w.line(format!(
                        "val {}: {resources}.{class} = {resources}.{class}(core)",
                        self.names.client_members[j]
                    ));
                }
            }
            ClientPlan::Namespaces(list) => {
                for (k, ns) in list.iter().enumerate() {
                    let class = &self.names.namespace_classes[k];
                    w.blank();
                    kdoc_line(&mut w, &format!("The {} namespace.", ns.name));
                    w.line(format!(
                        "val {}: {resources}.{class} = {resources}.{class}(core)",
                        self.names.client_members[k]
                    ));
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
                layer("KotlinRuntime"),
            ],
            &body,
        );
        let mut f = kotlin::file(self.ir, package, &imports);
        f.line(body.trim_end());
        out.push((format!("{dir}/{client}.kt"), f.finish()));

        out.push((format!("{dir}/Dispatch.kt"), self.dispatch(infos)));
        if has_macros {
            out.push((format!("{dir}/Macros.kt"), self.macros_file()));
        }
        out
    }

    fn dispatch(&self, infos: &[OpInfo]) -> String {
        let client = &self.names.client_class;
        let mut w = Writer::new("    ");
        kdoc(
            &mut w,
            "Runs operations by id and macros by name with an arguments object keyed by the arguments layout keys, through the typed functions; arguments that do not decode go to the runtime as they are, so pre-flight validation reports them. Results carry the decoded wire body.",
        );
        w.line(format!(
            "class Dispatch(private val client: {client}) : {DISPATCH} {{"
        ));
        w.indent();
        w.line("override fun core(): ClientCore = client.core");
        w.blank();
        w.line("override fun operations(): List<OperationDescriptor> = Ops.OPERATIONS");
        w.blank();
        w.line("override fun macros(): List<MacroDescriptor> = Ops.MACROS");
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
                w.block(format!("{} -> {{", lit(id)), "}", |w| {
                    w.line(format!(
                        "val request = Codec.decodeArgs(args) {{ v0 -> {}.fromJson(v0) }}",
                        request(info)
                    ));
                    w.line(format!(
                        "if (request == null) {} else {call}",
                        fallback(info)
                    ));
                });
            }
        };
        w.blank();
        w.block(
            "override fun invoke(operation: String, args: Any?, opts: CallOptions): Result<Any> =",
            "",
            |w| {
                w.block("when (operation) {", "}", |w| {
                    cases(
                        w,
                        &|i| {
                            Some(format!(
                                "Typed.untyped({}.{}(request, opts))",
                                self.accessor(i.resource),
                                i.member
                            ))
                        },
                        &|i| format!("client.core.call({}, args, opts)", self.op_const(i.index)),
                    );
                    w.line(format!(
                        "else -> {DISPATCH}.unknownOperation(client.core, operation)"
                    ));
                });
            },
        );
        w.block(
            "override fun previewOperation(operation: String, args: Any?, opts: CallOptions): Result<PreviewResult> =",
            "",
            |w| {
                w.block("when (operation) {", "}", |w| {
                    cases(
                        w,
                        &|i| {
                            i.preview
                                .as_ref()
                                .map(|m| format!("{}.{m}(request, opts)", self.accessor(i.resource)))
                        },
                        &|i| format!("client.core.preview({}, args, opts)", self.op_const(i.index)),
                    );
                    w.block("else -> {", "}", |w| {
                        w.line(format!("val op = {DISPATCH}.find(Ops.OPERATIONS, operation)"));
                        w.line(format!(
                            "if (op == null) {DISPATCH}.unknownOperation(client.core, operation) else client.core.preview(op, args, opts)"
                        ));
                    });
                });
            },
        );
        w.block(
            "override fun paginate(operation: String, args: Any?, opts: CallOptions): List<Result<Page<Any>>> =",
            "",
            |w| {
                w.block("when (operation) {", "}", |w| {
                    cases(
                        w,
                        &|i| {
                            i.pages.as_ref().map(|(m, _)| {
                                format!("{}.{m}(request, opts).allRaw()", self.accessor(i.resource))
                            })
                        },
                        &|i| format!("client.core.pages({}, args, opts).all()", self.op_const(i.index)),
                    );
                    w.line(format!(
                        "else -> listOf({DISPATCH}.notPaginated<Page<Any>>(client.core, operation))"
                    ));
                });
            },
        );
        w.block(
            "override fun streamEvents(operation: String, args: Any?, opts: CallOptions): EventStream<*> =",
            "",
            |w| {
                w.block("when (operation) {", "}", |w| {
                    cases(
                        w,
                        &|i| {
                            i.stream
                                .as_ref()
                                .map(|(m, _)| format!("{}.{m}(request, opts)", self.accessor(i.resource)))
                        },
                        &|i| format!("client.core.stream({}, args, opts)", self.op_const(i.index)),
                    );
                    w.line(format!(
                        "else -> {DISPATCH}.noStream(client.core, {DISPATCH}.find(Ops.OPERATIONS, operation), operation)"
                    ));
                });
            },
        );
        w.block(
            "override fun runMacro(name: String, input: Any?, opts: CallOptions): Result<Any> {",
            "}",
            |w| {
                w.line(format!("val m = {DISPATCH}.findMacro(Ops.MACROS, name)"));
                w.line(format!(
                    "return if (m == null) {DISPATCH}.unknownMacro(client.core, name) else client.core.runMacro(m, input, opts)"
                ));
            },
        );
        w.blank();
        w.block(
            "override fun previewMacro(name: String, input: Any?, opts: CallOptions): Result<PreviewResult> {",
            "}",
            |w| {
                w.line(format!("val m = {DISPATCH}.findMacro(Ops.MACROS, name)"));
                w.line(format!(
                    "return if (m == null) {DISPATCH}.unknownMacro(client.core, name) else client.core.previewMacro(m, input, opts)"
                ));
            },
        );
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
                layer("Codec"),
            ],
            &body,
        );
        let mut f = kotlin::file(self.ir, &self.opts.package, &imports);
        f.line(body.trim_end());
        f.finish()
    }

    fn macros_file(&self) -> String {
        let mut w = Writer::new("    ");
        kdoc(
            &mut w,
            "The macros of the API: compiled multi-step workflows the runtime runs.",
        );
        w.line("class Macros(private val core: ClientCore) {");
        w.indent();
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
            if k > 0 {
                w.blank();
            }
            kdoc(
                &mut w,
                &format!("Runs the macro {}: {summary}{extends}", m.name()),
            );
            w.line(format!(
                "fun {member}(input: Any?, {OPTIONS}): Result<Any> ="
            ));
            w.line(format!(
                "    core.runMacro(Ops.DOCUMENT.macro({}), input, options)",
                lit(m.name())
            ));
            w.blank();
            kdoc(
                &mut w,
                &format!(
                    "Previews the macro {} without sending anything and issues its confirmation token.",
                    m.name()
                ),
            );
            w.line(format!(
                "fun {preview}(input: Any?, {OPTIONS}): Result<PreviewResult> ="
            ));
            w.line(format!(
                "    core.previewMacro(Ops.DOCUMENT.macro({}), input, options)",
                lit(m.name())
            ));
        }
        w.dedent();
        w.line("}");
        let mut f = kotlin::file(
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
