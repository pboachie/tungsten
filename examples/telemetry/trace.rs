// SPDX-License-Identifier: AGPL-3.0-only
//! Collect the telemetry of three reads in memory and print it (Rust). The
//! runner builds this file as a small binary next to the generated SDK and
//! runs it with the server's base URL.

use std::sync::Arc;

use records_sdk::resources::records::RecordsGetRequest;
use records_sdk::{CallOptions, ClientOptions, RecordsClient};
use serde_json::Value;
use tungsten_runtime::{MemoryTelemetryCollector, RequestScheduler, SchedulerOptions};

/// One line per operation: its phases, attempts and outcome, in order.
fn summary(events: &[Value]) -> Vec<String> {
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let mut ids: Vec<u64> = events.iter().filter_map(|e| e["call_id"].as_u64()).collect();
    ids.dedup();
    ids.iter()
        .map(|id| {
            let mut operation = String::new();
            let mut parts = Vec::new();
            for e in events.iter().filter(|e| e["call_id"].as_u64() == Some(*id)) {
                operation = e["operation"].as_str().unwrap_or_default().to_owned();
                match e["name"].as_str() {
                    Some("phase") => parts.push(format!(
                        "{} {}",
                        text(&e["phase"]).unwrap_or_default(),
                        text(&e["reason"]).or_else(|| text(&e["outcome"])).unwrap_or_default()
                    )),
                    Some("attempt.end") => {
                        let mut fields = vec!["attempt".to_owned()];
                        fields.extend(
                            ["attempt", "outcome", "category", "http_status"]
                                .iter()
                                .filter_map(|k| text(&e[*k])),
                        );
                        parts.push(fields.join(" "));
                    }
                    Some("operation.end") => {
                        let end: Vec<String> =
                            ["outcome", "category"].iter().filter_map(|k| text(&e[*k])).collect();
                        let attempts = e["attempts"].as_u64().unwrap_or_default();
                        let plural = if attempts == 1 { "" } else { "s" };
                        parts.push(format!("{} after {attempts} attempt{plural}", end.join(" ")));
                    }
                    _ => {}
                }
            }
            format!("call {id} {operation}: {}", parts.join("; "))
        })
        .collect()
}

#[tokio::main]
async fn main() {
    let base = std::env::args().nth(1).expect("usage: telemetry BASE_URL");
    reqwest::Client::new()
        .post(format!("{base}/__reset"))
        .send()
        .await
        .expect("server reachable");
    // Off unless an observer is given; the collector keeps the newest events
    // in memory and counts the ones it had to drop. Nothing is sent anywhere.
    let collector = Arc::new(MemoryTelemetryCollector::new(256));
    let client = RecordsClient::new(ClientOptions {
        base_url: Some(base.clone()),
        telemetry: Some(collector.clone()),
        scheduler: Some(RequestScheduler::new(SchedulerOptions::new(1)).expect("valid scheduler")),
        ..ClientOptions::default()
    })
    .expect("valid client options");
    for record_id in [7, 13, 404] {
        let _ = client
            .records()
            .get(RecordsGetRequest { record_id }, &CallOptions::default())
            .await;
    }
    let events: Vec<Value> = collector
        .events()
        .iter()
        .map(|e| serde_json::to_value(e).expect("events serialize"))
        .collect();
    for line in summary(&events) {
        println!("{line}");
    }
    // Events carry the operation id and enumerated values, never the URL,
    // arguments or bodies.
    let text = serde_json::to_string(&events).expect("events serialize");
    assert!(
        ![base.as_str(), "/records", "record 13"].iter().any(|s| text.contains(s)),
        "{text}"
    );
    println!(
        "{} events, {} dropped, no URL or payload in any event",
        events.len(),
        collector.dropped()
    );
}
