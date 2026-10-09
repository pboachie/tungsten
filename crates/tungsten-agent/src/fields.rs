// SPDX-License-Identifier: AGPL-3.0-only
//! Field paths into an operation's request and success response.
//!
//! A path is dotted wire names (`device_id`, `result.delivery.state`).
//! Arrays are looked through (`devices.device_id` names the field of each
//! item), nullable wrappers and named types are followed, a union or an
//! intersection has the fields of any member. A request path starts at a
//! parameter (any location) or a field of the JSON body.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_ir::{
    BodyContent, BodyEncoding, Operation, ResponseKind, Shape, TypeId, TypeRef, TypeTable,
};

/// Deepest type nesting followed (cycles end here).
const MAX_DEPTH: usize = 32;

/// The outcome of looking a path up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// The path exists; the last field, when it is a field of a named type.
    Found(Vec<(TypeId, String)>),
    /// The segment that names nothing.
    Missing(String),
    /// A map, `any` or untyped body: any path may exist.
    Opaque,
}

/// The fields a type offers: by wire name, with the named type that
/// declares each, or `None` when any name may exist.
type FieldView<'a> = Option<Vec<(Option<&'a TypeId>, &'a str, &'a TypeRef)>>;

fn fields<'a>(
    types: &'a TypeTable,
    ty: &'a TypeRef,
    owner: Option<&'a TypeId>,
    depth: usize,
) -> FieldView<'a> {
    if depth > MAX_DEPTH {
        return None;
    }
    let (shape, owner) = match ty {
        TypeRef::Named(id) => (&types.get(id)?.shape, Some(id)),
        TypeRef::Inline(shape) => (shape.as_ref(), owner),
    };
    match shape {
        Shape::Record { fields, .. } => Some(
            fields
                .iter()
                .map(|f| (owner, f.wire_name.as_str(), &f.ty))
                .collect(),
        ),
        Shape::Nullable { inner } => self::fields(types, inner, owner, depth + 1),
        Shape::Array { items, .. } => self::fields(types, items, owner, depth + 1),
        Shape::Union(u) => merge(
            u.variants
                .iter()
                .map(|v| self::fields(types, &v.ty, None, depth + 1)),
        ),
        Shape::Intersection { members } => merge(
            members
                .iter()
                .map(|m| self::fields(types, m, None, depth + 1)),
        ),
        Shape::Map { .. } | Shape::Any => None,
        Shape::Primitive { .. } | Shape::Enum { .. } | Shape::Const { .. } | Shape::Never => {
            Some(vec![])
        }
    }
}

fn merge<'a>(views: impl Iterator<Item = FieldView<'a>>) -> FieldView<'a> {
    let mut out = vec![];
    for view in views {
        out.extend(view?);
    }
    Some(out)
}

/// Look `path` up from the fields of `root`.
fn lookup_from<'a>(types: &'a TypeTable, mut view: FieldView<'a>, path: &str) -> Lookup {
    let mut last = vec![];
    for (depth, segment) in path.split('.').enumerate() {
        let Some(available) = view else {
            return Lookup::Opaque;
        };
        let matches: Vec<_> = available
            .into_iter()
            .filter(|(_, wire, _)| *wire == segment)
            .collect();
        if matches.is_empty() {
            return Lookup::Missing(segment.to_string());
        }
        last = matches
            .iter()
            .filter_map(|(owner, wire, _)| owner.map(|o| (o.clone(), wire.to_string())))
            .collect();
        view = merge(
            matches
                .iter()
                .map(|(_, _, ty)| fields(types, ty, None, depth + 1)),
        );
    }
    Lookup::Found(last)
}

/// The JSON content of a body, if any.
fn json(content: &[BodyContent]) -> Option<&TypeRef> {
    content
        .iter()
        .find(|c| c.encoding == BodyEncoding::Json)
        .map(|c| &c.ty)
}

/// The type of the operation's first success response with a JSON body.
pub(crate) fn success_body(op: &Operation) -> Option<&TypeRef> {
    op.responses
        .iter()
        .filter(|r| r.kind == ResponseKind::Success)
        .find_map(|r| json(&r.content))
}

pub(crate) fn request(types: &TypeTable, op: &Operation, path: &str) -> Lookup {
    let params = [
        &op.params.path,
        &op.params.query,
        &op.params.header,
        &op.params.cookie,
    ];
    let mut view: Vec<(Option<&TypeId>, &str, &TypeRef)> = params
        .iter()
        .flat_map(|ps| ps.iter())
        .map(|p| (None, p.wire_name.as_str(), &p.ty))
        .collect();
    if let Some(ty) = op.body.as_ref().and_then(|b| json(&b.content)) {
        match fields(types, ty, None, 0) {
            Some(body_fields) => view.extend(body_fields),
            None => return opaque_unless_param(types, view, path),
        }
    }
    lookup_from(types, Some(view), path)
}

/// A path that names a parameter is looked up; any other is opaque.
fn opaque_unless_param<'a>(
    types: &'a TypeTable,
    view: Vec<(Option<&'a TypeId>, &'a str, &'a TypeRef)>,
    path: &str,
) -> Lookup {
    let first = path.split('.').next().unwrap_or_default();
    if view.iter().any(|(_, wire, _)| *wire == first) {
        lookup_from(types, Some(view), path)
    } else {
        Lookup::Opaque
    }
}

pub(crate) fn response(types: &TypeTable, op: &Operation, path: &str) -> Lookup {
    match success_body(op) {
        Some(ty) => lookup_from(types, fields(types, ty, None, 0), path),
        None => Lookup::Missing(path.split('.').next().unwrap_or_default().to_string()),
    }
}

/// Dotted paths of the fields marked sensitive (`x-agent-sensitive`) in
/// success bodies. Each named type is walked once; a cycle contributes
/// nothing beyond its first pass.
#[derive(Debug)]
pub(crate) struct Sensitive<'a> {
    types: &'a TypeTable,
    memo: BTreeMap<&'a TypeId, Vec<String>>,
    active: BTreeSet<&'a TypeId>,
}

impl<'a> Sensitive<'a> {
    pub fn new(types: &'a TypeTable) -> Self {
        Self {
            types,
            memo: BTreeMap::new(),
            active: BTreeSet::new(),
        }
    }

    pub fn response_fields(&mut self, op: &Operation) -> Vec<String> {
        success_body(op).map_or_else(Vec::new, |ty| self.of(ty))
    }

    fn of(&mut self, ty: &TypeRef) -> Vec<String> {
        let types = self.types;
        match ty {
            TypeRef::Named(id) => {
                let Some(t) = types.get(id) else {
                    return vec![];
                };
                if let Some(paths) = self.memo.get(&t.id) {
                    return paths.clone();
                }
                if !self.active.insert(&t.id) {
                    return vec![];
                }
                let paths = self.shape(&t.shape);
                self.active.remove(&t.id);
                self.memo.insert(&t.id, paths.clone());
                paths
            }
            TypeRef::Inline(shape) => self.shape(shape),
        }
    }

    fn shape(&mut self, shape: &Shape) -> Vec<String> {
        let mut out = vec![];
        match shape {
            Shape::Record { fields, .. } => {
                for f in fields {
                    if f.sensitive {
                        out.push(f.wire_name.clone());
                    } else {
                        out.extend(
                            self.of(&f.ty)
                                .into_iter()
                                .map(|p| format!("{}.{p}", f.wire_name)),
                        );
                    }
                }
            }
            Shape::Nullable { inner } => out = self.of(inner),
            Shape::Array { items, .. } => out = self.of(items),
            Shape::Union(u) => {
                for v in &u.variants {
                    out.extend(self.of(&v.ty));
                }
            }
            Shape::Intersection { members } => {
                for m in members {
                    out.extend(self.of(m));
                }
            }
            _ => {}
        }
        out.sort();
        out.dedup();
        out
    }
}
