// SPDX-License-Identifier: AGPL-3.0-only
//! Incoming webhooks as SDK targets name them.
//!
//! Every target keys its receiving helpers by the same event key, so one API
//! description gives the same names in every language: the `webhooks` key
//! as written, followed by a space and the method when the entry has several
//! operations, and prefixed with `<namespace>.` when the IR has several
//! namespaces.

use tungsten_ir::{Ir, Webhook};

/// One webhook with its event key.
#[derive(Debug, Clone, Copy)]
pub struct WebhookEntry<'a> {
    pub namespace: &'a str,
    pub webhook: &'a Webhook,
    pub key: &'a str,
}

/// The webhooks of an IR with their event keys.
#[derive(Debug, Clone)]
pub struct WebhookKeys<'a> {
    items: Vec<(&'a str, &'a Webhook, String)>,
}

impl<'a> WebhookKeys<'a> {
    pub fn new(ir: &'a Ir) -> Self {
        let multi = ir.namespaces.len() > 1;
        let mut items = vec![];
        for ns in &ir.namespaces {
            for w in &ns.webhooks {
                let shared = ns
                    .webhooks
                    .iter()
                    .filter(|o| o.name.wire == w.name.wire)
                    .count()
                    > 1;
                let base = if shared {
                    format!("{} {}", w.name.wire, w.method.as_str())
                } else {
                    w.name.wire.clone()
                };
                let key = if multi {
                    format!("{}.{base}", ns.name.wire)
                } else {
                    base
                };
                items.push((ns.name.wire.as_str(), w, key));
            }
        }
        Self { items }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The webhooks in IR order.
    pub fn entries(&self) -> impl Iterator<Item = WebhookEntry<'_>> {
        self.items
            .iter()
            .map(|(namespace, webhook, key)| WebhookEntry {
                namespace,
                webhook,
                key,
            })
    }
}

/// Whether the IR declares any incoming webhook.
pub fn has_webhooks(ir: &Ir) -> bool {
    ir.namespaces.iter().any(|n| !n.webhooks.is_empty())
}
