// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tool names (planning/07 "MCP tool surface"):
//! `<namespace>_<resource path>_<method>` in snake_case, at most
//! [`MAX_TOOL_NAME`] characters, unique within the server.
//!
//! A name is a pure function of the IR: characters outside `[a-z0-9_]` become
//! `_`; a name over the limit keeps its start and ends with `_` and eight hex
//! digits of the digest of the full name (so it is stable across
//! regenerations); a name already taken gets the smallest free suffix `_2`,
//! `_3`, ... in tool order.

use std::collections::BTreeSet;

use tungsten_core::Digest;

/// Longest MCP tool name the emitter produces.
pub const MAX_TOOL_NAME: usize = 48;

/// Hex digits of the digest suffix of a shortened name.
const HASH_LEN: usize = 8;

/// Assigns unique tool names.
#[derive(Debug, Default)]
pub(crate) struct ToolNames {
    used: BTreeSet<String>,
}

impl ToolNames {
    /// The name for `base` (snake_case words joined by `_`).
    pub fn assign(&mut self, base: &str) -> String {
        let base = shorten(&sanitize(base), MAX_TOOL_NAME);
        let name = if self.used.contains(&base) {
            (2..)
                .map(|n| {
                    let suffix = format!("_{n}");
                    let mut stem = base.clone();
                    stem.truncate(MAX_TOOL_NAME.saturating_sub(suffix.len()));
                    format!("{}{suffix}", stem.trim_end_matches('_'))
                })
                .find(|candidate| !self.used.contains(candidate))
                .unwrap_or(base)
        } else {
            base
        };
        self.used.insert(name.clone());
        name
    }
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
