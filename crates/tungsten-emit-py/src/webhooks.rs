// SPDX-License-Identifier: AGPL-3.0-only
//! `<module>/webhooks.py`: validators of incoming webhook payloads and a
//! receiving helper over the runtime's `verify_webhook`. Written only when
//! the IR declares webhooks.

use tungsten_emit::Writer;
use tungsten_emit::webhooks::WebhookKeys;

use crate::plan::Plan;
use crate::py::{PyImports, docstring, string_lit, two_blank};
use crate::types::{Cx, Flavor};

pub(crate) fn webhooks_file(plan: &Plan<'_>, header: &str) -> String {
    let keys = WebhookKeys::new(plan.ir);
    let cx = Cx::new(plan, None);
    let mut entries = vec![];
    for e in keys.entries() {
        let schema = match &e.webhook.payload {
            Some(p) => cx.value(&cx.ty(&p.ty, Flavor::Schema)),
            None => {
                cx.uses.borrow_mut().typing.insert("Any");
                "Any".to_string()
            }
        };
        entries.push((e.key.to_string(), schema));
    }
    let mut uses = cx.uses.borrow().clone();
    uses.internal = true;
    let mut imports = PyImports::default();
    imports.add("__future__", "annotations");
    imports.add("collections.abc", "Callable");
    imports.add("collections.abc", "Sequence");
    imports.add("typing", "Any");
    imports.add("tungsten_runtime", "Invalid");
    imports.add("tungsten_runtime", "WebhookOutcome");
    imports.add("tungsten_runtime", "verify_webhook");
    uses.add_to(plan, &mut imports, ".", ".models");

    let mut w = Writer::new("    ");
    w.line(header);
    w.blank();
    docstring(
        &mut w,
        "Incoming webhooks: payload validators and a verifying parser.",
    );
    w.blank();
    w.line(imports.render());
    w.blank();
    w.line("WEBHOOKS: dict[str, _internal.Response] = {");
    w.indent();
    for (key, schema) in &entries {
        w.line(format!(
            "{}: _internal.Response(lambda: {schema}),",
            string_lit(key)
        ));
    }
    w.dedent();
    w.line("}");
    w.line("\"\"\"Payload validator of each incoming webhook, by event key (the `webhooks` key");
    w.line("of the API description). A webhook without a JSON body accepts any JSON value.\"\"\"");
    two_blank(&mut w);
    w.line("def parse_webhook(");
    w.indent();
    w.line("name: str,");
    w.line("body: bytes | str,");
    w.line("signature_header: str | None,");
    w.line("secrets: str | Sequence[str],");
    w.line("*,");
    w.line("tolerance_seconds: int | None = None,");
    w.line("max_body_bytes: int | None = None,");
    w.line("now: int | None = None,");
    w.line("claim: Callable[[str], bool] | None = None,");
    w.dedent();
    w.line(") -> WebhookOutcome:");
    w.indent();
    docstring(
        &mut w,
        "Verify one delivery of webhook ``name`` (signature, timestamp, body limit\nand replay claim, see ``verify_webhook``) and validate its payload with the\ndeclared schema before the replay claim. A payload that does not match is\nrejected as ``payload_invalid``; an unknown ``name`` raises ``ValueError``.",
    );
    w.line("validator = WEBHOOKS.get(name)");
    w.line("if validator is None:");
    w.indent();
    w.line("raise ValueError(f\"unknown webhook {name!r}\")");
    w.dedent();
    w.blank();
    w.line("def parse(event: Any) -> Any:");
    w.indent();
    w.line("result = validator.validate(event)");
    w.line("if isinstance(result, Invalid):");
    w.indent();
    w.line("raise ValueError(\"payload does not match\")");
    w.dedent();
    w.line("return result.data");
    w.dedent();
    w.blank();
    w.line("return verify_webhook(");
    w.indent();
    w.line("body,");
    w.line("signature_header,");
    w.line("secrets,");
    w.line("tolerance_seconds=tolerance_seconds,");
    w.line("max_body_bytes=max_body_bytes,");
    w.line("now=now,");
    w.line("claim=claim,");
    w.line("parse=parse,");
    w.dedent();
    w.line(")");
    w.dedent();
    w.finish()
}
