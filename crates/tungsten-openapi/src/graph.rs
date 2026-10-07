// SPDX-License-Identifier: AGPL-3.0-only
//! The `$ref` graph and its cycles.
//!
//! PHASE-1 STUB: the frontend work package builds this with petgraph
//! (nodes: every schema target that is referenced or that contains a
//! `$ref`; edges: schema → referenced schema) and computes strongly
//! connected components.

use std::collections::BTreeSet;

use crate::RefTarget;

#[derive(Debug, Clone, Default)]
pub struct RefGraph {
    /// Targets that participate in a cycle (SCC of size > 1, or self-edge).
    pub recursive: BTreeSet<RefTarget>,
    /// Every cycle as an ordered list of targets, sorted for determinism.
    pub cycles: Vec<Vec<RefTarget>>,
}

impl RefGraph {
    pub fn is_recursive(&self, target: &RefTarget) -> bool {
        self.recursive.contains(target)
    }
}
