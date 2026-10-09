// SPDX-License-Identifier: AGPL-3.0-only
//! The resource tree and method names (inference of the resource tree).
//!
//! Placement of an operation:
//! 1. `tungsten.yml` `resources` of the namespace: the configured resource
//!    whose `path` is the longest segment prefix of the operation's path
//!    (a `{param}` matches any `{param}`; ties go to the first declared).
//! 2. Otherwise inferred: a leading version segment (`v1`, `v2`,
//!    `v1beta`) is dropped, a trailing action (below) is set aside, and the
//!    remaining literal segments nest (`/v1/webhooks/{id}/deliveries` →
//!    `webhooks` › `deliveries`). An operation with no literal segment
//!    goes to a resource named `root`. A segment mixing text and
//!    placeholders (`{date}.csv`) is an item, like a `{param}`, and never
//!    names a resource.
//! 3. An rpc-unflattened method goes under the configured resource of its
//!    HTTP path (or the top level) and then the dotted parts of its id
//!    except the last (`action.send` → `action`); with a single part it is
//!    placed like an HTTP operation.
//!
//! An action is the last segment of a POST path when it is a literal
//! following a `{param}`, every operation on that exact path is a POST, no
//! other path continues below it, and it reads as a verb (a known verb
//! word, or a last word not ending in `s`): `/x/{id}/enable`. A custom
//! method suffix on the last segment (`/v1/users/{id}:archive`,
//! `/v1/users:search`, any HTTP method) is an action too: the operation is
//! placed by the segment without the suffix and named after the suffix.
//!
//! Method names: `naming.operations` wins; then, relative to the
//! resource's path, `list` (GET of a collection: the operation is
//! paginated, or its success JSON body is an array or an object with
//! exactly one array property, or it declares no JSON body and an item
//! path `<path>/{id}` exists or its last literal segment is a plural
//! such as `deliveries`), `get` (any other GET of the resource's own
//! path, a singleton such as `/users/me`), `create` (POST of the resource's own
//! path), `update` (PATCH, PUT) and `delete` on a singleton path, `get` /
//! `update` / `delete` on one item (a POST to one item is `update` when
//! no PUT or PATCH shares its path: the update convention of APIs that
//! create with `POST /x`), the action for actions, and the last
//! dotted part for rpc methods; anything else uses the operation id.
//! Collisions inside a resource are renamed with TG0401.
//!
//! `path_prefix`: the longest common segment prefix of the resource's
//! operations and the path that names it (its configured `path`, the
//! inferred path up to its literal segment, or the HTTP path of rpc
//! methods), without trailing parameters. A resource with neither uses the
//! operations of its descendants.
//!
//! Order: resources and children appear in the order of their first
//! operation in the spec; operations inside a resource keep spec order.

use std::collections::BTreeSet;

use indexmap::IndexMap;
use serde_json::Value;
use tungsten_config::ResourceConfig;
use tungsten_core::Diagnostic;
use tungsten_ir::naming::{Role, split_words};
use tungsten_ir::{
    BodyEncoding, HttpMethod, Ident, Operation, PathSegment, Resource, ResponseKind, Shape, TypeRef,
};
use tungsten_openapi::{DocId, RefTarget};

use crate::ctx::{Ctx, child, pointer};
use crate::names::disambiguate_all;
use crate::operations::{BuiltOp, METHODS};
use crate::params::{path_template, split_query};

/// Name of the resource holding operations with no literal path segment.
const ROOT_RESOURCE: &str = "root";

/// Words that make a trailing POST segment an action.
const VERBS: &[&str] = &[
    "accept",
    "activate",
    "approve",
    "archive",
    "assign",
    "authorize",
    "cancel",
    "capture",
    "charge",
    "check",
    "claim",
    "clone",
    "close",
    "complete",
    "confirm",
    "copy",
    "deactivate",
    "decline",
    "disable",
    "dismiss",
    "duplicate",
    "enable",
    "execute",
    "export",
    "finalize",
    "flag",
    "import",
    "invite",
    "lock",
    "login",
    "logout",
    "mark",
    "merge",
    "move",
    "pause",
    "ping",
    "process",
    "publish",
    "refresh",
    "refund",
    "reject",
    "release",
    "reopen",
    "replay",
    "reset",
    "resend",
    "restart",
    "restore",
    "resume",
    "retry",
    "revoke",
    "rotate",
    "run",
    "send",
    "start",
    "stop",
    "submit",
    "subscribe",
    "suspend",
    "sync",
    "test",
    "transfer",
    "trigger",
    "unarchive",
    "unlock",
    "unpublish",
    "unsubscribe",
    "upload",
    "validate",
    "verify",
    "void",
];

/// A configured resource, flattened.
#[derive(Debug)]
struct Configured {
    /// Resource names from the top level down.
    names: Vec<String>,
    path: Option<Vec<PathSegment>>,
}

/// Every path of the document with the methods it declares.
#[derive(Debug)]
struct PathIndex {
    paths: Vec<(Vec<PathSegment>, Vec<HttpMethod>)>,
}

/// One node while the tree is assembled.
#[derive(Debug)]
struct Node {
    name: Ident,
    /// The path that names this resource, when one does (see [`Level`]).
    anchor: Option<Vec<PathSegment>>,
    ops: Vec<BuiltOp>,
    children: Vec<Node>,
}

/// One resource level of a placement.
#[derive(Debug)]
struct Level {
    name: String,
    /// The path the resource stands for: the configured path, the inferred
    /// path up to and including its literal segment, or the HTTP path of
    /// rpc methods. It joins the operations' paths in `path_prefix`.
    anchor: Option<Vec<PathSegment>>,
}

/// Where an operation goes and what its method is called.
#[derive(Debug)]
struct Placement {
    levels: Vec<Level>,
    method: String,
}

/// Assign names to every operation and build the namespace's resources.
/// Planned operations get names but stay out of the tree.
pub(crate) fn build(
    cx: &mut Ctx<'_>,
    namespace: &str,
    doc: DocId,
    callable: Vec<BuiltOp>,
    planned: &mut [BuiltOp],
) -> Vec<Resource> {
    let index = PathIndex::of(cx, doc);
    let configured = configured(cx, namespace, &index);
    let collections: BTreeSet<String> = callable
        .iter()
        .chain(planned.iter())
        .filter(|b| b.op.method == HttpMethod::Get && returns_collection(cx, &index, &b.op))
        .map(|b| split_query(&b.op.path.raw).0.to_string())
        .collect();
    let paths = Paths {
        index: &index,
        collections: &collections,
    };
    let mut roots: Vec<Node> = vec![];
    for built in planned.iter_mut() {
        let placement = place(cx, &configured, &paths, &built.op);
        built.op.name = Ident::new(&placement.method);
    }
    for mut built in callable {
        let placement = place(cx, &configured, &paths, &built.op);
        built.op.name = Ident::new(&placement.method);
        insert(&mut roots, &placement.levels, built);
    }
    finish_level(cx, &mut roots);
    roots.into_iter().map(into_resource).collect()
}

impl PathIndex {
    fn of(cx: &Ctx<'_>, doc: DocId) -> Self {
        let paths_at = RefTarget {
            doc,
            pointer: "/paths".into(),
        };
        let mut paths = vec![];
        if let Some(Value::Object(map)) = cx.get(&paths_at) {
            for path in map.keys() {
                let item = cx.deref_value(&child(&paths_at, path)).map(|(_, v)| v);
                let methods = METHODS
                    .iter()
                    .filter(|(word, _)| item.is_some_and(|i| i.get(*word).is_some()))
                    .map(|(_, m)| *m)
                    .collect();
                paths.push((path_template(path).segments, methods));
            }
        }
        Self { paths }
    }

    /// Whether no PUT or PATCH is documented on the path of `segments`.
    fn has_no_put_or_patch(&self, segments: &[PathSegment]) -> bool {
        self.paths
            .iter()
            .filter(|(p, _)| same_path(p, segments))
            .all(|(_, methods)| {
                !methods
                    .iter()
                    .any(|m| matches!(m, HttpMethod::Put | HttpMethod::Patch))
            })
    }

    /// Whether `segments` is the prefix of some path of the document.
    fn has_prefix(&self, segments: &[PathSegment]) -> bool {
        self.paths.iter().any(|(p, _)| is_prefix(segments, p))
    }

    /// Whether the last segment of `segments` is an action of a POST.
    fn action(&self, segments: &[PathSegment], method: HttpMethod) -> Option<String> {
        let [.., item, PathSegment::Literal { value }] = segments else {
            return None;
        };
        if !is_item(item) || method != HttpMethod::Post || !reads_as_verb(value) {
            return None;
        }
        let only_post = self
            .paths
            .iter()
            .filter(|(p, _)| same_path(p, segments))
            .all(|(_, methods)| methods.iter().all(|m| *m == HttpMethod::Post));
        let has_children = self
            .paths
            .iter()
            .any(|(p, _)| p.len() > segments.len() && is_prefix(segments, p));
        (only_post && !has_children).then(|| value.clone())
    }
}

/// What placement knows about the namespace's paths.
struct Paths<'p> {
    index: &'p PathIndex,
    /// Raw paths (without a query string) whose GET returns a collection.
    collections: &'p BTreeSet<String>,
}

/// Whether a GET returns a collection: it is paginated, or its first
/// success JSON body is an array or an object with exactly one array
/// property, or (no JSON body says otherwise) an item path continues it
/// or its last literal segment reads as a plural.
fn returns_collection(cx: &Ctx<'_>, index: &PathIndex, op: &Operation) -> bool {
    if op.pagination.is_some() {
        return true;
    }
    let Some(body) = op
        .responses
        .iter()
        .filter(|r| r.kind == ResponseKind::Success)
        .flat_map(|r| &r.content)
        .find(|c| c.encoding == BodyEncoding::Json)
    else {
        let segments = &op.path.segments;
        let has_items = index.paths.iter().any(|(p, _)| {
            p.len() == segments.len() + 1 && is_prefix(segments, p) && p.last().is_some_and(is_item)
        });
        let plural = match segments.last() {
            Some(PathSegment::Literal { value }) => split_words(value)
                .last()
                .is_some_and(|w| w.ends_with('s') && !w.ends_with("ss")),
            _ => false,
        };
        return has_items || plural;
    };
    let is_array = |ty: &TypeRef| matches!(non_null(cx, ty), Some(Shape::Array { .. }));
    match non_null(cx, &body.ty) {
        Some(Shape::Array { .. }) => true,
        Some(Shape::Record { fields, .. }) => {
            fields.iter().filter(|f| is_array(&f.ty)).count() == 1
        }
        _ => false,
    }
}

/// The shape behind a type, looking through `Nullable`.
fn non_null<'s>(cx: &'s Ctx<'_>, ty: &'s TypeRef) -> Option<&'s Shape> {
    match cx.tb.shape_of(ty)? {
        Shape::Nullable { inner } => cx.tb.shape_of(inner),
        shape => Some(shape),
    }
}

/// A custom method suffix on the last segment (`{id}:archive`,
/// `users:search`): the segments with the suffix removed, and the verb.
fn custom_method(segments: &[PathSegment]) -> Option<(Vec<PathSegment>, String)> {
    let (last, head) = segments.split_last()?;
    let mut parts = match last {
        PathSegment::Template { parts } => parts.clone(),
        literal @ PathSegment::Literal { .. } => vec![literal.clone()],
        PathSegment::Param { .. } => return None,
    };
    let Some(PathSegment::Literal { value }) = parts.pop() else {
        return None;
    };
    let (before, verb) = value.rsplit_once(':')?;
    if verb.is_empty() || !verb.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    if !before.is_empty() {
        parts.push(PathSegment::Literal {
            value: before.to_string(),
        });
    }
    let base = match parts.len() {
        0 => return None,
        1 => parts.remove(0),
        _ => PathSegment::Template { parts },
    };
    let mut out = head.to_vec();
    out.push(base);
    Some((out, verb.to_string()))
}

/// A segment that stands for one item: a `{param}` or a template with one.
fn is_item(segment: &PathSegment) -> bool {
    !matches!(segment, PathSegment::Literal { .. })
}

pub(crate) fn reads_as_verb(segment: &str) -> bool {
    let words = split_words(segment);
    words.iter().any(|w| VERBS.contains(&w.as_str()))
        || words.last().is_some_and(|w| !w.ends_with('s'))
}

/// Segment equality where any parameter matches any parameter.
fn same_segment(a: &PathSegment, b: &PathSegment) -> bool {
    match (a, b) {
        (PathSegment::Param { .. }, PathSegment::Param { .. }) => true,
        (PathSegment::Literal { value: x }, PathSegment::Literal { value: y }) => x == y,
        (PathSegment::Template { parts: x }, PathSegment::Template { parts: y }) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same_segment(a, b))
        }
        _ => false,
    }
}

fn is_prefix(prefix: &[PathSegment], path: &[PathSegment]) -> bool {
    prefix.len() <= path.len() && prefix.iter().zip(path).all(|(a, b)| same_segment(a, b))
}

fn same_path(a: &[PathSegment], b: &[PathSegment]) -> bool {
    a.len() == b.len() && is_prefix(a, b)
}

/// `v1`, `v2`, `v1beta`, `v2alpha1`.
pub(crate) fn is_version(segment: &str) -> bool {
    let Some(rest) = segment.strip_prefix('v') else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let tail = &rest[digits..];
    let letters = tail.bytes().take_while(u8::is_ascii_lowercase).count();
    digits > 0 && tail[letters..].bytes().all(|b| b.is_ascii_digit())
}

/// Flatten the namespace's configured resources; paths that match no path
/// of the document are TG0604 warnings.
fn configured(cx: &mut Ctx<'_>, namespace: &str, index: &PathIndex) -> Vec<Configured> {
    let cfg = cx.cfg;
    let mut out = vec![];
    if let Some(top) = cfg.resources.get(namespace) {
        let base = pointer(["resources", namespace]);
        flatten(cx, index, top, &[], &base, &mut out);
    }
    out
}

fn flatten(
    cx: &mut Ctx<'_>,
    index: &PathIndex,
    level: &IndexMap<String, ResourceConfig>,
    parents: &[String],
    at: &str,
    out: &mut Vec<Configured>,
) {
    for (name, rc) in level {
        let here = tungsten_openapi::join_pointer(at, name);
        let mut names = parents.to_vec();
        names.push(name.clone());
        let path = rc.path.as_ref().map(|p| path_template(p).segments);
        if let (Some(raw), Some(segments)) = (&rc.path, &path)
            && !index.has_prefix(segments)
        {
            cx.report_manifest(
                Diagnostic::warning(
                    "TG0604",
                    format!(
                        "resource `{}` has path {raw}, which matches no path of the input",
                        names.join(".")
                    ),
                ),
                &tungsten_openapi::join_pointer(&here, "path"),
            );
        }
        out.push(Configured {
            names: names.clone(),
            path,
        });
        flatten(
            cx,
            index,
            &rc.children,
            &names,
            &tungsten_openapi::join_pointer(&here, "children"),
            out,
        );
    }
}

/// Decide the resource and method name of an operation.
fn place(cx: &Ctx<'_>, configured: &[Configured], paths: &Paths<'_>, op: &Operation) -> Placement {
    let custom = custom_method(&op.path.segments);
    let segments = custom.as_ref().map_or(&op.path.segments, |(base, _)| base);
    let best = configured
        .iter()
        .filter_map(|c| c.path.as_ref().map(|p| (c, p)))
        .filter(|(_, p)| is_prefix(p, segments))
        .fold(None::<(&Configured, usize)>, |best, (c, p)| match best {
            Some((_, len)) if len >= p.len() => best,
            _ => Some((c, p.len())),
        });
    let action = match &custom {
        Some((_, verb)) => Some(verb.clone()),
        None => paths.index.action(segments, op.method),
    };
    // A custom method keeps its item segment; a trailing action is set aside.
    let set_aside = action.is_some() && custom.is_none();
    let (levels, relative) = match best {
        Some((c, len)) => (configured_levels(configured, c), segments[len..].to_vec()),
        None => inferred(segments, set_aside),
    };
    let local = op.id.0.split_once('.').map_or(op.id.0.as_str(), |(_, l)| l);
    let override_name = cx.cfg.naming.operations.get(&op.id.0).cloned();
    if op.rpc.is_some() {
        let parts: Vec<&str> = local.split('.').collect();
        if let [resource_parts @ .., last] = parts.as_slice()
            && !resource_parts.is_empty()
        {
            let mut levels = best
                .map(|(c, _)| configured_levels(configured, c))
                .unwrap_or_default();
            levels.extend(resource_parts.iter().map(|p| Level {
                name: p.to_string(),
                anchor: Some(segments.to_vec()),
            }));
            return Placement {
                levels,
                method: override_name.unwrap_or_else(|| last.to_string()),
            };
        }
        return Placement {
            levels,
            method: override_name.unwrap_or_else(|| local.to_string()),
        };
    }
    let method = match &custom {
        Some((_, verb)) => override_name.unwrap_or_else(|| verb.clone()),
        None => {
            let collection = paths.collections.contains(split_query(&op.path.raw).0);
            override_name
                .or_else(|| {
                    crud_name(
                        op.method,
                        &relative,
                        action.as_deref(),
                        collection,
                        paths.index.has_no_put_or_patch(segments),
                    )
                })
                .unwrap_or_else(|| local.to_string())
        }
    };
    Placement { levels, method }
}

/// The levels of a configured resource: each ancestor with its own
/// configured path as anchor.
fn configured_levels(configured: &[Configured], target: &Configured) -> Vec<Level> {
    (1..=target.names.len())
        .map(|depth| {
            let names = &target.names[..depth];
            Level {
                name: names[depth - 1].clone(),
                anchor: configured
                    .iter()
                    .find(|c| c.names == names)
                    .and_then(|c| c.path.clone()),
            }
        })
        .collect()
}

/// Inferred levels (each anchored at the path up to its literal segment)
/// and the path relative to the innermost resource.
fn inferred(segments: &[PathSegment], has_action: bool) -> (Vec<Level>, Vec<PathSegment>) {
    let start = match segments.first() {
        Some(PathSegment::Literal { value }) if is_version(value) => 1,
        _ => 0,
    };
    let end = if has_action {
        segments.len().saturating_sub(1).max(start)
    } else {
        segments.len()
    };
    let mut levels: Vec<Level> = segments[start..end]
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s {
            PathSegment::Literal { value } => Some(Level {
                name: value.clone(),
                anchor: Some(segments[..start + i + 1].to_vec()),
            }),
            PathSegment::Param { .. } | PathSegment::Template { .. } => None,
        })
        .collect();
    let last_literal = segments[start..end]
        .iter()
        .rposition(|s| matches!(s, PathSegment::Literal { .. }))
        .map_or(start, |i| start + i + 1);
    let relative = segments[last_literal..].to_vec();
    if levels.is_empty() {
        levels.push(Level {
            name: ROOT_RESOURCE.to_string(),
            anchor: Some(segments[..start].to_vec()),
        });
    }
    (levels, relative)
}

/// CRUD and action names from the path relative to the resource.
/// `collection`: a GET of this exact path returns a collection.
/// `post_updates`: no PUT or PATCH is documented on the path, so a POST to
/// an item is its update.
fn crud_name(
    method: HttpMethod,
    relative: &[PathSegment],
    action: Option<&str>,
    collection: bool,
    post_updates: bool,
) -> Option<String> {
    let name = match (relative, method) {
        ([], HttpMethod::Get) if collection => "list",
        ([], HttpMethod::Get) => "get",
        ([], HttpMethod::Post) => "create",
        ([], HttpMethod::Patch | HttpMethod::Put) if !collection => "update",
        ([], HttpMethod::Delete) if !collection => "delete",
        ([item], HttpMethod::Get) if is_item(item) => "get",
        ([item], HttpMethod::Patch | HttpMethod::Put) if is_item(item) => "update",
        ([item], HttpMethod::Delete) if is_item(item) => "delete",
        ([item], HttpMethod::Post) if is_item(item) && post_updates => "update",
        ([item, PathSegment::Literal { value }], HttpMethod::Post)
            if is_item(item) && action == Some(value.as_str()) =>
        {
            value
        }
        _ => return None,
    };
    Some(name.to_string())
}

/// Insert an operation under its levels, creating nodes in
/// first-appearance order. A node keeps the anchor it was created with.
fn insert(nodes: &mut Vec<Node>, levels: &[Level], built: BuiltOp) {
    let Some((first, rest)) = levels.split_first() else {
        return;
    };
    let index = match nodes.iter().position(|n| n.name.wire == first.name) {
        Some(i) => i,
        None => {
            nodes.push(Node {
                name: Ident::new(&first.name),
                anchor: first.anchor.clone(),
                ops: vec![],
                children: vec![],
            });
            nodes.len() - 1
        }
    };
    let node = &mut nodes[index];
    if rest.is_empty() {
        node.ops.push(built);
    } else {
        insert(&mut node.children, rest, built);
    }
}

/// Disambiguate sibling resource names and the methods of every node,
/// recursively (TG0401).
fn finish_level(cx: &mut Ctx<'_>, level: &mut [Node]) {
    let mut names: Vec<Ident> = level.iter().map(|n| n.name.clone()).collect();
    for i in disambiguate_all(&mut names, Role::Module) {
        if let Some(at) = first_source(&level[i]) {
            cx.report(
                Diagnostic::warning(
                    "TG0401",
                    format!(
                        "resource `{}` collides with a sibling resource name; renamed to `{}`",
                        level[i].name.wire,
                        names[i].snake()
                    ),
                ),
                &at,
            );
        }
    }
    for (node, name) in level.iter_mut().zip(names) {
        node.name = name;
        let mut methods: Vec<Ident> = node.ops.iter().map(|b| b.op.name.clone()).collect();
        for i in disambiguate_all(&mut methods, Role::Method) {
            let built = &node.ops[i];
            cx.report(
                Diagnostic::warning(
                    "TG0401",
                    format!(
                        "method `{}` of `{}` collides with another method of resource `{}`; renamed to `{}`",
                        built.op.name.wire,
                        built.op.id.0,
                        node.name.wire,
                        methods[i].camel()
                    ),
                )
                .with_help("name the method in tungsten.yml `naming.operations`"),
                &built.at,
            );
        }
        for (built, name) in node.ops.iter_mut().zip(methods) {
            built.op.name = name;
        }
        finish_level(cx, &mut node.children);
    }
}

fn first_source(node: &Node) -> Option<RefTarget> {
    node.ops
        .first()
        .map(|b| b.at.clone())
        .or_else(|| node.children.iter().find_map(first_source))
}

fn into_resource(node: Node) -> Resource {
    let mut paths: Vec<Vec<PathSegment>> = node
        .ops
        .iter()
        .map(|b| placed_path(&b.op))
        .chain(node.anchor.clone())
        .collect();
    if paths.is_empty() {
        collect_paths(&node.children, &mut paths);
    }
    let paths: Vec<&[PathSegment]> = paths.iter().map(Vec::as_slice).collect();
    let path_prefix = common_prefix(&paths);
    Resource {
        name: node.name,
        path_prefix,
        doc: None,
        operations: node.ops.into_iter().map(|b| b.op).collect(),
        children: node.children.into_iter().map(into_resource).collect(),
    }
}

fn collect_paths(nodes: &[Node], out: &mut Vec<Vec<PathSegment>>) {
    for n in nodes {
        out.extend(n.ops.iter().map(|b| placed_path(&b.op)));
        collect_paths(&n.children, out);
    }
}

/// The path an operation is placed by: without a custom method suffix.
fn placed_path(op: &Operation) -> Vec<PathSegment> {
    custom_method(&op.path.segments).map_or_else(|| op.path.segments.clone(), |(base, _)| base)
}

/// The longest common segment prefix, without trailing parameters
/// (`/v1/webhooks/{endpoint_id}/deliveries`).
fn common_prefix(paths: &[&[PathSegment]]) -> String {
    let Some(first) = paths.first() else {
        return "/".into();
    };
    let mut len = first.len();
    for p in &paths[1..] {
        len = len.min(
            first
                .iter()
                .zip(p.iter())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    let mut prefix = &first[..len];
    while let [rest @ .., last] = prefix
        && is_item(last)
    {
        prefix = rest;
    }
    let rendered: Vec<String> = prefix.iter().map(render).collect();
    format!("/{}", rendered.join("/"))
}

fn render(segment: &PathSegment) -> String {
    match segment {
        PathSegment::Literal { value } => value.clone(),
        PathSegment::Param { name } => format!("{{{name}}}"),
        PathSegment::Template { parts } => parts.iter().map(render).collect(),
    }
}
