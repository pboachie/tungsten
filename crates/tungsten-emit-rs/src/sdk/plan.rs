// SPDX-License-Identifier: AGPL-3.0-only
//! Every name and file path the SDK half writes, decided once per IR.
//!
//! Identifiers come from [`tungsten_ir::naming`] for [`Target::Rust`] and
//! are made unique per scope with [`naming::disambiguate`]. Names the
//! generated code defines itself are never produced by naming for an API
//! name in the same scope: they are reserved before the API's names.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_ir::naming::{self, Case, Role, Target};
use tungsten_ir::{
    Ident, Ir, NamedType, Operation, OperationStatus, Resource, Shape, TypeId, TypeRef, Union,
    UnionStrategy,
};

use super::graph::Graph;

pub(crate) const RS: Target = Target::Rust;

pub(crate) fn ident(words: &[String]) -> Ident {
    Ident {
        wire: words.join(" "),
        words: words.to_vec(),
    }
}

/// `a` followed by `b`.
pub(crate) fn with_words(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().chain(b).cloned().collect()
}

/// `words` with `extra` appended.
pub(crate) fn with_word(words: &[String], extra: &str) -> Vec<String> {
    let mut w = words.to_vec();
    w.push(extra.to_string());
    w
}

/// Make the renderings of `entries` unique for `role` within one scope.
/// `reserved` names are taken first (they render to themselves), so an
/// entry that renders like one gets a numeric suffix instead.
pub(crate) fn unique(reserved: &[&str], entries: &[Vec<String>], role: Role) -> Vec<String> {
    let mut idents: Vec<Ident> = reserved.iter().map(|r| Ident::new(*r)).collect();
    idents.extend(entries.iter().map(|w| ident(w)));
    naming::disambiguate(&mut idents, RS, role);
    idents[reserved.len()..]
        .iter()
        .map(|i| naming::render(i, RS, role))
        .collect()
}

/// Like [`unique`], returning the final word lists.
fn unique_words(entries: &[Vec<String>], role: Role) -> Vec<Vec<String>> {
    let mut idents: Vec<Ident> = entries.iter().map(|w| ident(w)).collect();
    naming::disambiguate(&mut idents, RS, role);
    idents.into_iter().map(|i| i.words).collect()
}

/// The words of `i` without the numeric suffix the IR appended to
/// disambiguate it in its own scope (TG0401): Rust numbers again.
fn plain_words(i: &Ident) -> Vec<String> {
    let plain = naming::split_words(&i.wire);
    let numbered = i.words.len() == plain.len() + 1
        && i.words[..plain.len()] == plain[..]
        && i.words
            .last()
            .is_some_and(|w| w.chars().all(|c| c.is_ascii_digit()));
    if numbered { plain } else { i.words.clone() }
}

/// Field names of a record, in field order. `reserved` are names the
/// struct takes itself (the extras map).
pub(crate) fn field_names<'f>(
    wires: impl IntoIterator<Item = &'f Ident>,
    reserved: &[&str],
) -> Vec<String> {
    let words: Vec<Vec<String>> = wires.into_iter().map(plain_words).collect();
    unique(reserved, &words, Role::Field)
}

/// A module-scope function name made of `prefix` and the words of a type.
pub(crate) fn fn_name(prefix: &str, words: &[String]) -> String {
    let body = naming::to_case(words, Case::Snake);
    format!("{prefix}_{body}")
}

/// Names a models module uses unqualified, which a type cannot take.
pub(crate) const MODELS_RESERVED: &[&str] = &[
    "BTreeMap",
    "Checker",
    "Deserialize",
    "Deserializer",
    "Issue",
    "LazyLock",
    "Patch",
    "Regex",
    "Serialize",
    "Serializer",
    "Value",
];

/// A model namespace (one `models/<file>.rs`).
#[derive(Debug, Clone)]
pub(crate) struct ModelNs {
    /// Namespace name as in the IR.
    pub name: String,
    /// Module name under `models`.
    pub file: String,
    /// Named types in IR order.
    pub types: Vec<TypeId>,
}

#[derive(Debug, Clone)]
pub(crate) struct TypeInfo {
    /// Rust name (struct, enum, alias).
    pub name: String,
    pub ns: String,
    /// Name of the check function (`check_<words>`).
    pub check_fn: String,
}

/// One callable operation.
#[derive(Debug, Clone)]
pub(crate) struct OpInfo<'a> {
    pub op: &'a Operation,
    /// Namespace of the operation (for its error model).
    pub ns: String,
    /// Index constant in `descriptors.rs` (`OP_LIST_PETS`).
    pub konst: String,
    /// Builder function in `descriptors.rs` (`op_list_pets`).
    pub builder: String,
    /// Index into [`Plan::resources`].
    pub res: usize,
    /// Request struct name in the resource's module.
    pub request: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberKind {
    /// The operation's method; index into [`Plan::ops`].
    Op(usize),
    /// `preview_<op>`.
    Preview(usize),
    /// `<op>_pages`.
    Pages(usize),
    /// `<op>_stream`.
    Stream(usize),
    /// Index into [`Plan::resources`].
    Child(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub name: String,
    pub kind: MemberKind,
}

/// One resource (one module under `resources/`).
#[derive(Debug, Clone)]
pub(crate) struct ResInfo<'a> {
    pub res: &'a Resource,
    /// The resource struct (`WebhooksResource`).
    pub strukt: String,
    /// Module name under `resources`.
    pub module: String,
    pub members: Vec<Member>,
}

/// A namespace of the client (multi-namespace APIs only).
#[derive(Debug, Clone)]
pub(crate) struct ClientNs<'a> {
    pub ns: &'a tungsten_ir::Namespace,
    /// Accessor name on the client.
    pub member: String,
    pub strukt: String,
    /// Top-level resources: (accessor name, index into [`Plan::resources`]).
    pub members: Vec<(String, usize)>,
}

/// How a named type is emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Emit {
    Struct,
    /// A closed enum of string values.
    StrEnum,
    /// A closed enum of integer values.
    IntEnum,
    Union(UnionKind),
    /// `pub type Name = ...;`, or a transparent newtype when the type is on
    /// a reference cycle.
    Alias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnionKind {
    /// Discriminated by a property of the variants' records.
    Tagged,
    /// `#[serde(untagged)]` derive.
    Untagged,
    /// Untagged with a hand-written `Deserialize` (const variants).
    Literal,
}

/// Everything named, decided before any file is written.
#[derive(Debug, Clone)]
pub(crate) struct Plan<'a> {
    pub ir: &'a Ir,
    pub models: Vec<ModelNs>,
    pub types: BTreeMap<TypeId, TypeInfo>,
    pub ops: Vec<OpInfo<'a>>,
    pub op_by_id: BTreeMap<String, usize>,
    pub resources: Vec<ResInfo<'a>>,
    /// Multi-namespace APIs: one entry per namespace, in IR order.
    pub client_namespaces: Vec<ClientNs<'a>>,
    /// Single-namespace APIs: the client's resource accessors.
    pub client_resources: Vec<(String, usize)>,
    pub client_class: String,
    pub graph: Graph,
}

/// Method names of the client besides its resources or namespaces: its own
/// and those of `Dispatch`.
const CLIENT_RESERVED: &[&str] = &[
    "core",
    "descriptors",
    "invoke",
    "macros",
    "new",
    "operations",
    "pages",
    "preview",
    "preview_macro",
    "run_macro",
];

impl<'a> Plan<'a> {
    pub(crate) fn new(ir: &'a Ir) -> Self {
        let multi = ir.namespaces.len() > 1;
        let (types, models) = plan_types(ir);
        let ops = plan_ops(ir, multi);
        let op_by_id = ops
            .iter()
            .enumerate()
            .map(|(i, o)| (o.op.id.0.clone(), i))
            .collect();
        let mut plan = Plan {
            ir,
            models,
            types,
            ops,
            op_by_id,
            resources: vec![],
            client_namespaces: vec![],
            client_resources: vec![],
            client_class: String::new(),
            graph: Graph::default(),
        };
        plan.plan_resources(multi);
        plan.graph = Graph::new(&plan);
        plan
    }

    pub(crate) fn multi(&self) -> bool {
        !self.client_namespaces.is_empty()
    }

    pub(crate) fn model_ns(&self, name: &str) -> Option<&ModelNs> {
        self.models.iter().find(|m| m.name == name)
    }

    /// The shape behind a reference, following one named type.
    pub(crate) fn resolve<'r>(&'r self, r: &'r TypeRef) -> Option<&'r Shape> {
        match r {
            TypeRef::Inline(s) => Some(s),
            TypeRef::Named(id) => self.ir.types.get(id).map(|t| &t.shape),
        }
    }

    /// Resource modules, struct names and members, and the client's members.
    fn plan_resources(&mut self, multi: bool) {
        struct Pending<'r> {
            res: &'r Resource,
            /// Namespace words (multi-namespace APIs) and resource path words.
            words: Vec<String>,
            parent: Option<usize>,
        }
        fn walk<'r>(
            res: &'r [Resource],
            prefix: &[String],
            parent: Option<usize>,
            out: &mut Vec<Pending<'r>>,
        ) -> Vec<usize> {
            let mut indices = vec![];
            for r in res {
                let words: Vec<String> = prefix.iter().chain(&r.name.words).cloned().collect();
                let idx = out.len();
                out.push(Pending {
                    res: r,
                    words: words.clone(),
                    parent,
                });
                indices.push(idx);
                walk(&r.children, &words, Some(idx), out);
            }
            indices
        }
        let ir = self.ir;
        let mut pending: Vec<Pending<'a>> = vec![];
        let mut top: Vec<Vec<usize>> = vec![];
        for ns in &ir.namespaces {
            let prefix = if multi { ns.name.words.clone() } else { vec![] };
            top.push(walk(&ns.resources, &prefix, None, &mut pending));
        }

        // Module names under `resources`, flat and unique.
        let modules = unique_words(
            &pending.iter().map(|p| p.words.clone()).collect::<Vec<_>>(),
            Role::Module,
        );

        // Struct names, unique across the package.
        let mut struct_words: Vec<Vec<String>> = vec![with_word(&ir.api.name.words, "client")];
        if multi {
            for n in &ir.namespaces {
                struct_words.push(with_word(&n.name.words, "namespace"));
            }
        }
        for p in &pending {
            struct_words.push(with_word(&p.words, "resource"));
        }
        let structs = unique(&["Macros"], &struct_words, Role::Type);
        self.client_class = structs[0].clone();
        let ns_count = if multi { ir.namespaces.len() } else { 0 };
        let res_structs = &structs[1 + ns_count..];

        let mut children_of: Vec<Vec<usize>> = vec![vec![]; pending.len()];
        for (i, p) in pending.iter().enumerate() {
            if let Some(parent) = p.parent {
                children_of[parent].push(i);
            }
        }
        let mut request_words: Vec<(usize, Vec<String>)> = vec![];
        for (i, p) in pending.iter().enumerate() {
            let op_indices: Vec<usize> = p
                .res
                .operations
                .iter()
                .filter_map(|o| self.op_by_id.get(&o.id.0).copied())
                .collect();
            // Primary names first so they keep their rendering; derived
            // `preview_` and `_pages` names yield on a collision.
            let mut words: Vec<Vec<String>> = vec![];
            let mut kinds: Vec<MemberKind> = vec![];
            for &o in &op_indices {
                words.push(self.ops[o].op.name.words.clone());
                kinds.push(MemberKind::Op(o));
            }
            for &c in &children_of[i] {
                words.push(pending[c].res.name.words.clone());
                kinds.push(MemberKind::Child(c));
            }
            for &o in &op_indices {
                let base = self.ops[o].op.name.words.clone();
                if crate::sdk::ops::has_preview(self.ops[o].op) {
                    let mut w = vec!["preview".to_string()];
                    w.extend(base.iter().cloned());
                    words.push(w);
                    kinds.push(MemberKind::Preview(o));
                }
                if self.ops[o].op.pagination.is_some() {
                    words.push(with_word(&base, "pages"));
                    kinds.push(MemberKind::Pages(o));
                }
                if self.ops[o].op.stream.is_some() {
                    words.push(with_word(&base, "stream"));
                    kinds.push(MemberKind::Stream(o));
                }
            }
            let names = unique(&[], &words, Role::Method);
            let members = names
                .into_iter()
                .zip(kinds)
                .map(|(name, kind)| Member { name, kind })
                .collect();
            for &o in &op_indices {
                self.ops[o].res = i;
                request_words.push((
                    o,
                    with_word(&with_words(&p.words, &self.ops[o].op.name.words), "request"),
                ));
            }
            self.resources.push(ResInfo {
                res: p.res,
                strukt: res_structs[i].clone(),
                module: naming::to_case(&modules[i], Case::Snake),
                members,
            });
        }

        // Request struct names, unique across the package (the dispatch
        // table imports all of them) and apart from the other structs.
        let reserved: Vec<&str> = structs.iter().map(String::as_str).collect();
        let names = unique(
            &reserved,
            &request_words
                .iter()
                .map(|(_, w)| w.clone())
                .collect::<Vec<_>>(),
            Role::Type,
        );
        for ((o, _), name) in request_words.iter().zip(names) {
            self.ops[*o].request = name;
        }

        let top_members = |indices: &[usize], reserved: &[&str]| -> Vec<(String, usize)> {
            let words: Vec<Vec<String>> = indices
                .iter()
                .map(|&i| pending[i].res.name.words.clone())
                .collect();
            unique(reserved, &words, Role::Method)
                .into_iter()
                .zip(indices.iter().copied())
                .collect()
        };
        if multi {
            let names = unique(
                CLIENT_RESERVED,
                &ir.namespaces
                    .iter()
                    .map(|n| n.name.words.clone())
                    .collect::<Vec<_>>(),
                Role::Method,
            );
            for (k, ns) in ir.namespaces.iter().enumerate() {
                self.client_namespaces.push(ClientNs {
                    ns,
                    member: names[k].clone(),
                    strukt: structs[1 + k].clone(),
                    members: top_members(&top[k], &[]),
                });
            }
        } else {
            let all: Vec<usize> = top.into_iter().flatten().collect();
            self.client_resources = top_members(&all, CLIENT_RESERVED);
        }
    }
}

/// Type names and model namespaces.
fn plan_types(ir: &Ir) -> (BTreeMap<TypeId, TypeInfo>, Vec<ModelNs>) {
    let table = &ir.types.types;
    // Namespaces: every type's and every IR namespace, sorted.
    let mut ns_names: BTreeSet<String> = table.iter().map(|t| t.namespace.clone()).collect();
    ns_names.extend(ir.namespaces.iter().map(|n| n.name.wire.clone()));
    let ns_names: Vec<String> = ns_names.into_iter().collect();
    let files = unique_words(
        &ns_names
            .iter()
            .map(|n| Ident::new(n.as_str()).words)
            .collect::<Vec<_>>(),
        Role::Module,
    );

    let mut types: BTreeMap<TypeId, TypeInfo> = BTreeMap::new();
    for ns in &ns_names {
        let members: Vec<&NamedType> = table.iter().filter(|t| &t.namespace == ns).collect();
        let words: Vec<Vec<String>> = members.iter().map(|t| plain_words(&t.name)).collect();
        let rendered = unique(MODELS_RESERVED, &words, Role::Type);
        for (t, name) in members.into_iter().zip(rendered) {
            types.insert(
                t.id.clone(),
                TypeInfo {
                    name,
                    ns: t.namespace.clone(),
                    check_fn: String::new(),
                },
            );
        }
        // Check function names: unique among the namespace's functions.
        let finals: Vec<Vec<String>> = ns_types(table, ns)
            .map(|t| naming::split_words(&types[&t.id].name))
            .collect();
        let fns = unique_fn_names("check", &finals);
        for (t, f) in ns_types(table, ns).zip(fns) {
            if let Some(info) = types.get_mut(&t.id) {
                info.check_fn = f;
            }
        }
    }
    let models = ns_names
        .iter()
        .zip(&files)
        .map(|(name, words)| ModelNs {
            name: name.clone(),
            file: naming::render(&ident(words), RS, Role::Module),
            types: table
                .iter()
                .filter(|t| &t.namespace == name)
                .map(|t| t.id.clone())
                .collect(),
        })
        .collect();
    (types, models)
}

fn ns_types<'t>(table: &'t [NamedType], ns: &'t str) -> impl Iterator<Item = &'t NamedType> + 't {
    table.iter().filter(move |t| t.namespace == ns)
}

/// `<prefix>_<words>` function names, made unique by a numeric suffix.
fn unique_fn_names(prefix: &str, entries: &[Vec<String>]) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    entries
        .iter()
        .map(|w| {
            let base = fn_name(prefix, w);
            let mut name = base.clone();
            let mut n = 2;
            while !seen.insert(name.clone()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect()
}

/// Callable operations with their descriptor names.
fn plan_ops(ir: &Ir, multi: bool) -> Vec<OpInfo<'_>> {
    let mut entries: Vec<(&Operation, String)> = vec![];
    for ns in &ir.namespaces {
        fn walk<'r>(r: &'r Resource, ns: &str, out: &mut Vec<(&'r Operation, String)>) {
            // Planned operations live outside the tree; never make one callable.
            for o in r
                .operations
                .iter()
                .filter(|o| !matches!(o.status, OperationStatus::Planned { .. }))
            {
                out.push((o, ns.to_string()));
            }
            for c in &r.children {
                walk(c, ns, out);
            }
        }
        for r in &ns.resources {
            walk(r, &ns.name.wire, &mut entries);
        }
    }
    // Names are decided in id order so they do not depend on the tree.
    let mut by_id: Vec<usize> = (0..entries.len()).collect();
    by_id.sort_by(|&a, &b| entries[a].0.id.cmp(&entries[b].0.id));
    let words: Vec<Vec<String>> = by_id
        .iter()
        .map(|&i| {
            let (op, ns) = &entries[i];
            let id = op.id.0.as_str();
            let local = if multi {
                id
            } else {
                id.strip_prefix(&format!("{ns}.")).unwrap_or(id)
            };
            naming::split_words(local)
        })
        .collect();
    let builders = unique_fn_names("op", &words);
    let konsts = unique_konsts(&words);
    let mut named = vec![(String::new(), String::new()); entries.len()];
    for (k, &i) in by_id.iter().enumerate() {
        named[i] = (konsts[k].clone(), builders[k].clone());
    }
    entries
        .into_iter()
        .zip(named)
        .map(|((op, ns), (konst, builder))| OpInfo {
            op,
            ns,
            konst,
            builder,
            res: 0,
            request: String::new(),
        })
        .collect()
}

/// `OP_<WORDS>` constants, unique by a numeric suffix.
pub(crate) fn unique_konsts(entries: &[Vec<String>]) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    entries
        .iter()
        .map(|w| {
            let base = format!("OP_{}", naming::to_case(w, Case::ScreamingSnake));
            let mut name = base.clone();
            let mut n = 2;
            while !seen.insert(name.clone()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect()
}

/// How a named type is emitted.
pub(crate) fn emit_kind(ir: &Ir, nt: &NamedType) -> Emit {
    match &nt.shape {
        Shape::Record { .. } => Emit::Struct,
        Shape::Enum { values, .. } => {
            if !values.is_empty() && values.iter().all(|v| v.value.is_string()) {
                Emit::StrEnum
            } else if !values.is_empty() && values.iter().all(|v| v.value.is_i64()) {
                Emit::IntEnum
            } else {
                Emit::Alias
            }
        }
        Shape::Union(u) => match union_kind(ir, u) {
            Some(k) => Emit::Union(k),
            None => Emit::Alias,
        },
        _ => Emit::Alias,
    }
}

/// How a union is emitted; `None` for a union without variants.
pub(crate) fn union_kind(ir: &Ir, u: &Union) -> Option<UnionKind> {
    if u.variants.is_empty() {
        return None;
    }
    let tagged = u.strategy == UnionStrategy::Tagged
        && u.discriminator.is_some()
        && u.variants.iter().all(|v| {
            v.tag.is_some()
                && matches!(&v.ty, TypeRef::Named(id)
                    if matches!(ir.types.get(id).map(|t| &t.shape), Some(Shape::Record { .. })))
        });
    if tagged {
        return Some(UnionKind::Tagged);
    }
    let literal = u.variants.iter().any(|v| {
        matches!(
            &v.ty,
            TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Const { .. } | Shape::Never)
        )
    });
    Some(if literal {
        UnionKind::Literal
    } else {
        UnionKind::Untagged
    })
}

/// `MACRO_<WORDS>` constants, unique by a numeric suffix.
pub(crate) fn unique_macro_konsts(entries: &[Vec<String>]) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    entries
        .iter()
        .map(|w| {
            let base = format!("MACRO_{}", naming::to_case(w, Case::ScreamingSnake));
            let mut name = base.clone();
            let mut n = 2;
            while !seen.insert(name.clone()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect()
}
