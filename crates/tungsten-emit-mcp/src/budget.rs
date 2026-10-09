// SPDX-License-Identifier: AGPL-3.0-only
//! Token budgets of the MCP surface.
//!
//! - Per tool: `schemaTokens` (description and input schema) must stay
//!   within agent.yml `defaults.disclosure.schema_budget_tokens`; a tool
//!   over it is TG0721.
//! - Progressive mode: what a client receives before its first search, the
//!   `tools/list` answer of `@tungsten/mcp` in progressive mode
//!   (`search_tools`, `describe_tool`, `invoke`, `preview`, `list_clusters`;
//!   the opt-in `run_script` is not counted) plus the `initialize`
//!   instructions, which carry the cluster index, must stay within
//!   [`INDEX_BUDGET`] tokens; over it is TG0722.
//! - Discrete mode, for comparison: the `tools/list` answer in discrete mode
//!   (every tool with its input schema, advertised output schema and
//!   annotations, and the `preview` meta tool) plus that mode's
//!   instructions.
//!
//! [`tools_list`] renders `tools/list` exactly as `@tungsten/mcp`
//! (`runtimes/mcp/src/catalog.ts`, `listTools`) serves it for a manifest,
//! in the same key order, so the compact JSON is byte-identical to what a
//! client receives (the MCP gate compares the two). The emitter's budgets,
//! the generated README and `tungsten report` all count that rendering.

use serde_json::{Map, Value, json};
use tungsten_core::{Diagnostic, Diagnostics};

use crate::manifest::{McpManifest, Mode, tokens};

/// Tokens of the progressive tool list and index summary.
pub const INDEX_BUDGET: usize = 2000;

/// Replacement of a sensitive response field on a repeated identical call
/// (`REDACTED_REPEAT` of `@tungsten/mcp`).
pub const REDACTED_REPEAT: &str = "<redacted after first read>";

/// The measured budgets of one manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    /// The agent manifest's per-tool schema budget.
    pub schema_budget: usize,
    /// Progressive `tools/list` plus that mode's instructions.
    pub progressive: usize,
    /// Discrete `tools/list` plus that mode's instructions.
    pub discrete: usize,
    /// The tool with the largest `schemaTokens`.
    pub largest: Option<(String, usize)>,
    /// Median `schemaTokens` (the lower middle value).
    pub median: usize,
    /// Tools over the schema budget, in tool order.
    pub over: Vec<(String, usize)>,
}

const SAFETIES: [&str; 4] = ["read_only", "mutating", "destructive", "irreversible"];
const FLAGS: [&str; 4] = [
    "readOnlyHint",
    "destructiveHint",
    "idempotentHint",
    "openWorldHint",
];

/// A manifest tool as the runtime's catalog reads it.
struct Tool<'a> {
    raw: &'a Map<String, Value>,
    name: &'a str,
    safety: &'a str,
}

impl Tool<'_> {
    fn str(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }

    /// The annotations: the manifest's flags, else derived from the tier,
    /// then a non-empty `title`.
    fn annotations(&self) -> Value {
        let given = self.raw.get("annotations").and_then(Value::as_object);
        let s = self.safety;
        let derived = [
            s == "read_only",
            s == "destructive" || s == "irreversible",
            s == "read_only",
            false,
        ];
        let mut out = Map::new();
        for (flag, default) in FLAGS.iter().zip(derived) {
            let v = given
                .and_then(|a| a.get(*flag))
                .and_then(Value::as_bool)
                .unwrap_or(default);
            out.insert((*flag).to_string(), Value::Bool(v));
        }
        if let Some(title) = given
            .and_then(|a| a.get("title"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            out.insert("title".into(), json!(title));
        }
        Value::Object(out)
    }

    fn verifies(&self) -> bool {
        self.str("kind") == Some("operation") && self.safety == "irreversible"
    }

    fn sensitive(&self) -> Vec<&str> {
        self.raw
            .get("sensitiveResponseFields")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|f| !f.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn valid_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The tools the runtime keeps, in manifest order: valid entries, first of
/// each name, minus names the mode's meta tools take.
fn catalog(manifest: &Value, mode: Mode) -> Vec<Tool<'_>> {
    let reserved: &[&str] = match mode {
        Mode::Discrete => &["preview", "run_script"],
        Mode::Progressive => &[],
    };
    let mut out: Vec<Tool<'_>> = vec![];
    let raw = manifest.get("tools").and_then(Value::as_array);
    for entry in raw.into_iter().flatten() {
        let Some(raw) = entry.as_object() else {
            continue;
        };
        let Some(name) = raw.get("name").and_then(Value::as_str) else {
            continue;
        };
        let kind = raw.get("kind").and_then(Value::as_str);
        let target = raw.get("target").and_then(Value::as_str).unwrap_or("");
        let object_input = raw
            .get("inputSchema")
            .and_then(Value::as_object)
            .is_some_and(|s| s.get("type") == Some(&json!("object")));
        if !valid_name(name)
            || !matches!(kind, Some("operation" | "macro"))
            || target.is_empty()
            || !object_input
            || out.iter().any(|t| t.name == name)
            || reserved.contains(&name)
        {
            continue;
        }
        let safety = raw
            .get("safety")
            .and_then(Value::as_str)
            .filter(|s| SAFETIES.contains(s))
            .unwrap_or("irreversible");
        out.push(Tool { raw, name, safety });
    }
    out
}

/// `schema` with the field at `segments` also allowed to be the redaction
/// marker (`allowRedaction` of the runtime).
fn allow_redaction(schema: &Map<String, Value>, segments: &[&str]) -> Map<String, Value> {
    let Some((key, rest)) = segments.split_first() else {
        let mut out = Map::new();
        out.insert(
            "anyOf".into(),
            json!([Value::Object(schema.clone()), { "const": REDACTED_REPEAT }]),
        );
        return out;
    };
    if schema.get("type") == Some(&json!("array"))
        && let Some(items) = schema.get("items").and_then(Value::as_object)
    {
        let mut out = schema.clone();
        out.insert(
            "items".into(),
            Value::Object(allow_redaction(items, segments)),
        );
        return out;
    }
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return schema.clone();
    };
    let Some(field) = properties.get(*key).and_then(Value::as_object) else {
        return schema.clone();
    };
    let mut props = properties.clone();
    props.insert(
        (*key).to_string(),
        Value::Object(allow_redaction(field, rest)),
    );
    let mut out = schema.clone();
    out.insert("properties".into(), Value::Object(props));
    out
}

/// The `outputSchema` the runtime advertises (`advertisedOutputSchema`):
/// the body schema widened with the verification result, the redaction
/// marker and the error envelope, its `$defs` kept at the root.
fn advertised_output(tool: &Tool<'_>) -> Option<Value> {
    let schema = tool.raw.get("outputSchema").and_then(Value::as_object)?;
    if schema.get("type") != Some(&json!("object")) {
        return None;
    }
    let mut body = schema.clone();
    let dialect = body.shift_remove("$schema");
    let defs = body.shift_remove("$defs");
    if tool.verifies() {
        let mut properties = body
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if !properties.contains_key("verification") {
            properties.insert(
                "verification".into(),
                json!({
                    "type": "object",
                    "description": "Result of the operation's verification hook.",
                    "properties": {
                        "checked": { "type": "boolean" },
                        "passed": { "type": "boolean" },
                        "observed": {},
                        "timedOut": { "type": "boolean" },
                        "error": { "type": "object" },
                    },
                }),
            );
            body.insert("properties".into(), Value::Object(properties));
        }
    }
    for path in tool.sensitive() {
        let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
        body = allow_redaction(&body, &segments);
    }
    let mut out = Map::new();
    if let Some(d) = dialect {
        out.insert("$schema".into(), d);
    }
    out.insert("type".into(), json!("object"));
    if let Some(d) = defs {
        out.insert("$defs".into(), d);
    }
    out.insert(
        "anyOf".into(),
        json!([
            Value::Object(body),
            { "properties": { "status": { "const": "error" } }, "required": ["status", "category", "remediation"] },
        ]),
    );
    Some(Value::Object(out))
}

fn closed(properties: Value, required: Option<Value>) -> Value {
    let mut out = Map::new();
    out.insert("type".into(), json!("object"));
    out.insert("properties".into(), properties);
    if let Some(r) = required {
        out.insert("required".into(), r);
    }
    out.insert("additionalProperties".into(), json!(false));
    Value::Object(out)
}

fn arguments(what: &str) -> Value {
    json!({
        "type": "object",
        "description": format!("{what}: the tool's input object, including its reserved fields (idempotency key, confirmation token) when it has them."),
    })
}

fn meta_read() -> Value {
    json!({ "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false })
}

fn meta_preview() -> Value {
    json!({ "readOnlyHint": true, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false })
}

fn meta_write() -> Value {
    json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false })
}

/// The `tools/list` tools of `@tungsten/mcp` serving `manifest` (the
/// manifest as JSON, as written to `manifest.json`) in `mode`, without the
/// opt-in `run_script`.
pub fn tools_list(manifest: &Value, mode: Mode) -> Value {
    let tools = catalog(manifest, mode);
    match mode {
        Mode::Discrete => discrete_list(&tools),
        Mode::Progressive => progressive_list(manifest, tools.len()),
    }
}

fn discrete_list(tools: &[Tool<'_>]) -> Value {
    let mut sorted: Vec<&Tool<'_>> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(b.name));
    let mut listed: Vec<Value> = sorted
        .iter()
        .map(|t| {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(t.name));
            entry.insert(
                "description".into(),
                json!(t.str("description").unwrap_or("")),
            );
            entry.insert("inputSchema".into(), t.raw["inputSchema"].clone());
            entry.insert("annotations".into(), t.annotations());
            if let Some(output) = advertised_output(t) {
                entry.insert("outputSchema".into(), output);
            }
            Value::Object(entry)
        })
        .collect();
    let mut previewable: Vec<&str> = tools
        .iter()
        .filter(|t| t.safety != "read_only")
        .map(|t| t.name)
        .collect();
    previewable.sort_unstable();
    if !previewable.is_empty() {
        listed.push(json!({
            "name": "preview",
            "description": "Preview a non-read-only tool call without executing it: the rendered request, its effects and a confirmation_token bound to these exact arguments (required by destructive and irreversible tools).",
            "inputSchema": closed(
                json!({ "tool": { "type": "string", "enum": previewable }, "arguments": arguments("Arguments of the call to preview") }),
                Some(json!(["tool"])),
            ),
            "annotations": meta_preview(),
        }));
    }
    Value::Array(listed)
}

fn progressive_list(manifest: &Value, tool_count: usize) -> Value {
    let mut clusters: Vec<&str> = manifest
        .get("clusters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("name").and_then(Value::as_str))
        .filter(|n| !n.is_empty())
        .collect();
    clusters.sort_unstable();
    let mut cluster = Map::new();
    cluster.insert("type".into(), json!("string"));
    cluster.insert(
        "description".into(),
        json!("Restrict the search to one cluster (see list_clusters)."),
    );
    if !clusters.is_empty() {
        cluster.insert("enum".into(), json!(clusters));
    }
    let name = || json!({ "type": "string", "minLength": 1 });
    json!([
        {
            "name": "search_tools",
            "description": format!("Search the {tool_count} API tools by keywords. Returns name, summary, safety tier, idempotency policy and schema token cost."),
            "inputSchema": closed(
                json!({
                    "query": { "type": "string", "minLength": 1, "description": "Words describing the task, e.g. \"replay a failed webhook\"." },
                    "cluster": Value::Object(cluster),
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
                }),
                Some(json!(["query"])),
            ),
            "annotations": meta_read(),
        },
        {
            "name": "describe_tool",
            "description": "Full input and output schema of one tool, its safety tier, reserved fields, remediation table and an example invoke call.",
            "inputSchema": closed(json!({ "name": name() }), Some(json!(["name"]))),
            "annotations": meta_read(),
        },
        {
            "name": "invoke",
            "description": "Execute one tool. Returns the response body, or the error envelope with remediation. Destructive and irreversible tools need the confirmation_token from preview with the same arguments.",
            "inputSchema": closed(json!({ "name": name(), "arguments": arguments("Arguments of the tool") }), Some(json!(["name"]))),
            "annotations": meta_write(),
        },
        {
            "name": "preview",
            "description": "Preview a tool call without executing it: the rendered request, its effects and a confirmation_token bound to these exact arguments.",
            "inputSchema": closed(json!({ "name": name(), "arguments": arguments("Arguments of the call to preview") }), Some(json!(["name"]))),
            "annotations": meta_preview(),
        },
        {
            "name": "list_clusters",
            "description": "The tool clusters with their summaries and tool counts.",
            "inputSchema": closed(json!({}), None),
            "annotations": meta_read(),
        },
    ])
}

/// The `initialize` instructions the runtime sends in `mode`: the
/// manifest's `instructionsByMode` entry, else `instructions`.
pub fn instructions(manifest: &Value, mode: Mode) -> &str {
    let key = match mode {
        Mode::Discrete => "discrete",
        Mode::Progressive => "progressive",
    };
    manifest
        .get("instructionsByMode")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .or_else(|| manifest.get("instructions").and_then(Value::as_str))
        .unwrap_or("")
}

/// Tokens a client receives before its first tool call in `mode`: the
/// compact JSON of the `tools/list` tools plus the instructions, under the
/// manifest's counter.
pub fn listing_tokens(manifest: &Value, mode: Mode) -> usize {
    let list = serde_json::to_string(&tools_list(manifest, mode)).unwrap_or_default();
    tokens(&list) + tokens(instructions(manifest, mode))
}

/// Measure `manifest` against `schema_budget`.
pub fn measure(manifest: &McpManifest, schema_budget: usize) -> Budget {
    let value = serde_json::to_value(manifest).unwrap_or_default();
    let progressive = listing_tokens(&value, Mode::Progressive);
    let discrete = listing_tokens(&value, Mode::Discrete);
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
