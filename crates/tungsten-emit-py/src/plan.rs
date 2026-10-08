// SPDX-License-Identifier: AGPL-3.0-only
//! Every name and file path the emitter writes, decided once per IR.
//!
//! Identifiers come from [`tungsten_ir::naming`] for [`Target::Python`] and
//! are made unique per scope with [`naming::disambiguate`]. Names the
//! generated code defines itself always start with `_` (`_internal`,
//! `_m_public`, `_d`, `_core`), which naming never produces for an API
//! name (leading digits get a word prefix here instead of `_`), so they
//! cannot collide with a generated identifier.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_ir::naming::{self, Case, Role, Target};
use tungsten_ir::{Ident, Ir, Operation, OperationStatus, Resource, TypeId};

const PY: Target = Target::Python;

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

/// `words` with `prefix` in front when they render with a leading digit
/// (or are empty), so the name never starts with `_`: Pydantic treats such
/// fields as private attributes, and `_` names are the generator's own.
pub(crate) fn no_leading_digit(words: &[String], prefix: &str) -> Vec<String> {
    let starts_with_digit = words
        .iter()
        .find(|w| !w.is_empty())
        .is_none_or(|w| w.starts_with(|c: char| c.is_ascii_digit()));
    if starts_with_digit {
        let mut w = vec![prefix.to_string()];
        w.extend(words.iter().cloned());
        w
    } else {
        words.to_vec()
    }
}

/// Make the renderings of `entries` unique for `role` within one scope.
/// `reserved` names are taken first (they render to themselves), so an
/// entry that renders like one gets a numeric suffix instead.
pub(crate) fn unique(reserved: &[&str], entries: &[Vec<String>], role: Role) -> Vec<String> {
    let mut idents: Vec<Ident> = reserved.iter().map(|r| Ident::new(*r)).collect();
    idents.extend(entries.iter().map(|w| ident(w)));
    naming::disambiguate(&mut idents, PY, role);
    idents[reserved.len()..]
        .iter()
        .map(|i| naming::render(i, PY, role))
        .collect()
}

/// Like [`unique`], returning the final word lists.
fn unique_words(entries: &[Vec<String>], role: Role) -> Vec<Vec<String>> {
    let mut idents: Vec<Ident> = entries.iter().map(|w| ident(w)).collect();
    naming::disambiguate(&mut idents, PY, role);
    idents.into_iter().map(|i| i.words).collect()
}

/// Names a model class cannot take as a field: attributes of
/// `pydantic.BaseModel`, and the builtins model annotations use
/// unqualified (a field with a default would shadow them in the class
/// body), plus `l` (ruff E741). A field rendering to one gets `_`
/// appended, like a keyword (`json` → `json_`).
pub const FIELD_RESERVED: &[&str] = &[
    "bool",
    "bytes",
    "construct",
    "copy",
    "dict",
    "float",
    "from_orm",
    "int",
    "json",
    "l",
    "list",
    "model_computed_fields",
    "model_config",
    "model_construct",
    "model_copy",
    "model_dump",
    "model_dump_json",
    "model_extra",
    "model_fields",
    "model_fields_set",
    "model_json_schema",
    "model_parametrized_name",
    "model_post_init",
    "model_rebuild",
    "model_validate",
    "model_validate_json",
    "model_validate_strings",
    "object",
    "parse_file",
    "parse_obj",
    "parse_raw",
    "schema",
    "schema_json",
    "str",
    "update_forward_refs",
    "validate",
];

/// Python attribute names of a record's fields, in field order: rendered
/// for [`Role::Field`], a leading digit prefixed with `field`, made unique,
/// and [`FIELD_RESERVED`] names suffixed with `_`.
pub(crate) fn field_names<'f>(wires: impl IntoIterator<Item = &'f Ident>) -> Vec<String> {
    let words: Vec<Vec<String>> = wires
        .into_iter()
        .map(|i| no_leading_digit(&i.words, "field"))
        .collect();
    unique(&[], &words, Role::Field)
        .into_iter()
        .map(|n| {
            if FIELD_RESERVED.contains(&n.as_str()) {
                format!("{n}_")
            } else {
                n
            }
        })
        .collect()
}

/// Names a models module uses unqualified, which a type cannot take.
pub const MODELS_RESERVED: &[&str] = &[
    "Annotated",
    "Any",
    "ConfigDict",
    "Field",
    "I",
    "Literal",
    "O",
    "Tag",
    "Unset",
];

/// A model namespace (one `<module>/models/<file>.py`).
#[derive(Debug, Clone)]
pub(crate) struct ModelNs {
    /// Namespace name as in the IR.
    pub name: String,
    /// Module name under `models/`.
    pub file: String,
    /// Alias other modules import it under (`_m_public`).
    pub alias: String,
    /// Named types in IR order.
    pub types: Vec<TypeId>,
}

#[derive(Debug, Clone)]
pub(crate) struct TypeInfo {
    /// Python name (class or type alias).
    pub name: String,
    pub ns: String,
}

/// One callable operation.
#[derive(Debug, Clone)]
pub(crate) struct OpInfo<'a> {
    pub op: &'a Operation,
    /// Namespace of the operation (for its error model).
    pub ns: String,
    /// Descriptor constant in `_descriptors.py` (`LIST_PETS`).
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberKind {
    /// The operation's method; index into [`Plan::ops`].
    Op(usize),
    /// `preview_<op>`.
    Preview(usize),
    /// `<op>_pages`.
    Pages(usize),
    /// Index into [`Plan::resources`].
    Child(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub name: String,
    pub kind: MemberKind,
}

/// One resource (one module under `resources/` with a sync and an async
/// class).
#[derive(Debug, Clone)]
pub(crate) struct ResInfo<'a> {
    pub res: &'a Resource,
    pub class: String,
    pub async_class: String,
    /// Module name under `resources/`.
    pub module: String,
    pub members: Vec<Member>,
}

/// A namespace of the client (multi-namespace APIs only).
#[derive(Debug, Clone)]
pub(crate) struct ClientNs<'a> {
    pub ns: &'a tungsten_ir::Namespace,
    /// Attribute name on the client.
    pub member: String,
    pub class: String,
    pub async_class: String,
    /// Top-level resources: (attribute name, index into [`Plan::resources`]).
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
    /// Single-namespace APIs: the client's resource attributes.
    pub client_resources: Vec<(String, usize)>,
    pub client_class: String,
    pub async_client_class: String,
}

/// Names `_descriptors.py` defines besides the descriptor constants.
pub(crate) const DESCRIPTOR_RESERVED: &[&str] = &["API", "OPERATIONS"];

/// Attribute names of the client besides its resources or namespaces.
const CLIENT_RESERVED: &[&str] = &["aclose", "close", "core", "macros"];

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
            async_client_class: String::new(),
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

    /// Resource modules, classes and members, the client's members, and
    /// the class names (unique across the package).
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

        // Module names under `resources/`, flat and unique.
        let modules = unique_words(
            &pending
                .iter()
                .map(|p| no_leading_digit(&p.words, "resource"))
                .collect::<Vec<_>>(),
            Role::Module,
        );

        // Class names, unique across the package: each sync name and its
        // `Async` twin are decided together.
        let mut class_words: Vec<Vec<String>> = vec![];
        let mut push_pair = |w: Vec<String>| {
            let mut a = vec!["async".to_string()];
            a.extend(w.iter().cloned());
            class_words.push(w);
            class_words.push(a);
        };
        push_pair(with_word(&ir.api.name.words, "client"));
        if multi {
            for n in &ir.namespaces {
                push_pair(with_word(&n.name.words, "namespace"));
            }
        }
        for p in &pending {
            push_pair(with_word(&p.words, "resource"));
        }
        let classes = unique(&["AsyncMacros", "Macros"], &class_words, Role::Type);
        self.client_class = classes[0].clone();
        self.async_client_class = classes[1].clone();
        let ns_count = if multi { ir.namespaces.len() } else { 0 };
        let res_classes = &classes[2 + 2 * ns_count..];

        let mut children_of: Vec<Vec<usize>> = vec![vec![]; pending.len()];
        for (i, p) in pending.iter().enumerate() {
            if let Some(parent) = p.parent {
                children_of[parent].push(i);
            }
        }
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
                words.push(no_leading_digit(&self.ops[o].op.name.words, "call"));
                kinds.push(MemberKind::Op(o));
            }
            for &c in &children_of[i] {
                words.push(no_leading_digit(&pending[c].res.name.words, "resource"));
                kinds.push(MemberKind::Child(c));
            }
            for &o in &op_indices {
                let base = no_leading_digit(&self.ops[o].op.name.words, "call");
                if crate::ops::has_preview(self.ops[o].op) {
                    let mut w = vec!["preview".to_string()];
                    w.extend(base.iter().cloned());
                    words.push(w);
                    kinds.push(MemberKind::Preview(o));
                }
                if self.ops[o].op.pagination.is_some() {
                    words.push(with_word(&base, "pages"));
                    kinds.push(MemberKind::Pages(o));
                }
            }
            let names = unique(&["l"], &words, Role::Method);
            let members = names
                .into_iter()
                .zip(kinds)
                .map(|(name, kind)| Member { name, kind })
                .collect();
            self.resources.push(ResInfo {
                res: p.res,
                class: res_classes[2 * i].clone(),
                async_class: res_classes[2 * i + 1].clone(),
                module: naming::to_case(&modules[i], Case::Snake),
                members,
            });
        }

        let top_members = |indices: &[usize], reserved: &[&str]| -> Vec<(String, usize)> {
            let words: Vec<Vec<String>> = indices
                .iter()
                .map(|&i| no_leading_digit(&pending[i].res.name.words, "resource"))
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
                    .map(|n| no_leading_digit(&n.name.words, "namespace"))
                    .collect::<Vec<_>>(),
                Role::Method,
            );
            for (k, ns) in ir.namespaces.iter().enumerate() {
                self.client_namespaces.push(ClientNs {
                    ns,
                    member: names[k].clone(),
                    class: classes[2 + 2 * k].clone(),
                    async_class: classes[3 + 2 * k].clone(),
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
            .map(|n| no_leading_digit(&Ident::new(n.as_str()).words, "namespace"))
            .collect::<Vec<_>>(),
        Role::Module,
    );

    let mut types: BTreeMap<TypeId, TypeInfo> = BTreeMap::new();
    for ns in &ns_names {
        let members: Vec<&tungsten_ir::NamedType> =
            table.iter().filter(|t| &t.namespace == ns).collect();
        let rendered = unique(
            MODELS_RESERVED,
            &members
                .iter()
                .map(|t| no_leading_digit(&t.name.words, "model"))
                .collect::<Vec<_>>(),
            Role::Type,
        );
        for (t, name) in members.into_iter().zip(rendered) {
            types.insert(
                t.id.clone(),
                TypeInfo {
                    name,
                    ns: t.namespace.clone(),
                },
            );
        }
    }
    let models = ns_names
        .iter()
        .zip(&files)
        .map(|(name, words)| {
            let file = naming::render(&ident(words), PY, Role::Module);
            ModelNs {
                name: name.clone(),
                alias: format!("_m_{file}"),
                file,
                types: table
                    .iter()
                    .filter(|t| &t.namespace == name)
                    .map(|t| t.id.clone())
                    .collect(),
            }
        })
        .collect();
    (types, models)
}

/// Callable operations with their descriptor constant names.
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
            no_leading_digit(&naming::split_words(local), "op")
        })
        .collect();
    let keys = unique(DESCRIPTOR_RESERVED, &key_words, Role::EnumVariant);
    let mut key_of = vec![String::new(); entries.len()];
    for (&i, key) in by_id.iter().zip(keys) {
        key_of[i] = key;
    }
    entries
        .into_iter()
        .enumerate()
        .map(|(i, (op, ns))| OpInfo {
            op,
            ns,
            key: std::mem::take(&mut key_of[i]),
        })
        .collect()
}
