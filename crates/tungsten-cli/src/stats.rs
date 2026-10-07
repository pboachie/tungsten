// SPDX-License-Identifier: AGPL-3.0-only
//! Counts summarizing an IR, shared by `check` and `ir dump`.

use tungsten_ir::{Ir, Operation, OperationStatus, Resource};

use crate::output::{IrStats, OperationCounts};

pub(crate) fn ir_stats(ir: &Ir) -> IrStats {
    fn walk(r: &Resource, resources: &mut usize, ops: &mut OperationCounts) {
        *resources += 1;
        r.operations.iter().for_each(|op| count(op, ops));
        r.children.iter().for_each(|c| walk(c, resources, ops));
    }
    let mut resources = 0;
    let mut operations = OperationCounts::default();
    for ns in &ir.namespaces {
        ns.resources
            .iter()
            .for_each(|r| walk(r, &mut resources, &mut operations));
        // Planned operations are never callable, whatever status they carry.
        operations.planned += ns.planned.len();
        operations.total += ns.planned.len();
    }
    IrStats {
        api: ir.api.name.wire.clone(),
        namespaces: ir.namespaces.len(),
        resources,
        operations,
        types: ir.types.types.len(),
        auth_schemes: ir.auth.len(),
    }
}

fn count(op: &Operation, c: &mut OperationCounts) {
    c.total += 1;
    match op.status {
        OperationStatus::Implemented => c.implemented += 1,
        OperationStatus::Planned { .. } => c.planned += 1,
        OperationStatus::Gated { .. } => c.gated += 1,
    }
}

/// `1 document`, `2 documents`.
pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// One-line human summary of an IR's contents.
pub(crate) fn describe(s: &IrStats) -> String {
    format!(
        "{} · {} · {} · {} · {}",
        plural(s.namespaces, "namespace", "namespaces"),
        plural(s.resources, "resource", "resources"),
        plural(s.operations.total, "operation", "operations"),
        plural(s.types, "type", "types"),
        plural(s.auth_schemes, "auth scheme", "auth schemes"),
    )
}
