// SPDX-License-Identifier: AGPL-3.0-only
//! `lib/<lib>/models/<namespace>.rb`: one Ruby constant per named type.
//!
//! - A record (and an `allOf` merged into one) is a `Data.define` value
//!   object that includes `Tungsten::Runtime::Model`: `from_json` reads it
//!   by wire name (members the value does not have are `UNSET`), `to_h`
//!   writes it back by wire name without the `UNSET` members, and
//!   `wire_names` maps each member to its wire name. Optional members
//!   default to `UNSET`; required ones are required keywords.
//! - An enum is a module of frozen constants with `VALUES`, `member?` and
//!   an identity `from_json`.
//! - A union is a module whose `from_json` decodes the variant the value
//!   is: by the tag of a discriminated union, else by the first variant
//!   (in plan order) whose schema accepts it; a value no variant accepts is
//!   kept as it is.
//! - Any other named type (an alias, an `allOf` that cannot be merged) is a
//!   module whose `from_json` decodes the shape it stands for.

use serde_json::Value;
use tungsten_emit::Writer;
use tungsten_emit::sdk::{AllOfPlan, TypeKind, UnionPlan};
use tungsten_ir::{Field, NamedType, Presence, Shape};

use crate::context::Gen;
use crate::rb::{comment, str_lit};

/// Every models file.
pub fn files(g: &Gen<'_, '_>) -> Vec<(String, String)> {
    let mut out = vec![];
    for ns in &g.plan.namespaces {
        if ns.types.is_empty() {
            continue;
        }
        let mut w = Writer::new("  ");
        g.file_start(&mut w);
        g.in_module(&mut w, |w| {
            w.line("module Models");
            w.indent();
            comment(w, &format!("Models of the `{}` namespace.", ns.name));
            w.line(format!("module {}", g.ns_module(&ns.name)));
            w.indent();
            let mut first = true;
            for id in &ns.types {
                let Some(t) = g.plan.ir.types.get(id) else {
                    continue;
                };
                if !first {
                    w.blank();
                }
                first = false;
                named_type(g, w, t);
            }
            w.dedent();
            w.line("end");
            w.dedent();
            w.line("end");
        });
        out.push((
            format!("lib/{}/models/{}.rb", g.lib(), g.ns_file(&ns.name)),
            w.finish(),
        ));
    }
    out
}

fn doc_of(t: &NamedType) -> String {
    let mut text = String::new();
    if let Some(doc) = &t.doc {
        if let Some(s) = doc
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            text.push_str(s);
        }
        if let Some(d) = doc
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            && Some(d) != doc.summary.as_deref().map(str::trim)
        {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(d);
        }
    }
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(&format!("Wire type `{}`.", t.id.0));
    text
}

fn named_type(g: &Gen<'_, '_>, w: &mut Writer, t: &NamedType) {
    let Some(plan) = g.plan.types.get(&t.id) else {
        return;
    };
    let name = g.names.types.get(&t.id).cloned().unwrap_or_default();
    comment(w, &doc_of(t));
    match (&plan.kind, &t.shape) {
        (TypeKind::Record { .. }, Shape::Record { fields, .. }) => {
            let own: Vec<&Field> = fields.iter().collect();
            let names = g.names.fields.get(&t.id).cloned().unwrap_or_default();
            record(g, w, &name, &own, &names);
        }
        (TypeKind::AllOf(AllOfPlan::Merged { fields, .. }), Shape::Intersection { members }) => {
            let own: Vec<&Field> = fields
                .iter()
                .filter_map(|&(m, f)| match g.plan.resolve(members.get(m)?) {
                    Some(Shape::Record { fields, .. }) => fields.get(f),
                    _ => None,
                })
                .collect();
            let names = g.names.fields.get(&t.id).cloned().unwrap_or_default();
            record(g, w, &name, &own, &names);
        }
        (TypeKind::Enum(_), Shape::Enum { values, .. }) => {
            let consts = g
                .names
                .enum_variants
                .get(&t.id)
                .cloned()
                .unwrap_or_default();
            w.line(format!("module {name}"));
            w.indent();
            for (v, c) in values.iter().zip(&consts) {
                if let Some(doc) = v.doc.as_ref().and_then(|d| d.summary.as_deref()) {
                    comment(w, doc);
                }
                w.line(format!("{c} = {}", json_lit(&v.value)));
            }
            comment(w, "Every value, in declaration order.");
            w.line(format!("VALUES = [{}].freeze", consts.join(", ")));
            w.blank();
            comment(w, "Whether `value` is one of the values.");
            w.line("def self.member?(value) = VALUES.include?(value)");
            w.blank();
            comment(w, "The value itself: enum values stay plain JSON values.");
            w.line("def self.from_json(value) = value");
            w.dedent();
            w.line("end");
        }
        (TypeKind::Union(union), Shape::Union(u)) => {
            w.line(format!("module {name}"));
            w.indent();
            comment(
                w,
                "The variant `value` is, decoded; a value no variant accepts is returned as it is.",
            );
            w.line("def self.from_json(value)");
            w.indent();
            match union {
                UnionPlan::Tagged {
                    tag,
                    variants,
                    default,
                } => {
                    w.line("return value unless value.is_a?(Hash)");
                    w.blank();
                    // OpenAPI 3.2 `defaultMapping`: any other object decodes
                    // as the default variant.
                    let fallback = default
                        .and_then(|i| u.variants.get(i))
                        .and_then(|v| g.decode(&v.ty, "value", 1))
                        .unwrap_or_else(|| "value".to_string());
                    let arms: Vec<(String, String)> = variants
                        .iter()
                        .filter(|(i, tags)| !tags.is_empty() && *default != Some(*i))
                        .filter_map(|(i, tags)| {
                            let v = u.variants.get(*i)?;
                            let when: Vec<String> = tags.iter().map(|s| str_lit(s)).collect();
                            let body = g
                                .decode(&v.ty, "value", 1)
                                .unwrap_or_else(|| "value".to_string());
                            Some((when.join(", "), body))
                        })
                        .collect();
                    if arms.is_empty() {
                        w.line(fallback);
                    } else {
                        w.line(format!("case value[{}]", str_lit(tag)));
                        for (when, body) in arms {
                            w.line(format!("when {when} then {body}"));
                        }
                        w.line(format!("else {fallback}"));
                        w.line("end");
                    }
                }
                UnionPlan::Empty => {
                    w.line("value");
                }
                UnionPlan::LiteralTagged { .. } | UnionPlan::Untagged { .. } => {
                    w.line(format!(
                        "case ::Tungsten::Runtime::Models.variant({}::DESCRIPTORS, {}, value)",
                        g.module(),
                        str_lit(&t.id.0)
                    ));
                    for (i, v) in u.variants.iter().enumerate() {
                        let body = g
                            .decode(&v.ty, "value", 1)
                            .unwrap_or_else(|| "value".to_string());
                        w.line(format!("when {i} then {body}"));
                    }
                    w.line("else value");
                    w.line("end");
                }
            }
            w.dedent();
            w.line("end");
            w.dedent();
            w.line("end");
        }
        (_, shape) => {
            w.line(format!("module {name}"));
            w.indent();
            comment(w, "The JSON value, decoded where the type has models.");
            match g.decode_shape(shape, "value", 1) {
                Some(expr) if !matches!(plan.kind, TypeKind::AllOf(_)) => {
                    w.line(format!("def self.from_json(value) = {expr}"));
                }
                _ => {
                    w.line("def self.from_json(value) = value");
                }
            }
            w.dedent();
            w.line("end");
        }
    }
}

fn record(g: &Gen<'_, '_>, w: &mut Writer, name: &str, fields: &[&Field], names: &[String]) {
    let pairs: Vec<(&Field, &String)> = fields.iter().copied().zip(names).collect();
    let members: Vec<String> = pairs.iter().map(|(_, n)| format!(":{n}")).collect();
    if members.is_empty() {
        w.line(format!("{name} = Data.define do"));
    } else {
        w.line(format!("{name} = Data.define({}) do", members.join(", ")));
    }
    w.indent();
    w.line("include ::Tungsten::Runtime::Model");
    for (f, n) in &pairs {
        let mut text = String::new();
        if let Some(doc) = &f.doc {
            let d = doc
                .description
                .as_deref()
                .or(doc.summary.as_deref())
                .unwrap_or("")
                .trim();
            text.push_str(d);
        }
        let presence = match f.presence {
            Presence::Required => "required",
            Presence::RequiredNullable => "required, may be nil",
            Presence::Optional => "optional (UNSET when absent)",
            Presence::OptionalNullable => "optional (UNSET when absent), may be nil",
        };
        let line = format!(
            "@!attribute [r] {n}\n  @return [{}] wire `{}`, {presence}.",
            g.type_doc(&f.ty),
            f.wire_name
        );
        w.blank();
        comment(w, &line);
        if !text.is_empty() {
            for l in text.lines() {
                w.line(format!("#   {}", l.trim_end()).trim_end());
            }
        }
    }
    // Data's own initializer requires every member; one with defaults is
    // written only when a member is optional.
    let optional = pairs
        .iter()
        .any(|(f, _)| matches!(f.presence, Presence::Optional | Presence::OptionalNullable));
    if optional {
        let params: Vec<String> = pairs
            .iter()
            .map(|(f, n)| match f.presence {
                Presence::Required | Presence::RequiredNullable => format!("{n}:"),
                Presence::Optional | Presence::OptionalNullable => {
                    format!("{n}: ::Tungsten::Runtime::UNSET")
                }
            })
            .collect();
        w.blank();
        w.line(format!("def initialize({})", params.join(", ")));
        w.indent();
        w.line("super");
        w.dedent();
        w.line("end");
    }
    w.blank();
    comment(w, "The wire name of every member.");
    // `:member => "wire"`: a `secret: "secret"` pair reads as a credential
    // to secret scanners.
    let wires: Vec<String> = pairs
        .iter()
        .map(|(f, n)| format!(":{n} => {}", str_lit(&f.wire_name)))
        .collect();
    if wires.is_empty() {
        w.line("def self.wire_names = {}.freeze");
    } else {
        w.line(format!(
            "def self.wire_names = {{ {} }}.freeze",
            wires.join(", ")
        ));
    }
    w.blank();
    comment(
        w,
        "A model of a JSON object read by wire name; any other value is returned as it is.",
    );
    w.line("def self.from_json(value)");
    w.indent();
    w.line("return value unless value.is_a?(Hash)");
    w.blank();
    if pairs.is_empty() {
        w.line("new");
    } else {
        w.line("new(");
        w.indent();
        for (f, n) in &pairs {
            let read = format!(
                "value.fetch({}, ::Tungsten::Runtime::UNSET)",
                str_lit(&f.wire_name)
            );
            let expr = g.decode(&f.ty, &read, 1).unwrap_or(read);
            w.line(format!("{n}: {expr},"));
        }
        w.dedent();
        w.line(")");
    }
    w.dedent();
    w.line("end");
    w.dedent();
    w.line("end");
}

/// A Ruby literal of a JSON value.
pub fn json_lit(v: &Value) -> String {
    match v {
        Value::Null => "nil".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
                    format!("{}", f as i64)
                } else {
                    format!("{f:?}")
                }
            }
        }
        Value::String(s) => str_lit(s),
        Value::Array(_) | Value::Object(_) => format!(
            "::Tungsten::Runtime::Json.parse({}).freeze",
            str_lit(&v.to_string())
        ),
    }
}
