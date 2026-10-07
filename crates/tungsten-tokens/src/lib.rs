// SPDX-License-Identifier: AGPL-3.0-only
//! Token counting for agent-facing budgets (planning/01 NFR-3,
//! planning/05 "MCP server").
//!
//! Budgets are enforced with a deterministic estimate so the compiler has no
//! model-specific dependency; the test harness cross-checks the estimate
//! against a real BPE tokenizer.
//!
//! PHASE-3 CONTRACT: `count` and `Counter` are shared by the docs and MCP
//! emitters and the report. The tokens work package replaces the estimate's
//! internals (calibrated against a BPE reference) without changing them.

/// How tokens are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Counter {
    /// `ceil(chars / 4)`: the historical approximation.
    CharsDiv4,
    /// The calibrated deterministic estimate used for budgets.
    #[default]
    Estimate,
}

/// Number of tokens in `text` under `counter`.
pub fn count(text: &str, counter: Counter) -> usize {
    let chars = text.chars().count();
    match counter {
        Counter::CharsDiv4 | Counter::Estimate => chars.div_ceil(4),
    }
}
