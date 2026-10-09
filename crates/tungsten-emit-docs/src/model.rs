// SPDX-License-Identifier: AGPL-3.0-only
//! What the docs describe: callable operations with their resource path
//! and tool name, planned operations, and the text helpers shared by every
//! file.

use std::collections::BTreeSet;

use tungsten_core::Digest;
use tungsten_emit::schema::{collapse_whitespace, prune_sentences};
use tungsten_ir::{
    AuthScheme, CompositePart, HttpMethod, IdempotencyKind, Ir, Macro, Namespace, Operation,
    OperationStatus, Resource, Safety,
};

/// Longest tool name accepted by common function-calling APIs.
pub(crate) const MAX_TOOL_NAME: usize = 64;

/// A callable operation in document order.
#[derive(Debug)]
pub(crate) struct CallableOp<'a> {
    pub op: &'a Operation,
    /// Resource names from the namespace root (`["webhooks", "deliveries"]`).
    pub resources: Vec<String>,
    /// Unique tool name (`public_webhooks_deliveries_replay`).
    pub tool: String,
}

impl CallableOp<'_> {
    /// The SDK path: `public.webhooks.deliveries.replay`.
    pub fn sdk_path(&self, ns: &str) -> String {
        let mut parts = vec![ns.to_string()];
        parts.extend(self.resources.iter().cloned());
        parts.push(self.op.name.snake());
        parts.join(".")
    }
}

/// One namespace's operations.
#[derive(Debug)]
pub(crate) struct NamespaceDocs<'a> {
    pub ns: &'a Namespace,
    pub callable: Vec<CallableOp<'a>>,
    /// Planned operations (never callable), in document order.
    pub planned: Vec<&'a Operation>,
}

/// A macro exposed as a tool.
#[derive(Debug)]
pub(crate) struct MacroDocs<'a> {
    pub mac: &'a Macro,
    pub tool: String,
}

/// Everything the docs describe, with tool names assigned.
#[derive(Debug)]
pub(crate) struct Model<'a> {
    pub ir: &'a Ir,
    pub namespaces: Vec<NamespaceDocs<'a>>,
    pub macros: Vec<MacroDocs<'a>>,
}

impl<'a> Model<'a> {
    pub fn new(ir: &'a Ir) -> Self {
        let mut names = ToolNames::default();
        let mut namespaces = vec![];
        for ns in &ir.namespaces {
            let mut callable = vec![];
            let mut planned: Vec<&Operation> = vec![];
            for r in &ns.resources {
                walk(r, &mut vec![], &mut callable, &mut planned);
            }
            planned.extend(ns.planned.iter());
            let callable = callable
                .into_iter()
                .map(|(op, resources)| {
                    let mut words = vec![ns.name.snake()];
                    words.extend(resources.iter().cloned());
                    words.push(op.name.snake());
                    CallableOp {
                        tool: names.assign(&words.join("_")),
                        op,
                        resources,
                    }
                })
                .collect();
            namespaces.push(NamespaceDocs {
                ns,
                callable,
                planned,
            });
        }
        let macros = ir
            .agent
            .macros
            .iter()
            .map(|mac| MacroDocs {
                tool: names.assign(&macro_tool_base(&mac.name.0)),
                mac,
            })
            .collect();
        Model {
            ir,
            namespaces,
            macros,
        }
    }

    /// Callable operations of every namespace, in order.
    pub fn callable(&self) -> impl Iterator<Item = (&NamespaceDocs<'a>, &CallableOp<'a>)> {
        self.namespaces
            .iter()
            .flat_map(|n| n.callable.iter().map(move |c| (n, c)))
    }

    pub fn callable_count(&self) -> usize {
        self.namespaces.iter().map(|n| n.callable.len()).sum()
    }

    pub fn planned_count(&self) -> usize {
        self.namespaces.iter().map(|n| n.planned.len()).sum()
    }

    /// A callable operation by id.
    pub fn find(&self, id: &str) -> Option<(&NamespaceDocs<'a>, &CallableOp<'a>)> {
        self.callable().find(|(_, c)| c.op.id.0 == id)
    }
}

fn walk<'a>(
    r: &'a Resource,
    path: &mut Vec<String>,
    callable: &mut Vec<(&'a Operation, Vec<String>)>,
    planned: &mut Vec<&'a Operation>,
) {
    path.push(r.name.snake());
    for op in &r.operations {
        match op.status {
            OperationStatus::Planned { .. } => planned.push(op),
            _ => callable.push((op, path.clone())),
        }
    }
    for c in &r.children {
        walk(c, path, callable, planned);
    }
    path.pop();
}

/// `public.submitAlphaMessageAndAwait` → `public_submit_alpha_message_and_await`.
fn macro_tool_base(name: &str) -> String {
    name.split('.')
        .map(|part| tungsten_ir::Ident::new(part).snake())
        .collect::<Vec<_>>()
        .join("_")
}

/// Assigns unique tool names of at most [`MAX_TOOL_NAME`] characters.
#[derive(Debug, Default)]
struct ToolNames {
    used: BTreeSet<String>,
}

impl ToolNames {
    /// `base` when free and short enough; a long name keeps its start and
    /// ends with eight hex digits of its digest; a taken name gets the
    /// smallest free numeric suffix `_2`, `_3`, ...
    fn assign(&mut self, base: &str) -> String {
        let base = shorten(base, MAX_TOOL_NAME);
        let name = if self.used.contains(&base) {
            (2..)
                .map(|n| {
                    let suffix = format!("_{n}");
                    let mut stem = base.clone();
                    stem.truncate(MAX_TOOL_NAME - suffix.len());
                    format!("{stem}{suffix}")
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

fn shorten(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let digest = Digest::of(name.as_bytes());
    let hash = &digest.short()[..8];
    let keep = max - hash.len() - 1;
    let stem = name[..keep].trim_end_matches('_');
    format!("{stem}_{hash}")
}

// ── text helpers ───────────────────────────────────────────────────────────

/// One-line summary of an operation: its summary, else the first sentence
/// of its description, else its id.
pub(crate) fn summary(op: &Operation) -> String {
    let doc = op.doc.as_ref();
    let text = doc
        .and_then(|d| d.summary.as_deref())
        .map(collapse_whitespace)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.and_then(|d| d.description.as_deref())
                .map(|d| prune_sentences(d, 1))
                .filter(|s| !s.is_empty())
        });
    text.unwrap_or_else(|| op.id.0.clone())
}

pub(crate) fn method(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Put => "PUT",
        HttpMethod::Post => "POST",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Head => "HEAD",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Trace => "TRACE",
    }
}

/// `POST /v1/workflow/tools (method workflow.action.send)` for rpc methods,
/// `GET /v1/pets` otherwise.
pub(crate) fn endpoint(op: &Operation) -> String {
    let base = format!("{} {}", method(op.method), op.path.raw);
    match &op.rpc {
        Some(rpc) => format!(
            "{base} ({} {})",
            rpc.discriminator_field, rpc.discriminator_value
        ),
        None => base,
    }
}

pub(crate) fn safety(s: Safety) -> &'static str {
    match s {
        Safety::ReadOnly => "read_only",
        Safety::Mutating => "mutating",
        Safety::Destructive => "destructive",
        Safety::Irreversible => "irreversible",
    }
}

pub(crate) fn idempotency_kind(k: IdempotencyKind) -> &'static str {
    match k {
        IdempotencyKind::None => "none",
        IdempotencyKind::Auto => "auto",
        IdempotencyKind::CallerOwned => "caller_owned",
        IdempotencyKind::ContentHash => "content_hash",
        IdempotencyKind::ContentIdentity => "content_identity",
    }
}

/// `idempotency: caller_owned, header Idempotency-Key, uuid_v4, key required`.
pub(crate) fn idempotency(op: &Operation) -> String {
    let p = &op.agent.idempotency;
    let mut parts = vec![idempotency_kind(p.policy).to_string()];
    if let Some(h) = &p.header {
        parts.push(format!("header {h}"));
    }
    if let Some(f) = &p.format {
        parts.push(f.clone());
    }
    if p.persist_required {
        parts.push("key required".into());
    }
    format!("idempotency: {}", parts.join(", "))
}

/// `[gated: ENV, off by default, 404 when off]` for gated operations.
pub(crate) fn gate(op: &Operation) -> Option<String> {
    match &op.status {
        OperationStatus::Gated { gate } => Some(format!(
            "gated: {}, {} by default, {} when off",
            gate.env_var,
            if gate.default_on { "on" } else { "off" },
            gate.disabled_status
        )),
        _ => None,
    }
}

/// The bracketed markers of an llms line: tier, idempotency, gate,
/// deprecation.
pub(crate) fn markers(op: &Operation) -> String {
    let mut out = format!("[{}] [{}]", safety(op.agent.safety), idempotency(op));
    if let Some(g) = gate(op) {
        out.push_str(&format!(" [{g}]"));
    }
    if op.deprecated {
        out.push_str(" [deprecated]");
    }
    out
}

/// Why a planned operation is not callable.
pub(crate) fn planned_reason(op: &Operation) -> String {
    match &op.status {
        OperationStatus::Planned { reason } => collapse_whitespace(reason),
        _ => "planned".into(),
    }
}

/// `a + b | c`: OR of AND-sets of scheme names; `none` when empty.
pub(crate) fn security(op: &Operation) -> String {
    if op.security.is_empty() {
        return "none".into();
    }
    op.security
        .iter()
        .map(|req| {
            if req.all_of.is_empty() {
                "none".to_string()
            } else {
                req.all_of
                    .iter()
                    .map(|u| u.scheme.clone())
                    .collect::<Vec<_>>()
                    .join(" + ")
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The two auth lines: the schemes, then the composite profiles.
pub(crate) fn auth_lines(ir: &Ir) -> [String; 2] {
    let schemes: Vec<String> = ir
        .auth
        .iter()
        .filter(|s| !matches!(s, AuthScheme::Composite { .. }))
        .map(scheme)
        .collect();
    let profiles: Vec<String> = ir
        .auth
        .iter()
        .filter_map(|s| match s {
            AuthScheme::Composite {
                name,
                satisfies,
                parts,
            } => Some(format!(
                "{name} = {}; satisfies {}",
                parts.iter().map(part).collect::<Vec<_>>().join(", "),
                satisfies.join(" + ")
            )),
            _ => None,
        })
        .collect();
    let first = if schemes.is_empty() {
        "Auth: none declared.".to_string()
    } else {
        format!("Auth: {}.", schemes.join(" · "))
    };
    let second = if profiles.is_empty() {
        "Auth profiles: none; each operation names the schemes it needs (`a + b` all of, `a | b` any of).".to_string()
    } else {
        format!("Auth profiles: {}.", profiles.join(" · "))
    };
    [first, second]
}

fn scheme(s: &AuthScheme) -> String {
    match s {
        AuthScheme::ApiKey {
            name,
            location,
            wire_name,
            env,
            ..
        } => {
            let at = match location {
                tungsten_ir::ApiKeyIn::Header => "header",
                tungsten_ir::ApiKeyIn::Query => "query parameter",
                tungsten_ir::ApiKeyIn::Cookie => "cookie",
            };
            match env {
                Some(e) => format!("{name} (API key in {at} {wire_name}, from ${e})"),
                None => format!("{name} (API key in {at} {wire_name})"),
            }
        }
        AuthScheme::HttpBearer {
            name,
            format,
            prefix,
            env,
            ..
        } => {
            let mut notes = vec!["bearer token".to_string()];
            match (prefix, format) {
                (Some(p), _) => notes.push(format!("must start with {p}")),
                (None, Some(f)) => notes.push(format!("format {f}")),
                (None, None) => {}
            }
            if let Some(e) = env {
                notes.push(format!("from ${e}"));
            }
            format!("{name} ({})", notes.join(", "))
        }
        AuthScheme::HttpBasic { name, .. } => format!("{name} (HTTP basic)"),
        AuthScheme::OAuth2 { name, flows, .. } => {
            let kinds: Vec<&str> = flows.iter().map(|f| f.kind.as_str()).collect();
            format!("{name} (OAuth 2: {})", kinds.join(", "))
        }
        AuthScheme::OpenIdConnect { name, url, .. } => {
            format!("{name} (OpenID Connect, {url})")
        }
        AuthScheme::Composite { name, .. } => name.clone(),
    }
}

fn part(p: &CompositePart) -> String {
    match p {
        CompositePart::Cookie { name } => format!("cookie {name}"),
        CompositePart::Header {
            name,
            equals_cookie,
            from_config,
            mutation_only,
        } => {
            let mut notes = vec![];
            if let Some(c) = equals_cookie {
                notes.push(format!("equals cookie {c}"));
            }
            if let Some(c) = from_config {
                notes.push(format!("from config {c}"));
            }
            if *mutation_only {
                notes.push("mutations only".to_string());
            }
            if notes.is_empty() {
                format!("header {name}")
            } else {
                format!("header {name} ({})", notes.join(", "))
            }
        }
        CompositePart::Bearer { env, prefix } => {
            let mut notes = vec![];
            if let Some(e) = env {
                notes.push(format!("from env {e}"));
            }
            if let Some(p) = prefix {
                notes.push(format!("prefix {p}"));
            }
            if notes.is_empty() {
                "bearer token".to_string()
            } else {
                format!("bearer token ({})", notes.join(", "))
            }
        }
    }
}

/// The API's display title: `api.title`, unless it is only the machine
/// name of a single-document project, then that document's title.
pub(crate) fn api_title(ir: &Ir) -> String {
    match ir.namespaces.as_slice() {
        [ns] if ir.api.title == ir.api.name.wire && !ns.title.trim().is_empty() => {
            collapse_whitespace(&ns.title)
        }
        _ => ir.api.title.clone(),
    }
}

/// The API summary paragraph: its description, else a sentence naming it.
pub(crate) fn api_summary(ir: &Ir) -> String {
    ir.api
        .description
        .as_deref()
        .map(collapse_whitespace)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{} API, version {}.", api_title(ir), ir.api.version))
}

/// `API version 1.0.0 · base URL ... · 3 namespaces · 37 callable operations (1 gated) · 6 planned`.
pub(crate) fn overview(model: &Model<'_>) -> String {
    let ir = model.ir;
    let gated = model
        .callable()
        .filter(|(_, c)| matches!(c.op.status, OperationStatus::Gated { .. }))
        .count();
    let mut parts = vec![format!("API version {}", ir.api.version)];
    if let Some(server) = ir.api.servers.first() {
        parts.push(format!("base URL {}", server.url));
    }
    parts.push(plural(ir.namespaces.len(), "namespace", "namespaces"));
    let mut callable = plural(
        model.callable_count(),
        "callable operation",
        "callable operations",
    );
    if gated > 0 {
        callable.push_str(&format!(" ({gated} gated)"));
    }
    parts.push(callable);
    parts.push(format!("{} planned", model.planned_count()));
    if !model.macros.is_empty() {
        parts.push(plural(model.macros.len(), "macro", "macros"));
    }
    parts.join(" · ")
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The summary of a macro, collapsed to one line.
pub(crate) fn macro_summary(m: &Macro) -> String {
    let s = collapse_whitespace(&m.summary);
    if s.is_empty() { m.name.0.clone() } else { s }
}
