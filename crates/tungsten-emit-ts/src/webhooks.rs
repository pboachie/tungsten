// SPDX-License-Identifier: AGPL-3.0-only
//! `src/webhooks.ts`: typed payloads of incoming webhooks and a receiving
//! helper over the runtime's `verifyWebhook`. Written only when the IR
//! declares webhooks.

use tungsten_emit::webhooks::WebhookKeys;
use tungsten_emit::{CommentStyle, Imports, Writer};

use crate::models::{Prec, TypeCx, Uses, write_imports};
use crate::plan::Plan;
use crate::ts::{doc_text, string_lit};

pub(crate) fn webhooks_file(plan: &Plan<'_>, header: &str) -> String {
    let keys = WebhookKeys::new(plan.ir);
    let cx = TypeCx::outside(plan);
    let mut uses = Uses::default();
    let mut body = Writer::new("  ");
    body.doc(
        CommentStyle::JsDoc,
        "Payload type of each incoming webhook, by event key (the `webhooks` key of the API description).",
    );
    body.line("export interface WebhookPayloads {");
    body.indent();
    for e in keys.entries() {
        body.doc(CommentStyle::JsDoc, &doc_text(e.webhook.doc.as_ref()));
        let ty = match &e.webhook.payload {
            Some(p) => {
                uses.add_ref(plan, None, &p.ty);
                cx.ts_ref(&p.ty).at(Prec::Union)
            }
            None => "unknown".to_string(),
        };
        body.line(format!("{}: {ty};", string_lit(e.key)));
    }
    body.dedent();
    body.line("}");
    body.blank();
    body.line("/** An event key of {@link WebhookPayloads}. */");
    body.line("export type WebhookName = keyof WebhookPayloads;");
    body.blank();
    body.doc(
        CommentStyle::JsDoc,
        "Schema of each webhook payload; a webhook without a JSON body accepts any JSON value.",
    );
    body.line("export const webhookSchemas = {");
    body.indent();
    for e in keys.entries() {
        let schema = match &e.webhook.payload {
            Some(p) => cx.zod_ref(&p.ty),
            None => "z.unknown()".to_string(),
        };
        body.line(format!("{}: {schema},", string_lit(e.key)));
    }
    body.dedent();
    body.line("} satisfies { readonly [K in WebhookName]: z.ZodType };");
    body.blank();
    body.doc(
        CommentStyle::JsDoc,
        "Verify one delivery of webhook `name` (signature, timestamp, body limit and replay claim, see `verifyWebhook`) and decode its payload with the declared schema before the replay claim. A payload that does not match is rejected as `payload_invalid`.",
    );
    body.line("export async function parseWebhook<K extends WebhookName>(");
    body.indent();
    body.line("name: K,");
    body.line("options: Omit<VerifyWebhookOptions, \"parse\">,");
    body.dedent();
    body.line("): Promise<WebhookOutcome<WebhookPayloads[K]>> {");
    body.indent();
    body.line("const schema: z.ZodType = webhookSchemas[name];");
    body.line(
        "return verifyWebhook({ ...options, parse: (event) => schema.parse(event) as WebhookPayloads[K] });",
    );
    body.dedent();
    body.line("}");

    let mut imports = Imports::new();
    imports.add("@elysra/tungsten-runtime", "verifyWebhook");
    imports.add_type("@elysra/tungsten-runtime", "VerifyWebhookOptions");
    imports.add_type("@elysra/tungsten-runtime", "WebhookOutcome");
    imports.add("zod", "z");
    if uses.integer {
        imports.add("./internal.js", "integer");
    }
    if uses.pattern {
        imports.add("./internal.js", "withPattern");
    }
    if uses.length {
        imports.add("./internal.js", "withLength");
    }
    if uses.binary {
        imports.add("./internal.js", "binary");
        imports.add_type("./internal.js", "BinaryInput");
    }
    let mut w = Writer::new("  ");
    w.line(header);
    w.blank();
    w.line("// Incoming webhooks: typed payloads and a verifying parser.");
    w.blank();
    write_imports(&mut w, &imports);
    for ns in &uses.namespaces {
        if let Some(m) = plan.model_ns(ns) {
            w.line(format!(
                "import * as {} from \"./models/{}.js\";",
                m.alias, m.file
            ));
        }
    }
    w.blank();
    let mut out = w.finish();
    out.push('\n');
    out.push_str(&body.finish());
    out
}
