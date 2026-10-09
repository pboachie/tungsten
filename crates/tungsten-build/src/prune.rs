// SPDX-License-Identifier: AGPL-3.0-only
//! Pruning of types no operation reaches (`types.prune_unreferenced`).
//!
//! Every `components/schemas` entry becomes a named type while the IR is
//! built, which is right for a document that is only a library of schemas
//! but not for an API whose operations are filtered (`include`): the types
//! only the excluded operations use would be generated, validated and
//! reported on for nothing. After the agent manifest has been applied, the
//! types reachable from the operations (callable and planned), including the
//! error envelopes, are kept and the others dropped, together with the
//! warnings and notes about the schemas they came from.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_core::{Diagnostic, Diagnostics, Severity};
use tungsten_ir::{Additional, Ir, Operation, Shape, TypeId, TypeRef};

/// How many pruned type names the note lists.
const LISTED: usize = 8;

/// Drop the types of `ir` that no operation reaches, and the non-error
/// diagnostics about the schemas they were built from; report the count
/// (`TG0760`). A document without operations is left alone: it is a library
/// of schemas, and everything in it is the point.
pub(crate) fn prune_unreferenced(ir: &mut Ir, diagnostics: &mut Diagnostics) {
    let mut ops: Vec<&Operation> = ir.operations();
    ops.extend(ir.namespaces.iter().flat_map(|n| n.planned.iter()));
    if ops.is_empty() {
        return;
    }
    let mut roots: Vec<&TypeRef> = vec![];
    for op in &ops {
        let params = &op.params;
        roots.extend(
            params
                .path
                .iter()
                .chain(&params.query)
                .chain(&params.header)
                .chain(&params.cookie)
                .map(|p| &p.ty),
        );
        roots.extend(op.body.iter().flat_map(|b| b.content.iter().map(|c| &c.ty)));
        for r in &op.responses {
            roots.extend(r.content.iter().map(|c| &c.ty));
            roots.extend(r.headers.iter().map(|h| &h.ty));
        }
    }
    let mut keep: BTreeSet<TypeId> = BTreeSet::new();
    let mut pending: Vec<TypeId> = ir
        .namespaces
        .iter()
        .map(|n| &n.errors)
        .chain(std::iter::once(&ir.errors))
        .filter_map(|e| e.envelope.clone())
        .collect();
    let mut shapes: Vec<&Shape> = vec![];
    for r in roots {
        match r {
            TypeRef::Named(id) => pending.push(id.clone()),
            TypeRef::Inline(shape) => shapes.push(shape),
        }
    }
    for shape in shapes {
        collect(shape, &mut pending);
    }
    while let Some(id) = pending.pop() {
        if !keep.insert(id.clone()) {
            continue;
        }
        if let Some(t) = ir.types.get(&id) {
            collect(&t.shape, &mut pending);
        }
    }
    let pruned: Vec<TypeId> = ir
        .types
        .types
        .iter()
        .filter(|t| !keep.contains(&t.id))
        .map(|t| t.id.clone())
        .collect();
    if pruned.is_empty() {
        return;
    }

    // Diagnostics about a pruned schema: the owner of a location is the
    // type whose origin is the longest prefix of its pointer.
    let owners: BTreeMap<(&str, &str), bool> = ir
        .types
        .types
        .iter()
        .map(|t| {
            (
                (t.origin.file.as_str(), t.origin.pointer.as_str()),
                keep.contains(&t.id),
            )
        })
        .collect();
    let pruned_location = |file: &str, pointer: &str| -> bool {
        let mut at = pointer;
        loop {
            if let Some(kept) = owners.get(&(file, at)) {
                return !kept;
            }
            match at.rfind('/') {
                Some(cut) => at = &at[..cut],
                None => return false,
            }
        }
    };
    diagnostics.0.retain(|d| {
        d.severity == Severity::Error
            || d.labels
                .first()
                .is_none_or(|l| !pruned_location(&l.file, &l.pointer))
    });

    let mut names: Vec<&str> = pruned.iter().map(|id| id.0.as_str()).collect();
    names.sort_unstable();
    let more = names.len().saturating_sub(LISTED);
    let mut listed = names[..names.len() - more].join(", ");
    if more > 0 {
        listed.push_str(&format!(", and {more} more"));
    }
    let count = pruned.len();
    diagnostics.push(
        Diagnostic::info(
            "TG0760",
            format!(
                "{count} {} no operation reaches {} not generated: {listed}",
                if count == 1 { "type" } else { "types" },
                if count == 1 { "was" } else { "were" },
            ),
        )
        .with_help(
            "set `types.prune_unreferenced: false` in tungsten.yml to generate every schema",
        ),
    );
    let pruned: BTreeSet<TypeId> = pruned.into_iter().collect();
    ir.types.types.retain(|t| !pruned.contains(&t.id));
}

/// The named types a shape refers to.
fn collect(shape: &Shape, out: &mut Vec<TypeId>) {
    let push = |r: &TypeRef, out: &mut Vec<TypeId>| match r {
        TypeRef::Named(id) => out.push(id.clone()),
        TypeRef::Inline(shape) => collect(shape, out),
    };
    match shape {
        Shape::Primitive { .. } | Shape::Enum { .. } | Shape::Const { .. } => {}
        Shape::Any | Shape::Never => {}
        Shape::Array { items, .. } => push(items, out),
        Shape::Map { values } => push(values, out),
        Shape::Record { fields, additional } => {
            for f in fields {
                push(&f.ty, out);
            }
            if let Additional::Typed { values } = additional {
                push(values, out);
            }
        }
        Shape::Union(u) => {
            for v in &u.variants {
                push(&v.ty, out);
            }
            if let Some(d) = &u.discriminator {
                out.extend(d.mapping.iter().map(|(_, id)| id.clone()));
            }
        }
        Shape::Intersection { members } => {
            for m in members {
                push(m, out);
            }
        }
        Shape::Nullable { inner } => push(inner, out),
    }
}
