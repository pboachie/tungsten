// SPDX-License-Identifier: AGPL-3.0-only
//! Receive signed order webhooks with the generated Rust SDK.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::hash::{BuildHasher, RandomState};

use orders_sdk::webhooks::{WebhookEvent, WebhookName, parse_webhook};
use tungsten_runtime::webhooks::{VerifyWebhookOptions, sign_webhook};

const NOW: u64 = 1_700_000_000;

const SHIPPED: &[u8] = br#"{"id":"evt_1001","type":"order.shipped","data":{"order_id":"ord_42","carrier":"parcelpost"}}"#;
const TAMPERED: &[u8] = br#"{"id":"evt_1001","type":"order.shipped","data":{"order_id":"ord_43","carrier":"parcelpost"}}"#;
const INCOMPLETE: &[u8] =
    br#"{"id":"evt_1002","type":"order.shipped","data":{"order_id":"ord_44"}}"#;
const ROTATED: &[u8] = br#"{"id":"evt_1003","type":"order.shipped","data":{"order_id":"ord_45","carrier":"parcelpost"}}"#;

/// A fresh example secret per run: a real endpoint reads its secrets from
/// configuration.
fn example_secret(n: u64) -> String {
    let state = RandomState::new();
    format!("{:016x}{:016x}", state.hash_one(n), state.hash_one(!n))
}

fn main() {
    let (current, rotated) = (example_secret(0), example_secret(1));
    let (secret, previous) = (current.as_str(), rotated.as_str());
    // Application-owned replay tracking: accept each event id once.
    let seen = RefCell::new(BTreeSet::new());
    let claim = |key: &str| seen.borrow_mut().insert(key.to_string());
    let options = VerifyWebhookOptions {
        secrets: &[secret, previous],
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

    let signed = sign_webhook(SHIPPED, secret, NOW);
    receive(SHIPPED, &signed);
    receive(SHIPPED, &signed);
    receive(TAMPERED, &signed);
    receive(SHIPPED, &sign_webhook(SHIPPED, secret, NOW - 600));
    receive(INCOMPLETE, &sign_webhook(INCOMPLETE, secret, NOW));
    receive(ROTATED, &sign_webhook(ROTATED, previous, NOW));
}
