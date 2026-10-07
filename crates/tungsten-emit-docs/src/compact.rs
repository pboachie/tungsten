// SPDX-License-Identifier: AGPL-3.0-only
//! Compact, TypeScript-like type notation for `llms-full.txt`: short enough
//! to read in a model's context, precise enough to call the API.
//!
//! `string<uuid>`, `int64`, `"a" | "b"`, `T[]`, `{[key: string]: T}`,
//! `{ id: string, tag?: string | null, ... }` (`?` optional, `| null`
//! nullable, a trailing `...` for open records), `A & B`, named types by
//! their type id. Constraints follow the type: `string{1..64}` for lengths,
//! `integer[1..20]` for ranges, `/regex/` for patterns.

use std::collections::BTreeSet;

use tungsten_ir::{
    Additional, Constraints, Field, Ir, Presence, Primitive, Shape, StringFormat, TypeId, TypeRef,
};

/// Renders compact notation and records the named types it mentions.
#[derive(Debug, Default)]
pub(crate) struct Compact {
    /// Every named type referenced so far.
    pub mentioned: BTreeSet<TypeId>,
}

impl Compact {
    pub fn type_ref(&mut self, ty: &TypeRef) -> String {
        match ty {
            TypeRef::Named(id) => {
                self.mentioned.insert(id.clone());
                id.0.clone()
            }
            TypeRef::Inline(shape) => self.shape(shape),
        }
    }

    pub fn shape(&mut self, shape: &Shape) -> String {
        match shape {
            Shape::Primitive {
                primitive,
                constraints,
            } => format!(
                "{}{}",
                primitive_name(primitive),
                constraint_suffix(constraints)
            ),
            Shape::Enum { values, .. } => values
                .iter()
                .map(|v| v.value.to_string())
                .collect::<Vec<_>>()
                .join(" | "),
            Shape::Const { value } => value.to_string(),
            Shape::Array { items, .. } => format!("{}[]", self.operand(items)),
            Shape::Map { values } => format!("{{[key: string]: {}}}", self.type_ref(values)),
            Shape::Record { fields, additional } => self.record(fields, additional),
            Shape::Union(u) => u
                .variants
                .iter()
                .map(|v| self.operand(&v.ty))
                .collect::<Vec<_>>()
                .join(" | "),
            Shape::Intersection { members } => members
                .iter()
                .map(|m| self.operand(m))
                .collect::<Vec<_>>()
                .join(" & "),
            Shape::Nullable { inner } => format!("{} | null", self.operand(inner)),
            Shape::Any => "any".into(),
            Shape::Never => "never".into(),
        }
    }

    /// A type used inside `[]`, `|` or `&`: parenthesized when it is itself
    /// a union, intersection or nullable.
    fn operand(&mut self, ty: &TypeRef) -> String {
        let text = self.type_ref(ty);
        let compound = matches!(
            ty,
            TypeRef::Inline(shape) if matches!(
                **shape,
                Shape::Union(_) | Shape::Intersection { .. } | Shape::Nullable { .. }
            ) || matches!(**shape, Shape::Enum { ref values, .. } if values.len() > 1)
        );
        if compound { format!("({text})") } else { text }
    }

    pub fn record(&mut self, fields: &[Field], additional: &Additional) -> String {
        let mut parts: Vec<String> = fields.iter().map(|f| self.field(f, false)).collect();
        match additional {
            Additional::Closed => {}
            Additional::Open => parts.push("...".into()),
            Additional::Typed { values } => {
                parts.push(format!("[key: string]: {}", self.type_ref(values)))
            }
        }
        if parts.is_empty() {
            "{}".into()
        } else {
            format!("{{ {} }}", parts.join(", "))
        }
    }

    /// `name?: T | null`, with `readonly`/`writeonly` markers and the
    /// field's own constraints. `force_optional` marks a required field
    /// optional (the fields of an optional body).
    pub fn field(&mut self, f: &Field, force_optional: bool) -> String {
        let optional =
            force_optional || matches!(f.presence, Presence::Optional | Presence::OptionalNullable);
        let nullable = matches!(
            f.presence,
            Presence::RequiredNullable | Presence::OptionalNullable
        );
        // A field's constraints usually repeat those of its inline type.
        let repeated = matches!(
            &f.ty,
            TypeRef::Inline(shape) if matches!(
                &**shape,
                Shape::Primitive { constraints, .. } if *constraints == f.constraints
            )
        );
        let own = if repeated {
            String::new()
        } else {
            constraint_suffix(&f.constraints)
        };
        let mut ty = format!("{}{own}", self.type_ref(&f.ty));
        if nullable {
            ty.push_str(" | null");
        }
        let mut name = String::new();
        if f.read_only {
            name.push_str("readonly ");
        }
        if f.write_only {
            name.push_str("writeonly ");
        }
        name.push_str(&key(&f.wire_name));
        format!("{name}{}: {ty}", if optional { "?" } else { "" })
    }

    /// The definitions of every type mentioned so far and of the types they
    /// mention in turn, sorted by id: `(id, notation, doc)`.
    pub fn definitions(&mut self, ir: &Ir) -> Vec<(TypeId, String, Option<String>)> {
        let mut done = BTreeSet::new();
        let mut out = vec![];
        loop {
            let next = self.mentioned.difference(&done).next().cloned();
            let Some(id) = next else { break };
            done.insert(id.clone());
            if let Some(t) = ir.types.get(&id) {
                let text = self.shape(&t.shape);
                let doc = t
                    .doc
                    .as_ref()
                    .and_then(|d| d.summary.as_deref().or(d.description.as_deref()))
                    .map(|d| tungsten_emit::schema::prune_sentences(d, 1))
                    .filter(|d| !d.is_empty());
                out.push((id, text, doc));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// A record key, quoted when it is not a plain identifier.
pub(crate) fn key(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if plain {
        name.to_string()
    } else {
        serde_json::Value::String(name.to_string()).to_string()
    }
}

fn primitive_name(p: &Primitive) -> String {
    match p {
        Primitive::String { format: None } => "string".into(),
        Primitive::String {
            format: Some(StringFormat::Other(f)),
        } => format!("string<{f}>"),
        Primitive::String { format: Some(f) } => {
            let name = match f {
                StringFormat::Uuid => "uuid",
                StringFormat::DateTime => "date-time",
                StringFormat::Date => "date",
                StringFormat::Time => "time",
                StringFormat::Duration => "duration",
                StringFormat::Email => "email",
                StringFormat::Uri => "uri",
                StringFormat::Hostname => "hostname",
                StringFormat::Ipv4 => "ipv4",
                StringFormat::Ipv6 => "ipv6",
                StringFormat::Byte => "base64",
                StringFormat::Password => "password",
                StringFormat::Other(_) => "",
            };
            format!("string<{name}>")
        }
        Primitive::Int32 => "int32".into(),
        Primitive::Int64 => "int64".into(),
        Primitive::Integer => "integer".into(),
        Primitive::Float => "float".into(),
        Primitive::Double => "double".into(),
        Primitive::Number => "number".into(),
        Primitive::Bool => "boolean".into(),
        Primitive::Bytes => "bytes".into(),
    }
}

/// `{1..64}` lengths, `[0..100]` or `(0..)` ranges (parentheses for
/// exclusive bounds), `/re/` patterns, `%5` multiples.
fn constraint_suffix(c: &Constraints) -> String {
    let mut out = String::new();
    if c.min_length.is_some() || c.max_length.is_some() {
        out.push_str(&format!(
            "{{{}..{}}}",
            c.min_length.map(|n| n.to_string()).unwrap_or_default(),
            c.max_length.map(|n| n.to_string()).unwrap_or_default()
        ));
    }
    let low = c
        .minimum
        .as_ref()
        .map(|n| ('[', n.to_string()))
        .or_else(|| c.exclusive_minimum.as_ref().map(|n| ('(', n.to_string())));
    let high = c
        .maximum
        .as_ref()
        .map(|n| (']', n.to_string()))
        .or_else(|| c.exclusive_maximum.as_ref().map(|n| (')', n.to_string())));
    if low.is_some() || high.is_some() {
        let (open, low) = low.unwrap_or(('[', String::new()));
        let (close, high) = high.unwrap_or((']', String::new()));
        out.push_str(&format!("{open}{low}..{high}{close}"));
    }
    if let Some(m) = &c.multiple_of {
        out.push_str(&format!("%{m}"));
    }
    if let Some(p) = &c.pattern {
        out.push_str(&format!(" /{p}/"));
    }
    out
}
