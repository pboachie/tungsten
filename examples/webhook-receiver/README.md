# Receive signed webhooks

Verify signed order.shipped deliveries and decode typed payloads with the TypeScript, Python and Rust SDKs, without a web framework.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; Python venv and pip, Node 22 or later with npm, and the pinned Rust toolchain and Cargo.

From the repository root:

```sh
python3 examples/run.py --only webhook-receiver
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: the TypeScript, Python and Rust receivers each accept the signed delivery and the rotated-secret delivery and reject the replay, the tampered body, the stale timestamp and the incomplete payload.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

## Problem: trusting an incoming webhook

An API that calls your service back (an order shipped, a payment settled) sends
a signed request. Before acting on it, a receiver must check the signature
against the raw bytes, refuse stale and replayed deliveries, and only then
trust the payload's shape. The recipe's [openapi.json](openapi.json) declares
one OpenAPI 3.1 webhook, `order.shipped`; the generated TypeScript, Python and
Rust SDKs each get a `webhooks` module with its typed payload and a
`parseWebhook` / `parse_webhook` helper. [receive.py](receive.py),
[receive.mjs](receive.mjs) and [receive.rs](receive.rs) sign six example
deliveries locally with example secrets and pass them through that helper, with
no web framework and no network:

```text
accepted order.shipped evt_1001: ord_42 via parcelpost
rejected duplicate
rejected signature_mismatch
rejected timestamp_out_of_tolerance
rejected payload_invalid
accepted order.shipped evt_1003: ord_45 via parcelpost
```

Every language prints exactly these lines: the first delivery is accepted, its
replay is refused by the application's `claim` hook, a body changed after
signing fails the signature, a delivery signed ten minutes ago is outside the
default five-minute tolerance, a payload without `carrier` does not match the
declared schema, and a delivery signed with the previous secret is accepted
while both secrets are configured (rotation).

## Wiring it into a service

Give the helper the raw request body (bytes, before any JSON parsing), the
`Stripe-Signature` header value and the endpoint secrets:

```python
outcome = webhooks.parse_webhook("order.shipped", raw_body, signature, [current, previous], claim=claim)
```

```js
const outcome = await webhooks.parseWebhook("order.shipped", { body: rawBody, signatureHeader: signature, secrets: [current, previous], claim });
```

```rust
let outcome = parse_webhook(WebhookName::OrderShipped, &raw_body, signature, &options)?;
```

The signature scheme is the Stripe-compatible `t=<unix>,v1=<hex HMAC-SHA256 of
"t.body">` header. The helper bounds the body (1 MiB by default), compares
signatures in constant time and never puts secrets or payloads in its rejection
messages. Replay tracking, queues and the acknowledgement are the
application's: `claim` is called only for a verified, well-formed delivery, so
answer `2xx` once the event is durably handed off, and a non-`2xx` status lets
the provider redeliver. See each runtime README's Limits section.
