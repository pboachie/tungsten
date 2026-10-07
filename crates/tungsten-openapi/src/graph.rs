// SPDX-License-Identifier: AGPL-3.0-only
//! The `$ref` graph and its cycles.
//!
//! Nodes are every schema that is a `$ref` target plus every
//! `#/components/schemas/<Name>` entry of an entry document. There is an
//! edge A → B when a `$ref` to B occurs inside A's subtree, and when B's
//! root lies inside A's subtree (A contains B). The walk of A stops at other
//! node roots, so each reference belongs to its innermost node. Strongly
//! connected components with more than one member, or with a self-edge, are
//! cycles.

use std::collections::{BTreeMap, BTreeSet};

use petgraph::algo::tarjan_scc;
use petgraph::graphmap::DiGraphMap;
use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics, Label};

use crate::walk::{Kind, walk};
use crate::{RefTarget, Workspace, split_pointer};

#[derive(Debug, Clone, Default)]
pub struct RefGraph {
    /// Targets that participate in a cycle (SCC of size > 1, or self-edge).
    pub recursive: BTreeSet<RefTarget>,
    /// Every cycle as an ordered list of targets, sorted for determinism.
    pub cycles: Vec<Vec<RefTarget>>,
    /// Every schema node of the graph.
    pub nodes: BTreeSet<RefTarget>,
    /// Node → the nodes it references or contains.
    pub edges: BTreeMap<RefTarget, BTreeSet<RefTarget>>,
}

impl RefGraph {
    pub fn is_recursive(&self, target: &RefTarget) -> bool {
        self.recursive.contains(target)
    }

    /// The nodes `target` references or contains (empty when it is not a node).
    pub fn successors(&self, target: &RefTarget) -> impl Iterator<Item = &RefTarget> {
        self.edges.get(target).into_iter().flatten()
    }

    /// The cycle `target` belongs to, if any.
    pub fn cycle_of(&self, target: &RefTarget) -> Option<&[RefTarget]> {
        self.cycles
            .iter()
            .find(|c| c.binary_search(target).is_ok())
            .map(Vec::as_slice)
    }
}

/// Whether `pointer` is a direct `#/components/schemas/<Name>` entry.
pub fn is_named_schema(pointer: &str) -> bool {
    let tokens = split_pointer(pointer);
    tokens.len() == 3 && tokens[0] == "components" && tokens[1] == "schemas"
}

/// Build the graph over `targets` (schema `$ref` targets found while
/// resolving) and every component schema of the entry documents. Cycle
/// diagnostics go to `diags`.
pub(crate) fn build(
    ws: &Workspace,
    targets: &BTreeSet<RefTarget>,
    diags: &mut Diagnostics,
) -> RefGraph {
    let mut nodes = targets.clone();
    for &doc in &ws.entries {
        if let Some(Value::Object(schemas)) = ws.documents[doc].get("/components/schemas") {
            for name in schemas.keys() {
                nodes.insert(RefTarget {
                    doc,
                    pointer: crate::join_pointer("/components/schemas", name),
                });
            }
        }
    }

    let mut edges: BTreeMap<RefTarget, BTreeSet<RefTarget>> = BTreeMap::new();
    for node in &nodes {
        let Some(value) = ws.get(node) else {
            continue;
        };
        let out = edges.entry(node.clone()).or_default();
        walk(value, &node.pointer, Kind::Schema, |n| {
            if n.pointer != node.pointer {
                let inner = RefTarget {
                    doc: node.doc,
                    pointer: n.pointer.clone(),
                };
                if nodes.contains(&inner) {
                    out.insert(inner);
                    return false;
                }
            }
            if let Some(Value::String(r)) = n.reference()
                && let Some(t) = ws.resolve(node.doc, r)
                && nodes.contains(&t)
            {
                out.insert(t);
            }
            true
        });
    }

    let index: Vec<&RefTarget> = nodes.iter().collect();
    let position = |t: &RefTarget| index.binary_search(&t).expect("edge ends are nodes");
    let mut g: DiGraphMap<usize, ()> = DiGraphMap::new();
    for i in 0..index.len() {
        g.add_node(i);
    }
    for (from, tos) in &edges {
        for to in tos {
            g.add_edge(position(from), position(to), ());
        }
    }
    let mut cycles: Vec<Vec<RefTarget>> = tarjan_scc(&g)
        .into_iter()
        .filter(|scc| scc.len() > 1 || g.contains_edge(scc[0], scc[0]))
        .map(|scc| {
            let mut members: Vec<RefTarget> = scc.into_iter().map(|i| index[i].clone()).collect();
            members.sort();
            members
        })
        .collect();
    cycles.sort();

    for cycle in &cycles {
        diags.push(cycle_diagnostic(ws, cycle));
    }
    RefGraph {
        recursive: cycles.iter().flatten().cloned().collect(),
        cycles,
        nodes,
        edges,
    }
}

/// Members named in a cycle diagnostic's message and labels.
const SHOWN_MEMBERS: usize = 10;

fn cycle_diagnostic(ws: &Workspace, cycle: &[RefTarget]) -> Diagnostic {
    let named = cycle.iter().position(|t| is_named_schema(&t.pointer));
    // The named member (when there is one) comes first.
    let first = named.unwrap_or(0);
    let order: Vec<&RefTarget> = std::iter::once(&cycle[first])
        .chain(
            cycle
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != first)
                .map(|(_, t)| t),
        )
        .take(SHOWN_MEMBERS)
        .collect();
    let mut members: Vec<String> = order
        .iter()
        .map(|t| format!("{}#{}", ws.name(t.doc), t.pointer))
        .collect();
    if cycle.len() > SHOWN_MEMBERS {
        members.push(format!("and {} more", cycle.len() - SHOWN_MEMBERS));
    }
    let members = members.join(", ");
    let mut d = match named {
        Some(_) => Diagnostic::info(
            "TG0203",
            format!("circular $ref (recursion preserved): {members}"),
        ),
        None => Diagnostic::warning(
            "TG0205",
            format!("circular $ref without a named type to break the cycle: {members}"),
        )
        .with_help(
            "name one of the schemas under components/schemas, or list the edge in tungsten.yml `types.break_cycles`",
        ),
    };
    d.labels = order
        .into_iter()
        .map(|t| Label {
            file: ws.name(t.doc).to_string(),
            pointer: t.pointer.clone(),
            span: ws.span(t),
            message: Some("part of the cycle".into()),
        })
        .collect();
    d
}
