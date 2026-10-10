// SPDX-License-Identifier: AGPL-3.0-only
//! [`Namer`]: every word list of an [`SdkPlan`] rendered for one target.
//!
//! Names come from [`tungsten_ir::naming::render`] and are made unique per
//! scope with [`naming::disambiguate`] (a numeric word appended to the
//! later entries). The names a language's generated code defines itself in
//! a scope are passed as reserved names ([`NamerOptions::reserved`]); they
//! are rendered like the plan's names and taken first, so a plan name that
//! renders like one gets a number instead. In PHP, whose class, method,
//! function and namespace names are case-insensitive, uniqueness is also
//! checked case-insensitively. Every name that changed is listed in
//! [`NameMap::renames`], for the emitter's TG0401-style report.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_ir::naming::{self, Role, Target};
use tungsten_ir::{Ident, Shape, TypeId};

use super::macros::InputSource;
use super::plan::{AllOfPlan, ClientPlan, MemberPlan, SdkPlan, TypeKind};
use crate::args;

/// A naming scope. Names are unique within one instance of a scope (one
/// namespace's types, one record's fields, one resource's members, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Model namespace module names ([`Role::Module`]).
    Modules,
    /// Named types of one model namespace ([`Role::Type`]).
    Types,
    /// Fields of one record ([`Role::Field`]).
    Fields,
    /// Values of one enum ([`Role::EnumVariant`]).
    EnumVariants,
    /// Variants of one union, as types ([`Role::Type`]).
    UnionVariants,
    /// The client, namespace and resource classes, one scope for the
    /// package ([`Role::Type`]).
    Classes,
    /// Members of one resource ([`Role::Method`]).
    Members,
    /// Members of the client: top-level resources or namespaces
    /// ([`Role::Method`]); also the members of each namespace class.
    Client,
    /// Operation names for dispatch tables, one scope for the package
    /// ([`Role::Method`]).
    Operations,
    /// Arguments of one operation ([`Role::Param`]).
    Arguments,
    /// Macro members, one scope ([`Role::Method`]).
    Macros,
    /// The input of one macro: the extended operation's argument names
    /// stay, added fields are made unique against them ([`Role::Param`]).
    MacroInput,
}

impl Scope {
    /// The role names in this scope are rendered for.
    pub fn role(self) -> Role {
        match self {
            Scope::Modules => Role::Module,
            Scope::Types | Scope::UnionVariants | Scope::Classes => Role::Type,
            Scope::Fields => Role::Field,
            Scope::EnumVariants => Role::EnumVariant,
            Scope::Members | Scope::Client | Scope::Operations | Scope::Macros => Role::Method,
            Scope::Arguments | Scope::MacroInput => Role::Param,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Modules => "modules",
            Scope::Types => "types",
            Scope::Fields => "fields",
            Scope::EnumVariants => "enum_variants",
            Scope::UnionVariants => "union_variants",
            Scope::Classes => "classes",
            Scope::Members => "members",
            Scope::Client => "client",
            Scope::Operations => "operations",
            Scope::Arguments => "arguments",
            Scope::Macros => "macros",
            Scope::MacroInput => "macro_input",
        }
    }
}

/// What a language's names depend on besides the plan.
#[derive(Debug, Clone, Default)]
pub struct NamerOptions {
    /// Names the generated code defines itself, per scope, in the target's
    /// spelling (`Macros` among the classes, `opts` among the arguments).
    pub reserved: BTreeMap<Scope, Vec<String>>,
    /// Resource members derived from an operation, named after it:
    /// `preview <op>` for operations with a preview.
    pub preview_members: bool,
    /// `<op> pages` for paginated operations.
    pub pages_members: bool,
    /// `<op> stream` for operations with an event stream.
    pub stream_members: bool,
}

/// A resource member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    /// Index into [`SdkPlan::operations`].
    Operation(usize),
    /// Index into [`SdkPlan::resources`].
    Child(usize),
    /// The preview of the operation at this index.
    Preview(usize),
    /// The page iterator of the operation at this index.
    Pages(usize),
    /// The event stream of the operation at this index.
    Stream(usize),
}

/// A named member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
}

/// The names of one resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceNames {
    pub class: String,
    /// Its operations and children (in plan order) followed by the derived
    /// members, each operation's in the order preview, pages, stream.
    pub members: Vec<Member>,
}

/// The names of one operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationNames {
    /// The operation's name in the package-wide scope [`Scope::Operations`].
    pub name: String,
    /// One name per [`super::OpPlan::arguments`] entry.
    pub arguments: Vec<String>,
}

/// The names of one macro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroNames {
    pub member: String,
    /// One name per [`super::MacroPlan::input`] field.
    pub input: Vec<String>,
}

/// A name that is not the plain rendering of its words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub scope: Scope,
    /// What the scope belongs to: a type id, a namespace, an operation id,
    /// a macro name or a resource path; empty for package-wide scopes.
    pub owner: String,
    /// The words the name was rendered from, joined by spaces.
    pub words: String,
    /// The name given.
    pub name: String,
}

/// Every name of a plan for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMap {
    pub target: Target,
    /// Model namespace module names by namespace name.
    pub modules: BTreeMap<String, String>,
    /// Type names by id, unique within their namespace.
    pub types: BTreeMap<TypeId, String>,
    /// Field names by type id, in field order: records, and `allOf` types
    /// planned as [`AllOfPlan::Merged`] (in the plan's field order).
    pub fields: BTreeMap<TypeId, Vec<String>>,
    /// Enum value names by type id, in value order.
    pub enum_variants: BTreeMap<TypeId, Vec<String>>,
    /// Union variant names by type id, in variant order.
    pub union_variants: BTreeMap<TypeId, Vec<String>>,
    pub client_class: String,
    /// Multi-namespace APIs: one class per namespace, in IR order.
    pub namespace_classes: Vec<String>,
    /// The client's members, one per entry of [`SdkPlan::client`].
    pub client_members: Vec<String>,
    /// Multi-namespace APIs: the members of each namespace class (its
    /// top-level resources).
    pub namespace_members: Vec<Vec<String>>,
    /// One entry per [`SdkPlan::resources`].
    pub resources: Vec<ResourceNames>,
    /// One entry per [`SdkPlan::operations`].
    pub operations: Vec<OperationNames>,
    /// One entry per [`SdkPlan::macros`].
    pub macros: Vec<MacroNames>,
    pub renames: Vec<Rename>,
}

impl NameMap {
    /// The name of the argument with this args key of an operation.
    pub fn argument(&self, plan: &SdkPlan<'_>, op: usize, key: &str) -> Option<&str> {
        let i = plan
            .operations
            .get(op)?
            .arguments
            .iter()
            .position(|a| a.key == key)?;
        self.operations
            .get(op)?
            .arguments
            .get(i)
            .map(String::as_str)
    }
}

/// Renders a plan's names; see the module documentation.
#[derive(Debug, Clone, Copy)]
pub struct Namer<'p, 'a> {
    plan: &'p SdkPlan<'a>,
}

impl<'p, 'a> Namer<'p, 'a> {
    pub fn new(plan: &'p SdkPlan<'a>) -> Self {
        Self { plan }
    }

    /// Every name of the plan for `target`.
    pub fn names(&self, target: Target, options: &NamerOptions) -> NameMap {
        let plan = self.plan;
        let mut cx = Cx {
            target,
            options,
            renames: vec![],
        };
        let modules_words: Vec<Vec<String>> =
            plan.namespaces.iter().map(|n| n.words.clone()).collect();
        let modules = cx
            .unique(Scope::Modules, "", &modules_words)
            .into_iter()
            .zip(&plan.namespaces)
            .map(|(name, ns)| (ns.name.clone(), name))
            .collect();

        let mut types = BTreeMap::new();
        let mut fields = BTreeMap::new();
        let mut enum_variants = BTreeMap::new();
        let mut union_variants = BTreeMap::new();
        for ns in &plan.namespaces {
            let members: Vec<&TypeId> = plan
                .types
                .keys()
                .filter(|id| plan.types[*id].namespace == ns.name)
                .collect();
            let words: Vec<Vec<String>> = members
                .iter()
                .map(|id| plan.types[*id].words.clone())
                .collect();
            for (id, name) in members
                .into_iter()
                .zip(cx.unique(Scope::Types, &ns.name, &words))
            {
                types.insert(id.clone(), name);
            }
        }
        for t in plan.ir.types.types.iter() {
            let owner = t.id.0.as_str();
            match (&t.shape, plan.types.get(&t.id).map(|p| &p.kind)) {
                (Shape::Record { fields: own, .. }, _) => {
                    let words: Vec<Vec<String>> =
                        own.iter().map(|f| f.name.words.clone()).collect();
                    fields.insert(t.id.clone(), cx.unique(Scope::Fields, owner, &words));
                }
                (
                    Shape::Intersection { members },
                    Some(TypeKind::AllOf(AllOfPlan::Merged { fields: merged, .. })),
                ) => {
                    let words: Vec<Vec<String>> = merged
                        .iter()
                        .filter_map(|&(m, f)| match args::resolve(plan.ir, members.get(m)?) {
                            Some(Shape::Record { fields, .. }) => {
                                fields.get(f).map(|f| f.name.words.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    fields.insert(t.id.clone(), cx.unique(Scope::Fields, owner, &words));
                }
                (Shape::Enum { values, .. }, _) => {
                    let words: Vec<Vec<String>> =
                        values.iter().map(|v| v.name.words.clone()).collect();
                    enum_variants
                        .insert(t.id.clone(), cx.unique(Scope::EnumVariants, owner, &words));
                }
                (Shape::Union(u), _) => {
                    let words: Vec<Vec<String>> =
                        u.variants.iter().map(|v| v.name.words.clone()).collect();
                    union_variants
                        .insert(t.id.clone(), cx.unique(Scope::UnionVariants, owner, &words));
                }
                _ => {}
            }
        }

        // Classes: the client, the namespaces, the resources.
        let mut class_words = vec![with_word(&plan.ir.api.name.words, "client")];
        let namespaces = match &plan.client {
            ClientPlan::Namespaces(list) => list.as_slice(),
            ClientPlan::Resources(_) => &[],
        };
        class_words.extend(namespaces.iter().map(|n| with_word(&n.words, "namespace")));
        class_words.extend(
            plan.resources
                .iter()
                .map(|r| with_word(&r.path_words, "resource")),
        );
        let classes = cx.unique(Scope::Classes, "", &class_words);
        let client_class = classes[0].clone();
        let namespace_classes = classes[1..1 + namespaces.len()].to_vec();
        let resource_classes = &classes[1 + namespaces.len()..];

        let mut resources = vec![];
        for (i, r) in plan.resources.iter().enumerate() {
            let mut words: Vec<Vec<String>> = vec![];
            let mut kinds: Vec<MemberKind> = vec![];
            for m in &r.members {
                match *m {
                    MemberPlan::Operation(o) => {
                        words.push(plan.operations[o].op.name.words.clone());
                        kinds.push(MemberKind::Operation(o));
                    }
                    MemberPlan::Child(c) => {
                        words.push(plan.resources[c].resource.name.words.clone());
                        kinds.push(MemberKind::Child(c));
                    }
                }
            }
            for m in &r.members {
                let MemberPlan::Operation(o) = *m else {
                    continue;
                };
                let op = &plan.operations[o];
                let base = &op.op.name.words;
                if options.preview_members && op.has_preview {
                    let mut w = vec!["preview".to_string()];
                    w.extend(base.iter().cloned());
                    words.push(w);
                    kinds.push(MemberKind::Preview(o));
                }
                if options.pages_members && op.page.is_some() {
                    words.push(with_word(base, "pages"));
                    kinds.push(MemberKind::Pages(o));
                }
                if options.stream_members && op.stream.is_some() {
                    words.push(with_word(base, "stream"));
                    kinds.push(MemberKind::Stream(o));
                }
            }
            let owner = r.path_words.join(" ");
            let names = cx.unique(Scope::Members, &owner, &words);
            resources.push(ResourceNames {
                class: resource_classes[i].clone(),
                members: names
                    .into_iter()
                    .zip(kinds)
                    .map(|(name, kind)| Member { name, kind })
                    .collect(),
            });
        }

        let resource_words = |list: &[usize]| -> Vec<Vec<String>> {
            list.iter()
                .map(|&i| plan.resources[i].resource.name.words.clone())
                .collect()
        };
        let (client_members, namespace_members) = match &plan.client {
            ClientPlan::Resources(list) => {
                (cx.unique(Scope::Client, "", &resource_words(list)), vec![])
            }
            ClientPlan::Namespaces(list) => {
                let words: Vec<Vec<String>> = list.iter().map(|n| n.words.clone()).collect();
                let members = cx.unique(Scope::Client, "", &words);
                let inner = list
                    .iter()
                    .map(|n| cx.unique_plain(Role::Method, &n.name, &resource_words(&n.resources)))
                    .collect();
                (members, inner)
            }
        };

        let op_words: Vec<Vec<String>> = plan
            .operations
            .iter()
            .map(|o| o.key_words.clone())
            .collect();
        let op_names = cx.unique(Scope::Operations, "", &op_words);
        let mut operations = vec![];
        for (op, name) in plan.operations.iter().zip(op_names) {
            let words: Vec<Vec<String>> = op.arguments.iter().map(|a| a.words.clone()).collect();
            operations.push(OperationNames {
                name,
                arguments: cx.unique(Scope::Arguments, &op.op.id.0, &words),
            });
        }

        let macro_words: Vec<Vec<String>> = plan.macros.iter().map(|m| m.words.clone()).collect();
        let macro_members = cx.unique(Scope::Macros, "", &macro_words);
        let mut macros = vec![];
        for (m, member) in plan.macros.iter().zip(macro_members) {
            let base_names: Vec<String> = m
                .input
                .iter()
                .filter_map(|f| match f.source {
                    InputSource::Argument(i) => {
                        m.base.and_then(|b| operations[b].arguments.get(i).cloned())
                    }
                    InputSource::Added { .. } => None,
                })
                .collect();
            let added: Vec<Vec<String>> = m
                .input
                .iter()
                .filter(|f| matches!(f.source, InputSource::Added { .. }))
                .map(|f| f.words.clone())
                .collect();
            let mut taken: Vec<String> = cx.reserved(Scope::MacroInput);
            taken.extend(base_names.iter().cloned());
            let added_names = cx.unique_with(Scope::MacroInput, m.name(), &taken, &added);
            let mut input = base_names;
            input.extend(added_names);
            macros.push(MacroNames { member, input });
        }

        NameMap {
            target,
            modules,
            types,
            fields,
            enum_variants,
            union_variants,
            client_class,
            namespace_classes,
            client_members,
            namespace_members,
            resources,
            operations,
            macros,
            renames: cx.renames,
        }
    }
}

fn with_word(words: &[String], extra: &str) -> Vec<String> {
    let mut w = words.to_vec();
    w.push(extra.to_string());
    w
}

fn ident(words: &[String]) -> Ident {
    Ident {
        wire: words.join(" "),
        words: words.to_vec(),
    }
}

struct Cx<'o> {
    target: Target,
    options: &'o NamerOptions,
    renames: Vec<Rename>,
}

impl Cx<'_> {
    fn reserved(&self, scope: Scope) -> Vec<String> {
        self.options
            .reserved
            .get(&scope)
            .cloned()
            .unwrap_or_default()
    }

    /// Unique names for `entries` in one instance of `scope`.
    fn unique(&mut self, scope: Scope, owner: &str, entries: &[Vec<String>]) -> Vec<String> {
        let reserved = self.reserved(scope);
        self.unique_with(scope, owner, &reserved, entries)
    }

    /// Unique names without reserved names (the members of a namespace
    /// class, which only holds its resources).
    fn unique_plain(&mut self, role: Role, owner: &str, entries: &[Vec<String>]) -> Vec<String> {
        let (names, changed) = render_unique(self.target, role, &[], entries);
        for i in changed {
            self.renames.push(Rename {
                scope: Scope::Client,
                owner: owner.to_string(),
                words: entries[i].join(" "),
                name: names[i].clone(),
            });
        }
        names
    }

    fn unique_with(
        &mut self,
        scope: Scope,
        owner: &str,
        reserved: &[String],
        entries: &[Vec<String>],
    ) -> Vec<String> {
        let (names, changed) = render_unique(self.target, scope.role(), reserved, entries);
        for i in changed {
            self.renames.push(Rename {
                scope,
                owner: owner.to_string(),
                words: entries[i].join(" "),
                name: names[i].clone(),
            });
        }
        names
    }
}

/// Whether names of `target` collide regardless of case.
fn folds_case(target: Target) -> bool {
    target == Target::Php
}

/// Render `entries` unique for `role`, after the `reserved` names; returns
/// the names and the indexes of the entries whose words got a number.
fn render_unique(
    target: Target,
    role: Role,
    reserved: &[String],
    entries: &[Vec<String>],
) -> (Vec<String>, Vec<usize>) {
    let mut idents: Vec<Ident> = reserved.iter().map(|r| Ident::new(r.as_str())).collect();
    idents.extend(entries.iter().map(|w| ident(w)));
    naming::disambiguate(&mut idents, target, role);
    let mut names: Vec<String> = idents
        .iter()
        .map(|i| naming::render(i, target, role))
        .collect();
    if folds_case(target) {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let all: BTreeSet<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
        for (i, id) in idents.iter_mut().enumerate() {
            if seen.insert(names[i].to_ascii_lowercase()) {
                continue;
            }
            let base = id.words.clone();
            let mut n: u64 = 2;
            loop {
                let mut words = base.clone();
                words.push(n.to_string());
                let candidate = naming::render(&ident(&words), target, role);
                let folded = candidate.to_ascii_lowercase();
                if !all.contains(&folded) && seen.insert(folded) {
                    id.words = words;
                    names[i] = candidate;
                    break;
                }
                n += 1;
            }
        }
    }
    let changed = (0..entries.len())
        .filter(|&i| idents[reserved.len() + i].words != entries[i])
        .collect();
    (names.split_off(reserved.len()), changed)
}
