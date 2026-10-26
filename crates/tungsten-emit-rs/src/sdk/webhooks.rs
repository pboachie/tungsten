// SPDX-License-Identifier: AGPL-3.0-only
//! `src/webhooks.rs`: typed payloads of incoming webhooks and a receiving
//! helper over the runtime's `verify_webhook_with`. Written only when the IR
//! declares webhooks.

use tungsten_emit::Writer;
use tungsten_emit::webhooks::WebhookKeys;
use tungsten_ir::Ident;
use tungsten_ir::naming::Role;

use super::plan::{Plan, unique};
use super::rs::{doc, doc_text, string_lit};
use super::types::{Cx, emit_stmts, write_check_fn};

struct Entry {
    key: String,
    variant: String,
    doc: String,
    /// The payload type; `None` for a webhook without a JSON body.
    ty: Option<String>,
    /// The check function of the payload, when it has constraints.
    check: Option<String>,
}

pub(crate) fn webhooks_file(plan: &Plan<'_>, header: &str) -> String {
    let keys = WebhookKeys::new(plan.ir);
    let cx = Cx::new(plan, None);
    let words: Vec<Vec<String>> = keys.entries().map(|e| Ident::new(e.key).words).collect();
    let variants = unique(&["Self"], &words, Role::EnumVariant);
    let mut helpers = vec![];
    let entries: Vec<Entry> = keys
        .entries()
        .zip(variants)
        .map(|(e, variant)| {
            let ty = e.webhook.payload.as_ref().map(|p| cx.ty(&p.ty));
            let check = e.webhook.payload.as_ref().and_then(|p| match &p.ty {
                tungsten_ir::TypeRef::Named(id) => cx.check_path(id),
                inline => {
                    let mut stmts = vec![];
                    cx.check_stmts(inline, None, "v", 0, &mut stmts);
                    (!stmts.is_empty()).then(|| {
                        let words = Ident::new(&variant).words;
                        let name = format!("{}_webhook", super::plan::fn_name("check", &words));
                        helpers.push((name.clone(), ty.clone().unwrap_or_default(), stmts));
                        name
                    })
                }
            });
            Entry {
                key: e.key.to_string(),
                variant,
                doc: doc_text(e.webhook.doc.as_ref()),
                ty,
                check,
            }
        })
        .collect();
    let checks = entries.iter().any(|e| e.check.is_some());

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    w.line("//! Incoming webhooks: typed payloads and a verifying parser.");
    w.blank();
    if checks {
        w.line("use crate::support::Checker;");
    }
    w.line("use tungsten_runtime::webhooks::{");
    w.indent();
    w.line("RejectedWebhook, VerifiedWebhook, VerifyWebhookOptions, WebhookOptionsError,");
    w.line("verify_webhook_with,");
    w.dedent();
    w.line("};");
    w.blank();
    doc(
        &mut w,
        "An incoming webhook, by event key (the `webhooks` key of the API description).",
    );
    w.line("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]");
    w.line("pub enum WebhookName {");
    w.indent();
    for e in &entries {
        doc(&mut w, &e.doc);
        w.line(format!("{},", e.variant));
    }
    w.dedent();
    w.line("}");
    w.blank();
    w.line("impl WebhookName {");
    w.indent();
    doc(&mut w, "Every webhook, in API description order.");
    w.line("pub const ALL: &'static [WebhookName] = &[");
    w.indent();
    for e in &entries {
        w.line(format!("WebhookName::{},", e.variant));
    }
    w.dedent();
    w.line("];");
    w.blank();
    doc(&mut w, "The event key.");
    w.line("pub fn key(self) -> &'static str {");
    w.indent();
    w.line("match self {");
    w.indent();
    for e in &entries {
        w.line(format!(
            "WebhookName::{} => {},",
            e.variant,
            string_lit(&e.key)
        ));
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    doc(&mut w, "The webhook with event key `key`.");
    w.line("pub fn from_key(key: &str) -> Option<Self> {");
    w.indent();
    w.line("Self::ALL.iter().copied().find(|w| w.key() == key)");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    doc(
        &mut w,
        "The typed payload of a verified delivery. `Debug` prints only the webhook, never the payload.",
    );
    w.line("#[derive(Clone, PartialEq)]");
    w.line("pub enum WebhookEvent {");
    w.indent();
    for e in &entries {
        let ty = e.ty.as_deref().unwrap_or("serde_json::Value");
        w.line(format!("{}({ty}),", e.variant));
    }
    w.dedent();
    w.line("}");
    w.blank();
    w.line("impl WebhookEvent {");
    w.indent();
    doc(&mut w, "The webhook this payload belongs to.");
    w.line("pub fn name(&self) -> WebhookName {");
    w.indent();
    w.line("match self {");
    w.indent();
    for e in &entries {
        w.line(format!(
            "WebhookEvent::{0}(_) => WebhookName::{0},",
            e.variant
        ));
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    w.line("impl std::fmt::Debug for WebhookEvent {");
    w.indent();
    w.line("fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {");
    w.indent();
    w.line("write!(f, \"WebhookEvent({})\", self.name().key())");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    doc(
        &mut w,
        "What [`parse_webhook`] returns: the outer `Err` is only for invalid options; a rejected delivery is the inner `Err`.",
    );
    w.line("pub type WebhookOutcome =");
    w.indent();
    w.line("Result<Result<VerifiedWebhook<WebhookEvent>, RejectedWebhook>, WebhookOptionsError>;");
    w.dedent();
    w.blank();
    doc(
        &mut w,
        "Verify one delivery of webhook `name` (signature, timestamp, body limit and replay claim, see `tungsten_runtime::webhooks::verify_webhook`) and decode its payload with the declared schema, constraints included, before the replay claim. A payload that does not match is rejected as `payload_invalid`.",
    );
    w.line("pub fn parse_webhook(");
    w.indent();
    w.line("name: WebhookName,");
    w.line("body: &[u8],");
    w.line("signature_header: Option<&str>,");
    w.line("options: &VerifyWebhookOptions<'_>,");
    w.dedent();
    w.line(") -> WebhookOutcome {");
    w.indent();
    w.line("verify_webhook_with(body, signature_header, options, |value| match name {");
    w.indent();
    for e in &entries {
        if e.ty.is_none() {
            w.line(format!(
                "WebhookName::{0} => Some(WebhookEvent::{0}(value)),",
                e.variant
            ));
            continue;
        }
        w.line(format!("WebhookName::{} => {{", e.variant));
        w.indent();
        w.line("let event = serde_json::from_value(value).ok()?;");
        match &e.check {
            Some(check) => {
                w.line("let mut c = Checker::new();");
                emit_stmts(&mut w, 12, &[format!("{check}(&event, &mut c);")]);
                w.line("let valid = c.finish().is_empty();");
                w.line(format!(
                    "valid.then_some(WebhookEvent::{}(event))",
                    e.variant
                ));
            }
            None => {
                w.line(format!("Some(WebhookEvent::{}(event))", e.variant));
            }
        }
        w.dedent();
        w.line("}");
    }
    w.dedent();
    w.line("})");
    w.dedent();
    w.line("}");
    for (name, ty, stmts) in &helpers {
        write_check_fn(&mut w, name, ty, stmts);
    }
    w.finish()
}
