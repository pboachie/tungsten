// SPDX-License-Identifier: AGPL-3.0-only
//! JSON Pointer → byte span index of one document.
//!
//! Keys are RFC 6901 pointers. The index is a `BTreeMap` so that every
//! subtree (`P` plus all keys starting with `P/`) is one contiguous range,
//! which keeps relocation after overlays and normalization cheap.

use std::collections::BTreeMap;

use tungsten_core::Span;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SpanIndex(BTreeMap<String, Span>);

/// Exclusive upper bound of the key range holding the strict descendants of
/// `prefix`: every key starting with `prefix/` sorts before `prefix0`
/// (`'0'` is the character after `'/'`).
fn descendants_range(prefix: &str) -> (String, String) {
    (format!("{prefix}/"), format!("{prefix}0"))
}

impl SpanIndex {
    pub(crate) fn insert(&mut self, pointer: String, span: Span) {
        self.0.insert(pointer, span);
    }

    pub(crate) fn get(&self, pointer: &str) -> Option<Span> {
        self.0.get(pointer).copied()
    }

    /// The span of `pointer`, or of its nearest ancestor that has one.
    pub(crate) fn nearest(&self, pointer: &str) -> Option<Span> {
        let mut p = pointer;
        loop {
            if let Some(s) = self.0.get(p) {
                return Some(*s);
            }
            let cut = p.rfind('/')?;
            p = &p[..cut];
        }
    }

    /// Every entry of the subtree rooted at `prefix`, as (suffix, span)
    /// where the suffix is `""` for the root itself.
    pub(crate) fn subtree(&self, prefix: &str) -> Vec<(String, Span)> {
        let mut out = vec![];
        if let Some(s) = self.0.get(prefix) {
            out.push((String::new(), *s));
        }
        let (lo, hi) = descendants_range(prefix);
        out.extend(
            self.0
                .range(lo..hi)
                .map(|(k, s)| (k[prefix.len()..].to_string(), *s)),
        );
        out
    }

    /// Remove the subtree rooted at `prefix` and return it (see [`Self::subtree`]).
    pub(crate) fn take_subtree(&mut self, prefix: &str) -> Vec<(String, Span)> {
        let out = self.subtree(prefix);
        for (suffix, _) in &out {
            self.0.remove(&format!("{prefix}{suffix}"));
        }
        out
    }

    /// Insert entries produced by [`Self::subtree`] under a new root.
    pub(crate) fn insert_subtree(&mut self, prefix: &str, entries: Vec<(String, Span)>) {
        for (suffix, span) in entries {
            self.0.insert(format!("{prefix}{suffix}"), span);
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
        let (lo, hi) = descendants_range(array);
        let keys: Vec<String> = self.0.range(lo..hi).map(|(k, _)| k.clone()).collect();
        let mut moved = vec![];
        for key in keys {
            let rest = &key[array.len() + 1..];
            let (head, tail) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let Ok(i) = head.parse::<usize>() else {
                continue;
            };
            if i < index {
                continue;
            }
            let span = self.0.remove(&key).expect("key listed from the map");
            if i > index {
                moved.push((format!("{array}/{}{tail}", i - 1), span));
            }
        }
        self.0.extend(moved);
    }
}
