// SPDX-License-Identifier: AGPL-3.0-only
//! Operation references.
//!
//! A reference names an operation by its IR id (`public.submitAlphaMessage`,
//! `workflow.action.send`) or by `<namespace>.<resource path>.<method>`
//! (`public.webhooks.deliveries.replay`); an id wins over an equal path, and
//! a path two operations share resolves to neither. A glob is
//! `<namespace>.*` (every callable operation of the namespace) or
//! `<namespace>.<resource path>.*` (the resource's operations and its
//! children's).

use std::collections::BTreeMap;

use tungsten_ir::{Ir, Resource};

/// What the agent transform needs to know about one operation.
#[derive(Debug, Clone)]
pub(crate) struct OpInfo {
    pub namespace: String,
    pub planned: bool,
    pub paginated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    Callable(String),
    Planned(String),
    Unknown,
}

#[derive(Debug, Default)]
pub(crate) struct Index {
    pub ops: BTreeMap<String, OpInfo>,
    /// Callable operation ids in IR order.
    pub callable: Vec<String>,
    /// Resource-path spelling → operation id; `None` when ambiguous.
    aliases: BTreeMap<String, Option<String>>,
    /// `<namespace>.<resource path>` → callable ids below it, in IR order.
    resources: BTreeMap<String, Vec<String>>,
    /// Namespace → callable ids, in IR order.
    pub namespaces: BTreeMap<String, Vec<String>>,
}

impl Index {
    pub fn build(ir: &Ir) -> Index {
        let mut index = Index::default();
        for ns in &ir.namespaces {
            let name = ns.name.wire.clone();
            index.namespaces.entry(name.clone()).or_default();
            for r in &ns.resources {
                index.resource(&name, &name, r);
            }
            for op in &ns.planned {
                index.ops.insert(
                    op.id.0.clone(),
                    OpInfo {
                        namespace: name.clone(),
                        planned: true,
                        paginated: op.pagination.is_some(),
                    },
                );
            }
        }
        index
    }

    /// Adds `r` (path `prefix.<name>`) and returns its callable ids,
    /// children included.
    fn resource(&mut self, ns: &str, prefix: &str, r: &Resource) -> Vec<String> {
        let path = format!("{prefix}.{}", r.name.wire);
        let mut ids = vec![];
        for op in &r.operations {
            let id = op.id.0.clone();
            self.ops.insert(
                id.clone(),
                OpInfo {
                    namespace: ns.to_string(),
                    planned: false,
                    paginated: op.pagination.is_some(),
                },
            );
            self.callable.push(id.clone());
            self.namespaces
                .entry(ns.to_string())
                .or_default()
                .push(id.clone());
            let alias = format!("{path}.{}", op.name.wire);
            self.aliases
                .entry(alias)
                .and_modify(|v| *v = None)
                .or_insert_with(|| Some(id.clone()));
            ids.push(id);
        }
        for c in &r.children {
            ids.extend(self.resource(ns, &path, c));
        }
        self.resources.insert(path, ids.clone());
        ids
    }

    pub fn resolve(&self, reference: &str) -> Resolved {
        let id = match self.ops.get(reference) {
            Some(_) => Some(reference.to_string()),
            None => self.aliases.get(reference).cloned().flatten(),
        };
        match id.and_then(|id| self.ops.get(&id).map(|info| (id, info.planned))) {
            Some((id, false)) => Resolved::Callable(id),
            Some((id, true)) => Resolved::Planned(id),
            None => Resolved::Unknown,
        }
    }

    /// The callable ids a glob covers; `None` when its namespace or
    /// resource path does not exist.
    pub fn glob(&self, pattern: &str) -> Option<Vec<String>> {
        let prefix = pattern.strip_suffix(".*")?;
        self.namespaces
            .get(prefix)
            .or_else(|| self.resources.get(prefix))
            .cloned()
    }

    pub fn has_namespace(&self, ns: &str) -> bool {
        self.namespaces.contains_key(ns)
    }
}
