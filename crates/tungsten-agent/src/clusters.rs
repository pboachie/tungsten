// SPDX-License-Identifier: AGPL-3.0-only
//! Clusters (`disclosure.clusters`) for progressive disclosure.
//!
//! Each callable operation and each macro belongs to at most one cluster:
//! the one its tools entry or `x-agent-cluster` names, else the first
//! declared cluster whose list covers it. Lists hold operation references,
//! macro names and globs (`<namespace>.*` covers the namespace's operations
//! and macros, `<namespace>.<resource path>.*` a resource's operations and
//! its children's). Clusters keep declaration order; a cluster named only
//! by a tools entry or extension is added after them, by name.

use std::collections::{BTreeMap, BTreeSet};

use tungsten_ir::{AgentModel, Cluster, OperationId};

use crate::index::{Index, Resolved};
use crate::model::AgentConfig;
use crate::report::Reporter;

/// Assign clusters: fills `agent.clusters` and each macro's `cluster`, and
/// returns the cluster of every callable operation that has one.
pub(crate) fn assign(
    cfg: &AgentConfig,
    index: &Index,
    explicit: &BTreeMap<String, String>,
    agent: &mut AgentModel,
    r: &mut Reporter<'_>,
) -> BTreeMap<String, String> {
    let macro_names: BTreeSet<String> = agent.macros.iter().map(|m| m.name.0.clone()).collect();
    let mut member: BTreeMap<String, String> = BTreeMap::new();
    for (i, c) in cfg.disclosure.clusters.iter().enumerate() {
        for (j, entry) in c.operations.iter().enumerate() {
            let at = format!("/disclosure/clusters/{i}/operations/{j}");
            let ids: Vec<String> = if let Some(ns) = entry.strip_suffix(".*") {
                match index.glob(entry) {
                    Some(mut ids) => {
                        if index.has_namespace(ns) {
                            let prefix = format!("{ns}.");
                            ids.extend(
                                macro_names
                                    .iter()
                                    .filter(|m| m.starts_with(&prefix))
                                    .cloned(),
                            );
                        }
                        ids
                    }
                    None => {
                        r.warning(
                            "TG0604",
                            &at,
                            format!("`{entry}` matches no namespace or resource"),
                        );
                        continue;
                    }
                }
            } else if macro_names.contains(entry) {
                vec![entry.clone()]
            } else {
                match index.resolve(entry) {
                    Resolved::Callable(id) => vec![id],
                    Resolved::Planned(id) => {
                        r.warning(
                            "TG0603",
                            &at,
                            format!("`{id}` is a planned operation; not clustered"),
                        );
                        continue;
                    }
                    Resolved::Unknown => {
                        r.warning(
                            "TG0603",
                            &at,
                            format!("unknown operation or macro `{entry}`"),
                        );
                        continue;
                    }
                }
            };
            for id in ids {
                member.entry(id).or_insert_with(|| c.name.clone());
            }
        }
    }
    let mut ops: BTreeMap<String, String> = BTreeMap::new();
    for id in &index.callable {
        if let Some(name) = explicit.get(id).or_else(|| member.get(id)) {
            ops.insert(id.clone(), name.clone());
        }
    }
    for m in &mut agent.macros {
        m.cluster = member.get(&m.name.0).cloned();
    }
    let mut names: Vec<(String, Option<String>)> = cfg
        .disclosure
        .clusters
        .iter()
        .map(|c| (c.name.clone(), c.summary.clone()))
        .collect();
    let declared: BTreeSet<String> = names.iter().map(|(n, _)| n.clone()).collect();
    let implicit: BTreeSet<&String> = ops.values().filter(|n| !declared.contains(*n)).collect();
    names.extend(implicit.into_iter().map(|n| (n.clone(), None)));
    agent.clusters = names
        .into_iter()
        .map(|(name, summary)| {
            let mut operations: Vec<OperationId> = ops
                .iter()
                .filter(|(_, c)| **c == name)
                .map(|(id, _)| OperationId(id.clone()))
                .chain(
                    agent
                        .macros
                        .iter()
                        .filter(|m| m.cluster.as_deref() == Some(name.as_str()))
                        .map(|m| m.name.clone()),
                )
                .collect();
            operations.sort();
            operations.dedup();
            Cluster {
                name,
                summary,
                operations,
            }
        })
        .collect();
    ops
}
