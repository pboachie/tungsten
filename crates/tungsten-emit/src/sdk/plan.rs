// SPDX-License-Identifier: AGPL-3.0-only
//! [`SdkPlan`]: the structural decisions every SDK emitter makes, without
//! any spelling.
//!
//! The TypeScript, Python and Rust emitters each decide the same things in
//! their own `plan.rs`: which named types exist per namespace and in which
//! order, which are on reference cycles, how unions, enums and `allOf`
//! types are represented, which operations are callable and where they sit
//! in the resource tree, how an operation's arguments are laid out
//! ([`crate::args`]), what its success value is, how it pages and streams.
//! This module decides them once, from the IR alone, for the emitters of
//! the further SDK languages. Names come later, from [`super::Namer`] for one
//! target.
//!
//! Everything is deterministic (IR order, or sorted ids) and total: a plan
//! is built for every IR, and nothing here panics on a valid one.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_core::Diagnostics;
use tungsten_ir::{
    Additional, BodyEncoding, Field, IdempotencyKind, Ir, NamedType, Operation, OperationStatus,
    Pagination, PaginationStyle, Param, ParamRole, Presence, PreviewMode, Resource, ResponseKind,
    Safety, Shape, StatusMatch, TypeId, TypeRef, Union, UnionStrategy,
};

use super::macros::{MacroIssue, MacroPlan, plan_macros};
use crate::args::{self, ArgsLayout, BodyArg, ParamLocation};

/// The plan of one SDK: types, operations, the resource tree, the client and
/// the macros. Build it with [`SdkPlan::new`].
#[derive(Debug, Clone)]
pub struct SdkPlan<'a> {
    pub ir: &'a Ir,
    /// Model namespaces (every type's namespace and every IR namespace),
    /// sorted by name.
    pub namespaces: Vec<ModelNamespace>,
    /// Every named type of the IR by id.
    pub types: BTreeMap<TypeId, TypePlan>,
    /// Callable operations (not `planned`) in IR order: namespace order,
    /// then depth-first resource order ([`Ir::operations`]).
    pub operations: Vec<OpPlan<'a>>,
    /// Index into [`Self::operations`] by operation id.
    pub op_by_id: BTreeMap<String, usize>,
    /// Every resource, depth first in IR order.
    pub resources: Vec<ResourcePlan<'a>>,
    pub client: ClientPlan,
    /// The macros in the canonical form, in IR order.
    pub macros: Vec<MacroPlan<'a>>,
    /// The macros that are not in the canonical form, in IR order. Each
    /// emitter reports them under its own code (B+2 of its TG10xx block).
    pub macro_issues: Vec<MacroIssue>,
}

/// One model namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelNamespace {
    /// Namespace name as in the IR (`public`).
    pub name: String,
    /// Its words (`naming::split_words` of the name).
    pub words: Vec<String>,
    /// Its named types in declaration order: every type after the types it
    /// references, except within a reference cycle (by id there).
    pub types: Vec<TypeId>,
}

/// How a named type is represented.
#[derive(Debug, Clone, PartialEq)]
pub struct TypePlan {
    pub id: TypeId,
    pub namespace: String,
    /// The type's name words.
    pub words: Vec<String>,
    /// Strongly connected component; components are numbered dependencies
    /// first.
    pub scc: usize,
    /// The component is a real cycle (more than one type, or a self edge).
    pub cyclic: bool,
    /// A callable operation (or an error model) reaches the type. Without
    /// `types.prune_unreferenced` the IR also holds unreachable types; they
    /// are planned all the same.
    pub reachable: bool,
    pub kind: TypeKind,
}

/// The representation of a named type.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    /// A record: one [`Presence`] per field, in field order.
    Record {
        presence: Vec<Presence>,
    },
    Enum(EnumPlan),
    Union(UnionPlan),
    /// An `allOf` the builder could not flatten (`Shape::Intersection`).
    AllOf(AllOfPlan),
    /// Anything else (a primitive, array, map, constant, nullable, `any`,
    /// `never`): an alias of the shape.
    Alias,
}

/// How an enum's values are told apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumPlan {
    /// Every value is a string.
    Strings,
    /// Every value is an integer (an `i64`).
    Integers,
    /// Values of different JSON types, or numbers that are not integers:
    /// the JSON types present, sorted (`boolean`, `integer`, `null`,
    /// `number`, `string`, `array`, `object`).
    Mixed { json_types: Vec<&'static str> },
    /// No value at all.
    Empty,
}

/// How a union's variants are told apart.
#[derive(Debug, Clone, PartialEq)]
pub enum UnionPlan {
    /// A discriminator property selects the variant: every variant is a
    /// named type and has a tag. `variants` are (variant index, its tag
    /// values: the variant's tag and every further mapping value that
    /// names the same type), in IR order.
    Tagged {
        tag: String,
        variants: Vec<(usize, Vec<String>)>,
    },
    /// A value check tells the variants apart (IR `Literal` strategy, or a
    /// constant variant): each variant with what identifies it, in IR
    /// order.
    LiteralTagged { variants: Vec<(usize, LiteralKey)> },
    /// Variants tried in IR candidate order, the first that accepts the
    /// value wins (TG0301). A `Tagged` IR union lands here when a variant
    /// has no tag value or is not a named type (B+5 of the emitter's block).
    Untagged { ordered: Vec<usize> },
    /// A union without variants.
    Empty,
}

/// What identifies a variant of a [`UnionPlan::LiteralTagged`] union.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralKey {
    /// A constant: exactly this JSON value.
    Value(serde_json::Value),
    /// Any value of this JSON type (`string`, `integer`, `number`,
    /// `boolean`, `null`, `array`, `object`), or `any`.
    JsonType(&'static str),
}

/// How an unflattened `allOf` is represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllOfPlan {
    /// Every member is a record without typed extras and no wire name
    /// repeats: one record of the members' fields, as (member index, field
    /// index) in member order. Closed when any member is closed.
    Merged {
        fields: Vec<(usize, usize)>,
        closed: bool,
    },
    /// The members cannot be merged: a typed result is the runtime's JSON
    /// value, checked against every member.
    Intersection,
}

/// One callable operation.
#[derive(Debug, Clone)]
pub struct OpPlan<'a> {
    pub op: &'a Operation,
    /// Namespace of the operation (its error model).
    pub namespace: String,
    /// Index into [`SdkPlan::resources`].
    pub resource: usize,
    /// Words of the operation's key: the operation id, without the
    /// namespace prefix in a single-namespace API (`list webhook endpoints`).
    pub key_words: Vec<String>,
    /// The arguments object ([`crate::args::args_layout`]).
    pub layout: ArgsLayout<'a>,
    /// Every key of the arguments object, in layout order: parameters, then
    /// merged body fields or the body argument.
    pub arguments: Vec<ArgPlan<'a>>,
    /// Parameters supplied by the call options, the auth profile or the
    /// runtime (idempotency key, origin, auth, constant), in layout order.
    pub supplied: Vec<SuppliedParam<'a>>,
    pub success: SuccessPlan,
    /// A success may come without a body (a typed result is optional).
    pub bodiless_success: bool,
    pub page: Option<PagePlan>,
    pub stream: Option<StreamPlan>,
    /// The operation has a preview with a confirmation token (it is not
    /// read-only and its preview mode is not `none`).
    pub has_preview: bool,
    pub safety: Safety,
    pub idempotency: IdempotencyKind,
}

/// One key of an operation's arguments object.
#[derive(Debug, Clone)]
pub struct ArgPlan<'a> {
    /// The key in the arguments object (the TypeScript argument layout).
    pub key: String,
    /// The name on the wire (the parameter's wire name, the field's wire
    /// name, or the key of a whole body).
    pub wire: String,
    /// The words a target's name is rendered from.
    pub words: Vec<String>,
    pub source: ArgSource<'a>,
    pub presence: Presence,
    /// The value's type; `None` for a bytes or text body.
    pub ty: Option<TypeRef>,
    pub sensitive: bool,
}

/// Where an argument goes.
#[derive(Debug, Clone)]
pub enum ArgSource<'a> {
    Param {
        location: ParamLocation,
        param: &'a Param,
    },
    /// A field of a merged JSON body.
    Field(&'a Field),
    /// The whole body.
    Body {
        encoding: BodyEncoding,
        media_type: String,
    },
}

/// A parameter that is not an argument.
#[derive(Debug, Clone)]
pub struct SuppliedParam<'a> {
    /// Its key in the layout (informational).
    pub key: String,
    pub location: ParamLocation,
    pub role: ParamRole,
    pub param: &'a Param,
}

/// The success value of an operation, decided like the Rust SDK's response
/// enums: the bodies of the success responses (the first JSON content of
/// each, else its first content) are compared by type.
#[derive(Debug, Clone, PartialEq)]
pub enum SuccessPlan {
    /// No success response has a body.
    None,
    /// One JSON type (JSON Lines: an array of the line type).
    Json(TypeRef),
    /// A binary body.
    Bytes,
    /// A text body.
    Text,
    /// Different JSON bodies by exact status: every success response has an
    /// exact status and a JSON body or none, in IR order.
    ByStatus(Vec<(StatusMatch, StatusBody)>),
    /// No typed result: the runtime's JSON value (or raw bytes), for this
    /// reason.
    Raw(String),
}

/// The body of one status of [`SuccessPlan::ByStatus`].
#[derive(Debug, Clone, PartialEq)]
pub enum StatusBody {
    Empty,
    Json(TypeRef),
}

/// How an operation pages.
#[derive(Debug, Clone, PartialEq)]
pub struct PagePlan {
    pub pagination: Pagination,
    /// The type of one item, when the success body is a JSON type whose
    /// `items_field` is an array (the body itself when the field is empty).
    pub item: Option<TypeRef>,
    /// Arguments keys of the request parameters the runtime sets: the
    /// cursor, offset or page parameter, and the page size or limit one.
    pub request_key: Option<String>,
    pub size_key: Option<String>,
}

/// The event stream of an operation.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamPlan {
    /// The type of one event's `data`.
    pub event: TypeRef,
    /// Events are typed (the event type is not `any`).
    pub typed: bool,
    /// A `data` value that ends the stream (`[DONE]`).
    pub done: Option<String>,
    /// The boolean request body field that selects the stream.
    pub flag: Option<String>,
    /// The same status also answers with a body that is not a stream.
    pub also_plain: bool,
}

/// One resource.
#[derive(Debug, Clone)]
pub struct ResourcePlan<'a> {
    pub resource: &'a Resource,
    pub namespace: String,
    /// Namespace words (multi-namespace APIs only) followed by the words of
    /// every resource from the top to this one.
    pub path_words: Vec<String>,
    /// Index of the parent resource.
    pub parent: Option<usize>,
    /// Its operations and child resources, in IR order.
    pub members: Vec<MemberPlan>,
}

/// A member of a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberPlan {
    /// Index into [`SdkPlan::operations`].
    Operation(usize),
    /// Index into [`SdkPlan::resources`].
    Child(usize),
}

/// The client's members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientPlan {
    /// A single-namespace API: the top-level resources (indexes into
    /// [`SdkPlan::resources`]).
    Resources(Vec<usize>),
    /// A multi-namespace API: one member per namespace, in IR order.
    Namespaces(Vec<ClientNamespace>),
}

/// One namespace member of a multi-namespace client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientNamespace {
    pub name: String,
    pub words: Vec<String>,
    /// Its top-level resources.
    pub resources: Vec<usize>,
}

impl<'a> SdkPlan<'a> {
    /// The plan of `ir`, and no diagnostics: problems a language reports
    /// (macros not in the canonical form) are data of the plan
    /// ([`Self::macro_issues`]), so each emitter words them under its own
    /// code. The diagnostics are kept for additions.
    pub fn new(ir: &'a Ir) -> (SdkPlan<'a>, Diagnostics) {
        let multi = ir.namespaces.len() > 1;
        let (types, namespaces) = plan_types(ir);
        let mut plan = SdkPlan {
            ir,
            namespaces,
            types,
            operations: vec![],
            op_by_id: BTreeMap::new(),
            resources: vec![],
            client: ClientPlan::Resources(vec![]),
            macros: vec![],
            macro_issues: vec![],
        };
        plan.plan_tree(multi);
        let (macros, issues) = plan_macros(&plan);
        plan.macros = macros;
        plan.macro_issues = issues;
        (plan, Diagnostics::new())
    }

    /// Whether the API has more than one namespace.
    pub fn multi(&self) -> bool {
        matches!(self.client, ClientPlan::Namespaces(_))
    }

    /// The operation with this id.
    pub fn operation(&self, id: &str) -> Option<&OpPlan<'a>> {
        self.op_by_id.get(id).map(|&i| &self.operations[i])
    }

    /// The shape a reference denotes, through named types and their
    /// nullable wrappers ([`crate::args::resolve`]).
    pub fn resolve<'r>(&'r self, ty: &'r TypeRef) -> Option<&'r Shape> {
        args::resolve(self.ir, ty)
    }

    /// Resources, operations and the client, walking the tree once.
    fn plan_tree(&mut self, multi: bool) {
        let ir = self.ir;
        let mut namespaces = vec![];
        for ns in &ir.namespaces {
            let prefix = if multi { ns.name.words.clone() } else { vec![] };
            let mut top = vec![];
            for r in &ns.resources {
                top.push(self.walk(r, &ns.name.wire, &prefix, None, multi));
            }
            namespaces.push(ClientNamespace {
                name: ns.name.wire.clone(),
                words: ns.name.words.clone(),
                resources: top,
            });
        }
        self.client = if multi {
            ClientPlan::Namespaces(namespaces)
        } else {
            ClientPlan::Resources(namespaces.into_iter().flat_map(|n| n.resources).collect())
        };
    }

    fn walk(
        &mut self,
        r: &'a Resource,
        ns: &str,
        prefix: &[String],
        parent: Option<usize>,
        multi: bool,
    ) -> usize {
        let index = self.resources.len();
        let path_words: Vec<String> = prefix.iter().chain(&r.name.words).cloned().collect();
        self.resources.push(ResourcePlan {
            resource: r,
            namespace: ns.to_string(),
            path_words: path_words.clone(),
            parent,
            members: vec![],
        });
        let mut members = vec![];
        // Planned operations live outside the tree; never make one callable.
        for o in r
            .operations
            .iter()
            .filter(|o| !matches!(o.status, OperationStatus::Planned { .. }))
        {
            let op = plan_op(self.ir, o, ns, index, multi);
            let at = self.operations.len();
            self.op_by_id.insert(o.id.0.clone(), at);
            self.operations.push(op);
            members.push(MemberPlan::Operation(at));
        }
        for c in &r.children {
            let child = self.walk(c, ns, &path_words, Some(index), multi);
            members.push(MemberPlan::Child(child));
        }
        self.resources[index].members = members;
        index
    }
}

// ---------------------------------------------------------------- types

/// Every named type a shape references directly (through inline shapes).
pub fn shape_refs(shape: &Shape, out: &mut Vec<TypeId>) {
    let add = |r: &TypeRef, out: &mut Vec<TypeId>| match r {
        TypeRef::Named(id) => out.push(id.clone()),
        TypeRef::Inline(s) => shape_refs(s, out),
    };
    match shape {
        Shape::Array { items, .. } => add(items, out),
        Shape::Map { values } => add(values, out),
        Shape::Nullable { inner } => add(inner, out),
        Shape::Record { fields, additional } => {
            for f in fields {
                add(&f.ty, out);
            }
            if let Additional::Typed { values } = additional {
                add(values, out);
            }
        }
        Shape::Union(u) => {
            for v in &u.variants {
                add(&v.ty, out);
            }
        }
        Shape::Intersection { members } => {
            for m in members {
                add(m, out);
            }
        }
        Shape::Primitive { .. }
        | Shape::Enum { .. }
        | Shape::Const { .. }
        | Shape::Any
        | Shape::Never => {}
    }
}

fn type_ref_ids(ty: &TypeRef, out: &mut Vec<TypeId>) {
    match ty {
        TypeRef::Named(id) => out.push(id.clone()),
        TypeRef::Inline(s) => shape_refs(s, out),
    }
}

/// Strongly connected components of a graph given as sorted adjacency
/// lists, numbered so that every component comes after the components it
/// reaches (Tarjan's algorithm, iterative).
fn components(adjacency: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adjacency.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = vec![];
    let mut out: Vec<Vec<usize>> = vec![];
    let mut next = 0usize;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        // (node, position in its adjacency list)
        let mut frames: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut at)) = frames.last_mut() {
            if let Some(&w) = adjacency[v].get(*at) {
                *at += 1;
                if index[w] == usize::MAX {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    frames.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut comp = vec![];
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                comp.sort_unstable();
                out.push(comp);
            }
        }
    }
    out
}

fn plan_types(ir: &Ir) -> (BTreeMap<TypeId, TypePlan>, Vec<ModelNamespace>) {
    let table = &ir.types.types;
    let index: BTreeMap<&TypeId, usize> =
        table.iter().enumerate().map(|(i, t)| (&t.id, i)).collect();
    let mut adjacency: Vec<Vec<usize>> = vec![];
    let mut self_edge = vec![false; table.len()];
    for (i, t) in table.iter().enumerate() {
        let mut refs = vec![];
        shape_refs(&t.shape, &mut refs);
        let targets: BTreeSet<usize> = refs.iter().filter_map(|r| index.get(r).copied()).collect();
        self_edge[i] = targets.contains(&i);
        adjacency.push(targets.into_iter().collect());
    }
    let sccs = components(&adjacency);
    let mut scc_of = vec![0usize; table.len()];
    let mut cyclic = vec![false; table.len()];
    let mut order: Vec<usize> = vec![];
    for (k, comp) in sccs.iter().enumerate() {
        let is_cycle = comp.len() > 1 || comp.iter().any(|&m| self_edge[m]);
        for &m in comp {
            scc_of[m] = k;
            cyclic[m] = is_cycle;
        }
        order.extend(comp.iter().copied());
    }
    let reachable = reachable_types(ir, &index, &adjacency);

    let mut ns_names: BTreeSet<String> = table.iter().map(|t| t.namespace.clone()).collect();
    ns_names.extend(ir.namespaces.iter().map(|n| n.name.wire.clone()));
    let namespaces = ns_names
        .iter()
        .map(|name| ModelNamespace {
            name: name.clone(),
            words: tungsten_ir::naming::split_words(name),
            types: order
                .iter()
                .filter(|&&i| &table[i].namespace == name)
                .map(|&i| table[i].id.clone())
                .collect(),
        })
        .collect();
    let types = table
        .iter()
        .enumerate()
        .map(|(i, t)| {
            (
                t.id.clone(),
                TypePlan {
                    id: t.id.clone(),
                    namespace: t.namespace.clone(),
                    words: t.name.words.clone(),
                    scc: scc_of[i],
                    cyclic: cyclic[i],
                    reachable: reachable[i],
                    kind: type_kind(ir, t),
                },
            )
        })
        .collect();
    (types, namespaces)
}

/// Which named types a callable operation, a macro's added input or an
/// error model reaches, directly or through other types.
fn reachable_types(
    ir: &Ir,
    index: &BTreeMap<&TypeId, usize>,
    adjacency: &[Vec<usize>],
) -> Vec<bool> {
    let mut roots: Vec<TypeId> = vec![];
    for op in ir.operations() {
        if matches!(op.status, OperationStatus::Planned { .. }) {
            continue;
        }
        let params = &op.params;
        for p in params
            .path
            .iter()
            .chain(&params.query)
            .chain(&params.header)
            .chain(&params.cookie)
        {
            type_ref_ids(&p.ty, &mut roots);
        }
        if let Some(b) = &op.body {
            for c in &b.content {
                type_ref_ids(&c.ty, &mut roots);
            }
        }
        for r in &op.responses {
            for c in &r.content {
                type_ref_ids(&c.ty, &mut roots);
            }
            for h in &r.headers {
                type_ref_ids(&h.ty, &mut roots);
            }
        }
        if let Some(s) = &op.stream {
            type_ref_ids(&s.event, &mut roots);
        }
    }
    roots.extend(ir.errors.envelope.iter().cloned());
    for ns in &ir.namespaces {
        roots.extend(ns.errors.envelope.iter().cloned());
    }
    let mut seen = vec![false; adjacency.len()];
    let mut todo: Vec<usize> = roots.iter().filter_map(|r| index.get(r).copied()).collect();
    while let Some(i) = todo.pop() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        todo.extend(adjacency[i].iter().copied().filter(|&j| !seen[j]));
    }
    seen
}

fn type_kind(ir: &Ir, t: &NamedType) -> TypeKind {
    match &t.shape {
        Shape::Record { fields, .. } => TypeKind::Record {
            presence: fields.iter().map(|f| f.presence).collect(),
        },
        Shape::Enum { values, .. } => TypeKind::Enum(enum_plan(values)),
        Shape::Union(u) => TypeKind::Union(union_plan(ir, u)),
        Shape::Intersection { members } => TypeKind::AllOf(all_of_plan(ir, members)),
        _ => TypeKind::Alias,
    }
}

/// The JSON type of a value: `null`, `boolean`, `integer` (an `i64` or
/// `u64`), `number`, `string`, `array` or `object`.
pub fn json_type(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// The order [`EnumPlan::Mixed`] lists JSON types in.
const JSON_TYPE_ORDER: &[&str] = &[
    "boolean", "integer", "null", "number", "string", "array", "object",
];

fn enum_plan(values: &[tungsten_ir::EnumValue]) -> EnumPlan {
    if values.is_empty() {
        EnumPlan::Empty
    } else if values.iter().all(|v| v.value.is_string()) {
        EnumPlan::Strings
    } else if values.iter().all(|v| v.value.is_i64()) {
        EnumPlan::Integers
    } else {
        let present: BTreeSet<&'static str> = values.iter().map(|v| json_type(&v.value)).collect();
        EnumPlan::Mixed {
            json_types: JSON_TYPE_ORDER
                .iter()
                .copied()
                .filter(|t| present.contains(t))
                .collect(),
        }
    }
}

fn union_plan(ir: &Ir, u: &Union) -> UnionPlan {
    if u.variants.is_empty() {
        return UnionPlan::Empty;
    }
    if let Some(d) = &u.discriminator {
        let tagged = u.strategy == UnionStrategy::Tagged
            && u.variants.iter().all(|v| {
                v.tag.is_some() && matches!(&v.ty, TypeRef::Named(id) if ir.types.get(id).is_some())
            });
        if tagged {
            let variants = u
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let mut tags: Vec<String> = v.tag.iter().cloned().collect();
                    if let TypeRef::Named(id) = &v.ty {
                        for (value, target) in &d.mapping {
                            if target == id && !tags.contains(value) {
                                tags.push(value.clone());
                            }
                        }
                    }
                    (i, tags)
                })
                .collect();
            return UnionPlan::Tagged {
                tag: d.property.clone(),
                variants,
            };
        }
    }
    let constant = |v: &tungsten_ir::Variant| match &v.ty {
        TypeRef::Inline(s) => matches!(s.as_ref(), Shape::Const { .. }),
        TypeRef::Named(id) => ir
            .types
            .get(id)
            .is_some_and(|t| matches!(t.shape, Shape::Const { .. })),
    };
    if u.strategy == UnionStrategy::Literal || u.variants.iter().any(constant) {
        return UnionPlan::LiteralTagged {
            variants: u
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| (i, literal_key(ir, &v.ty)))
                .collect(),
        };
    }
    UnionPlan::Untagged {
        ordered: (0..u.variants.len()).collect(),
    }
}

/// What identifies a value of `ty` among literal variants.
fn literal_key(ir: &Ir, ty: &TypeRef) -> LiteralKey {
    let shape = match ty {
        TypeRef::Inline(s) => Some(s.as_ref()),
        TypeRef::Named(id) => ir.types.get(id).map(|t| &t.shape),
    };
    match shape {
        Some(Shape::Const { value }) => LiteralKey::Value(value.clone()),
        Some(Shape::Primitive { primitive, .. }) => {
            LiteralKey::JsonType(primitive_json_type(primitive))
        }
        Some(Shape::Enum { base, .. }) => LiteralKey::JsonType(primitive_json_type(base)),
        Some(Shape::Array { .. }) => LiteralKey::JsonType("array"),
        Some(Shape::Map { .. } | Shape::Record { .. }) => LiteralKey::JsonType("object"),
        _ => LiteralKey::JsonType("any"),
    }
}

/// The JSON type a primitive's values have (`bytes` travel as strings).
pub fn primitive_json_type(p: &tungsten_ir::Primitive) -> &'static str {
    use tungsten_ir::Primitive as P;
    match p {
        P::String { .. } | P::Bytes => "string",
        P::Int32 | P::Int64 | P::Integer => "integer",
        P::Float | P::Double | P::Number => "number",
        P::Bool => "boolean",
    }
}

fn all_of_plan(ir: &Ir, members: &[TypeRef]) -> AllOfPlan {
    let mut fields = vec![];
    let mut wires: BTreeSet<&str> = BTreeSet::new();
    let mut closed = false;
    for (m, member) in members.iter().enumerate() {
        match args::resolve(ir, member) {
            Some(Shape::Record {
                fields: own,
                additional,
            }) if !matches!(additional, Additional::Typed { .. }) => {
                closed |= matches!(additional, Additional::Closed);
                for (f, field) in own.iter().enumerate() {
                    if !wires.insert(field.wire_name.as_str()) {
                        return AllOfPlan::Intersection;
                    }
                    fields.push((m, f));
                }
            }
            _ => return AllOfPlan::Intersection,
        }
    }
    if members.is_empty() {
        return AllOfPlan::Intersection;
    }
    AllOfPlan::Merged { fields, closed }
}

// ----------------------------------------------------------- operations

fn plan_op<'a>(
    ir: &'a Ir,
    op: &'a Operation,
    ns: &str,
    resource: usize,
    multi: bool,
) -> OpPlan<'a> {
    let id = op.id.0.as_str();
    let local = if multi {
        id
    } else {
        id.strip_prefix(&format!("{ns}.")).unwrap_or(id)
    };
    let layout = args::args_layout(ir, op);
    let mut arguments: Vec<ArgPlan<'a>> = layout
        .params
        .iter()
        .map(|a| ArgPlan {
            key: a.key.clone(),
            wire: a.param.wire_name.clone(),
            words: a.param.name.words.clone(),
            source: ArgSource::Param {
                location: a.location,
                param: a.param,
            },
            presence: if a.param.required {
                Presence::Required
            } else {
                Presence::Optional
            },
            ty: Some(a.param.ty.clone()),
            sensitive: false,
        })
        .collect();
    match &layout.body {
        Some(BodyArg::Merged { fields, .. }) => {
            for f in fields {
                arguments.push(ArgPlan {
                    key: f.wire_name.clone(),
                    wire: f.wire_name.clone(),
                    words: f.name.words.clone(),
                    source: ArgSource::Field(f),
                    presence: f.presence,
                    ty: Some(f.ty.clone()),
                    sensitive: f.sensitive,
                });
            }
        }
        Some(BodyArg::Arg { key, content }) => {
            let ty = match content.encoding {
                BodyEncoding::Bytes | BodyEncoding::Jsonl | BodyEncoding::Text => None,
                BodyEncoding::Json | BodyEncoding::Form | BodyEncoding::Multipart => {
                    Some(content.ty.clone())
                }
            };
            arguments.push(ArgPlan {
                key: key.clone(),
                wire: key.clone(),
                words: tungsten_ir::naming::split_words(key),
                source: ArgSource::Body {
                    encoding: content.encoding,
                    media_type: content.media_type.clone(),
                },
                presence: if layout.body_required {
                    Presence::Required
                } else {
                    Presence::Optional
                },
                ty,
                sensitive: false,
            });
        }
        None => {}
    }
    let supplied = layout
        .supplied
        .iter()
        .map(|a| SuppliedParam {
            key: a.key.clone(),
            location: a.location,
            role: a.param.role,
            param: a.param,
        })
        .collect();
    let (success, bodiless_success) = success_plan(op);
    let page = op.pagination.as_ref().map(|p| PagePlan {
        pagination: p.clone(),
        item: match &success {
            SuccessPlan::Json(body) => items_ref(ir, body, &p.items_field),
            _ => None,
        },
        request_key: page_param(&layout, &p.style, true),
        size_key: page_param(&layout, &p.style, false).or_else(|| {
            p.page_size_param
                .as_deref()
                .and_then(|w| query_key(&layout, w))
        }),
    });
    let stream = op.stream.as_ref().map(|s| StreamPlan {
        event: s.event.clone(),
        typed: !matches!(&s.event, TypeRef::Inline(shape) if matches!(**shape, Shape::Any)),
        done: s.done.clone(),
        flag: s.request_flag.clone(),
        also_plain: s.also_plain,
    });
    OpPlan {
        op,
        namespace: ns.to_string(),
        resource,
        key_words: tungsten_ir::naming::split_words(local),
        layout,
        arguments,
        supplied,
        success,
        bodiless_success,
        page,
        stream,
        has_preview: has_preview(op),
        safety: op.agent.safety,
        idempotency: op.agent.idempotency.policy,
    }
}

/// Whether an operation has a preview with a confirmation token.
pub fn has_preview(op: &Operation) -> bool {
    op.agent.safety != Safety::ReadOnly && op.agent.preview != PreviewMode::None
}

/// The args key of the query parameter with this wire name.
fn query_key(layout: &ArgsLayout<'_>, wire: &str) -> Option<String> {
    layout
        .params
        .iter()
        .find(|a| a.location == ParamLocation::Query && a.param.wire_name == wire)
        .map(|a| a.key.clone())
}

/// The args key of the paging request parameter (`first`) or of the size
/// parameter (`!first`) of a pagination style.
fn page_param(layout: &ArgsLayout<'_>, style: &PaginationStyle, first: bool) -> Option<String> {
    let wire = match (style, first) {
        (PaginationStyle::Cursor { request_param, .. }, true) => request_param,
        (PaginationStyle::Offset { offset_param, .. }, true) => offset_param,
        (PaginationStyle::Offset { limit_param, .. }, false) => limit_param,
        (PaginationStyle::Page { page_param, .. }, true) => page_param,
        (PaginationStyle::Page { size_param, .. }, false) => size_param,
        _ => return None,
    };
    query_key(layout, wire)
}

/// The item type of a page: the items of the array at `items_field` of the
/// success body (the body itself when the field is empty).
fn items_ref(ir: &Ir, body: &TypeRef, items_field: &str) -> Option<TypeRef> {
    let list = if items_field.is_empty() {
        body
    } else {
        let Some(Shape::Record { fields, .. }) = args::resolve(ir, body) else {
            return None;
        };
        &fields.iter().find(|f| f.wire_name == items_field)?.ty
    };
    match args::resolve(ir, list)? {
        Shape::Array { items, .. } => Some(items.clone()),
        _ => None,
    }
}

/// One success body, as the success plan compares them.
#[derive(Debug, Clone, PartialEq)]
enum Body {
    Json(TypeRef),
    Bytes,
    Text,
    Form(TypeRef),
}

fn success_plan(op: &Operation) -> (SuccessPlan, bool) {
    let mut bodies: Vec<Body> = vec![];
    let mut by_status: Vec<(StatusMatch, StatusBody)> = vec![];
    let mut status_ok = true;
    let mut bodiless = false;
    for r in op
        .responses
        .iter()
        .filter(|r| r.kind == ResponseKind::Success)
    {
        let content = r
            .content
            .iter()
            .find(|c| c.encoding == BodyEncoding::Json)
            .or_else(|| r.content.first());
        let body = content.map(|c| match c.encoding {
            BodyEncoding::Json | BodyEncoding::Jsonl => Body::Json(c.value_type()),
            BodyEncoding::Bytes => Body::Bytes,
            BodyEncoding::Text => Body::Text,
            BodyEncoding::Form | BodyEncoding::Multipart => Body::Form(c.ty.clone()),
        });
        match (&r.status, &body) {
            (StatusMatch::Exact(n), None) if !by_status.iter().any(|(s, _)| *s == r.status) => {
                by_status.push((StatusMatch::Exact(*n), StatusBody::Empty));
            }
            (StatusMatch::Exact(n), Some(Body::Json(t)))
                if !by_status.iter().any(|(s, _)| *s == r.status) =>
            {
                by_status.push((StatusMatch::Exact(*n), StatusBody::Json(t.clone())));
            }
            _ => status_ok = false,
        }
        match body {
            None => bodiless = true,
            Some(b) => {
                if !bodies.contains(&b) {
                    bodies.push(b);
                }
            }
        }
    }
    let plan = match bodies.as_slice() {
        [] if bodiless => SuccessPlan::None,
        [] => SuccessPlan::Raw("the operation declares no success response".into()),
        [Body::Json(t)] => SuccessPlan::Json(t.clone()),
        [Body::Bytes] => SuccessPlan::Bytes,
        [Body::Text] => SuccessPlan::Text,
        [Body::Form(_)] => SuccessPlan::Raw("the success body is form encoded".into()),
        _ if status_ok => SuccessPlan::ByStatus(by_status),
        _ => SuccessPlan::Raw(
            "the success bodies differ and are not one JSON body per exact status".into(),
        ),
    };
    (plan, bodiless)
}
