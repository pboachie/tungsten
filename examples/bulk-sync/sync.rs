// SPDX-License-Identifier: AGPL-3.0-only
//! Read many records at once through a request scheduler (Rust). The runner
//! builds this file as a small binary next to the generated SDK and runs it
//! with the server's base URL.

use std::sync::Arc;

use records_sdk::{CallOptions, ClientOptions, RecordsClient};
use records_sdk::resources::records::RecordsGetRequest;
use tungsten_runtime::{RequestScheduler, SchedulerOptions};

const RECORDS: i64 = 200;

async fn server(http: &reqwest::Client, base: &str, path: &str, post: bool) -> serde_json::Value {
    let url = format!("{base}{path}");
    let request = if post { http.post(url) } else { http.get(url) };
    let response = request.send().await.expect("server reachable");
    response.json().await.expect("server answers JSON")
}

#[tokio::main]
async fn main() {
    let base = std::env::args().nth(1).expect("usage: bulk-sync BASE_URL");
    let http = reqwest::Client::new();

    // One scheduler per quota. Clone it into every client that uses the same
    // quota; give each client a tenant when quotas are per tenant.
    let mut limits = SchedulerOptions::new(4);
    limits.max_queued = RECORDS as u32;
    limits.max_queue = std::time::Duration::from_secs(60);
    limits.retry_ratio = Some(0.1);
    let scheduler = RequestScheduler::new(limits).expect("valid scheduler options");
    let client = Arc::new(
        RecordsClient::new(ClientOptions {
            base_url: Some(base.clone()),
            scheduler: Some(scheduler.clone()),
            ..ClientOptions::default()
        })
        .expect("valid client options"),
    );

    server(&http, &base, "/__reset", true).await;
    // Every read starts at once; the scheduler, not the caller, bounds requests.
    let mut reads = tokio::task::JoinSet::new();
    for record_id in 0..RECORDS {
        let client = Arc::clone(&client);
        reads.spawn(async move {
            let result = client
                .records()
                .get(RecordsGetRequest::new(record_id), &CallOptions::default())
                .await;
            (record_id, result)
        });
    }
    let mut failures = Vec::new();
    while let Some(joined) = reads.join_next().await {
        let (record_id, result) = joined.expect("read task finished");
        match result {
            Ok(record) => assert_eq!(record.value.id, record_id),
            Err(error) => failures.push(format!("{:?}", error.diagnostic.category)),
        }
    }
    scheduler.drained().await;
    let seen = server(&http, &base, "/__stats", false).await;
    println!(
        "rust scheduled: {}/{RECORDS} read, server saw peak {} and refused {}",
        RECORDS - failures.len() as i64,
        seen["peak"],
        seen["limited"]
    );
    assert!(failures.is_empty(), "failed reads: {failures:?}");
    assert!(seen["peak"].as_u64() <= Some(4) && seen["limited"] == 0, "quota exceeded: {seen}");
}
