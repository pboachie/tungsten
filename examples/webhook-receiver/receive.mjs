// SPDX-License-Identifier: AGPL-3.0-only
// Receive signed order webhooks with the generated TypeScript SDK.
import { signWebhook } from "@elysra/tungsten-runtime";
import { webhooks } from "./dist/index.js";

// Example values only: a real endpoint reads its secrets from configuration.
const SECRET = "whsec_gallery_current";
const PREVIOUS = "whsec_gallery_previous";
const NOW = 1_700_000_000;

const SHIPPED = '{"id":"evt_1001","type":"order.shipped","data":{"order_id":"ord_42","carrier":"parcelpost"}}';
const TAMPERED = '{"id":"evt_1001","type":"order.shipped","data":{"order_id":"ord_43","carrier":"parcelpost"}}';
const INCOMPLETE = '{"id":"evt_1002","type":"order.shipped","data":{"order_id":"ord_44"}}';
const ROTATED = '{"id":"evt_1003","type":"order.shipped","data":{"order_id":"ord_45","carrier":"parcelpost"}}';

// Application-owned replay tracking: accept each event id once.
const seen = new Set();
const claim = (key) => {
  if (seen.has(key)) return false;
  seen.add(key);
  return true;
};

async function receive(body, header) {
  const outcome = await webhooks.parseWebhook("order.shipped", {
    body,
    signatureHeader: header,
    secrets: [SECRET, PREVIOUS],
    now: NOW,
    claim,
  });
  if (outcome.ok) {
    const { data } = outcome.event;
    console.log(`accepted ${outcome.type} ${outcome.id}: ${data.order_id} via ${data.carrier}`);
  } else {
    console.log(`rejected ${outcome.reason}`);
  }
}

const signed = await signWebhook(SHIPPED, SECRET, NOW);
await receive(SHIPPED, signed);
await receive(SHIPPED, signed);
await receive(TAMPERED, signed);
await receive(SHIPPED, await signWebhook(SHIPPED, SECRET, NOW - 600));
await receive(INCOMPLETE, await signWebhook(INCOMPLETE, SECRET, NOW));
await receive(ROTATED, await signWebhook(ROTATED, PREVIOUS, NOW));
