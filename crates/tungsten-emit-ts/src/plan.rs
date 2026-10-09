// SPDX-License-Identifier: AGPL-3.0-only
//! Every name and file path the emitter writes, decided once per IR.
//!
//! Identifiers come from [`tungsten_ir::naming`] and are made unique per
//! scope with [`naming::disambiguate`], so generated code never declares a
//! name twice or shadows one it uses. Module aliases for model namespaces
//! start with `$`, which naming never produces, so they cannot collide with
//! a generated identifier.

use std::collections::{BTreeMap, BTreeSet};

use petgraph::graph::{DiGraph, NodeIndex};
use tungsten_ir::naming::{self, Case, Role, Target};
use tungsten_ir::{Ident, Ir, Operation, OperationStatus, Resource, Shape, TypeId, TypeRef};

const TS: Target = Target::TypeScript;

fn ident(words: &[String]) -> Ident {
    Ident {
        wire: words.join(" "),
        words: words.to_vec(),
    }
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
    naming::disambiguate(&mut idents, TS, role);
    idents[reserved.len()..]
        .iter()
        .map(|i| naming::render(i, TS, role))
        .collect()
}

/// Like [`unique`], returning the final word lists (for names derived
/// further, such as file stems).
fn unique_words(entries: &[Vec<String>], role: Role) -> Vec<Vec<String>> {
    let mut idents: Vec<Ident> = entries.iter().map(|w| ident(w)).collect();
    naming::disambiguate(&mut idents, TS, role);
    idents.into_iter().map(|i| i.words).collect()
}

/// File stem for a word list: kebab case, never the hand-editable segment
/// name `custom`.
fn stem_words(words: &[String]) -> Vec<String> {
    if naming::to_case(words, Case::Kebab) == "custom" {
        with_word(words, "generated")
    } else {
        words.to_vec()
    }
}

/// A model namespace (one `src/models/<file>.ts`).
#[derive(Debug, Clone)]
pub(crate) struct ModelNs {
    /// Namespace name as in the IR.
    pub name: String,
    /// Import alias (`$public`).
    pub alias: String,
    /// File stem under `src/models/`.
    pub file: String,
    /// Named types in declaration order (dependencies first).
    pub types: Vec<TypeId>,
}

#[derive(Debug, Clone)]
pub(crate) struct TypeInfo {
    /// TypeScript name (type and schema constant).
    pub name: String,
    pub ns: String,
    /// Strongly connected component index; components are numbered with
    /// dependencies first.
    pub scc: usize,
    /// The component is a real cycle (more than one type, or a self edge).
    pub cyclic: bool,
}

/// One callable operation.
#[derive(Debug, Clone)]
pub(crate) struct OpInfo<'a> {
    pub op: &'a Operation,
    /// Namespace of the operation (for its error model).
    pub ns: String,
    /// Descriptor constant in `descriptors.ts`.
    pub key: String,
    /// Args type in `descriptors.ts`.
    pub args_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberKind {
    /// Index into [`Plan::ops`].
    Op(usize),
    /// Index into [`Plan::resources`].
    Child(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub name: String,
    pub kind: MemberKind,
}

/// One resource class (one file under `src/resources/`).
#[derive(Debug, Clone)]
pub(crate) struct ResInfo<'a> {
    pub res: &'a Resource,
    pub class: String,
    /// Path relative to the package root (`src/resources/webhooks.ts`).
    pub file: String,
    pub members: Vec<Member>,
}

/// A namespace of the client (multi-namespace APIs only).
#[derive(Debug, Clone)]
pub(crate) struct ClientNs<'a> {
    pub ns: &'a tungsten_ir::Namespace,
    /// Property name on the client.
    pub member: String,
    pub class: String,
    /// Top-level resources: (property name, index into [`Plan::resources`]).
    pub members: Vec<(String, usize)>,
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
    /// Single-namespace APIs: the client's resource properties.
    pub client_resources: Vec<(String, usize)>,
    pub client_class: String,
}

/// Names `descriptors.ts` declares or imports besides the descriptor
/// constants.
pub(crate) const DESCRIPTOR_RESERVED: &[&str] = &["z", "api", "operations", "toSchemaLike", "rpc"];

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
        };
        plan.plan_resources(multi);
        plan
    }

    pub(crate) fn multi(&self) -> bool {
        !self.client_namespaces.is_empty()
    }

    pub(crate) fn model_ns(&self, name: &str) -> Option<&ModelNs> {
        self.models.iter().find(|m| m.name == name)
    }

    /// Import alias of a model namespace.
    pub(crate) fn alias(&self, ns: &str) -> &str {
        self.model_ns(ns).map_or("$models", |m| m.alias.as_str())
    }

    /// Resource classes, files and members, the client's members, and the
    /// class names (unique across the package, `Macros` reserved).
    fn plan_resources(&mut self, multi: bool) {
        struct Pending<'r> {
            res: &'r Resource,
            ns_words: Vec<String>,
            path_words: Vec<String>,
            dir: Vec<String>,
            stem: String,
            parent: Option<usize>,
        }
        let mut pending: Vec<Pending<'a>> = vec![];
        fn walk<'r>(
            res: &'r [Resource],
            ns_words: &[String],
            prefix: &[String],
            dir: &[String],
            parent: Option<usize>,
            out: &mut Vec<Pending<'r>>,
        ) -> Vec<usize> {
            let stems = unique_words(
                &res.iter()
                    .map(|r| stem_words(&r.name.words))
                    .collect::<Vec<_>>(),
                Role::Module,
            );
            let mut indices = vec![];
            for (r, stem) in res.iter().zip(stems) {
                let path_words: Vec<String> = prefix.iter().chain(&r.name.words).cloned().collect();
                let stem = naming::to_case(&stem, Case::Kebab);
                let idx = out.len();
                out.push(Pending {
                    res: r,
                    ns_words: ns_words.to_vec(),
                    path_words: path_words.clone(),
                    dir: dir.to_vec(),
                    stem: stem.clone(),
                    parent,
                });
                indices.push(idx);
                let mut child_dir = dir.to_vec();
                child_dir.push(stem);
                walk(
                    &r.children,
                    ns_words,
                    &path_words,
                    &child_dir,
                    Some(idx),
                    out,
                );
            }
            indices
        }
        let ir = self.ir;
        let ns_stems = unique_words(
            &ir.namespaces
                .iter()
                .map(|n| stem_words(&n.name.words))
                .collect::<Vec<_>>(),
            Role::Module,
        );
        let mut top: Vec<Vec<usize>> = vec![];
        for (ns, stem) in ir.namespaces.iter().zip(&ns_stems) {
            let (ns_words, dir) = if multi {
                (
                    ns.name.words.clone(),
                    vec![naming::to_case(stem, Case::Kebab)],
                )
            } else {
                (vec![], vec![])
            };
            top.push(walk(
                &ns.resources,
                &ns_words,
                &[],
                &dir,
                None,
                &mut pending,
            ));
        }

        // Class names, unique across the package.
        let mut class_words: Vec<Vec<String>> = vec![with_word(&ir.api.name.words, "client")];
        if multi {
            class_words.extend(
                ir.namespaces
                    .iter()
                    .map(|n| with_word(&n.name.words, "namespace")),
            );
        }
        class_words.extend(pending.iter().map(|p| {
            let mut w = p.ns_words.clone();
            w.extend(p.path_words.iter().cloned());
            with_word(&w, "resource")
        }));
        let classes = unique(&["Macros"], &class_words, Role::Type);
        self.client_class = classes[0].clone();
        let ns_count = if multi { ir.namespaces.len() } else { 0 };
        let res_classes = &classes[1 + ns_count..];

        // Resource files and members.
        let mut children_of: Vec<Vec<usize>> = vec![vec![]; pending.len()];
        for (i, p) in pending.iter().enumerate() {
            if let Some(parent) = p.parent {
                children_of[parent].push(i);
            }
        }
        for (i, p) in pending.iter().enumerate() {
            let mut path = vec!["src".to_string(), "resources".to_string()];
            path.extend(p.dir.iter().cloned());
            path.push(format!("{}.ts", p.stem));
            let op_indices: Vec<usize> = p
                .res
                .operations
                .iter()
                .filter_map(|o| self.op_by_id.get(&o.id.0).copied())
                .collect();
            let mut words: Vec<Vec<String>> = op_indices
                .iter()
                .map(|&o| self.ops[o].op.name.words.clone())
                .collect();
            words.extend(
                children_of[i]
                    .iter()
                    .map(|&c| pending[c].res.name.words.clone()),
            );
            let names = unique(&[], &words, Role::Method);
            let kinds = op_indices
                .iter()
                .map(|&o| MemberKind::Op(o))
                .chain(children_of[i].iter().map(|&c| MemberKind::Child(c)));
            let members = names
                .into_iter()
                .zip(kinds)
                .map(|(name, kind)| Member { name, kind })
                .collect();
            self.resources.push(ResInfo {
                res: p.res,
                class: res_classes[i].clone(),
                file: path.join("/"),
                members,
            });
        }

        // Client members: `core` and `macros` keep their names.
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
                &["core", "macros"],
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
                    class: classes[1 + k].clone(),
                    members: top_members(&top[k], &[]),
                });
            }
        } else {
            let all: Vec<usize> = top.into_iter().flatten().collect();
            self.client_resources = top_members(&all, &["core", "macros"]);
        }
    }
}

/// Type names, model namespaces and declaration order.
fn plan_types(ir: &Ir) -> (BTreeMap<TypeId, TypeInfo>, Vec<ModelNs>) {
    let table = &ir.types.types;
    let mut graph: DiGraph<usize, ()> = DiGraph::new();
    let nodes: Vec<NodeIndex> = (0..table.len()).map(|i| graph.add_node(i)).collect();
    let index: BTreeMap<&TypeId, usize> =
        table.iter().enumerate().map(|(i, t)| (&t.id, i)).collect();
    let mut self_edge = vec![false; table.len()];
    for (i, t) in table.iter().enumerate() {
        let mut refs = vec![];
        shape_refs(&t.shape, &mut refs);
        let targets: BTreeSet<usize> = refs.iter().filter_map(|r| index.get(r).copied()).collect();
        for j in targets {
            if j == i {
                self_edge[i] = true;
            }
            graph.add_edge(nodes[i], nodes[j], ());
        }
    }
    // Tarjan yields components in reverse topological order: a type's
    // dependencies come before it.
    let sccs = petgraph::algo::tarjan_scc(&graph);
    let mut scc_of = vec![0usize; table.len()];
    let mut cyclic = vec![false; table.len()];
    let mut order: Vec<usize> = vec![];
    for (k, comp) in sccs.iter().enumerate() {
        let mut members: Vec<usize> = comp.iter().map(|n| graph[*n]).collect();
        members.sort_unstable();
        let is_cycle = members.len() > 1 || members.iter().any(|&m| self_edge[m]);
        for &m in &members {
            scc_of[m] = k;
            cyclic[m] = is_cycle;
        }
        order.extend(members);
    }

    // Namespaces: every type's and every IR namespace, sorted.
    let mut ns_names: BTreeSet<String> = table.iter().map(|t| t.namespace.clone()).collect();
    ns_names.extend(ir.namespaces.iter().map(|n| n.name.wire.clone()));
    let ns_names: Vec<String> = ns_names.into_iter().collect();
    let ns_words = unique_words(
        &ns_names
            .iter()
            .map(|n| Ident::new(n.as_str()).words)
            .collect::<Vec<_>>(),
        Role::Module,
    );
    let stems = unique_words(
        &ns_words.iter().map(|w| stem_words(w)).collect::<Vec<_>>(),
        Role::Module,
    );

    // Type names, unique per namespace.
    let mut names: BTreeMap<TypeId, String> = BTreeMap::new();
    for ns in &ns_names {
        let members: Vec<&tungsten_ir::NamedType> =
            table.iter().filter(|t| &t.namespace == ns).collect();
        // `Uint8Array` is used unqualified for bytes and `BinaryInput` for
        // binary request fields; the other globals the models use are
        // naming builtins already.
        let rendered = unique(
            &["Uint8Array", "BinaryInput"],
            &members
                .iter()
                .map(|t| t.name.words.clone())
                .collect::<Vec<_>>(),
            Role::Type,
        );
        for (t, n) in members.into_iter().zip(rendered) {
            names.insert(t.id.clone(), n);
        }
    }

    let types: BTreeMap<TypeId, TypeInfo> = table
        .iter()
        .enumerate()
        .map(|(i, t)| {
            (
                t.id.clone(),
                TypeInfo {
                    name: names.get(&t.id).cloned().unwrap_or_default(),
                    ns: t.namespace.clone(),
                    scc: scc_of[i],
                    cyclic: cyclic[i],
                },
            )
        })
        .collect();
    let models = ns_names
        .iter()
        .zip(ns_words.iter().zip(&stems))
        .map(|(name, (words, stem))| ModelNs {
            name: name.clone(),
            alias: format!("${}", naming::to_case(words, Case::Camel)),
            file: naming::to_case(stem, Case::Kebab),
            types: order
                .iter()
                .filter(|&&i| &table[i].namespace == name)
                .map(|&i| table[i].id.clone())
                .collect(),
        })
        .collect();
    (types, models)
}

/// Every named type a shape references directly (through inline shapes).
pub(crate) fn shape_refs(shape: &Shape, out: &mut Vec<TypeId>) {
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
            if let tungsten_ir::Additional::Typed { values } = additional {
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

/// Callable operations with their descriptor and args type names.
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
    // Keys are decided in id order so they do not depend on the tree.
    let mut by_id: Vec<usize> = (0..entries.len()).collect();
    by_id.sort_by(|&a, &b| entries[a].0.id.cmp(&entries[b].0.id));
    let key_words: Vec<Vec<String>> = by_id
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
    let keys = unique(DESCRIPTOR_RESERVED, &key_words, Role::Param);
    let mut key_of = vec![String::new(); entries.len()];
    for (&i, key) in by_id.iter().zip(keys) {
        key_of[i] = key;
    }
    // Args types follow the keys' words, so they are unique too.
    let args_types = unique(
        &[],
        &by_id
            .iter()
            .map(|&i| with_word(&naming::split_words(&key_of[i]), "args"))
            .collect::<Vec<_>>(),
        Role::Type,
    );
    let mut args_of = vec![String::new(); entries.len()];
    for (&i, a) in by_id.iter().zip(args_types) {
        args_of[i] = a;
    }
    entries
        .into_iter()
        .enumerate()
        .map(|(i, (op, ns))| OpInfo {
            op,
            ns,
            key: std::mem::take(&mut key_of[i]),
            args_type: std::mem::take(&mut args_of[i]),
        })
        .collect()
}
