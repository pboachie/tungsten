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
//!   object (MCP structured content is an object); otherwise `null`.
//! - Descriptions are the first sentence of `compact_doc` and the tier and
//!   key rule in brackets, cut to the agent manifest's
//!   `description_budget_tokens`.
//! - Annotations follow the tier: `readOnlyHint` for read-only tools,
//!   `destructiveHint` for destructive and irreversible ones,
//!   `idempotentHint` for read-only tools and keyed policies, and
//!   `openWorldHint: false`.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Value, json};
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::Emitter;
use tungsten_emit::args::{BodyArg, args_layout};
use tungsten_emit::compact::{
    callable_operation, hoist_repeats, macro_parameters, operation_parameters, set_required,
    tool_schema_options, without_dialect,
};
use tungsten_emit::schema::{SchemaBuilder, Usage, collapse_whitespace, prune_sentences};
use tungsten_ir::{
    BodyEncoding, DisclosureMode, IdempotencyKind, IdempotencyPolicy, Ir, Macro, Operation,
    OperationStatus, ResponseKind, Safety, StatusMatch,
};
use tungsten_tokens::{Counter, count};

use crate::index::{self, SearchIndex};
use crate::names::{self, macro_base};

/// The counter every token figure of the manifest uses.
pub const COUNTER: Counter = Counter::Estimate;
/// `McpManifest.tokenCounter` for [`COUNTER`].
pub const COUNTER_NAME: &str = "estimate";

/// Number of tokens of `text` under the manifest's counter.
pub fn tokens(text: &str) -> usize {
    count(text, COUNTER)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Discrete,
    Progressive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Operation,
    Macro,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    pub read_only_hint: bool,
    pub destructive_hint: bool,
    pub idempotent_hint: bool,
    pub open_world_hint: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reserved {
    pub idempotency_key: Option<String>,
    pub confirmation_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolEntry {
    pub name: String,
    pub kind: ToolKind,
    pub target: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: ToolAnnotations,
    pub safety: &'static str,
    pub idempotency: &'static str,
    pub reserved: Reserved,
    pub cluster: Option<String>,
    pub sensitive_response_fields: Vec<String>,
    pub shown_once: bool,
    pub schema_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterEntry {
    pub name: String,
    pub summary: Option<String>,
    pub tools: Vec<String>,
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
    let mut diags = Diagnostics::new();
    let budget = ir.agent.disclosure.description_budget_tokens as usize;
    let mut tools = vec![];
    let mut docs: Vec<Vec<String>> = vec![];
    let ops = callable(ir);
    let skipped = macros_without_sdk(ir);
    let macros: Vec<&Macro> = ir
        .agent
        .macros
        .iter()
        .filter(|m| !skipped.contains(&m.name.0))
        .collect();
    let op_bases: Vec<String> = ops
        .iter()
        .map(|(ns, resources, op)| {
            let mut words = vec![ns.clone()];
            words.extend(resources.iter().cloned());
            words.push(op.name.snake());
            words.join("_")
        })
        .collect();
    let entries: Vec<(String, String)> = op_bases
        .iter()
        .zip(&ops)
        .map(|(base, (_, _, op))| (base.clone(), op.id.0.clone()))
        .chain(
            macros
                .iter()
                .map(|m| (macro_base(&m.name.0), m.name.0.clone())),
        )
        .collect();
    let (names, collisions) = names::assign(&entries);
    for (shared, keys) in &collisions {
        diags.push(
            Diagnostic::warning(
                "TG0724",
                format!(
                    "{} would all be the MCP tool `{shared}`; each is named with a digest of its id instead",
                    keys.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
                ),
            )
            .with_help("Give them distinct method names with `naming.operations` in tungsten.yml, so their tool names say what they do."),
        );
    }
    let mut names = names.into_iter();
    for (base, (_, resources, op)) in op_bases.iter().zip(&ops) {
        let name = names.next().unwrap_or_default();
        let (tool, terms) = operation_tool(ir, op, base, name, resources, budget);
        tools.push(tool);
        docs.push(terms);
    }
    for mac in &ir.agent.macros {
        if skipped.contains(&mac.name.0) {
            diags.push(
                Diagnostic::warning(
                    "TG0723",
                    format!(
                        "macro `{}` is not an MCP tool: the TypeScript SDK does not emit it",
                        mac.name.0
                    ),
                )
                .with_help("Fix the macro in agent.yml (see TG0710 of the typescript target)."),
            );
            continue;
        }
        let base = macro_base(&mac.name.0);
        let name = names.next().unwrap_or_default();
        let (tool, terms) = macro_tool(ir, mac, &base, name, budget);
        tools.push(tool);
        docs.push(terms);
    }
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

/// Callable, visible operations in namespace and depth-first resource
/// order: `(namespace, resource names, operation)`.
fn callable(ir: &Ir) -> Vec<(String, Vec<String>, &Operation)> {
    fn walk<'a>(
        r: &'a tungsten_ir::Resource,
        ns: &str,
        path: &mut Vec<String>,
        out: &mut Vec<(String, Vec<String>, &'a Operation)>,
    ) {
        path.push(r.name.snake());
        for op in &r.operations {
            let planned = matches!(op.status, OperationStatus::Planned { .. });
            if !planned && !op.agent.hidden {
                out.push((ns.to_string(), path.clone(), op));
            }
        }
        for c in &r.children {
            walk(c, ns, path, out);
        }
        path.pop();
    }
    let mut out = vec![];
    for ns in &ir.namespaces {
        let name = ns.name.snake();
        for r in &ns.resources {
            walk(r, &name, &mut vec![], &mut out);
        }
    }
    out
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

pub(crate) fn safety_name(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "read_only",
        Safety::Mutating => "mutating",
        Safety::Destructive => "destructive",
        Safety::Irreversible => "irreversible",
    }
}

pub(crate) fn idempotency_name(k: IdempotencyKind) -> &'static str {
    match k {
        IdempotencyKind::None => "none",
        IdempotencyKind::Auto => "auto",
        IdempotencyKind::CallerOwned => "caller_owned",
        IdempotencyKind::ContentHash => "content_hash",
        IdempotencyKind::ContentIdentity => "content_identity",
    }
}

/// Policies whose tools take an idempotency key field.
fn takes_key(k: IdempotencyKind) -> bool {
    matches!(
        k,
        IdempotencyKind::CallerOwned | IdempotencyKind::Auto | IdempotencyKind::ContentHash
    )
}

fn needs_confirmation(s: Safety) -> bool {
    matches!(s, Safety::Destructive | Safety::Irreversible)
}

fn annotations(safety: Safety, policy: IdempotencyKind) -> ToolAnnotations {
    ToolAnnotations {
        read_only_hint: safety == Safety::ReadOnly,
        destructive_hint: needs_confirmation(safety),
        idempotent_hint: safety == Safety::ReadOnly || takes_key(policy),
        open_world_hint: false,
    }
}

/// `base`, or `base` with as many leading `_` as it takes not to equal a
/// key of `taken`.
fn free_name(base: &str, taken: &[String]) -> String {
    let mut name = base.to_string();
    while taken.contains(&name) {
        name.insert(0, '_');
    }
    name
}

/// `UUIDv4` for `uuid_v4`, the format as written otherwise.
fn format_label(format: &str) -> String {
    if format.eq_ignore_ascii_case("uuid_v4") || format.eq_ignore_ascii_case("uuidv4") {
        "UUIDv4".into()
    } else {
        format.to_string()
    }
}

/// The key rule of a policy, for descriptions.
fn key_rule(p: &IdempotencyPolicy, safety: Safety, field: Option<&str>) -> Option<String> {
    let header = p.header.as_deref().unwrap_or("idempotency key");
    let field = field.unwrap_or("idempotency_key");
    match p.policy {
        IdempotencyKind::CallerOwned => {
            let mut rule = format!("caller-owned {header}");
            if let Some(f) = &p.format {
                rule.push(' ');
                rule.push_str(&format_label(f));
            }
            if p.persist_required {
                rule.push_str(" required");
            }
            rule.push_str(&format!(" in {field}; reuse it on every retry"));
            Some(rule)
        }
        IdempotencyKind::Auto => Some(format!("{header} generated unless {field} is given")),
        IdempotencyKind::ContentHash => Some("idempotent by content".into()),
        IdempotencyKind::ContentIdentity => Some("retry only with identical arguments".into()),
        IdempotencyKind::None if safety != Safety::ReadOnly => {
            Some("not idempotent: never retry blindly".into())
        }
        IdempotencyKind::None => None,
    }
}

/// The schema of the idempotency key field.
fn key_field(p: &IdempotencyPolicy) -> Value {
    let header = p.header.as_deref().unwrap_or("Idempotency key");
    let uuid = p
        .format
        .as_deref()
        .is_some_and(|f| format_label(f) == "UUIDv4");
    let description = match p.policy {
        IdempotencyKind::CallerOwned => format!(
            "{header}: generate {} once per intent, persist it and reuse it on every retry.",
            if uuid { "a UUIDv4" } else { "a unique key" }
        ),
        IdempotencyKind::Auto => format!(
            "{header} for this intent; generated when omitted. Reuse it to retry after an unknown outcome."
        ),
        _ => format!("{header}; derived from the arguments when omitted."),
    };
    let mut schema = json!({ "type": "string" });
    if uuid {
        schema["format"] = json!("uuid");
    }
    schema["description"] = json!(description);
    schema
}

fn confirmation_field() -> Value {
    json!({
        "type": "string",
        "description": "Token from preview for exactly these arguments.",
    })
}

/// Add the reserved fields to an input schema; returns their names.
fn add_reserved(schema: &mut Value, policy: &IdempotencyPolicy, safety: Safety) -> Reserved {
    let taken: Vec<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();
    let key = takes_key(policy.policy).then(|| free_name("idempotency_key", &taken));
    let token = needs_confirmation(safety).then(|| free_name("confirmation_token", &taken));
    if let Some(Value::Object(props)) = schema.get_mut("properties") {
        if let Some(k) = &key {
            props.insert(k.clone(), key_field(policy));
        }
        if let Some(t) = &token {
            props.insert(t.clone(), confirmation_field());
        }
    }
    if let (Some(k), true) = (&key, policy.persist_required) {
        let mut required: Vec<Value> = schema
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        required.push(json!(k));
        set_required(schema, required);
    }
    Reserved {
        idempotency_key: key,
        confirmation_token: token,
    }
}

/// `summary` and the bracketed `notes`, within `budget` tokens: the summary
/// is cut at a word boundary (ending in `...`) before any note is dropped,
/// and the tier (the first note) is always kept.
fn fit_description(summary: &str, notes: &[String], budget: usize) -> String {
    let compose = |text: &str, notes: &[String]| {
        let bracket = if notes.is_empty() {
            String::new()
        } else {
            format!("[{}]", notes.join("; "))
        };
        match (text.is_empty(), bracket.is_empty()) {
            (true, _) => bracket,
            (false, true) => text.to_string(),
            (false, false) => format!("{text} {bracket}"),
        }
    };
    let words: Vec<&str> = summary.split_whitespace().collect();
    let mut kept = notes.len();
    loop {
        let notes = &notes[..kept];
        let full = compose(summary, notes);
        if tokens(&full) <= budget {
            return full;
        }
        for n in (1..words.len()).rev() {
            let cut = format!("{}...", words[..n].join(" "));
            let text = compose(&cut, notes);
            if tokens(&text) <= budget {
                return text;
            }
        }
        if kept <= 1 {
            return compose("", notes);
        }
        kept -= 1;
    }
}

/// One line of an operation: the compacted doc, else the summary, else the
/// first sentence of the description, else the id.
fn operation_text(op: &Operation) -> String {
    let compact = collapse_whitespace(&op.agent.compact_doc);
    if !compact.is_empty() {
        return compact;
    }
    let doc = op.doc.as_ref();
    doc.and_then(|d| d.summary.as_deref())
        .map(collapse_whitespace)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.and_then(|d| d.description.as_deref())
                .map(|d| prune_sentences(d, 1))
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| op.id.0.clone())
}

/// The keys of an input schema that are not reserved.
fn arg_names(schema: &Value, reserved: &Reserved) -> Vec<String> {
    let skip = [
        reserved.idempotency_key.as_deref(),
        reserved.confirmation_token.as_deref(),
    ];
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| {
            p.keys()
                .filter(|k| !skip.contains(&Some(k.as_str())))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The description note of a tool whose result holds values the API
/// returns once: the fields by name (at most three), so a client without
/// the output schema (a discrete macro, a progressive search result) knows
/// which value to store.
fn shown_once_note(fields: &[String]) -> String {
    match fields {
        [] => "returns a secret once".into(),
        [one] => format!("returns {one} once"),
        many if many.len() <= 3 => format!("returns {} once", many.join(", ")),
        _ => "returns secrets once".into(),
    }
}

fn schema_tokens(description: &str, input: &Value) -> usize {
    tokens(description) + tokens(&serde_json::to_string(input).unwrap_or_default())
}

/// What a binary body argument is through MCP, prepended to its
/// description: JSON carries it as base64 text, which `@tungsten/mcp`
/// decodes into the bytes the SDK sends (a spec's "never base64-wrapped"
/// is about the HTTP body).
pub const BINARY_BODY_NOTE: &str =
    "Pass the raw bytes base64-encoded; this server decodes them and sends the bytes.";

/// Prefix [`BINARY_BODY_NOTE`] to the description of a binary (`bytes`
/// encoded) body argument.
fn note_binary_body(ir: &Ir, op: &Operation, input: &mut Value) {
    let Some(BodyArg::Arg { key, content }) = args_layout(ir, op).body else {
        return;
    };
    if content.encoding != BodyEncoding::Bytes {
        return;
    }
    let Some(Value::Object(prop)) = input.pointer_mut(&format!(
        "/properties/{}",
        key.replace('~', "~0").replace('/', "~1")
    )) else {
        return;
    };
    let description = match prop.get("description").and_then(Value::as_str) {
        Some(d) if !d.is_empty() => format!("{BINARY_BODY_NOTE} {d}"),
        _ => BINARY_BODY_NOTE.to_string(),
    };
    prop.insert("description".into(), Value::String(description));
}

/// An operation's tool and its index terms. `base` is the name before
/// shortening and numbering, whose words are indexed (a digest suffix is
/// not a search term).
fn operation_tool(
    ir: &Ir,
    op: &Operation,
    base: &str,
    name: String,
    resources: &[String],
    budget: usize,
) -> (ToolEntry, Vec<String>) {
    let a = &op.agent;
    let mut input = operation_parameters(ir, op);
    note_binary_body(ir, op, &mut input);
    let reserved = add_reserved(&mut input, &a.idempotency, a.safety);
    let mut notes = vec![safety_name(a.safety).to_string()];
    if let Some(t) = &reserved.confirmation_token {
        notes.push(format!("preview first, pass {t}"));
    }
    notes.extend(key_rule(
        &a.idempotency,
        a.safety,
        reserved.idempotency_key.as_deref(),
    ));
    if let OperationStatus::Gated { gate } = &op.status {
        notes.push(format!("gated by {}", gate.env_var));
    }
    if a.shown_once {
        notes.push(shown_once_note(&a.sensitive_response_fields));
    }
    if op.deprecated {
        notes.push("deprecated".into());
    }
    let text = operation_text(op);
    let description = fit_description(&prune_sentences(&text, 1), &notes, budget);
    let mut terms = index::tokenize(base);
    terms.extend(index::tokenize(&text));
    for arg in arg_names(&input, &reserved) {
        terms.extend(index::tokenize(&arg));
    }
    for r in resources {
        terms.extend(index::tokenize(r));
    }
    for tag in &op.tags {
        terms.extend(index::tokenize(tag));
    }
    let tool = ToolEntry {
        schema_tokens: schema_tokens(&description, &input),
        name,
        kind: ToolKind::Operation,
        target: op.id.0.clone(),
        description,
        output_schema: output_schema(ir, op),
        input_schema: input,
        annotations: annotations(a.safety, a.idempotency.policy),
        safety: safety_name(a.safety),
        idempotency: idempotency_name(a.idempotency.policy),
        reserved,
        cluster: a.cluster.clone(),
        sensitive_response_fields: a.sensitive_response_fields.clone(),
        shown_once: a.shown_once,
    };
    (tool, terms)
}

/// The idempotency policy a macro's key applies to: that of its first step
/// with a caller-owned or automatic key (the runtime hands the macro's key
/// to that step only).
fn macro_key_policy(ir: &Ir, mac: &Macro) -> IdempotencyPolicy {
    mac.steps
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("operation").and_then(Value::as_str))
        .filter_map(|id| callable_operation(ir, id))
        .map(|op| &op.agent.idempotency)
        .find(|p| {
            matches!(
                p.policy,
                IdempotencyKind::CallerOwned | IdempotencyKind::Auto
            )
        })
        .cloned()
        .unwrap_or_default()
}

/// A macro's tool and its index terms (`base` as for [`operation_tool`]).
fn macro_tool(
    ir: &Ir,
    mac: &Macro,
    base: &str,
    name: String,
    budget: usize,
) -> (ToolEntry, Vec<String>) {
    let policy = macro_key_policy(ir, mac);
    let mut input = macro_parameters(ir, mac);
    let reserved = add_reserved(&mut input, &policy, mac.safety);
    let mut notes = vec![format!("{} macro", safety_name(mac.safety))];
    if let Some(t) = &reserved.confirmation_token {
        notes.push(format!("preview the macro first, pass {t}"));
    }
    if policy.policy != IdempotencyKind::None {
        notes.extend(key_rule(
            &policy,
            mac.safety,
            reserved.idempotency_key.as_deref(),
        ));
    }
    if mac.shown_once {
        notes.push(shown_once_note(&mac.sensitive_response_fields));
    }
    let text = {
        let s = collapse_whitespace(&mac.summary);
        if s.is_empty() { mac.name.0.clone() } else { s }
    };
    let description = fit_description(&prune_sentences(&text, 1), &notes, budget);
    let mut terms = index::tokenize(base);
    terms.extend(index::tokenize(&text));
    for arg in arg_names(&input, &reserved) {
        terms.extend(index::tokenize(&arg));
    }
    let tool = ToolEntry {
        schema_tokens: schema_tokens(&description, &input),
        name,
        kind: ToolKind::Macro,
        target: mac.name.0.clone(),
        description,
        input_schema: input,
        output_schema: None,
        annotations: annotations(mac.safety, policy.policy),
        safety: safety_name(mac.safety),
        idempotency: idempotency_name(policy.policy),
        reserved,
        cluster: mac.cluster.clone(),
        sensitive_response_fields: mac.sensitive_response_fields.clone(),
        shown_once: mac.shown_once,
    };
    (tool, terms)
}

/// Sort key of a success status: exact codes first, by code, then ranges,
/// then `default`.
fn status_rank(s: &StatusMatch) -> (u8, u16) {
    match s {
        StatusMatch::Exact(code) => (0, *code),
        StatusMatch::Range(n) => (1, u16::from(*n)),
        StatusMatch::Default => (2, 0),
    }
}

/// The schema of the first JSON success body when it describes an object.
fn output_schema(ir: &Ir, op: &Operation) -> Option<Value> {
    let mut successes: Vec<&tungsten_ir::Response> = op
        .responses
        .iter()
        .filter(|r| matches!(r.kind, ResponseKind::Success))
        .collect();
    successes.sort_by_key(|r| status_rank(&r.status));
    let content = successes
        .iter()
        .find_map(|r| r.content.iter().find(|c| c.encoding == BodyEncoding::Json))?;
    let mut b = SchemaBuilder::new(ir, tool_schema_options(Usage::Response));
    let root = b.type_ref(&content.ty);
    let schema = hoist_repeats(without_dialect(b.finish(root)));
    (schema.get("type").and_then(Value::as_str) == Some("object")).then_some(schema)
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
