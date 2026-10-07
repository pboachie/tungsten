// SPDX-License-Identifier: AGPL-3.0-only
//! `README.md`: the human index of the docs target. Callable operations in
//! tables per namespace, planned ones in their own table with the reason
//! they are not callable (NFR-8), and the error categories every SDK
//! returns.

use std::fmt::Write as _;

use tungsten_emit::header_text;
use tungsten_emit::schema::{collapse_whitespace, prune_sentences};
use tungsten_ir::{AuthScheme, Doc, Ir};

use crate::model::{
    Model, api_summary, api_title, auth_lines, endpoint, gate, idempotency_kind, macro_summary,
    overview, planned_reason, safety, summary,
};

/// The error categories of the runtime envelope (planning/06), with their
/// default `retryable`.
const CATEGORIES: &[(&str, &str, &str)] = &[
    (
        "VALIDATION_FAILED",
        "arguments failed pre-flight validation, or a 400 with an error body",
        "never",
    ),
    ("MALFORMED_REQUEST", "a 400 without an error body", "never"),
    ("REQUEST_TOO_LARGE", "413", "never"),
    ("AUTH_FAILED", "401 or 403", "never"),
    ("NOT_FOUND", "404", "never"),
    ("CONFLICT", "409", "never"),
    (
        "PRECONDITION_FAILED",
        "402, 412 or 422 as the manifest says",
        "never",
    ),
    ("RATE_LIMITED", "429", "after_delay"),
    ("UPSTREAM_UNAVAILABLE", "502, 503 or 504", "after_delay"),
    (
        "OUTCOME_UNKNOWN",
        "a timeout, reset or ambiguous status on a mutation",
        "same_key_only",
    ),
    (
        "TRANSPORT_FAILED",
        "DNS, TLS or connection refused before sending",
        "after_delay",
    ),
    (
        "CONFIRMATION_REQUIRED",
        "a destructive or irreversible call without confirmation",
        "never",
    ),
    (
        "GATE_DISABLED",
        "a gated route answered with its disabled status",
        "never",
    ),
    (
        "UNEXPECTED_RESPONSE",
        "a response that failed strict validation",
        "never",
    ),
];

pub(crate) fn readme(model: &Model<'_>) -> String {
    let ir = model.ir;
    let mut out = format!(
        "<!-- {} -->\n\n",
        header_text(ir).replace("--", "- -").replace('\n', " ")
    );
    let _ = writeln!(
        out,
        "# {}\n\n{}\n\n{}\n",
        api_title(ir),
        api_summary(ir),
        overview(model)
    );
    out.push_str(
        "## Files\n\n| File | Purpose |\n|---|---|\n\
         | `llms.txt` | Index for agents: one line per callable operation with its safety tier and idempotency rule. |\n\
         | `llms-full.txt` | Full reference for agents: arguments, responses, errors, remediation and types. |\n\
         | `tools.json` | Function-calling manifest: one tool per callable operation, parameters as JSON Schema. |\n",
    );
    authentication(ir, &mut out);
    for n in model.namespaces.iter().filter(|n| !n.callable.is_empty()) {
        let title = collapse_whitespace(&n.ns.title);
        let _ = writeln!(out, "\n## {} operations\n", n.ns.name.wire);
        if !title.is_empty() {
            let _ = writeln!(out, "{title} (version {}).\n", n.ns.version);
        }
        out.push_str(
            "| Operation | Endpoint | Summary | Safety | Idempotency |\n|---|---|---|---|---|\n",
        );
        for c in &n.callable {
            let mut text = cell(&summary(c.op));
            if let Some(g) = gate(c.op) {
                text.push_str(&format!(" ({g})"));
            }
            if c.op.deprecated {
                text.push_str(" (deprecated)");
            }
            let _ = writeln!(
                out,
                "| `{}` | `{}` | {} | {} | {} |",
                c.sdk_path(&n.ns.name.wire),
                endpoint(c.op),
                text,
                safety(c.op.agent.safety),
                idempotency_kind(c.op.agent.idempotency.policy)
            );
        }
    }
    if !model.macros.is_empty() {
        out.push_str("\n## Macros\n\n| Macro | Summary | Safety |\n|---|---|---|\n");
        for m in &model.macros {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} |",
                m.mac.name.0,
                cell(&macro_summary(m.mac)),
                safety(m.mac.safety)
            );
        }
    }
    if model.planned_count() > 0 {
        out.push_str(
            "\n## Planned (not available)\n\n\
             These operations are documented by the API but are not callable: no SDK method or tool exists for them.\n\n\
             | Operation | Endpoint | Summary | Why |\n|---|---|---|---|\n",
        );
        for n in &model.namespaces {
            for op in &n.planned {
                let _ = writeln!(
                    out,
                    "| `{}` | `{}` | {} | {} |",
                    op.id.0,
                    endpoint(op),
                    cell(&summary(op)),
                    cell(&planned_reason(op))
                );
            }
        }
    }
    out.push_str(
        "\n## Errors\n\nSDK calls never throw for API or transport errors: they return a result whose error is one envelope with `category`, `retryable` and `remediation`. `llms-full.txt` lists the API's error codes with their remediation.\n\n\
         | Category | When | Retryable by default |\n|---|---|---|\n",
    );
    for (category, when, retryable) in CATEGORIES {
        let _ = writeln!(out, "| `{category}` | {when} | {retryable} |");
    }
    out.push_str(
        "\n## Regenerating\n\nThese files are generated from the API's OpenAPI documents and its tungsten manifests. Run `tungsten generate` after changing them; `tungsten check --ci` fails while these files are stale.\n",
    );
    out
}

fn authentication(ir: &Ir, out: &mut String) {
    out.push_str("\n## Authentication\n\n");
    let [first, second] = auth_lines(ir);
    let _ = writeln!(out, "{first}\n{second}\n");
    for s in &ir.auth {
        let doc = match s {
            AuthScheme::ApiKey { doc, .. }
            | AuthScheme::HttpBearer { doc, .. }
            | AuthScheme::HttpBasic { doc, .. }
            | AuthScheme::OAuth2 { doc, .. }
            | AuthScheme::OpenIdConnect { doc, .. } => doc.as_ref(),
            AuthScheme::Composite { .. } => None,
        };
        if let Some(text) = doc_text(doc) {
            let _ = writeln!(out, "- `{}`: {text}", s.name());
        }
    }
}

fn doc_text(doc: Option<&Doc>) -> Option<String> {
    let doc = doc?;
    let text = doc.description.as_deref().or(doc.summary.as_deref())?;
    let text = prune_sentences(text, 2);
    (!text.is_empty()).then_some(text)
}

fn cell(text: &str) -> String {
    collapse_whitespace(text).replace('|', "\\|")
}
