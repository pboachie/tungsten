// SPDX-License-Identifier: AGPL-3.0-only
//! The resource classes (`Resources/<Class>.fs`): one per resource, with a
//! request record per operation and the task-based methods of each
//! operation (`<Op>Async`, `Preview<Op>Async`, `<Op>PagesAsync`,
//! `<Op>StreamAsync`), the namespace classes of multi-namespace APIs, and
//! `Ops.fs` (the descriptor document and one value per operation and
//! macro).
//!
//! Every public method returns F# results (`Result<'T, Diagnostic>`); each
//! has an internal twin (`call<Member>`, camelCase so it never meets a
//! generated PascalCase name) that returns the .NET runtime's `Result<T>`
//! with the response metadata, which `Dispatch.fs` calls. Request records
//! live in the root namespace before their class (F# has no nested types
//! in classes), named `<Class><Member>Request`; their `FromJson` decodes
//! strictly (unknown keys refused) and their `ToJson` writes the
//! TypeScript arguments layout, keyed by arguments key.

use tungsten_emit::Writer;
use tungsten_emit::sdk::{ArgSource, ClientPlan, MemberKind, OpPlan, SuccessPlan};
use tungsten_ir::{BodyEncoding, Doc, Presence, TypeRef};

use crate::Ctx;
use crate::fs::{self, lit};
use crate::typeref::{self, Fk, Ft};

/// The text of a documentation entry (summary and description).
pub fn doc_text(doc: Option<&Doc>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    match (&doc.summary, &doc.description) {
        (Some(s), Some(d)) if d.trim().starts_with(s.trim()) => d.trim().to_string(),
        (Some(s), Some(d)) => format!("{}\n\n{}", s.trim(), d.trim()),
        (Some(s), None) => s.trim().to_string(),
        (None, Some(d)) => d.trim().to_string(),
        (None, None) => String::new(),
    }
}

/// The decoder (`Codec.Dec<'T>`) of a value of type `ft`.
pub fn dec(ft: &Ft) -> String {
    if ft.nullable && ft.kind != Fk::Any {
        return format!("(Codec.nullable {})", dec(&ft.non_null()));
    }
    match &ft.kind {
        Fk::Str => "Codec.string".into(),
        Fk::Bool => "Codec.bool".into(),
        Fk::I32 => "Codec.int32".into(),
        Fk::I64 => "Codec.int64".into(),
        Fk::F64 => "Codec.float".into(),
        Fk::Bytes => "Codec.bytes".into(),
        Fk::Binary => "Codec.binary".into(),
        Fk::Any => "Codec.any".into(),
        Fk::Named(name) => format!("(fun n d -> {name}.FromJson(n, d))"),
        Fk::List(item) => format!("(Codec.list {})", dec(item)),
        Fk::Map(value) => format!("(Codec.map {})", dec(value)),
    }
}

/// The encoder (`Codec.Enc<'T>`) of a value of type `ft`.
pub fn enc(ft: &Ft) -> String {
    if ft.nullable && ft.kind != Fk::Any {
        return format!("(Codec.encNullable {})", enc(&ft.non_null()));
    }
    match &ft.kind {
        Fk::Str => "Codec.encString".into(),
        Fk::Bool => "Codec.encBool".into(),
        Fk::I32 => "Codec.encInt32".into(),
        Fk::I64 => "Codec.encInt64".into(),
        Fk::F64 => "Codec.encFloat".into(),
        Fk::Bytes => "Codec.encBytes".into(),
        Fk::Binary => "Codec.encBinary".into(),
        Fk::Any => "Codec.encAny".into(),
        Fk::Named(name) => format!("(fun (v: {name}) -> v.ToJson())"),
        Fk::List(item) => format!("(Codec.encList {})", enc(item)),
        Fk::Map(value) => format!("(Codec.encMap {})", enc(value)),
    }
}

/// How a member of type `ft` with this presence is declared, decoded from
/// the object `o` (decoder `d`) and encoded from `value`, following
/// [`typeref::field_ty`].
#[derive(Debug)]
pub struct MemberCode {
    pub ty: String,
    /// The expression decoding the member.
    pub dec: String,
    /// The statement writing the member of `value` into `o`.
    pub enc: String,
    /// The member's value when it is not given (optional members only).
    pub absent: Option<&'static str>,
}

/// The [`MemberCode`] of a member keyed `key` whose value expression is
/// `value`.
pub fn member_code(ft: &Ft, presence: Presence, key: &str, value: &str) -> MemberCode {
    let k = lit(key);
    let optional = matches!(presence, Presence::Optional | Presence::OptionalNullable);
    let null_allowed = ft.nullable
        || matches!(
            presence,
            Presence::RequiredNullable | Presence::OptionalNullable
        );
    let inner = ft.non_null();
    let ty = typeref::field_ty(ft, presence);
    match (optional, null_allowed) {
        (false, false) => MemberCode {
            ty,
            dec: format!("Codec.required d o {k} {}", dec(&inner)),
            enc: format!("Codec.put o {k} ({} {value})", enc(&inner)),
            absent: None,
        },
        (false, true) if inner.kind == Fk::Any => MemberCode {
            ty,
            dec: format!("Codec.required d o {k} Codec.any"),
            enc: format!("Codec.put o {k} (Codec.encAny {value})"),
            absent: None,
        },
        (false, true) => MemberCode {
            ty,
            dec: format!("Codec.requiredNullable d o {k} {}", dec(&inner)),
            enc: format!(
                "Codec.put o {k} ((Codec.encNullable {}) {value})",
                enc(&inner)
            ),
            absent: None,
        },
        (true, false) => MemberCode {
            ty,
            dec: format!("Codec.optional d o {k} {}", dec(&inner)),
            enc: format!("Codec.putOption o {k} {} {value}", enc(&inner)),
            absent: Some("None"),
        },
        (true, true) => MemberCode {
            ty,
            dec: format!("Codec.patch d o {k} {}", dec(&inner)),
            enc: format!("Codec.putPatch o {k} {} {value}", enc(&inner)),
            absent: Some("Patch.Absent"),
        },
    }
}

/// One member of an arguments record: its field, key and type.
#[derive(Debug)]
pub struct ArgField {
    pub prop: String,
    pub key: String,
    pub ft: Ft,
    pub presence: Presence,
    pub doc: String,
}

/// The request (or macro input) of a method: its record and how the
/// method takes it.
#[derive(Debug)]
pub struct Request {
    /// The record; `None` when there are no arguments (F# has no empty
    /// records): the method then takes no request.
    pub record: Option<String>,
    /// Every argument is optional: the method takes `?request` and
    /// defaults it to the record's `Empty`.
    pub optional: bool,
    pub fields: Vec<ArgField>,
}

impl Request {
    /// The request parameter of a public method, with its trailing comma
    /// and space; empty without a record.
    pub fn param(&self, name: &str) -> String {
        match &self.record {
            None => String::new(),
            Some(r) if self.optional => format!("?{name}: {r}, "),
            Some(r) => format!("{name}: {r}, "),
        }
    }

    /// The request parameter of an internal method (always given).
    pub fn internal_param(&self, name: &str) -> String {
        match &self.record {
            None => String::new(),
            Some(r) => format!("{name}: {r}, "),
        }
    }

    /// The request argument a public method passes on to its internal
    /// twin, with its trailing comma and space.
    pub fn pass(&self, name: &str) -> String {
        match &self.record {
            None => String::new(),
            Some(r) if self.optional => format!("defaultArg {name} {r}.Empty, "),
            Some(_) => format!("{name}, "),
        }
    }

    /// The argument an internal call passes for a decoded request, with
    /// its trailing comma and space.
    pub fn passed(&self, name: &str) -> String {
        match &self.record {
            None => String::new(),
            Some(_) => format!("{name}, "),
        }
    }

    /// The arguments object of the request `name`.
    pub fn args(&self, name: &str) -> String {
        match &self.record {
            None => "(JsonObject())".into(),
            Some(_) => format!("({name}.ToJson())"),
        }
    }

    /// The strict decoder of the request as a `Func` for
    /// `Typed.TryDecodeArgs`.
    pub fn decoder(&self) -> String {
        match &self.record {
            None => "System.Func<JsonNode, Decoder, unit>(fun n d -> Codec.known d (Codec.object d n) [||])".into(),
            Some(r) => format!("System.Func<JsonNode, Decoder, {r}>(fun n d -> {r}.FromJson(n, d))"),
        }
    }

    /// The type a strict decode yields.
    pub fn decoded_ty(&self) -> String {
        self.record.clone().unwrap_or_else(|| "unit".into())
    }
}

/// What the methods of one operation are made of.
#[derive(Debug)]
pub struct OpInfo {
    /// The member name (`List`): methods are `ListAsync`, ...
    pub member: String,
    pub preview: Option<String>,
    pub pages: Option<String>,
    pub stream: Option<String>,
    /// The `Ops` value.
    pub field: String,
    pub request: Request,
    /// The typed result.
    pub result: Ft,
    /// No success body at all (`Result<unit, Diagnostic>`).
    pub no_content: bool,
    pub item: Ft,
    pub event: Ft,
}

/// The internal twin of a public member.
pub fn internal(member: &str) -> String {
    format!("call{member}")
}

impl Ctx<'_> {
    /// The F# type of an argument.
    fn arg_ft(&self, ty: Option<&TypeRef>, source: &ArgSource<'_>) -> Ft {
        match (source, ty) {
            (
                ArgSource::Body {
                    encoding: BodyEncoding::Multipart,
                    ..
                },
                _,
            ) => Ft::any(),
            (_, Some(ty)) => self.ft(ty, None),
            (
                ArgSource::Body {
                    encoding: BodyEncoding::Text,
                    ..
                },
                None,
            ) => Ft::of(Fk::Str),
            (_, None) => Ft::of(Fk::Binary),
        }
    }

    /// The field names of an arguments record, unique and never a member
    /// every record has.
    pub fn arg_props(words: &[Vec<String>]) -> Vec<String> {
        let mut taken: Vec<String> = fs::RECORD_MEMBERS.iter().map(|s| s.to_string()).collect();
        words
            .iter()
            .map(|w| {
                let base = fs::member(w);
                let base = if base.is_empty() {
                    "Value".to_string()
                } else {
                    fs::case_name(base)
                };
                fs::unique(base, &mut taken)
            })
            .collect()
    }

    /// The type names of the root namespace that generated classes take,
    /// plus the runtime's: request and input records must avoid them.
    pub fn root_taken(&self) -> Vec<String> {
        let mut taken: Vec<String> = typeref::TAKEN_TYPES.iter().map(|s| s.to_string()).collect();
        taken.extend(self.names.resources.iter().map(|r| r.class.clone()));
        taken.extend(self.names.namespace_classes.iter().cloned());
        taken.push(self.names.client_class.clone());
        taken
    }

    /// The methods of every operation, in plan order.
    pub fn op_infos(&self) -> Vec<OpInfo> {
        let mut taken = self.root_taken();
        taken.push(self.macros_class());
        taken.push(self.dispatch_class());
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
            let words: Vec<Vec<String>> = p.arguments.iter().map(|a| a.words.clone()).collect();
            let props = Self::arg_props(&words);
            let fields: Vec<ArgField> = p
                .arguments
                .iter()
                .zip(props)
                .map(|(a, prop)| ArgField {
                    prop,
                    key: a.key.clone(),
                    ft: self.arg_ft(a.ty.as_ref(), &a.source),
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
            let request =
                Self::request(fields, format!("{}{member}Request", res.class), &mut taken);
            let (result, no_content) = self.success_ft(p);
            let item = p
                .page
                .as_ref()
                .and_then(|pg| pg.item.as_ref())
                .map_or_else(Ft::any, |t| self.ft(t, None));
            let event = match &p.stream {
                Some(s) if s.typed => self.ft(&s.event, None),
                _ => Ft::any(),
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
                result,
                no_content,
                item,
                event,
            });
        }
        infos
    }

    /// A request of these fields, its record named `name` (made unique
    /// against `taken`) unless there are none.
    pub fn request(fields: Vec<ArgField>, name: String, taken: &mut Vec<String>) -> Request {
        let optional = fields
            .iter()
            .all(|f| matches!(f.presence, Presence::Optional | Presence::OptionalNullable));
        let record = (!fields.is_empty()).then(|| fs::unique(name, taken));
        Request {
            record,
            optional,
            fields,
        }
    }

    fn success_ft(&self, p: &OpPlan<'_>) -> (Ft, bool) {
        let ft = match &p.success {
            SuccessPlan::None => return (Ft::any(), true),
            SuccessPlan::Json(ty) => self.ft(ty, None),
            SuccessPlan::Bytes => Ft::of(Fk::Binary),
            SuccessPlan::Text => Ft::of(Fk::Str),
            SuccessPlan::ByStatus(_) | SuccessPlan::Raw(_) => Ft::any(),
        };
        if p.bodiless_success {
            (ft.with_null(), false)
        } else {
            (ft, false)
        }
    }

    /// A record of arguments: fields, a strict-capable `FromJson`, `ToJson`
    /// keyed by arguments key and, when every field is optional, `Empty`.
    pub fn args_record(w: &mut Writer, doc: &str, request: &Request) {
        let Some(name) = &request.record else {
            return;
        };
        let fields = &request.fields;
        let codes: Vec<MemberCode> = fields
            .iter()
            .map(|f| {
                member_code(
                    &f.ft,
                    f.presence,
                    &f.key,
                    &format!("this.{}", fs::ident(&f.prop)),
                )
            })
            .collect();
        fs::doc_line(w, doc);
        w.line(format!("type {name} ="));
        w.indent();
        // `{` alone on its line, then each field after its comment.
        w.line("{");
        for (f, code) in fields.iter().zip(&codes) {
            let doc = if f.doc.trim().is_empty() {
                format!("The argument `{}`.", f.key)
            } else {
                f.doc.clone()
            };
            let doc = doc.split_whitespace().collect::<Vec<_>>().join(" ");
            w.line(format!("  /// {}", fs::xml(&doc)));
            w.line(format!("  {}: {}", fs::ident(&f.prop), code.ty));
        }
        w.line("}");
        w.blank();
        fs::doc_line(
            w,
            "Decodes an arguments object; strict decoding refuses keys the operation does not take.",
        );
        w.line(format!(
            "static member FromJson(node: JsonNode, d: Decoder) : {name} ="
        ));
        w.indent();
        w.line("let o = Codec.object d node");
        let keys: Vec<String> = fields.iter().map(|f| f.key.clone()).collect();
        w.line(format!("Codec.known d o {}", fs::string_array(&keys)));
        for (k, (f, code)) in fields.iter().zip(&codes).enumerate() {
            let open = if k == 0 { "{ " } else { "  " };
            let close = if k + 1 == fields.len() { " }" } else { "" };
            w.line(format!(
                "{open}{} = {}{close}",
                fs::ident(&f.prop),
                code.dec
            ));
        }
        w.dedent();
        w.blank();
        fs::doc_line(w, "The arguments object, keyed by arguments key.");
        w.line("member this.ToJson() : JsonObject =");
        w.indent();
        w.line("let o = JsonObject()");
        for code in &codes {
            w.line(&code.enc);
        }
        w.line("o");
        w.dedent();
        if request.optional {
            w.blank();
            fs::doc_line(w, "No arguments: every member absent.");
            w.line(format!("static member Empty: {name} ="));
            w.indent();
            for (k, (f, code)) in fields.iter().zip(&codes).enumerate() {
                let open = if k == 0 { "{ " } else { "  " };
                let close = if k + 1 == fields.len() { " }" } else { "" };
                w.line(format!(
                    "{open}{} = {}{close}",
                    fs::ident(&f.prop),
                    code.absent.unwrap_or("None")
                ));
            }
            w.dedent();
        }
        w.dedent();
    }

    /// The opens of the files in the root namespace.
    pub fn root_opens(w: &mut Writer) {
        w.line("open System.Collections.Generic");
        w.line("open System.Text.Json.Nodes");
        w.line("open System.Threading");
        w.line("open System.Threading.Tasks");
        w.line("open Tungsten.Runtime");
        // After Tungsten.Runtime: `Patch` is the F# layer's union.
        w.line("open Tungsten.Runtime.FSharp");
        w.blank();
    }

    /// Every resource and namespace class file, and `Ops.fs`.
    pub fn resource_files(&self) -> Vec<(String, String)> {
        let infos = self.op_infos();
        let mut out = self.resources(&infos);
        out.extend(self.namespace_classes());
        out.push(("Ops.fs".into(), self.ops_file(&infos)));
        out
    }

    /// Every resource class file.
    pub fn resources(&self, infos: &[OpInfo]) -> Vec<(String, String)> {
        let mut out = vec![];
        for (r, res) in self.plan.resources.iter().enumerate() {
            let names = &self.names.resources[r];
            let class = &names.class;
            let mut w = self.file_writer();
            w.line(format!("namespace {}", self.opts.namespace));
            w.blank();
            Self::root_opens(&mut w);
            for m in &names.members {
                if let MemberKind::Operation(i) = m.kind {
                    let info = &infos[i];
                    Self::args_record(
                        &mut w,
                        &format!(
                            "The arguments of {} ({}.{}Async).",
                            self.plan.operations[i].op.id.0, class, info.member
                        ),
                        &info.request,
                    );
                    if info.request.record.is_some() {
                        w.blank();
                    }
                }
            }
            let doc = doc_text(res.resource.doc.as_ref());
            if doc.is_empty() {
                fs::doc_line(
                    &mut w,
                    &format!("The {} resource.", res.path_words.join(" ")),
                );
            } else {
                fs::doc(&mut w, &doc);
            }
            w.line("[<Sealed>]");
            w.line(format!("type {class} internal (core: ClientCore) ="));
            w.indent();
            let children: Vec<(String, usize)> = names
                .members
                .iter()
                .filter_map(|m| match m.kind {
                    MemberKind::Child(c) => Some((m.name.clone(), c)),
                    _ => None,
                })
                .collect();
            let has_ops = names
                .members
                .iter()
                .any(|m| matches!(m.kind, MemberKind::Operation(_)));
            for (k, (_, c)) in children.iter().enumerate() {
                w.line(format!(
                    "let child{k} = {}(core)",
                    self.names.resources[*c].class
                ));
            }
            if children.is_empty() && !has_ops {
                w.line("do ignore core");
            }
            for (k, (name, c)) in children.iter().enumerate() {
                w.blank();
                fs::doc_line(
                    &mut w,
                    &format!(
                        "The {} resource.",
                        self.plan.resources[*c].path_words.join(" ")
                    ),
                );
                w.line(format!(
                    "member _.{name}: {} = child{k}",
                    self.names.resources[*c].class
                ));
            }
            for m in &names.members {
                if let MemberKind::Operation(i) = m.kind {
                    self.operation_methods(&mut w, &self.plan.operations[i], &infos[i]);
                }
            }
            w.dedent();
            out.push((Ctx::class_file(class), w.finish()));
        }
        out
    }

    fn operation_methods(&self, w: &mut Writer, p: &OpPlan<'_>, info: &OpInfo) {
        let op = p.op;
        let summary = tungsten_emit::sdk::descriptors::op_summary(op);
        let req = &info.request;
        let param = req.param("request");
        let internal_param = req.internal_param("request");
        let pass = req.pass("request");
        let args = req.args("request");
        let tail = "?options: CallOptions, ?cancellationToken: CancellationToken";
        let internal_tail =
            "options: CallOptions option, cancellationToken: CancellationToken option";
        let field = format!("Ops.{}", info.field);
        let doc = if summary.is_empty() {
            format!("Calls {}.", op.id.0)
        } else {
            summary.clone()
        };
        let call = internal(&info.member);
        w.blank();
        if info.no_content {
            w.line(format!(
                "member internal _.{call}({internal_param}{internal_tail}) : Task<Result<NoContent>> ="
            ));
            w.line(format!(
                "    Call.noContent core {field} {args} options cancellationToken"
            ));
            w.blank();
            fs::doc_line(w, &doc);
            w.line(format!(
                "member this.{}Async({param}{tail}) : Task<Result<unit, Diagnostic>> =",
                info.member
            ));
            w.line(format!(
                "    this.{call}({pass}options, cancellationToken) |> TaskResult.map ignore"
            ));
        } else {
            let ty = info.result.ty();
            w.line(format!(
                "member internal _.{call}({internal_param}{internal_tail}) : Task<Result<{ty}>> ="
            ));
            w.line(format!(
                "    Call.typed core {field} {args} {} options cancellationToken",
                dec(&info.result)
            ));
            w.blank();
            fs::doc_line(w, &doc);
            w.line(format!(
                "member this.{}Async({param}{tail}) : Task<Result<{ty}, Diagnostic>> =",
                info.member
            ));
            w.line(format!(
                "    this.{call}({pass}options, cancellationToken) |> TaskResult.toResult"
            ));
        }
        if let Some(preview) = &info.preview {
            let call = internal(preview);
            w.blank();
            w.line(format!(
                "member internal _.{call}({internal_param}{internal_tail}) : Task<Result<PreviewResult>> ="
            ));
            w.line(format!(
                "    Call.preview core {field} {args} options cancellationToken"
            ));
            w.blank();
            fs::doc_line(
                w,
                &format!(
                    "Renders the request of {} without sending it, with a confirmation token for these exact arguments.",
                    op.id.0
                ),
            );
            w.line(format!(
                "member this.{preview}Async({param}{tail}) : Task<Result<PreviewResult, Diagnostic>> ="
            ));
            w.line(format!(
                "    this.{call}({pass}options, cancellationToken) |> TaskResult.toResult"
            ));
        }
        if let Some(pages) = &info.pages {
            let call = internal(pages);
            let ty = info.item.ty();
            w.blank();
            w.line(format!(
                "member internal _.{call}({internal_param}{internal_tail}) : IAsyncEnumerable<Result<Page<{ty}>>> ="
            ));
            w.line(format!(
                "    Call.pages core {field} {args} {} options cancellationToken",
                dec(&info.item)
            ));
            w.blank();
            fs::doc_line(
                w,
                &format!("The pages of {}; a failure ends the sequence.", op.id.0),
            );
            w.line(format!(
                "member this.{pages}Async({param}{tail}) : IAsyncEnumerable<Result<Page<{ty}>, Diagnostic>> ="
            ));
            w.line(format!(
                "    this.{call}({pass}options, cancellationToken) |> Pages.results"
            ));
        }
        if let Some(stream) = &info.stream {
            let call = internal(stream);
            let ty = info.event.ty();
            w.blank();
            w.line(format!(
                "member internal _.{call}({internal_param}options: CallOptions option) : EventStream<{ty}> ="
            ));
            w.line(format!(
                "    Call.stream core {field} {args} {} options",
                dec(&info.event)
            ));
            w.blank();
            fs::doc_line(
                w,
                &format!(
                    "The server-sent events of {}; nothing is sent until the stream is read.",
                    op.id.0
                ),
            );
            w.line(format!(
                "member this.{stream}Async({param}?options: CallOptions) : EventStream<{ty}> ="
            ));
            w.line(format!("    this.{call}({pass}options)"));
        }
    }

    /// The namespace classes of a multi-namespace API.
    pub fn namespace_classes(&self) -> Vec<(String, String)> {
        let mut out = vec![];
        let ClientPlan::Namespaces(namespaces) = &self.plan.client else {
            return out;
        };
        for (k, ns) in namespaces.iter().enumerate() {
            let class = &self.names.namespace_classes[k];
            let members = &self.names.namespace_members[k];
            let mut w = self.file_writer();
            w.line(format!("namespace {}", self.opts.namespace));
            w.blank();
            w.line("open Tungsten.Runtime");
            w.blank();
            fs::doc_line(&mut w, &format!("The {} namespace of the API.", ns.name));
            w.line("[<Sealed>]");
            w.line(format!("type {class} internal (core: ClientCore) ="));
            w.indent();
            for (j, r) in ns.resources.iter().enumerate() {
                w.line(format!(
                    "let resource{j} = {}(core)",
                    self.names.resources[*r].class
                ));
            }
            if ns.resources.is_empty() {
                w.line("do ignore core");
            }
            for (j, (m, r)) in members.iter().zip(&ns.resources).enumerate() {
                w.blank();
                fs::doc_line(
                    &mut w,
                    &format!(
                        "The {} resource.",
                        self.plan.resources[*r].path_words.join(" ")
                    ),
                );
                w.line(format!(
                    "member _.{m}: {} = resource{j}",
                    self.names.resources[*r].class
                ));
            }
            w.dedent();
            out.push((Ctx::class_file(class), w.finish()));
        }
        out
    }

    /// `Ops.fs`: the descriptor document and one value per operation and
    /// macro.
    pub fn ops_file(&self, infos: &[OpInfo]) -> String {
        let mut w = self.file_writer();
        w.line(format!("namespace {}", self.opts.namespace));
        w.blank();
        w.line("open Tungsten.Runtime");
        w.blank();
        fs::doc_line(
            &mut w,
            "The SDK descriptor document (the embedded resource tungsten-descriptors.json) and its operations and macros.",
        );
        w.line("module internal Ops =");
        w.indent();
        fs::doc_line(&mut w, "The descriptor document.");
        w.line("let Document: SdkDescriptors =");
        w.line("    SdkDescriptors.Load(System.Reflection.Assembly.GetExecutingAssembly().GetManifestResourceStream(\"tungsten-descriptors.json\"))");
        for (p, info) in self.plan.operations.iter().zip(infos) {
            w.blank();
            fs::doc_line(&mut w, &format!("The operation {}.", p.op.id.0));
            w.line(format!(
                "let {}: OperationDescriptor = Document.Operation({})",
                info.field,
                lit(&p.op.id.0)
            ));
        }
        for (k, m) in self.plan.macros.iter().enumerate() {
            w.blank();
            fs::doc_line(&mut w, &format!("The macro {}.", m.name()));
            w.line(format!(
                "let Macro{}: MacroDescriptor = Document.Macro({})",
                self.names.macros[k].member,
                lit(m.name())
            ));
        }
        w.dedent();
        w.finish()
    }
}
