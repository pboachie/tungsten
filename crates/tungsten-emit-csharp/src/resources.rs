// SPDX-License-Identifier: AGPL-3.0-only
//! The resource classes: one per resource, with a request record and the
//! async methods of each operation (`<Op>Async`, `Preview<Op>Async`,
//! `<Op>PagesAsync`, `<Op>StreamAsync`), the namespace classes of
//! multi-namespace APIs, and `Ops` (the descriptor document and one field
//! per operation and macro).

use tungsten_emit::Writer;
use tungsten_emit::sdk::{ArgSource, MemberKind, OpPlan, SuccessPlan};
use tungsten_ir::{BodyEncoding, Presence, TypeRef};

use crate::Ctx;
use crate::cs::{self, lit};
use crate::types::{Ck, Ct, doc_text, member_code};

/// One member of an arguments record: its property, key and type.
pub struct ArgField {
    pub prop: String,
    pub key: String,
    pub ct: Ct,
    pub presence: Presence,
    pub doc: String,
}

/// What the methods of one operation are made of.
pub struct OpInfo {
    /// The member name (`List`): methods are `ListAsync`, ...
    pub member: String,
    pub preview: Option<String>,
    pub pages: Option<String>,
    pub stream: Option<String>,
    /// The `Ops` field.
    pub field: String,
    /// The request record (nested in the resource class).
    pub request: String,
    /// Every argument is optional: the request may be omitted.
    pub optional_request: bool,
    /// The typed result.
    pub result: Ct,
    /// No success body at all (`Result<NoContent>`).
    pub no_content: bool,
    pub item: Ct,
    pub event: Ct,
    pub fields: Vec<ArgField>,
}

impl Ctx<'_> {
    /// The C# type of an argument.
    fn arg_ct(&self, ty: Option<&TypeRef>, source: &ArgSource<'_>) -> Ct {
        match (source, ty) {
            (
                ArgSource::Body {
                    encoding: BodyEncoding::Multipart,
                    ..
                },
                _,
            ) => Ct::any(),
            (_, Some(ty)) => self.ct(ty, None),
            (
                ArgSource::Body {
                    encoding: BodyEncoding::Text,
                    ..
                },
                None,
            ) => Ct::of(Ck::Str),
            (_, None) => Ct::of(Ck::Binary),
        }
    }

    /// The property names of an arguments record, unique and never the
    /// record's own name.
    pub fn arg_props(words: &[Vec<String>], record: &str) -> Vec<String> {
        let mut taken: Vec<String> = cs::RECORD_MEMBERS.iter().map(|s| s.to_string()).collect();
        taken.push(record.to_string());
        words
            .iter()
            .map(|w| {
                let base = cs::member(w);
                let base = if base == record {
                    format!("{base}Value")
                } else {
                    base
                };
                cs::unique(base, &mut taken)
            })
            .collect()
    }

    /// The methods of every operation, in plan order.
    pub fn op_infos(&self) -> Vec<OpInfo> {
        let mut infos = vec![];
        for (i, p) in self.plan.operations.iter().enumerate() {
            let res = &self.names.resources[p.resource];
            let member_of = |kind: MemberKind| {
                res.members
                    .iter()
                    .find(|m| m.kind == kind)
                    .map(|m| m.name.clone())
            };
            let member = member_of(MemberKind::Operation(i)).unwrap_or_else(|| format!("Op{i}"));
            let request = format!("{member}Request");
            let words: Vec<Vec<String>> = p.arguments.iter().map(|a| a.words.clone()).collect();
            let props = Self::arg_props(&words, &request);
            let fields: Vec<ArgField> = p
                .arguments
                .iter()
                .zip(props)
                .map(|(a, prop)| ArgField {
                    prop,
                    key: a.key.clone(),
                    ct: self.arg_ct(a.ty.as_ref(), &a.source),
                    presence: a.presence,
                    doc: match &a.source {
                        ArgSource::Param { param, .. } => doc_text(param.doc.as_ref()),
                        ArgSource::Field(f) => doc_text(f.doc.as_ref()),
                        ArgSource::Body { media_type, .. } => {
                            format!("The request body ({media_type}).")
                        }
                    },
                })
                .collect();
            let optional_request = fields
                .iter()
                .all(|f| matches!(f.presence, Presence::Optional | Presence::OptionalNullable));
            let (result, no_content) = self.success_ct(p);
            let item = p
                .page
                .as_ref()
                .and_then(|pg| pg.item.as_ref())
                .map_or_else(Ct::any, |t| self.ct(t, None));
            let event = match &p.stream {
                Some(s) if s.typed => self.ct(&s.event, None),
                _ => Ct::any(),
            };
            infos.push(OpInfo {
                member,
                preview: member_of(MemberKind::Preview(i)),
                pages: member_of(MemberKind::Pages(i)),
                stream: member_of(MemberKind::Stream(i)),
                field: self
                    .names
                    .operations
                    .get(i)
                    .map(|o| o.name.clone())
                    .unwrap_or_else(|| format!("Op{i}")),
                request,
                optional_request,
                result,
                no_content,
                item,
                event,
                fields,
            });
        }
        infos
    }

    fn success_ct(&self, p: &OpPlan<'_>) -> (Ct, bool) {
        let ct = match &p.success {
            SuccessPlan::None => return (Ct::any(), true),
            SuccessPlan::Json(ty) => self.ct(ty, None),
            SuccessPlan::Bytes => Ct::of(Ck::Binary),
            SuccessPlan::Text => Ct::of(Ck::Str),
            SuccessPlan::ByStatus(_) | SuccessPlan::Raw(_) => Ct::any(),
        };
        if p.bodiless_success {
            (ct.with_null(), false)
        } else {
            (ct, false)
        }
    }

    /// A record of arguments: properties, a strict-capable `FromJson` and
    /// `ToJson` keyed by arguments key.
    pub fn args_record(&self, w: &mut Writer, name: &str, doc: &str, fields: &[ArgField]) {
        cs::summary_line(w, doc);
        w.line(format!("public sealed record {name}"));
        w.line("{");
        w.indent();
        let codes: Vec<_> = fields
            .iter()
            .map(|f| member_code(&f.ct, f.presence, &f.prop, &f.key))
            .collect();
        for (f, code) in fields.iter().zip(&codes) {
            if f.doc.trim().is_empty() {
                w.line(format!(
                    "/// <summary>The argument <c>{}</c>.</summary>",
                    cs::xml(&f.key)
                ));
            } else {
                cs::summary(w, &f.doc);
            }
            let required = if code.required { "required " } else { "" };
            w.line(format!(
                "public {required}{} {} {{ get; init; }}",
                code.ty, f.prop
            ));
            w.blank();
        }
        cs::summary_line(
            w,
            "Decodes an arguments object; strict decoding refuses keys the operation does not take.",
        );
        w.line(format!(
            "public static {name} FromJson(JsonNode? node, Decoder d)"
        ));
        w.line("{");
        w.indent();
        w.line("var o = d.Object(node);");
        let keys: Vec<String> = fields.iter().map(|f| lit(&f.key)).collect();
        if keys.is_empty() {
            w.line("d.Known(o);");
        } else {
            w.line(format!("d.Known(o, {});", keys.join(", ")));
        }
        w.line(format!("return new {name}"));
        w.line("{");
        w.indent();
        for (f, code) in fields.iter().zip(&codes) {
            w.line(format!("{} = {},", f.prop, code.dec));
        }
        w.dedent();
        w.line("};");
        w.dedent();
        w.line("}");
        w.blank();
        cs::summary_line(w, "The arguments object, keyed by arguments key.");
        w.line("public JsonObject ToJson()");
        w.line("{");
        w.indent();
        w.line("var o = new JsonObject();");
        for code in &codes {
            w.line(&code.enc);
        }
        w.line("return o;");
        w.dedent();
        w.line("}");
        w.dedent();
        w.line("}");
    }

    /// Every resource and namespace class file.
    pub fn resources(&self, infos: &[OpInfo]) -> Vec<(String, String)> {
        let mut out = vec![];
        for (r, res) in self.plan.resources.iter().enumerate() {
            let names = &self.names.resources[r];
            let class = &names.class;
            let mut w = self.file_writer();
            Self::root_usings(&mut w);
            w.line(format!("namespace {};", self.opts.namespace));
            w.blank();
            let doc = doc_text(res.resource.doc.as_ref());
            if doc.is_empty() {
                cs::summary_line(
                    &mut w,
                    &format!("The {} resource.", res.path_words.join(" ")),
                );
            } else {
                cs::summary(&mut w, &doc);
            }
            w.line(format!("public sealed class {class}"));
            w.line("{");
            w.indent();
            let has_ops = names
                .members
                .iter()
                .any(|m| matches!(m.kind, MemberKind::Operation(_)));
            if has_ops {
                w.line("private readonly ClientCore core;");
                w.blank();
            }
            w.line(format!("internal {class}(ClientCore core)"));
            w.line("{");
            w.indent();
            if has_ops {
                w.line("this.core = core;");
            }
            for m in &names.members {
                if let MemberKind::Child(c) = m.kind {
                    w.line(format!(
                        "{} = new {}(core);",
                        m.name, self.names.resources[c].class
                    ));
                }
            }
            w.dedent();
            w.line("}");
            w.blank();
            for m in &names.members {
                if let MemberKind::Child(c) = m.kind {
                    cs::summary_line(
                        &mut w,
                        &format!(
                            "The {} resource.",
                            self.plan.resources[c].path_words.join(" ")
                        ),
                    );
                    w.line(format!(
                        "public {} {} {{ get; }}",
                        self.names.resources[c].class, m.name
                    ));
                    w.blank();
                }
            }
            for m in &names.members {
                if let MemberKind::Operation(i) = m.kind {
                    self.operation_methods(&mut w, &self.plan.operations[i], &infos[i]);
                }
            }
            w.dedent();
            w.line("}");
            out.push((format!("Resources/{class}.cs"), w.finish()));
        }
        out
    }

    fn operation_methods(&self, w: &mut Writer, p: &OpPlan<'_>, info: &OpInfo) {
        let op = p.op;
        let summary = tungsten_emit::sdk::descriptors::op_summary(op);
        self.args_record(
            w,
            &info.request,
            &format!("The arguments of {} ({}).", op.id.0, info.member),
            &info.fields,
        );
        w.blank();
        let request_param = if info.optional_request {
            format!("{}? request = null", info.request)
        } else {
            format!("{} request", info.request)
        };
        let args = if info.optional_request {
            format!("(request ?? new {}()).ToJson()", info.request)
        } else {
            "request.ToJson()".to_string()
        };
        let tail = "CallOptions? options = null, CancellationToken cancellationToken = default";
        let doc = if summary.is_empty() {
            format!("Calls {}.", op.id.0)
        } else {
            summary.clone()
        };
        let field = format!("Ops.{}", info.field);
        cs::summary_line(w, &doc);
        if info.no_content {
            w.line(format!(
                "public Task<Result<NoContent>> {}Async({request_param}, {tail}) =>",
                info.member
            ));
            w.line(format!(
                "    Typed.CallNoContentAsync(core, {field}, {args}, options, cancellationToken);"
            ));
        } else {
            w.line(format!(
                "public Task<Result<{}>> {}Async({request_param}, {tail}) =>",
                info.result.ty(),
                info.member
            ));
            w.line(format!(
                "    Typed.CallAsync<{}>(core, {field}, {args}, {}, options, cancellationToken);",
                info.result.ty(),
                info.result.dec(0)
            ));
        }
        w.blank();
        if let Some(preview) = &info.preview {
            cs::summary_line(
                w,
                &format!(
                    "Renders the request of {} without sending it, with a confirmation token for these exact arguments.",
                    op.id.0
                ),
            );
            w.line(format!(
                "public Task<Result<PreviewResult>> {preview}Async({request_param}, {tail}) =>"
            ));
            w.line(format!(
                "    core.PreviewAsync({field}, {args}, options, cancellationToken);"
            ));
            w.blank();
        }
        if let Some(pages) = &info.pages {
            cs::summary_line(
                w,
                &format!("The pages of {}; a failure ends the sequence.", op.id.0),
            );
            w.line(format!(
                "public IAsyncEnumerable<Result<Page<{}>>> {pages}Async({request_param}, {tail}) =>",
                info.item.ty()
            ));
            w.line(format!(
                "    Typed.PagesAsync<{}>(core, {field}, {args}, {}, options, cancellationToken);",
                info.item.ty(),
                info.item.dec(0)
            ));
            w.blank();
        }
        if let Some(stream) = &info.stream {
            cs::summary_line(
                w,
                &format!(
                    "The server-sent events of {}; nothing is sent until the stream is read.",
                    op.id.0
                ),
            );
            w.line(format!(
                "public EventStream<{}> {stream}Async({request_param}, CallOptions? options = null) =>",
                info.event.ty()
            ));
            w.line(format!(
                "    Typed.Stream<{}>(core, {field}, {args}, {}, options);",
                info.event.ty(),
                info.event.dec(0)
            ));
            w.blank();
        }
    }

    /// The namespace classes of a multi-namespace API.
    pub fn namespace_classes(&self) -> Vec<(String, String)> {
        let mut out = vec![];
        let tungsten_emit::sdk::ClientPlan::Namespaces(namespaces) = &self.plan.client else {
            return out;
        };
        for (k, ns) in namespaces.iter().enumerate() {
            let class = &self.names.namespace_classes[k];
            let members = &self.names.namespace_members[k];
            let mut w = self.file_writer();
            Self::root_usings(&mut w);
            w.line(format!("namespace {};", self.opts.namespace));
            w.blank();
            cs::summary_line(&mut w, &format!("The {} namespace of the API.", ns.name));
            w.line(format!("public sealed class {class}"));
            w.line("{");
            w.indent();
            w.line(format!("internal {class}(ClientCore core)"));
            w.line("{");
            w.indent();
            for (m, r) in members.iter().zip(&ns.resources) {
                w.line(format!(
                    "{m} = new {}(core);",
                    self.names.resources[*r].class
                ));
            }
            w.dedent();
            w.line("}");
            for (m, r) in members.iter().zip(&ns.resources) {
                w.blank();
                cs::summary_line(
                    &mut w,
                    &format!(
                        "The {} resource.",
                        self.plan.resources[*r].path_words.join(" ")
                    ),
                );
                w.line(format!(
                    "public {} {m} {{ get; }}",
                    self.names.resources[*r].class
                ));
            }
            w.dedent();
            w.line("}");
            out.push((format!("Resources/{class}.cs"), w.finish()));
        }
        out
    }

    /// `Ops.cs`: the descriptor document and one field per operation and
    /// macro.
    pub fn ops_file(&self, infos: &[OpInfo]) -> String {
        let mut w = self.file_writer();
        w.line("using Tungsten.Runtime;");
        w.blank();
        w.line(format!("namespace {};", self.opts.namespace));
        w.blank();
        cs::summary_line(
            &mut w,
            "The SDK descriptor document (the embedded resource tungsten-descriptors.json) and its operations and macros.",
        );
        w.line("internal static class Ops");
        w.line("{");
        w.indent();
        w.line("internal static readonly SdkDescriptors Document = SdkDescriptors.Load(typeof(Ops).Assembly.GetManifestResourceStream(\"tungsten-descriptors.json\"));");
        for (p, info) in self.plan.operations.iter().zip(infos) {
            w.blank();
            w.line(format!(
                "internal static readonly OperationDescriptor {} = Document.Operation({});",
                info.field,
                lit(&p.op.id.0)
            ));
        }
        for (k, m) in self.plan.macros.iter().enumerate() {
            w.blank();
            w.line(format!(
                "internal static readonly MacroDescriptor Macro{} = Document.Macro({});",
                self.names.macros[k].member,
                lit(m.name())
            ));
        }
        w.dedent();
        w.line("}");
        w.finish()
    }

    /// The usings of the files in the root namespace.
    pub fn root_usings(w: &mut Writer) {
        w.line("using System.Collections.Generic;");
        w.line("using System.Text.Json.Nodes;");
        w.line("using System.Threading;");
        w.line("using System.Threading.Tasks;");
        w.line("using Tungsten.Runtime;");
        w.blank();
    }
}
