// SPDX-License-Identifier: AGPL-3.0-only
//! Receive signed order webhooks with the generated Rust SDK.

use std::cell::RefCell;
use std::collections::BTreeSet;

use orders_sdk::webhooks::{WebhookEvent, WebhookName, parse_webhook};
use tungsten_runtime::webhooks::{VerifyWebhookOptions, sign_webhook};

// Example values only: a real endpoint reads its secrets from configuration.
const SECRET: &str = "whsec_gallery_current";
const PREVIOUS: &str = "whsec_gallery_previous";
const NOW: u64 = 1_700_000_000;

const SHIPPED: &[u8] = br#"{"id":"evt_1001","type":"order.shipped","data":{"order_id":"ord_42","carrier":"parcelpost"}}"#;
const TAMPERED: &[u8] = br#"{"id":"evt_1001","type":"order.shipped","data":{"order_id":"ord_43","carrier":"parcelpost"}}"#;
const INCOMPLETE: &[u8] =
    br#"{"id":"evt_1002","type":"order.shipped","data":{"order_id":"ord_44"}}"#;
const ROTATED: &[u8] = br#"{"id":"evt_1003","type":"order.shipped","data":{"order_id":"ord_45","carrier":"parcelpost"}}"#;

fn main() {
    // Application-owned replay tracking: accept each event id once.
    let seen = RefCell::new(BTreeSet::new());
    let claim = |key: &str| seen.borrow_mut().insert(key.to_string());
    let options = VerifyWebhookOptions {
        secrets: &[SECRET, PREVIOUS],
        now: Some(NOW),
        claim: Some(&claim),
        ..Default::default()
    };
    let receive = |body: &[u8], header: &str| {
        let outcome = parse_webhook(WebhookName::OrderShipped, body, Some(header), &options)
            .expect("valid options");
        match outcome {
            Ok(delivery) => {
                let WebhookEvent::OrderShipped(event) = &delivery.event;
                println!(
                    "accepted {} {}: {} via {}",
                    delivery.r#type.as_deref().unwrap_or(""),
                    delivery.id.as_deref().unwrap_or(""),
                    event.data.order_id,
                    event.data.carrier
                );
            }
            Err(rejected) => println!("rejected {}", rejected.reason.as_str()),
        }
    };

    let signed = sign_webhook(SHIPPED, SECRET, NOW);
    receive(SHIPPED, &signed);
    receive(SHIPPED, &signed);
    receive(TAMPERED, &signed);
    receive(SHIPPED, &sign_webhook(SHIPPED, SECRET, NOW - 600));
    receive(INCOMPLETE, &sign_webhook(INCOMPLETE, SECRET, NOW));
    receive(ROTATED, &sign_webhook(ROTATED, PREVIOUS, NOW));
}
