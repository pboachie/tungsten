// SPDX-License-Identifier: AGPL-3.0-only
//! The model files (`Models/...`): one file per model group of
//! [`Ctx::model_groups`], in compile order, a recursive group declared as
//! one `type ... and ...` set.
//!
//! - Records (and merged `allOf`s) are F# records with PascalCase fields
//!   whose types follow presence ([`typeref::field_ty`]): `'T`, `'T option`
//!   or the F# layer's `Patch<'T>`. Open records keep the members they do
//!   not list in `AdditionalProperties: JsonObject option`, records with
//!   typed additional members in `AdditionalProperties: Map<string, 'T>
//!   option`; closed records refuse unknown members when decoding strictly.
//!   A closed record without fields is a one-case union (F# records need a
//!   field).
//! - String and integer enums are unions of their known values plus
//!   `Unknown of string` (or `int64`), which keeps a value the SDK does not
//!   know when decoding leniently; strict decoding refuses it. `Value` is
//!   the wire value and `Values` the known cases.
//! - Unions are unions with one case per variant: decoded by tag, by a
//!   literal or JSON type, else by the first variant that decodes in plan
//!   order (TG1085 is reported for a discriminator with a tag-less
//!   variant).
//!
//! Every model has `static member FromJson(node, d)` (strict or lenient by
//! the runtime `Decoder`), `static member FromJson(node)` (lenient) and
//! `member ToJson() : JsonNode`, written over the runtime JSON tree with the
//! F# layer's `Codec` helpers, without reflection.

use tungsten_emit::Writer;
use tungsten_emit::sdk::{AllOfPlan, EnumPlan, LiteralKey, TypeKind, UnionPlan};
use tungsten_ir::{Additional, Doc, Field, NamedType, Presence, Shape, TypeId};

use crate::Ctx;
use crate::fs::{self, lit};
use crate::typeref::{self, Fk, Ft, ModelGroup};

/// The `Codec` decoder (`Dec<'T>`) of a value of type `ft`.
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

/// The `Codec` encoder (`Enc<'T>`) of a value of type `ft`.
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

/// The decoding of the JSON value `n` with the decoder `d` as a value of
/// type `ft`, as an expression.
pub fn dec_call(ft: &Ft, n: &str, d: &str) -> String {
    match &ft.kind {
        Fk::Named(name) if !ft.nullable => format!("{name}.FromJson({n}, {d})"),
        _ => format!("{} {n} {d}", dec(ft)),
    }
}

/// Writes the binding `head` (`let known =`) of an F# list of `items`: on
/// one line when short, else one item per line under it.
fn list_binding(w: &mut Writer, head: &str, items: &[String]) {
    let line = if items.is_empty() {
        format!("{head} []")
    } else {
        format!("{head} [ {} ]", items.join("; "))
    };
    if line.len() <= 88 || items.len() < 2 {
        w.line(line);
        return;
    }
    w.line(head);
    w.indent();
    w.line("[");
    w.indent();
    for item in items {
        w.line(item);
    }
    w.dedent();
    w.line("]");
    w.dedent();
}

/// The JSON of the value `e` of type `ft`, as an expression.
pub fn enc_call(ft: &Ft, e: &str) -> String {
    match &ft.kind {
        Fk::Named(_) if !ft.nullable => format!("{e}.ToJson()"),
        _ => format!("{} {e}", enc(ft)),
    }
}

/// How a member with a presence is declared, decoded and encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCode {
    /// The field type ([`typeref::field_ty`]).
    pub ty: String,
    /// The decode expression, given the object `o` and the decoder `d`.
    pub dec: String,
    /// The encode statement, given the object `o`.
    pub enc: String,
}

/// The declaration, decoding and encoding of the member `wire` of type
/// `ft` with this presence, read from the expression `value`.
pub fn member_code(ft: &Ft, presence: Presence, value: &str, wire: &str) -> MemberCode {
    let w = lit(wire);
    let ty = typeref::field_ty(ft, presence);
    let optional = matches!(presence, Presence::Optional | Presence::OptionalNullable);
    let null_allowed = ft.nullable
        || matches!(
            presence,
            Presence::RequiredNullable | Presence::OptionalNullable
        );
    let inner = ft.non_null();
    let (dec, enc) = match (optional, null_allowed) {
        (false, false) => (
            format!("Codec.required d o {w} {}", dec(&inner)),
            format!("Codec.put o {w} ({})", enc_call(&inner, value)),
        ),
        (false, true) if inner.kind == Fk::Any => (
            format!("Codec.required d o {w} Codec.any"),
            format!("Codec.put o {w} (Codec.encAny {value})"),
        ),
        (false, true) => (
            format!("Codec.requiredNullable d o {w} {}", dec(&inner)),
            format!(
                "Codec.put o {w} (Codec.encNullable {} {value})",
                enc(&inner)
            ),
        ),
        (true, false) => (
            format!("Codec.optional d o {w} {}", dec(&inner)),
            format!("Codec.putOption o {w} {} {value}", enc(&inner)),
        ),
        (true, true) => (
            format!("Codec.patch d o {w} {}", dec(&inner)),
            format!("Codec.putPatch o {w} {} {value}", enc(&inner)),
        ),
    };
    MemberCode { ty, dec, enc }
}

/// The documentation text of an IR doc.
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

/// The members of a generated F# record that no field may take: its
/// codec, its additional members and the members .NET and F# give every
/// record. Sorted.
pub const RECORD_TAKEN: &[&str] = &[
    "AdditionalProperties",
    "CompareTo",
    "Equals",
    "Finalize",
    "FromJson",
    "GetHashCode",
    "GetType",
    "MemberwiseClone",
    "ToJson",
    "ToString",
];

/// The `JsonKind` case of a JSON type name.
fn json_kind(kind: &str) -> &'static str {
    match kind {
        "string" => "JsonKind.String",
        "integer" => "JsonKind.Integer",
        "number" => "JsonKind.Number",
        "boolean" => "JsonKind.Boolean",
        "null" => "JsonKind.Null",
        "array" => "JsonKind.Array",
        _ => "JsonKind.Object",
    }
}

/// The record or union fields a model declares (the fields of a record,
/// the merged fields of an `allOf`) and its additional members.
struct RecordParts<'a> {
    fields: Vec<&'a Field>,
    additional: Additional,
}

impl Ctx<'_> {
    /// Every model file: (path, text), in compile order.
    pub fn models(&self) -> Vec<(String, String)> {
        self.model_groups()
            .into_iter()
            .map(|g| {
                let text = self.group_file(&g);
                (g.path, text)
            })
            .collect()
    }

    fn group_file(&self, g: &ModelGroup) -> String {
        let mut w = self.file_writer();
        w.line(format!("namespace {}", g.namespace));
        w.blank();
        w.line("open System.Text.Json.Nodes");
        w.line("open Tungsten.Runtime");
        // After Tungsten.Runtime: `Patch` and `Codec` are the F# layer's.
        w.line("open Tungsten.Runtime.FSharp");
        let mut first = true;
        for id in &g.types {
            let Some(t) = self.ir.types.get(id) else {
                continue;
            };
            w.blank();
            self.model(&mut w, t, &g.namespace, first);
            first = false;
        }
        w.finish()
    }

    /// The name of a model, as declared.
    fn type_name(&self, id: &TypeId) -> String {
        self.names.types.get(id).cloned().unwrap_or_default()
    }

    /// One model of a group: `type` for the first, `and` for the others.
    fn model(&self, w: &mut Writer, t: &NamedType, ns: &str, first: bool) {
        let Some(plan) = self.plan.types.get(&t.id) else {
            return;
        };
        let name = self.type_name(&t.id);
        fs::doc(w, &doc_text(t.doc.as_ref()));
        let keyword = if first { "type" } else { "and" };
        match &plan.kind {
            TypeKind::Record { .. } => {
                if let Shape::Record { fields, additional } = &t.shape {
                    let parts = RecordParts {
                        fields: fields.iter().collect(),
                        additional: additional.clone(),
                    };
                    self.record(w, t, &name, ns, keyword, &parts);
                }
            }
            TypeKind::AllOf(AllOfPlan::Merged { fields, closed }) => {
                let mut refs: Vec<&Field> = vec![];
                if let Shape::Intersection { members } = &t.shape {
                    for (m, f) in fields {
                        if let Some(member) = members.get(*m)
                            && let Some(Shape::Record { fields, .. }) = self.plan.resolve(member)
                            && let Some(field) = fields.get(*f)
                        {
                            refs.push(field);
                        }
                    }
                }
                let parts = RecordParts {
                    fields: refs,
                    additional: if *closed {
                        Additional::Closed
                    } else {
                        Additional::Open
                    },
                };
                self.record(w, t, &name, ns, keyword, &parts);
            }
            TypeKind::Enum(e @ (EnumPlan::Strings | EnumPlan::Integers)) => {
                self.enumeration(w, t, &name, keyword, e)
            }
            TypeKind::Union(u) if !matches!(u, UnionPlan::Empty) => {
                self.union(w, t, &name, ns, keyword, u)
            }
            _ => {}
        }
    }

    /// The field names of a record's fields: the namer's, never a member
    /// every generated record has ([`RECORD_TAKEN`]).
    fn field_names(&self, id: &TypeId, count: usize) -> Vec<String> {
        let names = self.names.fields.get(id).cloned().unwrap_or_default();
        let mut taken: Vec<String> = RECORD_TAKEN.iter().map(|s| s.to_string()).collect();
        (0..count)
            .map(|i| {
                let base = names.get(i).cloned().unwrap_or_else(|| format!("Field{i}"));
                fs::unique(fs::case_name(base), &mut taken)
            })
            .collect()
    }

    fn record(
        &self,
        w: &mut Writer,
        t: &NamedType,
        name: &str,
        ns: &str,
        keyword: &str,
        parts: &RecordParts<'_>,
    ) {
        let from = Some(ns);
        let props = self.field_names(&t.id, parts.fields.len());
        let codes: Vec<MemberCode> = parts
            .fields
            .iter()
            .zip(&props)
            .map(|(f, p)| {
                member_code(
                    &self.ft(&f.ty, from),
                    f.presence,
                    &format!("this.{}", fs::ident(p)),
                    &f.wire_name,
                )
            })
            .collect();
        let extra = match &parts.additional {
            Additional::Typed { values } => Some(self.ft(values, from)),
            _ => None,
        };
        let extra_ty = match (&parts.additional, &extra) {
            (Additional::Open, _) => Some("JsonObject option".to_string()),
            (Additional::Typed { .. }, Some(ft)) => {
                Some(format!("Map<string, {}> option", ft.ty()))
            }
            _ => None,
        };
        let known = fs::string_array(
            &parts
                .fields
                .iter()
                .map(|f| f.wire_name.clone())
                .collect::<Vec<_>>(),
        );
        let closed = matches!(parts.additional, Additional::Closed);
        let empty = codes.is_empty() && extra_ty.is_none();
        if empty {
            // F# records need a field: a closed record without any is a
            // one-case union.
            w.line(format!("{keyword} [<RequireQualifiedAccess>] {name} ="));
            w.indent();
            fs::doc_line(w, "The only value: an empty object.");
            w.line(format!("| {name}"));
        } else {
            w.line(format!("{keyword} {name} ="));
            w.indent();
            w.line("{");
            w.indent();
            for ((f, p), code) in parts.fields.iter().zip(&props).zip(&codes) {
                let doc = doc_text(f.doc.as_ref());
                if doc.is_empty() {
                    w.line(format!("/// The <c>{}</c> member.", fs::xml(&f.wire_name)));
                } else {
                    fs::doc(w, &doc);
                }
                w.line(format!("{}: {}", fs::ident(p), code.ty));
            }
            if let Some(ty) = &extra_ty {
                let text = if closed || extra.is_none() {
                    "Members the API description does not list, kept as they are."
                } else {
                    "Members the API description does not list by name."
                };
                fs::doc_line(w, text);
                w.line(format!("AdditionalProperties: {ty}"));
            }
            w.dedent();
            w.line("}");
        }
        w.blank();
        fs::doc_line(
            w,
            "Decodes a JSON value; strict decoding refuses members a closed type does not list.",
        );
        w.line(format!(
            "static member FromJson(node: JsonNode, d: Decoder) : {name} ="
        ));
        w.indent();
        w.line("let o = Codec.object d node");
        if closed {
            w.line(format!("Codec.known d o {known}"));
        }
        if empty {
            w.line(format!("{name}.{name}"));
        } else {
            w.line("{");
            w.indent();
            let mut qualified = false;
            for (p, code) in props.iter().zip(&codes) {
                // The first label names the type: records of one namespace
                // may share labels.
                let label = if qualified {
                    fs::ident(p)
                } else {
                    qualified = true;
                    format!("{name}.{}", fs::ident(p))
                };
                w.line(format!("{label} = {}", code.dec));
            }
            let label = if qualified {
                "AdditionalProperties".to_string()
            } else {
                format!("{name}.AdditionalProperties")
            };
            match (&parts.additional, &extra) {
                (Additional::Open, _) => w.line(format!("{label} = Codec.extra d o {known}")),
                (Additional::Typed { .. }, Some(ft)) => w.line(format!(
                    "{label} = Codec.extraTyped d o {known} {}",
                    dec(ft)
                )),
                _ => w,
            };
            w.dedent();
            w.line("}");
        }
        w.dedent();
        w.blank();
        fs::doc_line(
            w,
            "Decodes a JSON value, ignoring members the type does not list.",
        );
        w.line(format!(
            "static member FromJson(node: JsonNode) : {name} = {name}.FromJson(node, Decoder.Lenient())"
        ));
        w.blank();
        fs::doc_line(w, "The value as JSON.");
        if empty {
            w.line("member _.ToJson() : JsonNode = JsonObject() :> JsonNode");
        } else {
            w.line("member this.ToJson() : JsonNode =");
            w.indent();
            w.line("let o = JsonObject()");
            for code in &codes {
                w.line(&code.enc);
            }
            match (&parts.additional, &extra) {
                (Additional::Open, _) => w.line("Codec.putExtra o this.AdditionalProperties"),
                (Additional::Typed { .. }, Some(ft)) => w.line(format!(
                    "Codec.putExtraTyped o {} this.AdditionalProperties",
                    enc(ft)
                )),
                _ => w,
            };
            w.line("o :> JsonNode");
            w.dedent();
        }
        w.dedent();
    }

    fn enumeration(
        &self,
        w: &mut Writer,
        t: &NamedType,
        name: &str,
        keyword: &str,
        plan: &EnumPlan,
    ) {
        let Shape::Enum { values, .. } = &t.shape else {
            return;
        };
        let strings = matches!(plan, EnumPlan::Strings);
        let (vty, wire_dec, wire_enc) = if strings {
            ("string", "Codec.string", "Codec.encString")
        } else {
            ("int64", "Codec.int64", "Codec.encInt64")
        };
        let variant_names = self
            .names
            .enum_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        let mut taken: Vec<String> = typeref::CASE_NAMES
            .iter()
            .chain(fs::RECORD_MEMBERS)
            .map(|s| s.to_string())
            .collect();
        let cases: Vec<(String, String)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let base = variant_names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("Value{i}"));
                let literal = match &v.value {
                    serde_json::Value::String(s) => lit(s),
                    other => format!("{other}L"),
                };
                (fs::unique(fs::case_name(base), &mut taken), literal)
            })
            .collect();
        w.line(format!("{keyword} [<RequireQualifiedAccess>] {name} ="));
        w.indent();
        for ((c, literal), v) in cases.iter().zip(values) {
            let doc = v
                .doc
                .as_ref()
                .map(|d| doc_text(Some(d)))
                .unwrap_or_default();
            if doc.is_empty() {
                w.line(format!("/// The value <c>{}</c>.", fs::xml(literal)));
            } else {
                fs::doc(w, &doc);
            }
            w.line(format!("| {c}"));
        }
        fs::doc_line(
            w,
            "A value this SDK does not know, kept as it is (lenient decoding only).",
        );
        w.line(format!("| Unknown of {vty}"));
        w.blank();
        fs::doc_line(w, "The value on the wire.");
        w.line(format!("member this.Value : {vty} ="));
        w.indent();
        w.line("match this with");
        for (c, literal) in &cases {
            w.line(format!("| {name}.{c} -> {literal}"));
        }
        w.line(format!("| {name}.Unknown value -> value"));
        w.dedent();
        w.blank();
        fs::doc_line(w, "Every known value, in the API description's order.");
        let all: Vec<String> = cases.iter().map(|(c, _)| format!("{name}.{c}")).collect();
        list_binding(w, &format!("static member Values : {name} list ="), &all);
        w.blank();
        fs::doc_line(
            w,
            "Decodes a JSON value: a known value, else a failure (strict) or `Unknown` (lenient).",
        );
        w.line(format!(
            "static member FromJson(node: JsonNode, d: Decoder) : {name} ="
        ));
        w.indent();
        let known: Vec<String> = cases
            .iter()
            .map(|(c, literal)| format!("{literal}, {name}.{c}"))
            .collect();
        list_binding(w, "let known =", &known);
        w.line(format!(
            "Codec.oneOf d node known {wire_dec} {wire_enc} {name}.Unknown"
        ));
        w.dedent();
        w.blank();
        fs::doc_line(
            w,
            "Decodes a JSON value, keeping a value the SDK does not know.",
        );
        w.line(format!(
            "static member FromJson(node: JsonNode) : {name} = {name}.FromJson(node, Decoder.Lenient())"
        ));
        w.blank();
        fs::doc_line(w, "The value as JSON.");
        w.line(format!(
            "member this.ToJson() : JsonNode = {wire_enc} this.Value"
        ));
        w.blank();
        if strings {
            w.line("override this.ToString() = this.Value");
        } else {
            w.line(
                "override this.ToString() = this.Value.ToString(System.Globalization.CultureInfo.InvariantCulture)",
            );
        }
        w.dedent();
    }

    fn union(
        &self,
        w: &mut Writer,
        t: &NamedType,
        name: &str,
        ns: &str,
        keyword: &str,
        plan: &UnionPlan,
    ) {
        let Shape::Union(u) = &t.shape else {
            return;
        };
        let from = Some(ns);
        let variant_names = self
            .names
            .union_variants
            .get(&t.id)
            .cloned()
            .unwrap_or_default();
        let mut taken: Vec<String> = typeref::CASE_NAMES
            .iter()
            .chain(fs::RECORD_MEMBERS)
            .map(|s| s.to_string())
            .collect();
        let variants: Vec<(String, Ft)> = u
            .variants
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let base = variant_names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("Variant{i}"));
                (
                    fs::unique(fs::case_name(base), &mut taken),
                    self.ft(&v.ty, from),
                )
            })
            .collect();
        w.line(format!("{keyword} [<RequireQualifiedAccess>] {name} ="));
        w.indent();
        for (c, ft) in &variants {
            fs::doc_line(w, &format!("The variant {c}."));
            w.line(format!("| {c} of {}", ft.ty()));
        }
        w.blank();
        fs::doc_line(w, "The value as JSON.");
        w.line("member this.ToJson() : JsonNode =");
        w.indent();
        w.line("match this with");
        for (c, ft) in &variants {
            w.line(format!("| {name}.{c} value -> {}", enc_call(ft, "value")));
        }
        w.dedent();
        w.blank();
        fs::doc_line(w, "Decodes a JSON value into its variant.");
        w.line(format!(
            "static member FromJson(node: JsonNode, d: Decoder) : {name} ="
        ));
        w.indent();
        let case =
            |c: &str, ft: &Ft, n: &str, d: &str| format!("{name}.{c}({})", dec_call(ft, n, d));
        let trials = |order: &mut dyn Iterator<Item = usize>| -> Vec<String> {
            order
                .filter_map(|i| variants.get(i))
                .map(|(c, ft)| format!("(fun n d -> {})", case(c, ft, "n", "d")))
                .collect()
        };
        let first_of = |w: &mut Writer, trials: &[String]| {
            list_binding(w, "let variants =", trials);
            w.line(format!("Codec.firstOf d node {} variants", lit(name)));
        };
        match plan {
            UnionPlan::Tagged {
                tag,
                variants: tagged,
            } => {
                w.line("let o = Codec.object d node");
                w.line(format!("match Codec.tag d o {} with", lit(tag)));
                let mut all = vec![];
                for (index, tags) in tagged {
                    let Some((c, ft)) = variants.get(*index) else {
                        continue;
                    };
                    for value in tags {
                        all.push(value.clone());
                        w.line(format!("| {} -> {}", lit(value), case(c, ft, "node", "d")));
                    }
                }
                w.line(format!(
                    "| other -> raise (Codec.unknownTag d {} other {})",
                    lit(tag),
                    fs::string_array(&all)
                ));
            }
            UnionPlan::LiteralTagged { variants: keyed } => {
                let mut first = true;
                for (index, key) in keyed {
                    let Some((c, ft)) = variants.get(*index) else {
                        continue;
                    };
                    let test = match key {
                        LiteralKey::Value(v) => format!(
                            "Codec.isLiteral node {}",
                            lit(&serde_json::to_string(v).unwrap_or_else(|_| "null".into()))
                        ),
                        LiteralKey::JsonType("any") => "true".to_string(),
                        LiteralKey::JsonType(kind) => {
                            format!("Codec.isKind node {}", json_kind(kind))
                        }
                    };
                    let keyword = if first { "if" } else { "elif" };
                    first = false;
                    w.line(format!("{keyword} {test} then"));
                    w.line(format!("    {}", case(c, ft, "node", "d")));
                }
                let rest = trials(&mut keyed.iter().map(|(i, _)| *i));
                if first {
                    first_of(w, &rest);
                } else {
                    w.line("else");
                    w.indent();
                    first_of(w, &rest);
                    w.dedent();
                }
            }
            UnionPlan::Untagged { ordered } => {
                let rest = trials(&mut ordered.iter().copied());
                first_of(w, &rest);
            }
            UnionPlan::Empty => first_of(w, &[]),
        }
        w.dedent();
        w.blank();
        fs::doc_line(
            w,
            "Decodes a JSON value, ignoring members the variants do not list.",
        );
        w.line(format!(
            "static member FromJson(node: JsonNode) : {name} = {name}.FromJson(node, Decoder.Lenient())"
        ));
        w.dedent();
    }
}
