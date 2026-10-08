// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tool names (planning/07 "MCP tool surface"):
//! `<namespace>_<resource path>_<method>` in snake_case, at most
//! [`MAX_TOOL_NAME`] characters, unique within the server.
//!
//! A name is a pure function of the IR: characters outside `[a-z0-9_]` become
//! `_`; a name over the limit keeps its start and ends with `_` and eight hex
//! digits of the digest of the full name (so it is stable across
//! regenerations). Tools whose names would collide (`/a-b/c` and `/a/b-c`
//! both give `..._a_b_c_get`) all end with `_` and eight hex digits of the
//! digest of their operation id or macro name instead, so a name never
//! passes to another operation when one of them is added or removed
//! (TG0724 reports the collision).

use std::collections::{BTreeMap, BTreeSet};

use tungsten_core::Digest;

/// Longest MCP tool name the emitter produces.
pub const MAX_TOOL_NAME: usize = 48;

/// Hex digits of the digest suffix of a shortened name.
const HASH_LEN: usize = 8;

/// Tool names that collide: the shared name and the keys that share it.
pub(crate) type Collision = (String, Vec<String>);

/// Unique names for `(base, key)` entries, in entry order (`base` is
/// snake_case words joined by `_`, `key` the operation id or macro name).
/// A name only one entry has is kept; entries sharing a name are each named
/// with the digest of their key. Also returns the collisions.
pub(crate) fn assign(entries: &[(String, String)]) -> (Vec<String>, Vec<Collision>) {
    let plain: Vec<String> = entries
        .iter()
        .map(|(base, _)| shorten(&sanitize(base), MAX_TOOL_NAME))
        .collect();
    let mut sharing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, (_, key)) in plain.iter().zip(entries) {
        sharing.entry(name.as_str()).or_default().push(key.as_str());
    }
    let mut used: BTreeSet<String> = plain
        .iter()
        .filter(|n| sharing[n.as_str()].len() == 1)
        .cloned()
        .collect();
    let mut names = Vec::with_capacity(entries.len());
    for (name, (_, key)) in plain.iter().zip(entries) {
        if sharing[name.as_str()].len() == 1 {
            names.push(name.clone());
            continue;
        }
        let hash: String = Digest::of(key.as_bytes())
            .short()
            .chars()
            .take(HASH_LEN)
            .collect();
        let mut stem = name.clone();
        stem.truncate(MAX_TOOL_NAME - HASH_LEN - 1);
        let keyed = format!("{}_{hash}", stem.trim_end_matches('_'));
        // Only a digest collision or a literal name equal to a keyed one
        // can still clash.
        let unique = if used.contains(&keyed) {
            (2..)
                .map(|n| {
                    let suffix = format!("_{n}");
                    let mut s = keyed.clone();
                    s.truncate(MAX_TOOL_NAME.saturating_sub(suffix.len()));
                    format!("{}{suffix}", s.trim_end_matches('_'))
                })
                .find(|c| !used.contains(c))
                .unwrap_or(keyed)
        } else {
            keyed
        };
        used.insert(unique.clone());
        names.push(unique);
    }
    let collisions = sharing
        .into_iter()
        .filter(|(_, keys)| keys.len() > 1)
        .map(|(name, keys)| {
            (
                name.to_string(),
                keys.into_iter().map(str::to_string).collect(),
            )
        })
        .collect();
    (names, collisions)
}

/// `name` restricted to `[a-z0-9_]`: other characters become `_`, runs of
/// `_` collapse and leading or trailing ones are dropped; `tool` when
/// nothing is left.
pub fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '_'
        };
        if c == '_' && (out.is_empty() || out.ends_with('_')) {
            continue;
        }
        out.push(c);
    }
    let out = out.trim_end_matches('_').to_string();
    if out.is_empty() { "tool".into() } else { out }
}

/// `name` when it fits in `max` characters, else its start, `_` and the
/// first eight hex digits of its digest. `name` is ASCII.
pub fn shorten(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let digest = Digest::of(name.as_bytes());
    let hash: String = digest.short().chars().take(HASH_LEN).collect();
    let keep = max.saturating_sub(HASH_LEN + 1).min(name.len());
    let stem = name[..keep].trim_end_matches('_');
    format!("{stem}_{hash}")
}

/// `public.submitAlphaMessageAndAwait` → `public_submit_alpha_message_and_await`.
pub(crate) fn macro_base(name: &str) -> String {
    name.split('.')
        .map(|part| tungsten_ir::Ident::new(part).snake())
        .collect::<Vec<_>>()
        .join("_")
}
