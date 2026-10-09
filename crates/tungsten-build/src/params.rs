// SPDX-License-Identifier: AGPL-3.0-only
//! Operation parameters: path-level and operation-level declarations
//! merged (the operation's declaration replaces the path item's with the
//! same location and name), serialization defaults from OpenAPI, roles
//! (planning/03 `ParamRole`) and names unique within the operation.

use serde_json::Value;
use tungsten_core::Diagnostic;
use tungsten_ir::naming::Role;
use tungsten_ir::{
    ApiKeyIn, ConstQuery, Ident, Param, ParamRole, ParamSet, ParamStyle, PathSegment, PathTemplate,
    Primitive, Shape, TypeRef,
};
use tungsten_openapi::RefTarget;

use crate::auth::AuthTable;
use crate::ctx::{Ctx, child, doc, flag, str_of};
use crate::names::disambiguate_all;
use crate::operations::OpScope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    Path,
    Query,
    Header,
    Cookie,
}

impl Location {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "path" => Some(Self::Path),
            "query" => Some(Self::Query),
            "header" => Some(Self::Header),
            "cookie" => Some(Self::Cookie),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Query => "query",
            Self::Header => "header",
            Self::Cookie => "cookie",
        }
    }

    /// OpenAPI's default style per location.
    fn default_style(self) -> ParamStyle {
        match self {
            Self::Path | Self::Header => ParamStyle::Simple,
            Self::Query | Self::Cookie => ParamStyle::Form,
        }
    }
}

/// One parameter declaration after `$ref` resolution.
struct Declared<'v> {
    location: Location,
    wire: String,
    /// The Parameter Object (after `$ref`s).
    at: RefTarget,
    value: &'v Value,
}

impl Declared<'_> {
    /// Same location and name; header names compare case-insensitively.
    fn same_key(&self, other: &Declared<'_>) -> bool {
        self.location == other.location
            && match self.location {
                Location::Header => self.wire.eq_ignore_ascii_case(&other.wire),
                _ => self.wire == other.wire,
            }
    }
}

/// Header parameters OpenAPI says to ignore (they are set by the client).
const IGNORED_HEADERS: [&str; 3] = ["accept", "content-type", "authorization"];

/// Build the parameter set of an operation.
pub(crate) fn build(
    cx: &mut Ctx<'_>,
    auth: &AuthTable,
    scope: &OpScope<'_>,
    path_item: &RefTarget,
    op: &RefTarget,
    template: &PathTemplate,
) -> ParamSet {
    let mut merged: Vec<Declared<'_>> = vec![];
    for owner in [path_item, op] {
        let list = child(owner, "parameters");
        let Some(items) = cx.get(&list) else {
            continue;
        };
        let Some(items) = items.as_array() else {
            cx.report(
                Diagnostic::warning("TG0508", "`parameters` must be an array; ignored"),
                &list,
            );
            continue;
        };
        for i in 0..items.len() {
            let Some(decl) = declared(cx, &child(&list, &i.to_string())) else {
                continue;
            };
            match merged.iter_mut().find(|d| d.same_key(&decl)) {
                Some(slot) => *slot = decl,
                None => merged.push(decl),
            }
        }
    }

    let in_template = template_params(split_query(&template.raw).0);
    let mut built: Vec<(Location, Param, RefTarget)> = vec![];
    for decl in &merged {
        if decl.location == Location::Path && !in_template.contains(&decl.wire) {
            cx.report(
                Diagnostic::warning(
                    "TG0508",
                    format!(
                        "path parameter `{}` does not appear in the path template {}; ignored",
                        decl.wire, template.raw
                    ),
                ),
                &decl.at,
            );
            continue;
        }
        let param = param(cx, auth, scope, decl);
        built.push((decl.location, param, decl.at.clone()));
    }
    for name in &in_template {
        let declared = built
            .iter()
            .any(|(l, p, _)| *l == Location::Path && &p.wire_name == name);
        if !declared {
            cx.report(
                Diagnostic::warning(
                    "TG0507",
                    format!(
                        "path parameter `{name}` of {} has no parameter definition; assumed a required string",
                        template.raw
                    ),
                ),
                op,
            );
            built.push((Location::Path, undeclared_path_param(name), op.clone()));
        }
    }
    // Path parameters in template order; the others in declaration order.
    let position = |p: &Param| in_template.iter().position(|n| *n == p.wire_name);
    let mut ordered: Vec<(Location, Param, RefTarget)> = vec![];
    for location in [
        Location::Path,
        Location::Query,
        Location::Header,
        Location::Cookie,
    ] {
        let mut group: Vec<_> = built
            .iter()
            .filter(|(l, ..)| *l == location)
            .cloned()
            .collect();
        if location == Location::Path {
            group.sort_by_key(|(_, p, _)| position(p));
        }
        ordered.extend(group);
    }

    let mut idents: Vec<Ident> = ordered.iter().map(|(_, p, _)| p.name.clone()).collect();
    for i in disambiguate_all(&mut idents, Role::Param) {
        let (location, p, at) = &ordered[i];
        cx.report(
            Diagnostic::warning(
                "TG0401",
                format!(
                    "{} parameter `{}` of `{}` collides with another parameter name; renamed to `{}`",
                    location.word(),
                    p.wire_name,
                    scope.id,
                    idents[i].camel()
                ),
            ),
            at,
        );
    }

    let mut set = ParamSet {
        path: vec![],
        query: vec![],
        header: vec![],
        cookie: vec![],
    };
    for ((location, mut p, _), name) in ordered.into_iter().zip(idents) {
        p.name = name;
        match location {
            Location::Path => set.path.push(p),
            Location::Query => set.query.push(p),
            Location::Header => set.header.push(p),
            Location::Cookie => set.cookie.push(p),
        }
    }
    set
}

/// Read one entry of a `parameters` list. Malformed entries are TG0508
/// warnings; dangling `$ref`s were reported by the frontend.
fn declared<'v>(cx: &mut Ctx<'v>, at: &RefTarget) -> Option<Declared<'v>> {
    let (target, value) = cx.deref_value(at)?;
    let wire = str_of(value, "name");
    let location = str_of(value, "in").and_then(Location::parse);
    let (Some(wire), Some(location)) = (wire, location) else {
        cx.report(
            Diagnostic::warning(
                "TG0508",
                "a parameter needs `name` and `in` (path, query, header or cookie); ignored",
            ),
            &target,
        );
        return None;
    };
    if location == Location::Header && IGNORED_HEADERS.contains(&wire.to_ascii_lowercase().as_str())
    {
        return None;
    }
    Some(Declared {
        location,
        wire: wire.to_string(),
        at: target,
        value,
    })
}

fn param(cx: &mut Ctx<'_>, auth: &AuthTable, scope: &OpScope<'_>, decl: &Declared<'_>) -> Param {
    let (schema, media_type) = schema_target(cx, decl);
    let ty = match &schema {
        Some(t) => cx
            .tb
            .type_ref(scope.ns, t, &[&scope.hint, &decl.wire, "Param"]),
        None => TypeRef::Inline(Box::new(Shape::Any)),
    };
    let style = str_of(decl.value, "style")
        .and_then(parse_style)
        .unwrap_or_else(|| decl.location.default_style());
    let explode = decl
        .value
        .get("explode")
        .and_then(Value::as_bool)
        .unwrap_or(style == ParamStyle::Form);
    Param {
        wire_name: decl.wire.clone(),
        name: Ident::new(&decl.wire),
        ty,
        required: decl.location == Location::Path || flag(decl.value, "required"),
        doc: doc(None, str_of(decl.value, "description")),
        style,
        explode,
        role: role(cx, auth, scope.ns_index, decl, schema.as_ref()),
        deprecated: flag(decl.value, "deprecated"),
        media_type,
    }
}

/// The parameter's schema: `schema`, or the schema of its single `content`
/// media type, with that media type.
fn schema_target(cx: &Ctx<'_>, decl: &Declared<'_>) -> (Option<RefTarget>, Option<String>) {
    if decl.value.get("schema").is_some() {
        return (Some(child(&decl.at, "schema")), None);
    }
    let Some((media, entry)) = decl
        .value
        .get("content")
        .and_then(Value::as_object)
        .and_then(|c| c.iter().next())
    else {
        return (None, None);
    };
    let target = child(&child(&child(&decl.at, "content"), media), "schema");
    let schema = entry
        .get("schema")
        .and_then(|_| cx.get(&target))
        .map(|_| target);
    (schema, Some(media.clone()))
}

fn parse_style(s: &str) -> Option<ParamStyle> {
    Some(match s {
        "simple" => ParamStyle::Simple,
        "form" => ParamStyle::Form,
        "label" => ParamStyle::Label,
        "matrix" => ParamStyle::Matrix,
        "spaceDelimited" => ParamStyle::SpaceDelimited,
        "pipeDelimited" => ParamStyle::PipeDelimited,
        "deepObject" => ParamStyle::DeepObject,
        _ => return None,
    })
}

/// The role of a parameter. Header conventions win over credentials, so
/// an `Origin` header that a composite profile also supplies stays
/// `Origin`.
fn role(
    cx: &Ctx<'_>,
    auth: &AuthTable,
    ns_index: usize,
    decl: &Declared<'_>,
    schema: Option<&RefTarget>,
) -> ParamRole {
    if decl.location == Location::Header {
        let lower = decl.wire.to_ascii_lowercase();
        if lower == "idempotency-key" {
            return ParamRole::IdempotencyKey;
        }
        if lower == "origin" {
            return ParamRole::Origin;
        }
        if is_dry_run_header(&lower) || (lower == "prefer" && mentions_dry_run(cx, decl, schema)) {
            return ParamRole::DryRun;
        }
    }
    let location = match decl.location {
        Location::Header => ApiKeyIn::Header,
        Location::Cookie => ApiKeyIn::Cookie,
        Location::Query => ApiKeyIn::Query,
        Location::Path => return ParamRole::Plain,
    };
    if auth.is_credential(ns_index, location, &decl.wire) {
        ParamRole::Auth
    } else {
        ParamRole::Plain
    }
}

/// `X-Dry-Run`, `Dry-Run`, `x-dryrun`, `dry_run`.
fn is_dry_run_header(lower: &str) -> bool {
    let bare = lower.strip_prefix("x-").unwrap_or(lower);
    bare.replace(['-', '_'], "") == "dryrun"
}

/// Whether a `Prefer` header's schema or examples offer `dry-run`.
fn mentions_dry_run(cx: &Ctx<'_>, decl: &Declared<'_>, schema: Option<&RefTarget>) -> bool {
    let mut values: Vec<&Value> = vec![];
    if let Some((_, s)) = schema.and_then(|t| cx.deref_value(t)) {
        values.extend(["const", "default"].iter().filter_map(|k| s.get(*k)));
        for key in ["enum", "examples"] {
            values.extend(s.get(key).and_then(Value::as_array).into_iter().flatten());
        }
    }
    values.extend(decl.value.get("example"));
    if let Some(examples) = decl.value.get("examples").and_then(Value::as_object) {
        values.extend(examples.values().filter_map(|e| e.get("value")));
    }
    values.iter().any(|v| {
        v.as_str()
            .is_some_and(|s| s.to_ascii_lowercase().contains("dry-run"))
    })
}

/// Names of the `{param}` placeholders of a path template, in order,
/// without duplicates.
pub(crate) fn template_params(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut rest = raw;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        let name = &after[..close];
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

/// Parse a path template into segments. A segment that is exactly one
/// `{name}` is a parameter, one without placeholders is literal text, and
/// one that mixes both (`{date}.csv`, `{id}:archive`) is a template of
/// literal and parameter parts. Unbalanced braces are literal text.
///
/// A query string in the key (`/v1/messages?beta=true`, which OpenAPI
/// forbids but real documents use) is not part of the segments: its pairs
/// are the template's constant query parameters, and `raw` keeps the whole
/// key so the runtime sends them.
pub(crate) fn path_template(raw: &str) -> PathTemplate {
    let (path, query) = split_query(raw);
    let segments = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(segment)
        .collect();
    PathTemplate {
        raw: raw.to_string(),
        segments,
        query: query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                ConstQuery {
                    name: name.to_string(),
                    value: value.to_string(),
                }
            })
            .collect(),
    }
}

/// A path key split at its first `?` into the path and the query string
/// (empty when there is none).
pub(crate) fn split_query(key: &str) -> (&str, &str) {
    key.split_once('?').unwrap_or((key, ""))
}

fn segment(text: &str) -> PathSegment {
    let literal = || PathSegment::Literal {
        value: text.to_string(),
    };
    let mut parts = vec![];
    let mut rest = text;
    while !rest.is_empty() {
        let Some(open) = rest.find('{') else {
            if rest.contains('}') {
                return literal();
            }
            parts.push(PathSegment::Literal {
                value: rest.to_string(),
            });
            break;
        };
        if rest[..open].contains('}') {
            return literal();
        }
        if open > 0 {
            parts.push(PathSegment::Literal {
                value: rest[..open].to_string(),
            });
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return literal();
        };
        let name = &after[..close];
        if name.is_empty() || name.contains('{') {
            return literal();
        }
        parts.push(PathSegment::Param {
            name: name.to_string(),
        });
        rest = &after[close + 1..];
    }
    match parts.as_slice() {
        [PathSegment::Param { .. }] | [PathSegment::Literal { .. }] => parts.remove(0),
        [] => literal(),
        _ => PathSegment::Template { parts },
    }
}

/// A path placeholder with no declaration: a required string.
fn undeclared_path_param(name: &str) -> Param {
    Param {
        wire_name: name.to_string(),
        name: Ident::new(name),
        ty: TypeRef::Inline(Box::new(Shape::Primitive {
            primitive: Primitive::String { format: None },
            constraints: Default::default(),
        })),
        required: true,
        doc: None,
        style: ParamStyle::Simple,
        explode: false,
        role: ParamRole::Plain,
        deprecated: false,
        media_type: None,
    }
}
