// SPDX-License-Identifier: AGPL-3.0-only
//! JSON Pointer → byte span index of one document.
//!
//! The index is a tree that mirrors the document: one node per pointer
//! token, holding the span of the value at that pointer when it has one.
//! Memory grows with the number of values, not with the length of their
//! pointers, so a deep document with large arrays costs what its values
//! cost (a map of full pointer strings grew with depth × values). Array
//! children are kept as a vector; object children as a sorted map of
//! tokens (escaped as in the pointer).

use std::collections::BTreeMap;

use tungsten_core::Span;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SpanIndex {
    /// `nodes[0]` is the root (pointer `""`) once anything is inserted.
    /// Detached subtrees stay in the vector, unreachable.
    nodes: Vec<Node>,
    /// The parent pointer of the last insertion and its node: parsers
    /// insert siblings one after another, so most insertions start there
    /// instead of walking from the root. Cleared when a subtree moves.
    last_parent: Option<(String, u32)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Node {
    span: Option<Span>,
    children: Children,
}

/// Children by token: `Seq` while the tokens are `0, 1, 2, ...` in
/// insertion order (array elements), `Map` otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Children {
    /// Node index per element; [`ABSENT`] for a removed element.
    Seq(Vec<u32>),
    Map(BTreeMap<Box<str>, u32>),
}

impl Default for Children {
    fn default() -> Self {
        Children::Seq(vec![])
    }
}

const ABSENT: u32 = u32::MAX;

/// The tokens of a pointer, as written (still escaped). `None` when the
/// pointer is not `""` and does not start with `/`.
fn tokens(pointer: &str) -> Option<std::str::Split<'_, char>> {
    if pointer.is_empty() {
        // An iterator that yields nothing.
        let mut empty = "".split('/');
        empty.next();
        return Some(empty);
    }
    pointer.strip_prefix('/').map(|rest| rest.split('/'))
}

/// A token that names array element `n` (canonical decimal).
fn index_of(token: &str) -> Option<usize> {
    let n: usize = token.parse().ok()?;
    (n.to_string() == token).then_some(n)
}

impl Children {
    fn get(&self, token: &str) -> Option<u32> {
        match self {
            Children::Seq(items) => index_of(token)
                .and_then(|i| items.get(i).copied())
                .filter(|&n| n != ABSENT),
            Children::Map(map) => map.get(token).copied(),
        }
    }

    /// Remove the child `token` without renumbering its siblings.
    fn remove(&mut self, token: &str) {
        match self {
            Children::Seq(items) => {
                if let Some(slot) = index_of(token).and_then(|i| items.get_mut(i)) {
                    *slot = ABSENT;
                }
            }
            Children::Map(map) => {
                map.remove(token);
            }
        }
    }

    fn as_map(&mut self) -> &mut BTreeMap<Box<str>, u32> {
        if let Children::Seq(items) = self {
            let map = items
                .iter()
                .enumerate()
                .filter(|(_, n)| **n != ABSENT)
                .map(|(i, n)| (i.to_string().into_boxed_str(), *n))
                .collect();
            *self = Children::Map(map);
        }
        match self {
            Children::Map(map) => map,
            Children::Seq(_) => unreachable!("converted above"),
        }
    }

    /// (token, node) of every child, arrays in index order.
    fn entries(&self) -> Vec<(String, u32)> {
        match self {
            Children::Seq(items) => items
                .iter()
                .enumerate()
                .filter(|(_, n)| **n != ABSENT)
                .map(|(i, n)| (i.to_string(), *n))
                .collect(),
            Children::Map(map) => map.iter().map(|(k, n)| (k.to_string(), *n)).collect(),
        }
    }
}

impl SpanIndex {
    fn find(&self, pointer: &str) -> Option<u32> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut node = 0u32;
        for token in tokens(pointer)? {
            node = self.nodes[node as usize].children.get(token)?;
        }
        Some(node)
    }

    fn find_or_create(&mut self, pointer: &str) -> Option<u32> {
        if self.nodes.is_empty() {
            self.nodes.push(Node::default());
        }
        let mut node = 0u32;
        for token in tokens(pointer)? {
            node = self.child_or_create(node, token);
        }
        Some(node)
    }

    fn child_or_create(&mut self, node: u32, token: &str) -> u32 {
        if let Some(n) = self.nodes[node as usize].children.get(token) {
            return n;
        }
        let fresh = self.nodes.len() as u32;
        self.nodes.push(Node::default());
        let children = &mut self.nodes[node as usize].children;
        match children {
            Children::Seq(items) if index_of(token) == Some(items.len()) => items.push(fresh),
            _ => {
                children.as_map().insert(token.into(), fresh);
            }
        }
        fresh
    }

    pub(crate) fn insert(&mut self, pointer: String, span: Span) {
        let node = match pointer.rsplit_once('/') {
            Some((parent, token)) => {
                let cached = self
                    .last_parent
                    .as_ref()
                    .filter(|(p, _)| p == parent)
                    .map(|(_, n)| *n);
                let parent_node = match cached {
                    Some(n) => Some(n),
                    None => self.find_or_create(parent),
                };
                parent_node.map(|p| {
                    self.last_parent = Some((parent.to_string(), p));
                    self.child_or_create(p, token)
                })
            }
            None => self.find_or_create(&pointer),
        };
        if let Some(node) = node {
            self.nodes[node as usize].span = Some(span);
        }
    }

    pub(crate) fn get(&self, pointer: &str) -> Option<Span> {
        self.nodes[self.find(pointer)? as usize].span
    }

    /// The span of `pointer`, or of its nearest ancestor that has one.
    pub(crate) fn nearest(&self, pointer: &str) -> Option<Span> {
        let root = self.nodes.first()?;
        let mut best = root.span;
        let mut node = root;
        for token in tokens(pointer)? {
            let Some(next) = node.children.get(token) else {
                break;
            };
            node = &self.nodes[next as usize];
            best = node.span.or(best);
        }
        best
    }

    /// Every entry of the subtree rooted at `prefix`, as (suffix, span)
    /// where the suffix is `""` for the root itself.
    pub(crate) fn subtree(&self, prefix: &str) -> Vec<(String, Span)> {
        let mut out = vec![];
        let Some(start) = self.find(prefix) else {
            return out;
        };
        let mut stack = vec![(String::new(), start)];
        while let Some((suffix, node)) = stack.pop() {
            let node = &self.nodes[node as usize];
            if let Some(span) = node.span {
                out.push((suffix.clone(), span));
            }
            for (token, child) in node.children.entries().into_iter().rev() {
                stack.push((format!("{suffix}/{token}"), child));
            }
        }
        out
    }

    /// Remove the subtree rooted at `prefix` and return it (see [`Self::subtree`]).
    pub(crate) fn take_subtree(&mut self, prefix: &str) -> Vec<(String, Span)> {
        self.last_parent = None;
        let out = self.subtree(prefix);
        if prefix.is_empty() {
            self.nodes.clear();
            return out;
        }
        let Some((parent, token)) = prefix.rsplit_once('/') else {
            return out;
        };
        if let Some(p) = self.find(parent) {
            self.nodes[p as usize].children.remove(token);
        }
        out
    }

    /// Insert entries produced by [`Self::subtree`] under a new root.
    pub(crate) fn insert_subtree(&mut self, prefix: &str, entries: Vec<(String, Span)>) {
        for (suffix, span) in entries {
            self.insert(format!("{prefix}{suffix}"), span);
        }
    }

    /// Move the subtree at `from` to `to`, replacing whatever was at `to`.
    pub(crate) fn move_subtree(&mut self, from: &str, to: &str) {
        let entries = self.take_subtree(from);
        self.take_subtree(to);
        self.insert_subtree(to, entries);
    }

    /// After removing element `index` from the array at `array`, renumber
    /// the spans of the following elements.
    pub(crate) fn shift_after_removal(&mut self, array: &str, index: usize) {
        self.last_parent = None;
        let Some(node) = self.find(array) else {
            return;
        };
        match &mut self.nodes[node as usize].children {
            Children::Seq(items) => {
                if index < items.len() {
                    items.remove(index);
                }
            }
            Children::Map(map) => {
                let mut moved = vec![];
                map.retain(|token, child| match index_of(token) {
                    Some(i) if i == index => false,
                    Some(i) if i > index => {
                        moved.push(((i - 1).to_string().into_boxed_str(), *child));
                        false
                    }
                    _ => true,
                });
                map.extend(moved);
            }
        }
    }
}
