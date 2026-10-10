// SPDX-License-Identifier: AGPL-3.0-only
//! Documentation shown on hover, condensed from the manifest specification.
//!
//! Patterns are dot-separated keys; `[]` is a sequence item and `*` any
//! key. The first matching entry wins, so specific patterns come first.
//! A field without an entry falls back to the description in the
//! published JSON Schema.

use crate::scan::Seg;

/// Which document a path is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `tungsten.yml`
    Manifest,
    /// `agent.yml`
    Agent,
    /// An OpenAPI Overlay 1.0 document.
    Overlay,
}

type Table = &'static [(&'static str, &'static str)];

pub fn field(kind: Kind, path: &[Seg]) -> Option<&'static str> {
    table(kind)
        .iter()
        .find(|(pattern, _)| matches(pattern, path))
        .map(|(_, text)| *text)
}

fn table(kind: Kind) -> Table {
    match kind {
        Kind::Manifest => MANIFEST,
        Kind::Agent => AGENT,
        Kind::Overlay => OVERLAY,
    }
}

fn matches(pattern: &str, path: &[Seg]) -> bool {
    let mut want: Vec<Option<&str>> = vec![];
    for token in pattern.split('.') {
        match token.strip_suffix("[]") {
            Some(name) => {
                want.push(Some(name));
                want.push(None);
            }
            None => want.push(Some(token)),
        }
    }
    want.len() == path.len()
        && want.iter().zip(path).all(|(w, seg)| match (w, seg) {
            (Some("*"), Seg::Key(_)) => true,
            (Some(name), Seg::Key(key)) => name == key,
            (None, Seg::Item(_)) => true,
            _ => false,
        })
}

/// Documentation of one value of an enumerated field, by the field's
/// pattern and the value.
pub fn value(kind: Kind, path: &[Seg], value: &str) -> Option<&'static str> {
    let table = match kind {
        Kind::Agent => AGENT_VALUES,
        Kind::Manifest | Kind::Overlay => return None,
    };
    table
        .iter()
        .find(|(pattern, v, _)| *v == value && matches(pattern, path))
        .map(|(_, _, text)| *text)
}

const MANIFEST: Table = &[
    ("tungsten", "Manifest format version. Must be `1`."),
    (
        "api",
        "Identity of the API: `name`, `title`, `description` and `docs_url`.",
    ),
    (
        "api.name",
        "Machine name of the API, lowercase (`^[a-z][a-z0-9_]*$`). Names the generated packages and the default CLI binary.",
    ),
    (
        "api.title",
        "Human title of the API, used in generated documentation.",
    ),
    (
        "api.docs_url",
        "Link to the API's documentation, shown in generated docs and READMEs.",
    ),
    (
        "inputs",
        "OpenAPI documents to compile. Each input is a namespace: operation ids are prefixed with it (`public.listWebhooks`), and several documents can be compiled into one SDK.",
    ),
    (
        "inputs[].namespace",
        "Namespace of this document's operations and types, unique, lowercase (`^[a-z][a-z0-9_]*$`).",
    ),
    (
        "inputs[].spec",
        "Path of the OpenAPI 3.0 or 3.1 document, relative to this file. Ctrl-click an operation id in `agent.yml` to jump into it.",
    ),
    (
        "inputs[].overlays",
        "OpenAPI Overlay 1.0 files applied to this document, in order, before the manifest-level `overlays`.",
    ),
    (
        "inputs[].include",
        "Keep only operations whose `extension` equals `equals`. An operation that fails it is planned when it matches `planned_from` and dropped otherwise (TG0504 info).",
    ),
    (
        "inputs[].planned_from",
        "Operations whose `extension` equals `equals` are recorded as planned: documented, never callable.",
    ),
    (
        "inputs[].rpc_unflatten",
        "Turns one JSON-RPC style envelope operation into one operation per method. Needs the `path` and `method` to exist and a `oneOf` request body whose members have a `const` under `discriminator` (TG0506 otherwise).",
    ),
    (
        "inputs[].note",
        "Free text for maintainers; not used by the compiler.",
    ),
    (
        "overlays",
        "OpenAPI Overlay 1.0 files applied to every input, after the per-input overlays.",
    ),
    (
        "servers",
        "Server URL handling: `default` and `allow_override`.",
    ),
    (
        "servers.default",
        "Base URL the generated clients use unless it is overridden. Falls back to the first document's `servers`.",
    ),
    (
        "resources",
        "Overrides of the resource tree, which is otherwise inferred from paths: namespace, then resource name, with a `path` (and optional `children`). A `path` that matches no path of the input is TG0604.",
    ),
    (
        "resources.*",
        "Resources of this namespace by name. The configured resource whose `path` is the longest segment prefix of an operation's path wins.",
    ),
    ("naming", "Names of generated methods and fields."),
    (
        "naming.operations",
        "Operation id → method name. A key that names no operation is TG0603.",
    ),
    (
        "naming.fields",
        "Field naming options, such as `preserve_wire_names_in_ts`.",
    ),
    (
        "auth_profiles",
        "Authentication beyond what the spec declares: composite schemes (cookie plus CSRF header, `Origin`), bearer prefixes and environment variables.",
    ),
    (
        "auth_profiles.*",
        "One profile, named after the scheme it configures or a new composite scheme. Use `composite` with `satisfies`, or `bearer`.",
    ),
    (
        "auth_profiles.*.composite",
        "Parts a request must carry together: `cookie`, `header` (with `equals_cookie`, `from_config`, `when: mutation`) or `bearer`.",
    ),
    (
        "auth_profiles.*.satisfies",
        "Names of the spec's security requirements this composite scheme satisfies. A name that matches no scheme is TG0503.",
    ),
    (
        "auth_profiles.*.bearer",
        "Bearer credential: `env` names the environment variable, `prefix` is enforced by the runtime before sending.",
    ),
    (
        "pagination",
        "Pagination overrides by operation id; otherwise pagination is inferred (TG0501). One style: `cursor`, `offset`, `page`, `link_header`, or `none: true` to switch inference off.",
    ),
    (
        "pagination.*",
        "Pagination of this operation. A key that names no operation is TG0603.",
    ),
    ("types", "Options for the generated types."),
    (
        "types.prune_unreferenced",
        "Default `true`: drop the types no operation reaches and note them once (TG0760). `false` generates every schema.",
    ),
    (
        "types.break_cycles",
        "`Type.field` edges to box explicitly when a cycle has no named type.",
    ),
    (
        "targets",
        "What to generate. Known targets: `typescript`, `python`, `rust`, `mcp`, `docs`, `mock`. Each needs its own `out` directory; directories must be distinct and not nested (TG0612).",
    ),
    (
        "targets.*",
        "Options of this target. `out` is the output directory; package names depend on the target (`package` or `crate`).",
    ),
    (
        "targets.*.out",
        "Output directory, relative to this file. `generate` writes `.tungsten/manifest.json` there and removes files it no longer generates.",
    ),
    (
        "targets.*.package",
        "Package name of the generated SDK or server.",
    ),
    ("targets.*.crate", "Crate name of the generated Rust SDK."),
    (
        "targets.*.cli",
        "Generate a command-line tool from the Rust SDK: `bin` names the binary.",
    ),
    (
        "targets.*.models",
        "Python model flavour, such as `pydantic`.",
    ),
    (
        "targets.*.runtime",
        "Version requirement of the runtime library the generated code depends on.",
    ),
    (
        "agent",
        "Path of the agent manifest, relative to this file. When absent, `agent.yml` next to this file is used if it exists.",
    ),
];

const AGENT: Table = &[
    ("agent", "Manifest format version. Must be `1`."),
    (
        "defaults",
        "Defaults for every operation. Precedence: `tools` > the spec's `x-agent-*` extensions > inferred values > these defaults > the method defaults.",
    ),
    (
        "defaults.safety",
        "Safety tier by HTTP method. Built-in: safe methods `read_only`, `delete` `destructive`, the rest `mutating`.",
    ),
    (
        "defaults.safety.*",
        "Safety tier of this HTTP method: `read_only`, `mutating`, `destructive` or `irreversible`.",
    ),
    (
        "defaults.idempotency",
        "Policy of operations with side effects that declare none. A required `Idempotency-Key` header still infers `caller_owned`.",
    ),
    (
        "defaults.preview",
        "How `preview` works for operations with side effects: `local`, `header`, `endpoint` or `none`.",
    ),
    (
        "defaults.unknown_outcome",
        "What the runtime reports when a mutation's outcome is unknown. Never `retry`: a timed-out mutation may have happened.",
    ),
    (
        "defaults.retries",
        "Automatic retries by safety tier, plus `honor_retry_after`.",
    ),
    (
        "defaults.retries.read_only",
        "Retry policy of read-only operations: `max` attempts and a `backoff` (`base_ms`, `max_ms`, `jitter`).",
    ),
    (
        "defaults.retries.mutating",
        "Retry policy of mutating operations. `max: 0` means retries happen only with an idempotency key.",
    ),
    (
        "defaults.retries.honor_retry_after",
        "Wait for the server's `Retry-After` before retrying.",
    ),
    (
        "defaults.disclosure",
        "How MCP tools are disclosed: `mode` (`auto`, `discrete`, `progressive`) and token budgets.",
    ),
    (
        "defaults.disclosure.mode",
        "`discrete` lists one MCP tool per operation, `progressive` a search tool plus an execute tool; `auto` is progressive when the discrete tool list exceeds `list_budget_tokens`.",
    ),
    (
        "tools",
        "Per-operation rules, at most one entry per operation. Every key except `operation` overrides the defaults and the spec's `x-agent-*` extensions.",
    ),
    (
        "tools[].operation",
        "The operation this entry configures: its id (`public.submitAlphaMessage`) or `<namespace>.<resource path>.<method>`. Ctrl-click to open it in the OpenAPI document.",
    ),
    (
        "tools[].safety",
        "Safety tier: `read_only` (no side effects), `mutating` (reversible or idempotent), `destructive` (needs confirmation) or `irreversible` (needs a preview token).",
    ),
    (
        "tools[].idempotency",
        "Idempotency policy: `none`, `auto`, `caller_owned`, `content_hash` or `content_identity`, or an object with `policy`, `header`, `format`, `persist` and `note`.",
    ),
    (
        "tools[].idempotency.policy",
        "Who generates the key: `none` nobody; `auto` the runtime; `caller_owned` the caller; `content_hash` a hash of the body; `content_identity` the body itself.",
    ),
    (
        "tools[].idempotency.header",
        "Header carrying the key. Default: the operation's idempotency-key header, else `Idempotency-Key`.",
    ),
    (
        "tools[].idempotency.format",
        "Required key format for `caller_owned`: `uuid_v4`, `uuid` or `opaque`.",
    ),
    (
        "tools[].idempotency.persist",
        "`required` (with `caller_owned`) makes the runtime refuse a call without a key, so the key survives the agent's own crash.",
    ),
    (
        "tools[].idempotency.note",
        "Explanation shown in generated docs.",
    ),
    (
        "tools[].preview",
        "Preview mode for this operation: `local`, `header`, `endpoint` or `none`.",
    ),
    (
        "tools[].confirmation",
        "What a destructive or irreversible call shows before it runs: `summary_fields` of the arguments and a `message` with `{field}` placeholders.",
    ),
    (
        "tools[].confirmation.summary_fields",
        "Argument names shown in the confirmation summary.",
    ),
    (
        "tools[].confirmation.message",
        "Confirmation text; `{name}` is replaced by the argument's value.",
    ),
    (
        "tools[].verify",
        "A hook that checks the outcome of an uncertain mutation: `operation`, `args`, `expect`, `terminal` and `poll`.",
    ),
    (
        "tools[].verify.operation",
        "The read operation that reports the outcome.",
    ),
    (
        "tools[].verify.args",
        "Arguments of the verification call; `$response.field` refers to the original response.",
    ),
    (
        "tools[].verify.expect",
        "States that mean the mutation took effect.",
    ),
    (
        "tools[].verify.terminal",
        "States after which polling stops.",
    ),
    (
        "tools[].verify.poll",
        "Polling: `interval_ms` and `budget_ms`.",
    ),
    (
        "tools[].remediation",
        "API error code → what the agent should do for this operation. Looked up before the global `errors.codes`.",
    ),
    (
        "tools[].remediation.*",
        "Remediation for this error code: `text` for the agent and `retryable`.",
    ),
    (
        "tools[].remediation.*.text",
        "Instruction shown to the agent when this error occurs.",
    ),
    (
        "tools[].remediation.*.retryable",
        "`never`, `after_delay`, `same_key_only` (retry with the SAME idempotency key) or `after_remediation`.",
    ),
    (
        "tools[].remediation_note",
        "Side-effect note shown in previews.",
    ),
    (
        "tools[].response",
        "Response handling: `sensitive_fields` are redacted from logs and `shown_once` marks a secret that cannot be fetched again.",
    ),
    (
        "tools[].response.sensitive_fields",
        "Response fields that carry secrets.",
    ),
    (
        "tools[].response.shown_once",
        "The response is the only time the secret is available.",
    ),
    (
        "tools[].gate",
        "A runtime gate: an `x-runtime-gate` environment variable or a `gates` entry.",
    ),
    (
        "tools[].cluster",
        "Cluster name; overrides `disclosure.clusters` membership.",
    ),
    (
        "tools[].hidden",
        "Exclude from agent surfaces (MCP, tool lists); SDKs keep it.",
    ),
    (
        "errors",
        "API-wide error handling: envelope, ambiguous statuses, non-JSON errors and per-code categories.",
    ),
    (
        "errors.envelope",
        "The error body: `schema` names its type and `code_field` the field holding the machine code.",
    ),
    (
        "errors.ambiguous_statuses",
        "Statuses that make a mutation's outcome unknown (`OUTCOME_UNKNOWN`), usually `[408, 503]`.",
    ),
    (
        "errors.non_json",
        "Errors that are not JSON, matched by `status` and `media`, mapped to a `category` and `retryable`.",
    ),
    (
        "errors.codes",
        "Error code → `category`, `retryable` and `text`, used when the operation has no entry of its own.",
    ),
    (
        "errors.codes.*",
        "Handling of this error code: `category`, `retryable`, `text`.",
    ),
    (
        "errors.codes.*.category",
        "Category an agent can branch on, such as `AUTH_FAILED`, `RATE_LIMITED`, `NOT_FOUND`, `CONFLICT`, `VALIDATION_FAILED`.",
    ),
    (
        "errors.codes.*.retryable",
        "`never`, `after_delay`, `same_key_only` or `after_remediation`.",
    ),
    (
        "gates",
        "Deployment gates (environment variables): what agents are told when a gated operation is switched off. A gate no operation uses is reported.",
    ),
    (
        "gates.*",
        "The gate's `text` and optionally `disabled_status`, the status returned when it is off.",
    ),
    (
        "gates.*.text",
        "Explanation shown to agents: a deployment setting, not a missing resource.",
    ),
    (
        "macros",
        "Multi-step operations compiled into one tool: `call`, `poll` and `paginate` steps with `$input` and `$name.field` references.",
    ),
    ("macros[].name", "Macro id, `<namespace>.<name>`."),
    (
        "macros[].summary",
        "One-line description shown in tool lists.",
    ),
    (
        "macros[].safety",
        "Declared tier. The macro's tier is the strictest step's; a weaker declaration is raised (TG0609).",
    ),
    (
        "macros[].steps",
        "Steps run in order. `call` runs an operation, `poll` repeats one until a condition holds or its budget runs out, `paginate` collects pages.",
    ),
    (
        "macros[].steps[].call",
        "Operation to call. Ctrl-click to open it.",
    ),
    (
        "macros[].steps[].poll",
        "Operation to poll until `until` holds.",
    ),
    (
        "macros[].steps[].paginate",
        "Paginated operation to read page by page, up to `max_pages`.",
    ),
    (
        "macros[].steps[].args",
        "Arguments of the step; `$input` is the macro input and `$name.field` an earlier step's result.",
    ),
    ("macros[].steps[].as", "Name the step's result is bound to."),
    ("macros[].steps[].until", "Condition that ends a `poll`."),
    (
        "macros[].steps[].interval_ms",
        "Milliseconds between polls.",
    ),
    (
        "macros[].steps[].budget_ms",
        "Total milliseconds a `poll` may take; when it runs out `as` is bound to null.",
    ),
    (
        "macros[].steps[].max_pages",
        "Page limit of a `paginate` step.",
    ),
    (
        "macros[].output",
        "The macro's result, built from `$input` and step results.",
    ),
    (
        "macros[].response",
        "Response handling of the macro, as in `tools[].response`.",
    ),
    (
        "macros[].input",
        "Input of the macro: `extends` an operation's arguments and `add` fields.",
    ),
    (
        "disclosure",
        "How agents find tools: semantic `clusters` and description `prune` rules.",
    ),
    (
        "disclosure.clusters",
        "Semantic groups for `search_tools`. Each operation and macro belongs to at most one cluster.",
    ),
    (
        "disclosure.clusters[].name",
        "Cluster name, lowercase (`^[a-z][a-z0-9_]*$`).",
    ),
    (
        "disclosure.clusters[].operations",
        "Operation references, macro names, `<namespace>.*` and `<namespace>.<resource path>.*` globs the cluster covers.",
    ),
    (
        "disclosure.prune",
        "Shortens descriptions in agent tool schemas.",
    ),
    (
        "disclosure.prune.descriptions",
        "`max_sentences` to keep and `drop_phrases` whose sentences are removed.",
    ),
    (
        "disclosure.prune.drop_fields",
        "Accepted but not applied yet: a non-empty list is TG0613.",
    ),
    (
        "disclosure.prune.keep_examples",
        "Accepted but not applied yet: `true` is TG0613.",
    ),
];

const AGENT_VALUES: &[(&str, &str, &str)] = &[
    (
        "tools[].safety",
        "read_only",
        "No side effects. SDKs may retry; MCP sets `readOnlyHint`.",
    ),
    (
        "tools[].safety",
        "mutating",
        "Side effect, reversible or idempotent. No blind retry; a preview is available.",
    ),
    (
        "tools[].safety",
        "destructive",
        "Deletes or disables something; recoverable only by re-creation. Needs `confirm: true` (SDK), `--yes` (CLI) or a `confirmation_token` from `preview` (MCP).",
    ),
    (
        "tools[].safety",
        "irreversible",
        "Cannot be undone (money, messages to people, permanent deletion). Needs a confirmation token obtained from `preview` in the same session.",
    ),
    (
        "tools[].idempotency.policy",
        "none",
        "Nobody generates a key; a mutation is never retried automatically.",
    ),
    (
        "tools[].idempotency.policy",
        "auto",
        "The runtime generates a UUIDv4 per logical call and retries with the same key.",
    ),
    (
        "tools[].idempotency.policy",
        "caller_owned",
        "The caller supplies and persists the key; retried with the same key only.",
    ),
    (
        "tools[].idempotency.policy",
        "content_hash",
        "The runtime hashes the canonical body into the key.",
    ),
    (
        "tools[].idempotency.policy",
        "content_identity",
        "The body itself is the identity (binary envelopes): resend identical bytes only.",
    ),
    (
        "tools[].preview",
        "local",
        "Validate arguments, resolve auth and render the request without any network call.",
    ),
    (
        "tools[].preview",
        "header",
        "Render locally, then send with a dry-run header and return the server's answer.",
    ),
    (
        "tools[].preview",
        "endpoint",
        "Call a separate preview operation with mapped arguments.",
    ),
    ("tools[].preview", "none", "`.preview()` is not generated."),
    ("tools[].remediation.*.retryable", "never", "Do not retry."),
    (
        "tools[].remediation.*.retryable",
        "after_remediation",
        "Retry once the remediation was carried out.",
    ),
    (
        "tools[].remediation.*.retryable",
        "after_delay",
        "Retry after waiting.",
    ),
    (
        "tools[].remediation.*.retryable",
        "same_key_only",
        "Retry only with the SAME idempotency key.",
    ),
    ("errors.codes.*.retryable", "never", "Do not retry."),
    (
        "errors.codes.*.retryable",
        "after_remediation",
        "Retry once the remediation was carried out.",
    ),
    (
        "errors.codes.*.retryable",
        "after_delay",
        "Retry after waiting.",
    ),
    (
        "errors.codes.*.retryable",
        "same_key_only",
        "Retry only with the SAME idempotency key.",
    ),
    (
        "defaults.disclosure.mode",
        "auto",
        "Progressive when the discrete tool list exceeds `list_budget_tokens`, discrete otherwise.",
    ),
    (
        "defaults.disclosure.mode",
        "discrete",
        "One MCP tool per operation.",
    ),
    (
        "defaults.disclosure.mode",
        "progressive",
        "A search tool plus an execute tool, with schemas loaded on demand.",
    ),
];

const OVERLAY: Table = &[
    ("overlay", "Overlay specification version, `1.0.0`."),
    ("info", "Metadata of this overlay: `title` and `version`."),
    (
        "extends",
        "The document this overlay is meant for. tungsten ignores it; the manifest decides what is overlaid.",
    ),
    ("actions", "Edits applied in order to the OpenAPI document."),
    (
        "actions[].target",
        "JSONPath selecting the nodes to change, such as `$.paths['/v1/pets'].get`.",
    ),
    ("actions[].description", "Why the action exists."),
    (
        "actions[].update",
        "Object merged into every selected node: mappings merge recursively, arrays append.",
    ),
    ("actions[].remove", "`true` removes every selected node."),
];
