// SPDX-License-Identifier: AGPL-3.0-only
//! The IR prepared for serving: callable operations with their route
//! patterns, a type index, options and a cache of compiled patterns.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use fancy_regex::Regex;
use tungsten_ir::{
    ErrorModel, HttpMethod, Ir, Operation, ParamRole, RuntimeGate, Shape, TypeId, TypeRef,
};

use crate::MockOptions;
use crate::route::PathPattern;
use crate::state::lock;

/// Compiled patterns larger than this are treated as unusable.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Backtracking steps per match before a pattern counts as matching.
const BACKTRACK_LIMIT: usize = 100_000;

/// One callable operation.
#[derive(Debug)]
pub(crate) struct OpEntry {
    pub op: Operation,
    /// Index of its namespace in `Ir.namespaces`.
    pub ns: usize,
    pub path: PathPattern,
}

impl OpEntry {
    pub fn id(&self) -> &str {
        &self.op.id.0
    }

    /// The wire name of the idempotency key header, if the operation has one.
    pub fn idempotency_header(&self) -> Option<&str> {
        self.op
            .params
            .header
            .iter()
            .find(|p| p.role == ParamRole::IdempotencyKey)
            .map(|p| p.wire_name.as_str())
    }
}

#[derive(Debug)]
pub(crate) struct Model {
    pub ir: Ir,
    pub seed: u64,
    pub max_body_bytes: usize,
    pub ops: Vec<OpEntry>,
    enabled_gates: BTreeSet<String>,
    types: BTreeMap<TypeId, usize>,
    by_id: BTreeMap<String, usize>,
    regexes: Mutex<BTreeMap<String, Option<Arc<Regex>>>>,
}

impl Model {
    pub fn new(ir: Ir, opts: &MockOptions) -> Model {
        let mut ops = vec![];
        for (ns, namespace) in ir.namespaces.iter().enumerate() {
            let mut stack: Vec<&tungsten_ir::Resource> = namespace.resources.iter().rev().collect();
            while let Some(resource) = stack.pop() {
                for op in &resource.operations {
                    ops.push(OpEntry {
                        op: op.clone(),
                        ns,
                        path: PathPattern::new(&op.path),
                    });
                }
                stack.extend(resource.children.iter().rev());
            }
        }
        let types = ir
            .types
            .types
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.clone(), i))
            .collect();
        let mut by_id = BTreeMap::new();
        for (i, entry) in ops.iter().enumerate() {
            by_id.entry(entry.op.id.0.clone()).or_insert(i);
        }
        Model {
            ir,
            seed: opts.seed,
            max_body_bytes: opts.max_body_bytes,
            ops,
            enabled_gates: opts.enabled_gates.iter().cloned().collect(),
            types,
            by_id,
            regexes: Mutex::new(BTreeMap::new()),
        }
    }

    /// The callable operation with this id.
    pub fn op_index(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// The shape a type reference denotes; `None` for an unknown named type.
    pub fn shape<'a>(&'a self, ty: &'a TypeRef) -> Option<&'a Shape> {
        match ty {
            TypeRef::Inline(shape) => Some(shape),
            TypeRef::Named(id) => self.types.get(id).map(|&i| &self.ir.types.types[i].shape),
        }
    }

    /// Whether `text` matches a JSON Schema `pattern` (an unanchored
    /// search). A pattern the engine cannot compile, or a match that
    /// exceeds the backtracking limit, is not enforced.
    pub fn pattern_matches(&self, pattern: &str, text: &str) -> bool {
        let compiled = {
            let mut cache = lock(&self.regexes);
            cache
                .entry(pattern.to_string())
                .or_insert_with(|| {
                    fancy_regex::RegexBuilder::new(pattern)
                        .backtrack_limit(BACKTRACK_LIMIT)
                        .delegate_size_limit(REGEX_SIZE_LIMIT)
                        .delegate_dfa_size_limit(REGEX_SIZE_LIMIT)
                        .build()
                        .ok()
                        .map(Arc::new)
                })
                .clone()
        };
        compiled.is_none_or(|re| re.is_match(text).unwrap_or(true))
    }

    /// The error model answering for a namespace, or the API-wide one.
    pub fn errors(&self, ns: Option<usize>) -> &ErrorModel {
        ns.and_then(|i| self.ir.namespaces.get(i))
            .map(|n| &n.errors)
            .unwrap_or(&self.ir.errors)
    }

    pub fn gate_on(&self, gate: &RuntimeGate) -> bool {
        gate.default_on || self.enabled_gates.contains(&gate.env_var)
    }
}

pub(crate) fn method_str(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Put => "PUT",
        HttpMethod::Post => "POST",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Head => "HEAD",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Trace => "TRACE",
    }
}
