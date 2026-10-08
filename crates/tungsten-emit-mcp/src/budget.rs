// SPDX-License-Identifier: AGPL-3.0-only
//! Token budgets of the MCP surface (planning/01 NFR-3, planning/10 P5).
//!
//! - Per tool: `schemaTokens` (description and input schema) must stay
//!   within agent.yml `defaults.disclosure.schema_budget_tokens`; a tool
//!   over it is TG0721.
//! - Progressive mode: what a client receives before its first search, the
//!   meta-tool list of planning/05 (`search_tools`, `describe_tool`,
//!   `invoke`, `preview`, `list_clusters`; the opt-in `run_script` is not
//!   counted) with the cluster index carried in the instructions, must stay
//!   within [`INDEX_BUDGET`] tokens; over it is TG0722.
//!
//! The meta tools are measured in the shape below, which is how this
//! emitter documents them; `@tungsten/mcp` serves them.

use serde_json::{Value, json};
use tungsten_core::{Diagnostic, Diagnostics};

use crate::manifest::{McpManifest, Mode, tokens};

/// NFR-3: tokens of the progressive tool list and index summary.
pub const INDEX_BUDGET: usize = 2000;

/// The measured budgets of one manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    /// The agent manifest's per-tool schema budget.
    pub schema_budget: usize,
    /// Meta-tool list plus instructions (progressive mode).
    pub progressive: usize,
    /// Every tool as `tools/list` lists it plus instructions (discrete mode).
    pub discrete: usize,
    /// The tool with the largest `schemaTokens`.
    pub largest: Option<(String, usize)>,
    /// Median `schemaTokens` (the lower middle value).
    pub median: usize,
    /// Tools over the schema budget, in tool order.
    pub over: Vec<(String, usize)>,
}

/// The progressive-mode meta tools as listed to a client.
pub fn meta_tools() -> Value {
    let read = json!({ "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false });
    let name = json!({ "type": "string", "description": "Tool name from search_tools." });
    let args = json!({ "type": "object", "description": "The tool's arguments (describe_tool shows its schema)." });
    let object = |properties: Value, required: Value| json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false });
    json!([
        {
            "name": "search_tools",
            "description": "Search this API's tools by keywords. Returns name, summary, safety tier, idempotency and schema_tokens of the best matches.",
            "inputSchema": object(json!({
                "query": { "type": "string", "description": "What you want to do, in a few words." },
                "cluster": { "type": "string", "description": "Only tools of this cluster (see list_clusters)." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Most results to return (default 10)." },
            }), json!(["query"])),
            "annotations": read,
        },
        {
            "name": "describe_tool",
            "description": "Full input and output schema of one tool, its safety and idempotency rules and remediation.",
            "inputSchema": object(json!({ "name": name }), json!(["name"])),
            "annotations": read,
        },
        {
            "name": "invoke",
            "description": "Call a tool. Returns its result, or an error envelope whose remediation says what to do next.",
            "inputSchema": object(json!({ "name": name, "args": args }), json!(["name", "args"])),
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false },
        },
        {
            "name": "preview",
            "description": "Validate a call and show its request and effects without sending it; returns the confirmation_token destructive and irreversible tools need.",
            "inputSchema": object(json!({ "name": name, "args": args }), json!(["name", "args"])),
            "annotations": read,
        },
        {
            "name": "list_clusters",
            "description": "The groups of related tools with their summaries and tool counts.",
            "inputSchema": object(json!({}), json!([])),
            "annotations": read,
        },
    ])
}

/// Every tool as discrete mode lists it.
pub fn discrete_tools(manifest: &McpManifest) -> Value {
    Value::Array(
        manifest
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                });
                if let Some(output) = &t.output_schema {
                    tool["outputSchema"] = output.clone();
                }
                tool["annotations"] = serde_json::to_value(&t.annotations).unwrap_or_default();
                tool
            })
            .collect(),
    )
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Measure `manifest` against `schema_budget`.
pub fn measure(manifest: &McpManifest, schema_budget: usize) -> Budget {
    let instructions = tokens(&manifest.instructions);
    let progressive = tokens(&compact(&meta_tools())) + instructions;
    let discrete = tokens(&compact(&discrete_tools(manifest))) + instructions;
    let mut sizes: Vec<usize> = manifest.tools.iter().map(|t| t.schema_tokens).collect();
    sizes.sort_unstable();
    let median = if sizes.is_empty() {
        0
    } else {
        sizes[(sizes.len() - 1) / 2]
    };
    let largest = manifest
        .tools
        .iter()
        .fold(None::<(String, usize)>, |best, t| match best {
            Some((_, n)) if n >= t.schema_tokens => best,
            _ => Some((t.name.clone(), t.schema_tokens)),
        });
    let over = manifest
        .tools
        .iter()
        .filter(|t| t.schema_tokens > schema_budget)
        .map(|t| (t.name.clone(), t.schema_tokens))
        .collect();
    Budget {
        schema_budget,
        progressive,
        discrete,
        largest,
        median,
        over,
    }
}

/// TG0721 for each tool over the schema budget; TG0722 when the selected
/// mode is progressive and its listing is over [`INDEX_BUDGET`].
pub fn warnings(manifest: &McpManifest, budget: &Budget) -> Diagnostics {
    let mut d = Diagnostics::new();
    for (name, n) in &budget.over {
        d.push(
            Diagnostic::warning(
                "TG0721",
                format!(
                    "MCP tool `{name}` is about {n} tokens, over the schema budget of {}",
                    budget.schema_budget
                ),
            )
            .with_help("shorten descriptions (disclosure.prune for operations, the spec for fields), split the operation, or raise defaults.disclosure.schema_budget_tokens in agent.yml"),
        );
    }
    if manifest.mode == Mode::Progressive && budget.progressive > INDEX_BUDGET {
        d.push(
            Diagnostic::warning(
                "TG0722",
                format!(
                    "the progressive MCP tool list and index summary are about {} tokens, over the budget of {INDEX_BUDGET}",
                    budget.progressive
                ),
            )
            .with_help("shorten the cluster summaries in agent.yml or use fewer clusters"),
        );
    }
    d
}
