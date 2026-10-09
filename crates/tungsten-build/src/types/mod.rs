// SPDX-License-Identifier: AGPL-3.0-only
//! Schema → IR type conversion: types, nullability and field presence,
//! unions, identifiers and reference cycles.
//!
//! Model:
//! - Every `#/components/schemas/<Key>` of an entry document is the named
//!   type `<namespace>.<Key>`. Every other `$ref` target (a schema in a
//!   fragment file, a pointer into another schema) becomes a named type the
//!   first time it is referenced. A `$ref` is therefore always
//!   `TypeRef::Named`, and every reference cycle passes through a named
//!   type.
//! - Inline schemas whose shape needs a name in some target language
//!   (records, enums, unions, intersections) are registered as named types
//!   named after the hint words of their position; every other inline shape
//!   stays `TypeRef::Inline`.
//! - Conversion is memoized by [`RefTarget`]. A named type is registered
//!   with a placeholder shape before its schema is converted, so recursion
//!   terminates at the second visit.
//! - Nullability is kept apart from shapes: a named type's shape is never
//!   `Nullable`. Every reference to a schema that admits `null` carries it,
//!   as field presence or as `Shape::Nullable` in other positions.
//! - A named type whose schema only points at another named type (`$ref`
//!   with annotations, a one-member `allOf`, `anyOf: [$ref, null]`) is an
//!   alias: [`TypeBuilder::finish`] copies the target's shape into it.
//! - Names are made unique per scope with TG0401: fields and enum values
//!   when their record or enum is built, types per namespace in `finish`.

mod allof;
mod convert;
mod names;
mod schema;
mod union;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_ir::naming::Role;
use tungsten_ir::{Field, Ident, NamedType, Shape, SourceRef, TypeId, TypeRef, TypeTable};
use tungsten_openapi::{DocId, RefTarget, Workspace, is_named_schema, join_pointer, split_pointer};

/// Display name used to label diagnostics about `types.break_cycles`.
const MANIFEST_FILE: &str = "tungsten.yml";

/// Deepest nesting of named-type builds (each `$ref` to a type not built
/// yet starts a nested build). Deeper chains are TG0105 errors; the limit
/// sits far below what the compile thread's stack holds (driver.rs).
pub(crate) const MAX_BUILD_DEPTH: usize = 4096;

/// Converts schemas to IR types, memoizing named types by reference target
/// so each `components/schemas` entry becomes exactly one `NamedType`.
#[derive(Debug)]
pub struct TypeBuilder<'a> {
    ws: &'a Workspace,
    break_cycles: Vec<BreakCycle>,
    entries: BTreeMap<TypeId, Entry>,
    by_target: BTreeMap<RefTarget, TypeId>,
    /// Results for targets that did not need a named type.
    inline: BTreeMap<RefTarget, Resolved>,
    /// Namespace of each entry document passed to `add_components`.
    doc_namespace: BTreeMap<DocId, String>,
    /// `allOf` members being flattened (guards against `allOf` cycles).
    expanding: BTreeSet<RefTarget>,
    /// Named-type builds in progress (nesting depth).
    depth: usize,
    /// Memoized [`TypeBuilder::admits_null`] answers.
    null_estimates: BTreeMap<RefTarget, bool>,
    diagnostics: Diagnostics,
    /// (code, file, pointer, message) of every diagnostic already pushed.
    reported: BTreeSet<(String, String, String, String)>,
}

/// A `types.break_cycles` entry `Type.field` and the pointer it names.
#[derive(Debug)]
struct BreakCycle {
    text: String,
    pointer: Option<String>,
}

#[derive(Debug)]
struct Entry {
    ty: NamedType,
    /// Where the type is declared (its origin).
    target: RefTarget,
    /// The schema converted into the shape: `target`, or the fragment
    /// schema a component adopted.
    source: RefTarget,
    /// A `components/schemas` entry of an entry document.
    component: bool,
    /// The schema admits `null` (carried by references, not the shape).
    nullable: bool,
    /// The schema only references this other named type.
    alias: Option<TypeId>,
    /// Listed in `types.break_cycles`.
    forced_recursive: bool,
    state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Registered, conversion not started.
    Pending,
    /// Conversion in progress (the shape is a placeholder).
    Building,
    Done,
}

/// A converted reference: the non-null type and whether `null` is admitted.
#[derive(Debug, Clone)]
struct Resolved {
    ty: TypeRef,
    nullable: bool,
}

impl Resolved {
    fn any() -> Self {
        Self {
            ty: TypeRef::Inline(Box::new(Shape::Any)),
            nullable: false,
        }
    }

    /// The type for a position without presence (array items, map values,
    /// parameters, bodies): nullability becomes `Shape::Nullable`.
    fn positional(self) -> TypeRef {
        if self.nullable {
            TypeRef::Inline(Box::new(Shape::Nullable { inner: self.ty }))
        } else {
            self.ty
        }
    }
}

/// What converting one schema produced, before it is placed anywhere.
#[derive(Debug)]
enum Body {
    Shape(Shape),
    /// The schema is just a reference to this named type.
    Named(TypeId),
}

impl Body {
    fn of(ty: TypeRef) -> Self {
        match ty {
            TypeRef::Named(id) => Body::Named(id),
            TypeRef::Inline(shape) => Body::Shape(*shape),
        }
    }
}

#[derive(Debug)]
struct Conv {
    body: Body,
    nullable: bool,
}

impl Conv {
    fn shape(shape: Shape) -> Self {
        Self {
            body: Body::Shape(shape),
            nullable: false,
        }
    }

    fn of(resolved: Resolved) -> Self {
        Self {
            body: Body::of(resolved.ty),
            nullable: resolved.nullable,
        }
    }
}

/// Shapes that need a declaration (and so a name) in some target language.
fn needs_name(shape: &Shape) -> bool {
    matches!(
        shape,
        Shape::Record { .. } | Shape::Enum { .. } | Shape::Union(_) | Shape::Intersection { .. }
    )
}

impl<'a> TypeBuilder<'a> {
    pub fn new(ws: &'a Workspace, break_cycles: &[String]) -> Self {
        let break_cycles = break_cycles
            .iter()
            .map(|text| BreakCycle {
                text: text.clone(),
                pointer: text.split_once('.').map(|(ty, field)| {
                    join_pointer(
                        &join_pointer(&join_pointer("/components/schemas", ty), "properties"),
                        field,
                    )
                }),
            })
            .collect();
        Self {
            ws,
            break_cycles,
            entries: BTreeMap::new(),
            by_target: BTreeMap::new(),
            inline: BTreeMap::new(),
            doc_namespace: BTreeMap::new(),
            expanding: BTreeSet::new(),
            depth: 0,
            null_estimates: BTreeMap::new(),
            diagnostics: Diagnostics::new(),
            reported: BTreeSet::new(),
        }
    }

    /// Register every `#/components/schemas/*` entry of `doc` as a named
    /// type `namespace.Name`, in sorted order.
    pub fn add_components(&mut self, namespace: &str, doc: DocId) {
        let ws = self.ws;
        self.doc_namespace
            .entry(doc)
            .or_insert_with(|| namespace.to_string());
        let Some(Value::Object(schemas)) = ws
            .documents
            .get(doc)
            .and_then(|d| d.get("/components/schemas"))
        else {
            return;
        };
        let mut keys: Vec<&String> = schemas.keys().collect();
        keys.sort();
        // Reserve every component id before converting anything, so inline
        // types never take a component's id.
        let mut registered = vec![];
        for key in keys {
            let target = RefTarget {
                doc,
                pointer: join_pointer("/components/schemas", key),
            };
            if self.by_target.contains_key(&target) {
                continue;
            }
            let id = self.allocate(namespace, key);
            self.insert_entry(
                namespace,
                &target,
                id.clone(),
                Ident::new(key.as_str()),
                true,
            );
            registered.push(id);
        }
        // `X: {$ref: fragment.yaml#/X}` declares the fragment schema under
        // the component's name: the component adopts it instead of
        // aliasing a second type of the same name.
        for id in &registered {
            if let Some(adopted) = self.adoptable(id) {
                self.by_target.insert(adopted.clone(), id.clone());
                let doc = ws
                    .get(&adopted)
                    .and_then(Value::as_object)
                    .and_then(schema::doc_of);
                if let Some(e) = self.entries.get_mut(id) {
                    e.ty.doc = e.ty.doc.take().or(doc);
                    e.source = adopted;
                }
            }
        }
        for id in registered {
            self.build_if_pending(&id);
        }
    }

    /// Convert the schema at `target` (an inline schema object or a `$ref`)
    /// into a type reference. References to component schemas become
    /// `TypeRef::Named`. Inline records, enums and unions that need a name
    /// in some target language are registered as named types using
    /// `name_hint` words (for example `["SubmitAlphaMessage", "Body"]`).
    pub fn type_ref(&mut self, namespace: &str, target: &RefTarget, name_hint: &[&str]) -> TypeRef {
        let hint = names::hint_words(name_hint);
        self.resolve(namespace, target, &hint).positional()
    }

    /// The type id registered for a target, if any. A `$ref` schema gives
    /// the id of the schema it references.
    pub fn type_id_for(&self, target: &RefTarget) -> Option<TypeId> {
        if let Some(id) = self.by_target.get(target) {
            return Some(id.clone());
        }
        let resolved = self.ws.deref(target)?;
        self.by_target.get(&resolved).cloned()
    }

    /// The shape behind a type reference, following `Named` through the
    /// table built so far (and through aliases). Does not unwrap `Nullable`.
    pub fn shape_of<'s>(&'s self, r: &'s TypeRef) -> Option<&'s Shape> {
        match r {
            TypeRef::Inline(s) => Some(s),
            TypeRef::Named(id) => self.named_shape(id),
        }
    }

    /// The fields of the record behind a type reference, looking through
    /// `Named`, aliases and `Nullable`. `None` when it is not a record.
    pub fn record_fields<'s>(&'s self, r: &'s TypeRef) -> Option<&'s [Field]> {
        match self.shape_of(r)? {
            Shape::Record { fields, .. } => Some(fields),
            Shape::Nullable { inner } => match self.shape_of(inner)? {
                Shape::Record { fields, .. } => Some(fields),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether the schema behind a registered type admits `null`. References
    /// built by this builder already carry it; this is for callers that hold
    /// a bare `TypeId`.
    pub fn is_nullable(&self, id: &TypeId) -> bool {
        self.entries.get(id).is_some_and(|e| e.nullable)
    }

    /// Finish: resolve aliases, mark recursive types, make type names
    /// unique per namespace (TG0401), and return the table sorted by id
    /// with the diagnostics collected.
    pub fn finish(mut self) -> (TypeTable, Diagnostics) {
        self.resolve_aliases();
        for e in self.entries.values_mut() {
            let graph = &self.ws.graph;
            e.ty.recursive = e.forced_recursive
                || graph.is_recursive(&e.target)
                || graph.is_recursive(&e.source);
        }
        self.disambiguate_type_names();
        self.report_unmatched_break_cycles();
        let mut table = TypeTable {
            types: self.entries.into_values().map(|e| e.ty).collect(),
        };
        table.sort();
        (table, self.diagnostics)
    }

    // ----- registration -------------------------------------------------

    /// The fragment schema a component only references, when no other
    /// type has claimed it yet.
    fn adoptable(&self, id: &TypeId) -> Option<RefTarget> {
        let target = &self.entries.get(id)?.target;
        let map = self.ws.get(target)?.as_object()?;
        if map.keys().any(|k| k != "$ref" && schema::is_structural(k)) {
            return None;
        }
        let to = self.ws.resolve(target.doc, map.get("$ref")?.as_str()?)?;
        let fragment =
            !self.ws.entries.contains(&to.doc) && !self.doc_namespace.contains_key(&to.doc);
        (fragment && !self.by_target.contains_key(&to)).then_some(to)
    }

    /// Resolve the schema at `target`: memoized, registering a named type
    /// when the target is a `$ref` target, a `types.break_cycles` edge, or
    /// an inline schema whose shape needs a name.
    fn resolve(&mut self, ns: &str, target: &RefTarget, hint: &[String]) -> Resolved {
        if let Some(id) = self.by_target.get(target) {
            let id = id.clone();
            self.build_if_pending(&id);
            return Resolved {
                nullable: self.is_nullable(&id),
                ty: TypeRef::Named(id),
            };
        }
        if let Some(r) = self.inline.get(target) {
            return r.clone();
        }
        let forced = self.breaks_cycle(target);
        if forced || self.ws.graph.nodes.contains(target) {
            let id = self.register(ns, target, hint);
            if forced && let Some(e) = self.entries.get_mut(&id) {
                e.forced_recursive = true;
            }
            return Resolved {
                nullable: self.is_nullable(&id),
                ty: TypeRef::Named(id),
            };
        }
        let ws = self.ws;
        let Some(value) = ws.get(target) else {
            return Resolved::any();
        };
        let conv = self.convert(ns, target, value, hint);
        let ty = self.place(ns, target, conv.body, hint, conv.nullable, true);
        let resolved = Resolved {
            ty,
            nullable: conv.nullable,
        };
        if !self.by_target.contains_key(target) {
            self.inline.insert(target.clone(), resolved.clone());
        }
        resolved
    }

    /// Turn a converted body into a reference: shapes that need a name are
    /// registered as named types (memoized under `target` when `memo`),
    /// others stay inline.
    fn place(
        &mut self,
        ns: &str,
        target: &RefTarget,
        body: Body,
        hint: &[String],
        nullable: bool,
        memo: bool,
    ) -> TypeRef {
        match body {
            Body::Named(id) => TypeRef::Named(id),
            Body::Shape(shape) if needs_name(&shape) => {
                TypeRef::Named(self.insert_built(ns, target, hint, shape, nullable, memo))
            }
            Body::Shape(shape) => TypeRef::Inline(Box::new(shape)),
        }
    }

    /// Register `target` as a named type (once) and build it.
    fn register(&mut self, ns: &str, target: &RefTarget, hint: &[String]) -> TypeId {
        if let Some(id) = self.by_target.get(target) {
            let id = id.clone();
            self.build_if_pending(&id);
            return id;
        }
        let ns = self
            .doc_namespace
            .get(&target.doc)
            .cloned()
            .unwrap_or_else(|| ns.to_string());
        let tokens = split_pointer(&target.pointer);
        let in_entry = self.doc_namespace.contains_key(&target.doc);
        let (name, component) = if in_entry && is_named_schema(&target.pointer) {
            (Ident::new(tokens[2].as_str()), true)
        } else {
            let mut words = if in_entry && !hint.is_empty() {
                hint.to_vec()
            } else {
                names::pointer_words(&tokens)
            };
            if words.is_empty() {
                words = names::hint_words(&[self.file_stem(target.doc)]);
            }
            if words.is_empty() {
                words = vec!["schema".to_string()];
            }
            (names::type_ident(&words), false)
        };
        let id = self.allocate(&ns, &name.wire);
        self.insert_entry(&ns, target, id.clone(), name, component);
        self.build_if_pending(&id);
        id
    }

    /// Register an inline shape that was converted before being named.
    fn insert_built(
        &mut self,
        ns: &str,
        target: &RefTarget,
        hint: &[String],
        shape: Shape,
        nullable: bool,
        memo: bool,
    ) -> TypeId {
        let ns = self
            .doc_namespace
            .get(&target.doc)
            .cloned()
            .unwrap_or_else(|| ns.to_string());
        let mut words = hint.to_vec();
        if words.is_empty() {
            words = names::pointer_words(&split_pointer(&target.pointer));
        }
        if words.is_empty() {
            words = vec!["schema".to_string()];
        }
        let name = names::type_ident(&words);
        let id = self.allocate(&ns, &name.wire);
        let doc = if memo {
            self.ws
                .get(target)
                .and_then(Value::as_object)
                .and_then(schema::doc_of)
        } else {
            None
        };
        self.entries.insert(
            id.clone(),
            Entry {
                ty: NamedType {
                    id: id.clone(),
                    name,
                    namespace: ns,
                    shape,
                    doc,
                    recursive: false,
                    origin: self.source_ref(target),
                },
                target: target.clone(),
                source: target.clone(),
                component: false,
                nullable,
                alias: None,
                forced_recursive: false,
                state: State::Done,
            },
        );
        if memo {
            self.by_target.insert(target.clone(), id.clone());
        }
        id
    }

    /// Insert a pending entry with a placeholder shape.
    fn insert_entry(
        &mut self,
        ns: &str,
        target: &RefTarget,
        id: TypeId,
        name: Ident,
        component: bool,
    ) {
        let doc = self
            .ws
            .get(target)
            .and_then(Value::as_object)
            .and_then(schema::doc_of);
        let nullable = self.admits_null(target);
        self.entries.insert(
            id.clone(),
            Entry {
                ty: NamedType {
                    id: id.clone(),
                    name,
                    namespace: ns.to_string(),
                    shape: Shape::Any,
                    doc,
                    recursive: false,
                    origin: self.source_ref(target),
                },
                target: target.clone(),
                source: target.clone(),
                component,
                nullable,
                alias: None,
                forced_recursive: false,
                state: State::Pending,
            },
        );
        self.by_target.insert(target.clone(), id);
    }

    /// Convert a pending entry's schema into its shape.
    fn build_if_pending(&mut self, id: &TypeId) {
        let Some(e) = self.entries.get_mut(id) else {
            return;
        };
        if e.state != State::Pending {
            return;
        }
        e.state = State::Building;
        let target = e.source.clone();
        let ns = e.ty.namespace.clone();
        let hint = e.ty.name.words.clone();
        let ws = self.ws;
        if self.depth >= MAX_BUILD_DEPTH {
            e.state = State::Done;
            let message = format!(
                "schema references nest more than {MAX_BUILD_DEPTH} named types deep; this type is treated as any"
            );
            self.report(
                Diagnostic::error("TG0105", message)
                    .with_help("shorten the chain of schemas that each reference the next one"),
                &target.doc_pointer(),
            );
            return;
        }
        // A nested build is a fresh `allOf` flattening context.
        let expanding = std::mem::take(&mut self.expanding);
        self.depth += 1;
        let conv = match ws.get(&target) {
            Some(value) => self.convert(&ns, &target, value, &hint),
            None => Conv::shape(Shape::Any),
        };
        self.depth -= 1;
        self.expanding = expanding;
        let Some(e) = self.entries.get_mut(id) else {
            return;
        };
        e.state = State::Done;
        e.nullable = conv.nullable;
        match conv.body {
            Body::Shape(shape) => e.ty.shape = shape,
            Body::Named(other) if other != *id => e.alias = Some(other),
            // A schema that only references itself admits anything.
            Body::Named(_) => {}
        }
    }

    /// A free id `<ns>.<base>`, or `<ns>.<base><n>` with the smallest
    /// `n >= 2` that is free.
    fn allocate(&self, ns: &str, base: &str) -> TypeId {
        let first = TypeId(format!("{ns}.{base}"));
        if !self.entries.contains_key(&first) {
            return first;
        }
        (2u64..)
            .map(|n| TypeId(format!("{ns}.{base}{n}")))
            .find(|id| !self.entries.contains_key(id))
            .unwrap_or(first)
    }

    /// The shape behind a named type, following aliases. `None` for an
    /// alias cycle (a chain longer than the number of types repeats one).
    fn named_shape(&self, id: &TypeId) -> Option<&Shape> {
        let mut id = id;
        for _ in 0..=self.entries.len() {
            let e = self.entries.get(id)?;
            match &e.alias {
                Some(next) => id = next,
                None => return Some(&e.ty.shape),
            }
        }
        None
    }

    fn is_component(&self, id: &TypeId) -> bool {
        self.entries.get(id).is_some_and(|e| e.component)
    }

    /// The name words of a registered type (the hint for its children).
    fn name_words(&self, id: &TypeId) -> Vec<String> {
        self.entries
            .get(id)
            .map(|e| e.ty.name.words.clone())
            .unwrap_or_default()
    }

    fn breaks_cycle(&self, target: &RefTarget) -> bool {
        self.doc_namespace.contains_key(&target.doc)
            && self
                .break_cycles
                .iter()
                .any(|b| b.pointer.as_deref() == Some(target.pointer.as_str()))
    }

    // ----- finish -------------------------------------------------------

    /// Copy each alias target's final shape into the alias.
    fn resolve_aliases(&mut self) {
        let mut copies = vec![];
        for (id, e) in &self.entries {
            if e.alias.is_some() {
                let shape = self.named_shape(id).cloned().unwrap_or(Shape::Any);
                copies.push((id.clone(), shape));
            }
        }
        for (id, shape) in copies {
            if let Some(e) = self.entries.get_mut(&id) {
                e.ty.shape = shape;
            }
        }
    }

    /// Unique type names per namespace: components first (sorted by key),
    /// then other types sorted by name and id.
    fn disambiguate_type_names(&mut self) {
        let mut by_ns: BTreeMap<String, Vec<TypeId>> = BTreeMap::new();
        for (id, e) in &self.entries {
            by_ns
                .entry(e.ty.namespace.clone())
                .or_default()
                .push(id.clone());
        }
        for ids in by_ns.into_values() {
            let mut order: Vec<usize> = (0..ids.len()).collect();
            let key = |i: usize| {
                let e = &self.entries[&ids[i]];
                (!e.component, e.ty.name.wire.clone(), ids[i].clone())
            };
            order.sort_by_key(|&i| key(i));
            let mut idents: Vec<Ident> = ids
                .iter()
                .map(|id| self.entries[id].ty.name.clone())
                .collect();
            let changed = names::disambiguate_in_order(&mut idents, &order, Role::Type);
            for i in changed {
                let Some(e) = self.entries.get_mut(&ids[i]) else {
                    continue;
                };
                let old = e.ty.name.pascal();
                e.ty.name = idents[i].clone();
                let message = format!(
                    "type name `{old}` collides with another type in namespace `{}`; renamed to `{}`",
                    e.ty.namespace,
                    idents[i].pascal()
                );
                let target = e.target.clone();
                self.report(
                    Diagnostic::warning("TG0401", message),
                    &target.doc_pointer(),
                );
            }
        }
    }

    fn report_unmatched_break_cycles(&mut self) {
        let mut unmatched = vec![];
        for (i, b) in self.break_cycles.iter().enumerate() {
            let found = b.pointer.as_deref().is_some_and(|p| {
                self.doc_namespace
                    .keys()
                    .any(|&doc| self.ws.documents.get(doc).and_then(|d| d.get(p)).is_some())
            });
            if !found {
                unmatched.push((i, b.text.clone()));
            }
        }
        for (i, text) in unmatched {
            let d = Diagnostic::warning(
                "TG0308",
                format!(
                    "types.break_cycles entry `{text}` names no property of a component schema"
                ),
            )
            .with_help(
                "write entries as `Type.field`, where Type is a key under components/schemas",
            )
            .at(MANIFEST_FILE, format!("/types/break_cycles/{i}"), None);
            self.diagnostics.push(d);
        }
    }

    // ----- diagnostics and sources --------------------------------------

    /// Push a diagnostic labelled at `at` (document and pointer), once.
    fn report(&mut self, d: Diagnostic, at: &(DocId, String)) {
        let (doc, pointer) = at;
        let file = self.file_name(*doc);
        let key = (
            d.code.clone(),
            file.clone(),
            pointer.clone(),
            d.message.clone(),
        );
        if !self.reported.insert(key) {
            return;
        }
        let span = self.ws.span(&RefTarget {
            doc: *doc,
            pointer: pointer.clone(),
        });
        self.diagnostics.push(d.at(file, pointer.clone(), span));
    }

    fn file_name(&self, doc: DocId) -> String {
        self.ws
            .documents
            .get(doc)
            .map(|d| d.name.clone())
            .unwrap_or_default()
    }

    fn file_stem(&self, doc: DocId) -> String {
        let name = self.file_name(doc);
        let base = name.rsplit('/').next().unwrap_or(&name);
        base.split('.').next().unwrap_or(base).to_string()
    }

    fn source_ref(&self, target: &RefTarget) -> SourceRef {
        SourceRef {
            file: self.file_name(target.doc),
            pointer: target.pointer.clone(),
        }
    }
}

/// Labels for diagnostics: a target or a keyword below it.
trait At {
    fn doc_pointer(&self) -> (DocId, String);
    fn keyword(&self, key: &str) -> (DocId, String);
}

impl At for RefTarget {
    fn doc_pointer(&self) -> (DocId, String) {
        (self.doc, self.pointer.clone())
    }
    fn keyword(&self, key: &str) -> (DocId, String) {
        (self.doc, join_pointer(&self.pointer, key))
    }
}

/// The child target `<pointer>/<keys...>` of a target.
fn child(target: &RefTarget, keys: &[&str]) -> RefTarget {
    let pointer = keys
        .iter()
        .fold(target.pointer.clone(), |p, k| join_pointer(&p, k));
    RefTarget {
        doc: target.doc,
        pointer,
    }
}
