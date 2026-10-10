// SPDX-License-Identifier: AGPL-3.0-only
//! Facts about the type graph that decide how types are written: which
//! fields need a `Box` (recursion), which aliases become newtypes, which
//! types need a check function and which can derive `Default`.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_ir::{
    Additional, Constraints, Field, NamedType, Presence, Primitive, Shape, StringFormat, TypeId,
    TypeRef,
};

use super::plan::{Emit, Plan, UnionKind, emit_kind};

#[derive(Debug, Clone, Default)]
pub(crate) struct Graph {
    /// Aliases on a reference cycle: written as transparent newtypes.
    pub newtypes: BTreeSet<TypeId>,
    /// (record or union, field or variant index) values behind a `Box`.
    pub boxed: BTreeSet<(TypeId, usize)>,
    /// Types in a by-value cycle.
    pub cyclic: BTreeSet<TypeId>,
    /// Types with a check function.
    pub needs_check: BTreeSet<TypeId>,
    /// Types that can derive `Default`.
    pub defaultable: BTreeSet<TypeId>,
}

/// The constraints of a field that its type does not carry itself: the
/// IR copies an inline primitive's constraints onto the field.
pub(crate) fn field_constraints(f: &Field) -> Option<&Constraints> {
    if f.constraints.is_empty() {
        return None;
    }
    match &f.ty {
        TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Primitive { constraints, .. } if !constraints.is_empty()) => {
            None
        }
        _ => Some(&f.constraints),
    }
}

/// The string formats the SDK checks: the ones the TypeScript and Python
/// SDKs check, with the same rules.
pub(crate) fn checked_format(format: &StringFormat) -> Option<&'static str> {
    match format {
        StringFormat::Uuid => Some("Uuid"),
        StringFormat::Email => Some("Email"),
        StringFormat::DateTime => Some("DateTime"),
        StringFormat::Date => Some("Date"),
        StringFormat::Ipv4 => Some("Ipv4"),
        StringFormat::Ipv6 => Some("Ipv6"),
        _ => None,
    }
}

impl Graph {
    pub(crate) fn new(plan: &Plan<'_>) -> Graph {
        let ir = plan.ir;
        let table = &ir.types.types;
        let kinds: BTreeMap<&TypeId, Emit> =
            table.iter().map(|t| (&t.id, emit_kind(ir, t))).collect();

        // Full reference graph: aliases on a cycle become newtypes.
        let ids: Vec<&TypeId> = table.iter().map(|t| &t.id).collect();
        let index: BTreeMap<&TypeId, usize> =
            ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let full: Vec<Vec<usize>> = table
            .iter()
            .map(|t| {
                let mut refs = vec![];
                shape_refs(&t.shape, &mut refs);
                refs.iter()
                    .filter_map(|id| index.get(id).copied())
                    .collect()
            })
            .collect();
        let comps = sccs(&full);
        let on_cycle = |i: usize| comps.1[comps.0[i]] > 1 || full[i].contains(&i);
        let newtypes: BTreeSet<TypeId> = table
            .iter()
            .enumerate()
            .filter(|(i, t)| kinds[&t.id] == Emit::Alias && on_cycle(*i))
            .map(|(_, t)| t.id.clone())
            .collect();

        // By-value graph over written types.
        let mut graph = Graph {
            newtypes,
            ..Graph::default()
        };
        let by_value: Vec<Vec<(usize, usize)>> = table
            .iter()
            .map(|t| graph.by_value_edges(plan, &kinds, t, &index))
            .collect();
        let adjacency: Vec<Vec<usize>> = by_value
            .iter()
            .map(|e| e.iter().map(|(_, to)| *to).collect())
            .collect();
        let (comp, sizes) = sccs(&adjacency);
        for (i, edges) in by_value.iter().enumerate() {
            for &(slot, to) in edges {
                if comp[i] == comp[to] {
                    graph.boxed.insert((table[i].id.clone(), slot));
                }
            }
            if sizes[comp[i]] > 1 || adjacency[i].contains(&i) {
                graph.cyclic.insert(table[i].id.clone());
            }
        }
        graph.needs_check = graph.compute_needs_check(plan, &kinds);
        graph.defaultable = graph.compute_defaultable(plan, &kinds);
        graph
    }

    /// The by-value edges of `t`: (field or variant slot, target index).
    fn by_value_edges(
        &self,
        plan: &Plan<'_>,
        kinds: &BTreeMap<&TypeId, Emit>,
        t: &NamedType,
        index: &BTreeMap<&TypeId, usize>,
    ) -> Vec<(usize, usize)> {
        let mut edges = vec![];
        let mut add = |slot: usize, ty: &TypeRef| {
            let mut out = vec![];
            self.by_value(plan, kinds, ty, &mut out, 0);
            for id in out {
                if let Some(&i) = index.get(&id) {
                    edges.push((slot, i));
                }
            }
        };
        match (&t.shape, kinds[&t.id]) {
            (Shape::Record { fields, .. }, _) => {
                for (i, f) in fields.iter().enumerate() {
                    add(i, &f.ty);
                }
            }
            (Shape::Union(u), Emit::Union(_)) => {
                for (i, v) in u.variants.iter().enumerate() {
                    add(i, &v.ty);
                }
            }
            (shape, Emit::Alias) if self.newtypes.contains(&t.id) => {
                let mut out = vec![];
                self.by_value_shape(plan, kinds, shape, &mut out, 0);
                for id in out {
                    if let Some(&i) = index.get(&id) {
                        edges.push((0, i));
                    }
                }
            }
            _ => {}
        }
        edges
    }

    /// The written types a value of `ty` contains inline (not behind a
    /// `Vec` or map).
    fn by_value(
        &self,
        plan: &Plan<'_>,
        kinds: &BTreeMap<&TypeId, Emit>,
        ty: &TypeRef,
        out: &mut Vec<TypeId>,
        depth: usize,
    ) {
        if depth > 64 {
            return;
        }
        match ty {
            TypeRef::Inline(s) => self.by_value_shape(plan, kinds, s, out, depth + 1),
            TypeRef::Named(id) => match plan.ir.types.get(id) {
                Some(nt) if kinds[id] == Emit::Alias && !self.newtypes.contains(id) => {
                    self.by_value_shape(plan, kinds, &nt.shape, out, depth + 1)
                }
                Some(_) => out.push(id.clone()),
                None => {}
            },
        }
    }

    fn by_value_shape(
        &self,
        plan: &Plan<'_>,
        kinds: &BTreeMap<&TypeId, Emit>,
        shape: &Shape,
        out: &mut Vec<TypeId>,
        depth: usize,
    ) {
        if let Shape::Nullable { inner } = shape {
            self.by_value(plan, kinds, inner, out, depth);
        }
    }

    fn compute_needs_check(
        &self,
        plan: &Plan<'_>,
        kinds: &BTreeMap<&TypeId, Emit>,
    ) -> BTreeSet<TypeId> {
        let mut set: BTreeSet<TypeId> = BTreeSet::new();
        loop {
            let before = set.len();
            for t in &plan.ir.types.types {
                if set.contains(&t.id) {
                    continue;
                }
                if type_needs_check(plan, kinds[&t.id], &t.shape, &set) {
                    set.insert(t.id.clone());
                }
            }
            if set.len() == before {
                return set;
            }
        }
    }

    fn compute_defaultable(
        &self,
        plan: &Plan<'_>,
        kinds: &BTreeMap<&TypeId, Emit>,
    ) -> BTreeSet<TypeId> {
        let mut set: BTreeSet<TypeId> = BTreeSet::new();
        loop {
            let before = set.len();
            for t in &plan.ir.types.types {
                if set.contains(&t.id) || self.cyclic.contains(&t.id) {
                    continue;
                }
                let ok = match (&t.shape, kinds[&t.id]) {
                    (Shape::Record { fields, .. }, _) => fields.iter().all(|f| {
                        !matches!(f.presence, Presence::Required) || ref_defaultable(&f.ty, &set)
                    }),
                    (shape, Emit::Alias) => shape_defaultable(shape),
                    _ => false,
                };
                if ok {
                    set.insert(t.id.clone());
                }
            }
            if set.len() == before {
                return set;
            }
        }
    }

    /// Whether a field or variant is behind a `Box`.
    pub(crate) fn is_boxed(&self, owner: &TypeId, slot: usize) -> bool {
        self.boxed.contains(&(owner.clone(), slot))
    }
}

/// Named types a shape refers to, anywhere.
fn shape_refs(shape: &Shape, out: &mut Vec<TypeId>) {
    let ty = |r: &TypeRef, out: &mut Vec<TypeId>| match r {
        TypeRef::Named(id) => out.push(id.clone()),
        TypeRef::Inline(s) => shape_refs(s, out),
    };
    match shape {
        Shape::Array { items, .. } => ty(items, out),
        Shape::Map { values } => ty(values, out),
        Shape::Nullable { inner } => ty(inner, out),
        Shape::Record { fields, additional } => {
            for f in fields {
                ty(&f.ty, out);
            }
            if let Additional::Typed { values } = additional {
                ty(values, out);
            }
        }
        Shape::Union(u) => {
            for v in &u.variants {
                ty(&v.ty, out);
            }
        }
        Shape::Intersection { members } => {
            for m in members {
                ty(m, out);
            }
        }
        Shape::Primitive { .. }
        | Shape::Enum { .. }
        | Shape::Const { .. }
        | Shape::Any
        | Shape::Never => {}
    }
}

/// Strongly connected components of a graph: (component of each node,
/// size of each component). Iterative Tarjan.
fn sccs(adj: &[Vec<usize>]) -> (Vec<usize>, Vec<usize>) {
    let n = adj.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut comp = vec![usize::MAX; n];
    let mut sizes: Vec<usize> = vec![];
    let mut stack: Vec<usize> = vec![];
    let mut counter = 0;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some((v, next)) = work.pop() {
            if next == 0 {
                index[v] = counter;
                low[v] = counter;
                counter += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = adj[v].get(next) {
                work.push((v, next + 1));
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            if low[v] == index[v] {
                let id = sizes.len();
                let mut size = 0;
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp[w] = id;
                    size += 1;
                    if w == v {
                        break;
                    }
                }
                sizes.push(size);
            }
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
        }
    }
    (comp, sizes)
}

fn constraints_check(c: &Constraints) -> bool {
    !c.is_empty()
}

fn primitive_needs_check(p: &Primitive, c: &Constraints) -> bool {
    constraints_check(c)
        || matches!(p, Primitive::String { format: Some(f) } if checked_format(f).is_some())
}

fn ref_needs_check(plan: &Plan<'_>, r: &TypeRef, set: &BTreeSet<TypeId>) -> bool {
    match r {
        TypeRef::Named(id) => set.contains(id),
        TypeRef::Inline(s) => shape_needs_check(plan, s, set),
    }
}

/// Whether checking a value of this (inline) shape can report anything.
pub(crate) fn shape_needs_check(plan: &Plan<'_>, s: &Shape, set: &BTreeSet<TypeId>) -> bool {
    match s {
        Shape::Primitive {
            primitive,
            constraints,
        } => primitive_needs_check(primitive, constraints),
        Shape::Enum { .. } | Shape::Const { .. } | Shape::Never | Shape::Intersection { .. } => {
            true
        }
        Shape::Any => false,
        Shape::Array {
            items,
            min,
            max,
            unique,
        } => min.is_some() || max.is_some() || *unique || ref_needs_check(plan, items, set),
        Shape::Map { values } => ref_needs_check(plan, values, set),
        Shape::Nullable { inner } => ref_needs_check(plan, inner, set),
        Shape::Record { fields, additional } => {
            fields.iter().any(|f| field_needs_check(plan, f, set))
                || matches!(additional, Additional::Typed { values } if ref_needs_check(plan, values, set))
        }
        Shape::Union(u) => u
            .variants
            .iter()
            .any(|v| variant_needs_check(plan, &v.ty, set)),
    }
}

pub(crate) fn field_needs_check(plan: &Plan<'_>, f: &Field, set: &BTreeSet<TypeId>) -> bool {
    field_constraints(f).is_some() || ref_needs_check(plan, &f.ty, set)
}

/// A union variant of an inline constant is selected by its value; no check.
pub(crate) fn variant_needs_check(plan: &Plan<'_>, r: &TypeRef, set: &BTreeSet<TypeId>) -> bool {
    match r {
        TypeRef::Inline(s) if matches!(s.as_ref(), Shape::Const { .. }) => false,
        _ => ref_needs_check(plan, r, set),
    }
}

fn type_needs_check(plan: &Plan<'_>, kind: Emit, shape: &Shape, set: &BTreeSet<TypeId>) -> bool {
    match kind {
        // A closed enum of strings or integers is checked by its type.
        Emit::StrEnum | Emit::IntEnum => false,
        // Membership of the values is a check; the variants only name types.
        Emit::MixedEnum => true,
        Emit::Union(UnionKind::Literal | UnionKind::Untagged | UnionKind::Tagged) => {
            shape_needs_check(plan, shape, set)
        }
        Emit::Struct | Emit::Alias => shape_needs_check(plan, shape, set),
    }
}

pub(crate) fn ref_defaultable(r: &TypeRef, set: &BTreeSet<TypeId>) -> bool {
    match r {
        TypeRef::Named(id) => set.contains(id),
        TypeRef::Inline(s) => shape_defaultable(s),
    }
}

fn shape_defaultable(s: &Shape) -> bool {
    match s {
        Shape::Primitive { .. }
        | Shape::Array { .. }
        | Shape::Map { .. }
        | Shape::Any
        | Shape::Nullable { .. } => true,
        Shape::Const { value } => value.is_string() || value.is_i64() || value.is_boolean(),
        Shape::Record { .. }
        | Shape::Enum { .. }
        | Shape::Union(_)
        | Shape::Intersection { .. }
        | Shape::Never => false,
    }
}
