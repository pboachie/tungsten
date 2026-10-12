// SPDX-License-Identifier: AGPL-3.0-only
//! The `McpManifest` (`runtimes/mcp/src/types.ts`): one tool entry per
//! callable operation that is not hidden (planned operations are never
//! tools) and per macro the TypeScript SDK emits, then the clusters, the
//! selected disclosure mode, the BM25 index and the `initialize`
//! instructions.
//!
//! - Input schemas are the SDK's arguments object exactly as `tools.json`
//!   describes it (`tungsten_emit::compact`), plus the reserved fields the
//!   runtime takes out before calling the SDK: an idempotency key
//!   (`idempotency_key`, or `_idempotency_key` when an argument has that
//!   name) for `caller_owned`, `auto` and `content_hash` policies, required
//!   when the key must be persisted, and `confirmation_token` (same rule)
//!   for destructive and irreversible tools.
//! - Output schemas describe the first JSON success body when it is an
//!   object (MCP structured content is an object); otherwise `null`. The
//!   types they reference are bounded ([`bound_closure`]): a `$ref` more than
//!   [`OUTPUT_HOPS`] hops from the body is `{}`, and every one when the
//!   result is over the output budget.
//! - Descriptions are the first sentence of `compact_doc` and the tier and
//!   key rule in brackets, cut to the agent manifest's
//!   `description_budget_tokens`.
//! - Annotations follow the tier: `readOnlyHint` for read-only tools,
//!   `destructiveHint` for destructive and irreversible ones,
//!   `idempotentHint` for read-only tools and keyed policies, and
//!   `openWorldHint: false`.

use crate::index::{self, SearchIndex};
use serde::Serialize;
use std::collections::BTreeSet;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::Emitter;
use tungsten_emit::schema::{collapse_whitespace, prune_sentences};
#[allow(unused_imports)]
pub use tungsten_emit::tool_catalog::compiler::{
    BINARY_BODY_NOTE, COUNTER_NAME, ClusterEntry, OUTPUT_BUDGET_FACTOR, OUTPUT_HOPS, Reserved,
    ToolAnnotations, ToolEntry, ToolKind, bound_closure, idempotency_name, safety_name, tokens,
};
use tungsten_ir::{DisclosureMode, Ir};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Discrete,
    Progressive,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpManifest {
    pub manifest_version: u32,
    pub api: String,
    pub api_version: String,
    pub tungsten_version: String,
    pub mode: Mode,
    pub threshold: u32,
    /// Tokens of the discrete tool list above which `auto` selects progressive.
    pub list_budget_tokens: u32,
    pub token_counter: &'static str,
    pub tools: Vec<ToolEntry>,
    pub clusters: Vec<ClusterEntry>,
    pub index: SearchIndex,
    /// The terms of each tool's BM25 document, in tool order (not part of
    /// the manifest file; the embedding index embeds them).
    #[serde(skip)]
    pub index_docs: Vec<Vec<String>>,
    /// The `initialize` instructions of the selected mode.
    pub instructions: String,
    /// The instructions of each mode, for servers whose mode is overridden.
    pub instructions_by_mode: InstructionsByMode,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstructionsByMode {
    pub discrete: String,
    pub progressive: String,
}

/// The manifest of `ir` and its diagnostics (TG0723 for macros the
/// TypeScript SDK does not emit).
pub fn build(ir: &Ir) -> (McpManifest, Diagnostics) {
    let (tools, mut docs, mut diags) =
        tungsten_emit::tool_catalog::compiler::compile_entries(ir, &macros_without_sdk(ir));
    for (i, tool) in tools.iter().enumerate() {
        let cluster = ir
            .agent
            .clusters
            .iter()
            .find(|c| tool.cluster.as_deref() == Some(c.name.as_str()));
        if let Some(c) = cluster {
            docs[i].extend(index::tokenize(&c.name));
            docs[i].extend(index::tokenize(c.summary.as_deref().unwrap_or_default()));
        }
    }
    let clusters = clusters(ir, &tools);
    let d = &ir.agent.disclosure;
    let instructions_by_mode = InstructionsByMode {
        discrete: instructions(ir, Mode::Discrete, &tools, &clusters),
        progressive: instructions(ir, Mode::Progressive, &tools, &clusters),
    };
    let mut manifest = McpManifest {
        manifest_version: 1,
        api: ir.api.name.wire.clone(),
        api_version: ir.api.version.clone(),
        tungsten_version: ir.generator.tungsten_version.clone(),
        mode: Mode::Discrete,
        threshold: d.threshold,
        list_budget_tokens: d.list_budget_tokens,
        token_counter: COUNTER_NAME,
        index: index::build(&docs),
        index_docs: docs,
        tools,
        clusters,
        instructions: instructions_by_mode.discrete.clone(),
        instructions_by_mode,
    };
    manifest.mode = match d.mode {
        DisclosureMode::Discrete => Mode::Discrete,
        DisclosureMode::Progressive => Mode::Progressive,
        DisclosureMode::Auto => {
            let value = serde_json::to_value(&manifest).unwrap_or_default();
            let measured = crate::budget::listing_tokens(&value, Mode::Discrete);
            let mode = if measured > d.list_budget_tokens as usize {
                Mode::Progressive
            } else {
                Mode::Discrete
            };
            let (name, cmp) = match mode {
                Mode::Discrete => ("discrete", "within"),
                Mode::Progressive => ("progressive", "over"),
            };
            diags.push(
                Diagnostic::info(
                    "TG0725",
                    format!(
                        "disclosure.mode auto selected {name}: the discrete MCP tool list is about {measured} tokens, {cmp} the list budget of {}",
                        d.list_budget_tokens
                    ),
                )
                .with_help("set defaults.disclosure.mode to discrete or progressive, or change defaults.disclosure.list_budget_tokens in agent.yml"),
            );
            mode
        }
    };
    if manifest.mode == Mode::Progressive {
        manifest.instructions = manifest.instructions_by_mode.progressive.clone();
    }
    (manifest, diags)
}

/// Names of the IR macros the TypeScript SDK leaves out (its TG0710).
fn macros_without_sdk(ir: &Ir) -> BTreeSet<String> {
    if ir.agent.macros.is_empty() {
        return BTreeSet::new();
    }
    tungsten_emit_ts::TypeScriptEmitter
        .supports(ir)
        .iter()
        .filter(|d| d.code == "TG0710")
        .filter_map(|d| {
            let rest = d.message.split_once('`')?.1;
            Some(rest.split_once('`')?.0.to_string())
        })
        .collect()
}

/// Clusters in manifest order with the names of their tools.
fn clusters(ir: &Ir, tools: &[ToolEntry]) -> Vec<ClusterEntry> {
    ir.agent
        .clusters
        .iter()
        .map(|c| {
            let members: BTreeSet<&str> = c.operations.iter().map(|o| o.0.as_str()).collect();
            ClusterEntry {
                name: c.name.clone(),
                summary: c
                    .summary
                    .as_deref()
                    .map(collapse_whitespace)
                    .filter(|s| !s.is_empty()),
                tools: tools
                    .iter()
                    .filter(|t| members.contains(t.target.as_str()))
                    .map(|t| t.name.clone())
                    .collect(),
            }
        })
        .collect()
}

/// The `initialize` instructions: how to find and call tools, the
/// confirmation and idempotency rules when some tool takes those fields, and
/// how to read failures; in progressive mode also the cluster index.
fn instructions(ir: &Ir, mode: Mode, tools: &[ToolEntry], clusters: &[ClusterEntry]) -> String {
    let title = collapse_whitespace(&ir.api.title);
    let title = if title.is_empty() {
        ir.api.name.wire.clone()
    } else {
        title
    };
    let title = tungsten_ir::title_stem(&title);
    let mut out = vec![];
    match mode {
        Mode::Progressive => out.push(format!(
            "{title} API, {} tools behind search. Find tools with search_tools(query, cluster?, limit?), read one with describe_tool(name), call it with invoke(name, arguments){}.",
            tools.len(),
            if tools.iter().any(|t| t.safety == "read_only") {
                " (invoke_read for read_only tools)"
            } else {
                ""
            }
        )),
        Mode::Discrete => out.push(format!(
            "{title} API, {} tools, one per operation or macro.",
            tools.len()
        )),
    }
    if tools
        .iter()
        .any(|t| t.reserved.confirmation_token.is_some())
    {
        let preview = match mode {
            Mode::Progressive => "preview(name, arguments)",
            Mode::Discrete => "preview(tool, arguments)",
        };
        out.push(format!("Destructive and irreversible tools need {preview} first; pass its confirmation_token with the same arguments."));
    }
    let keyed = tools.iter().any(|t| t.reserved.idempotency_key.is_some());
    if keyed {
        out.push("Pass the idempotency key a tool asks for, persist it with your intent and reuse it on every retry; a new key can repeat a side effect.".into());
    }
    out.push(format!(
        "Failures are envelopes, not exceptions: follow category, retryable, remediation and next_action; after OUTCOME_UNKNOWN {}.",
        if keyed {
            "check or retry with the same key only"
        } else {
            "check the outcome before any retry"
        }
    ));
    if mode == Mode::Progressive && !clusters.is_empty() {
        let lines: Vec<String> = clusters
            .iter()
            .map(|c| {
                let n = c.tools.len();
                match &c.summary {
                    Some(s) => format!(
                        "{} ({n}): {}",
                        c.name,
                        prune_sentences(s, 1).trim_end_matches(['.', '!', '?'])
                    ),
                    None => format!("{} ({n})", c.name),
                }
            })
            .collect();
        let unclustered = tools
            .iter()
            .filter(|t| !clusters.iter().any(|c| c.tools.contains(&t.name)))
            .count();
        let mut text = format!("Clusters: {}", lines.join("; "));
        if unclustered > 0 {
            text.push_str(&format!("; {unclustered} more without a cluster"));
        }
        text.push('.');
        out.push(text);
    }
    out.join(" ")
}
