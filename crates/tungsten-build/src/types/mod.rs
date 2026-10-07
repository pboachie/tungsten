// SPDX-License-Identifier: AGPL-3.0-only
//! Schema → IR type conversion (planning/03 "Types").
//!
//! PHASE-1 STUB: the public API of `TypeBuilder` is the contract used by
//! the operations code. The types work package implements it.

use tungsten_core::Diagnostics;
use tungsten_ir::{Shape, TypeId, TypeRef, TypeTable};
use tungsten_openapi::{DocId, RefTarget, Workspace};

/// Converts schemas to IR types, memoizing named types by reference target
/// so each `components/schemas` entry becomes exactly one `NamedType`.
#[derive(Debug)]
pub struct TypeBuilder<'a> {
    ws: &'a Workspace,
    #[allow(dead_code)]
    break_cycles: Vec<String>,
    table: TypeTable,
    diagnostics: Diagnostics,
}

impl<'a> TypeBuilder<'a> {
    pub fn new(ws: &'a Workspace, break_cycles: &[String]) -> Self {
        Self {
            ws,
            break_cycles: break_cycles.to_vec(),
            table: TypeTable::default(),
            diagnostics: Diagnostics::new(),
        }
    }

    /// Register every `#/components/schemas/*` entry of `doc` as a named
    /// type `namespace.Name`, in sorted order.
    pub fn add_components(&mut self, namespace: &str, doc: DocId) {
        let _ = (namespace, doc, self.ws);
    }

    /// Convert the schema at `target` (an inline schema object or a `$ref`)
    /// into a type reference. References to component schemas become
    /// `TypeRef::Named`. Inline records, enums and unions that need a name
    /// in some target language are registered as named types using
    /// `name_hint` words (for example `["SubmitAlphaMessage", "Body"]`).
    pub fn type_ref(&mut self, namespace: &str, target: &RefTarget, name_hint: &[&str]) -> TypeRef {
        let _ = (namespace, target, name_hint);
        TypeRef::Inline(Box::new(Shape::Any))
    }

    /// The type id registered for a target, if any.
    pub fn type_id_for(&self, target: &RefTarget) -> Option<TypeId> {
        let _ = target;
        None
    }

    /// The shape behind a type reference, following `Named` through the
    /// table built so far. Does not unwrap `Nullable`.
    pub fn shape_of<'s>(&'s self, r: &'s TypeRef) -> Option<&'s Shape> {
        match r {
            TypeRef::Inline(s) => Some(s),
            TypeRef::Named(id) => self
                .table
                .types
                .iter()
                .find(|t| &t.id == id)
                .map(|t| &t.shape),
        }
    }

    /// Finish: sort the table and return it with the diagnostics collected.
    pub fn finish(mut self) -> (TypeTable, Diagnostics) {
        self.table.sort();
        (self.table, self.diagnostics)
    }
}
