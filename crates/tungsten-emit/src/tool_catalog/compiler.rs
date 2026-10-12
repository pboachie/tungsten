// SPDX-License-Identifier: AGPL-3.0-only
//! Shared tool metadata compiler, independent of transport search and registration.
#[path = "names.rs"]
mod names;
pub use names::{MAX_TOOL_NAME, sanitize, shorten};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::args::{BodyArg, args_layout};
use crate::compact::{
    callable_operation, hoist_repeats, macro_parameters, operation_parameters, set_required,
    tool_schema_options, without_dialect,
};
use crate::schema::{SchemaBuilder, Usage, collapse_whitespace, prune_sentences};
use serde::Serialize;
use serde_json::{Value, json};
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_ir::{
    BodyEncoding, IdempotencyKind, IdempotencyPolicy, Ir, Macro, Operation, OperationStatus,
    ResponseKind, Safety, StatusMatch,
};
use tungsten_tokens::{Counter, count};

use names::macro_base;
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

/// Words never indexed or searched (`STOP_WORDS` in the runtime contract).
pub const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "the", "this", "to", "with",
];

/// The terms of `text` under the shared rule.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut words: Vec<String> = vec![];
    let mut current = String::new();
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            let boundary = c.is_ascii_uppercase()
                && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit());
            if boundary {
                words.push(std::mem::take(&mut current));
            }
            current.push(c.to_ascii_lowercase());
        } else {
            words.push(std::mem::take(&mut current));
        }
        prev = Some(c);
    }
    words.push(current);
    words
        .into_iter()
        .filter(|w| w.len() >= 2 && !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// Compile common tool metadata. Exclusions belong to the selected SDK adapter.
pub fn compile_entries(
    ir: &Ir,
    skipped: &BTreeSet<String>,
) -> (Vec<ToolEntry>, Vec<Vec<String>>, Diagnostics) {
    let mut diags = Diagnostics::new();
    let budget = ir.agent.disclosure.description_budget_tokens as usize;
    let output_budget = output_budget(ir);
    let mut tools = vec![];
    let mut docs: Vec<Vec<String>> = vec![];
    let ops = callable(ir);
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
                    "{} would all be the tool `{shared}`; each is named with a digest of its id instead",
                    keys.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
                ),
            )
            .with_help("Give them distinct method names with `naming.operations` in tungsten.yml, so their tool names say what they do."),
        );
    }
    let mut names = names.into_iter();
    for (base, (_, resources, op)) in op_bases.iter().zip(&ops) {
        let name = names.next().unwrap_or_default();
        let (tool, terms) = operation_tool(ir, op, base, name, resources, budget, output_budget);
        tools.push(tool);
        docs.push(terms);
    }
    for mac in &ir.agent.macros {
        if skipped.contains(&mac.name.0) {
            diags.push(
                Diagnostic::warning(
                    "TG0723",
                    format!(
                        "macro `{}` is not a callable tool: the selected SDK cannot emit it",
                        mac.name.0
                    ),
                )
                .with_help("Fix the macro in agent.yml using the canonical macro form."),
            );
            continue;
        }
        let base = macro_base(&mac.name.0);
        let name = names.next().unwrap_or_default();
        let (tool, terms) = macro_tool(ir, mac, &base, name, budget);
        tools.push(tool);
        docs.push(terms);
    }
    (tools, docs, diags)
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

pub fn safety_name(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "read_only",
        Safety::Mutating => "mutating",
        Safety::Destructive => "destructive",
        Safety::Irreversible => "irreversible",
    }
}

pub fn idempotency_name(k: IdempotencyKind) -> &'static str {
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
    if p.format.as_deref() == Some("ascii_token_128") {
        schema["minLength"] = json!(1);
        schema["maxLength"] = json!(128);
        schema["pattern"] = json!("^[A-Za-z0-9._-]+$");
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
/// description: JSON carries it as base64 text, which `@elysra/tungsten-mcp`
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
    output_budget: usize,
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
    let mut terms = tokenize(base);
    terms.extend(tokenize(&text));
    for arg in arg_names(&input, &reserved) {
        terms.extend(tokenize(&arg));
    }
    for r in resources {
        terms.extend(tokenize(r));
    }
    for tag in &op.tags {
        terms.extend(tokenize(tag));
    }
    let tool = ToolEntry {
        schema_tokens: schema_tokens(&description, &input),
        name,
        kind: ToolKind::Operation,
        target: op.id.0.clone(),
        description,
        output_schema: output_schema(ir, op, output_budget),
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
    let mut terms = tokenize(base);
    terms.extend(tokenize(&text));
    for arg in arg_names(&input, &reserved) {
        terms.extend(tokenize(&arg));
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

/// How many reference hops from a tool's output body keep their definition.
/// Measured on Stripe (every `expandable` field is `anyOf [string, Resource]`,
/// so the transitive closure of one response is about 860 definitions and
/// 1 MB): one hop keeps the body's own nested types, and a typical tool
/// stays in the low thousands of tokens; two hops already pulls in most of
/// the cyclic graph.
pub const OUTPUT_HOPS: usize = 1;

/// A tool's output schema may be this many times the per-tool schema budget
/// before it falls back to the zero-hop form.
pub const OUTPUT_BUDGET_FACTOR: usize = 4;

/// Token budget of one tool's output schema.
fn output_budget(ir: &Ir) -> usize {
    OUTPUT_BUDGET_FACTOR * ir.agent.disclosure.schema_budget_tokens as usize
}

/// The `$defs` entry a `$ref` points to, when it is a local definition.
fn def_name(map: &serde_json::Map<String, Value>) -> Option<&str> {
    map.get("$ref")?.as_str()?.strip_prefix("#/$defs/")
}

/// Hoisted scalar definitions (`hoist_repeats`): leaves that never count as
/// a hop.
fn is_shared(name: &str) -> bool {
    name.starts_with("shared_")
}

/// Replace every reference in `value` (a schema at reference depth `depth`)
/// that is more than `hops` hops from the body by `{}`, and queue the
/// definitions it keeps.
fn bound_refs(
    value: &mut Value,
    depth: usize,
    hops: usize,
    seen: &mut BTreeMap<String, usize>,
    queue: &mut VecDeque<(String, usize)>,
) {
    match value {
        Value::Object(map) => {
            if let Some(name) = def_name(map).map(str::to_string) {
                let at = if is_shared(&name) { depth } else { depth + 1 };
                if at > hops {
                    map.clear();
                } else if !seen.contains_key(&name) {
                    seen.insert(name.clone(), at);
                    queue.push_back((name, at));
                }
                return;
            }
            for child in map.values_mut() {
                bound_refs(child, depth, hops, seen, queue);
            }
        }
        Value::Array(list) => {
            for child in list {
                bound_refs(child, depth, hops, seen, queue);
            }
        }
        _ => {}
    }
}

/// `schema` with every `$ref` more than `hops` reference hops from its body
/// replaced by `{}` (any value, so the real structured content still
/// validates) and the `$defs` that are no longer reachable dropped. Hops
/// are counted from the body (its direct references are hop 1); hoisted
/// shared scalars do not count. Definitions keep their order, so the result
/// is deterministic.
pub fn bound_closure(schema: Value, hops: usize) -> Value {
    let Value::Object(mut root) = schema else {
        return schema;
    };
    let Some(Value::Object(mut defs)) = root.shift_remove("$defs") else {
        return Value::Object(root);
    };
    let mut seen = BTreeMap::new();
    let mut queue = VecDeque::new();
    let mut body = Value::Object(root);
    bound_refs(&mut body, 0, hops, &mut seen, &mut queue);
    while let Some((name, depth)) = queue.pop_front() {
        if let Some(def) = defs.get_mut(&name) {
            bound_refs(def, depth, hops, &mut seen, &mut queue);
        }
    }
    defs.retain(|name, _| seen.contains_key(name));
    if let Value::Object(root) = &mut body
        && !defs.is_empty()
    {
        root.insert("$defs".into(), Value::Object(defs));
    }
    body
}

/// The output schema of a response type bounded to [`OUTPUT_HOPS`]
/// reference hops, or to none when that is still over `budget` tokens.
fn bounded_output(schema: Value, budget: usize) -> Value {
    let bounded = bound_closure(schema.clone(), OUTPUT_HOPS);
    if tokens(&serde_json::to_string(&bounded).unwrap_or_default()) <= budget {
        bounded
    } else {
        bound_closure(schema, 0)
    }
}

/// The schema of the first JSON success body when it describes an object.
/// None for an operation that only streams: a call returns the collected
/// events (`{events: [...]}`), which a plain body's schema does not describe.
/// An operation that also has a plain body keeps its schema.
///
/// The closure of the body's types is bounded ([`bound_closure`]): a
/// reference more than [`OUTPUT_HOPS`] hops from the body is `{}`, and when
/// the result is still over `budget` tokens every reference is. Response
/// graphs of large APIs are cyclic and densely connected, so the full
/// closure of one tool can be the whole API.
fn output_schema(ir: &Ir, op: &Operation, budget: usize) -> Option<Value> {
    if op.stream.as_ref().is_some_and(|s| !s.also_plain) {
        return None;
    }
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
    (schema.get("type").and_then(Value::as_str) == Some("object"))
        .then(|| bounded_output(schema, budget))
}

/// Stable tool name assignment with collision information.
pub fn assign_names(entries: &[(String, String)]) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    names::assign(entries)
}
