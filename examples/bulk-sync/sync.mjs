// SPDX-License-Identifier: AGPL-3.0-only
// Read many records at once through a request scheduler (TypeScript).
// The runner copies this file next to the generated package and runs
// `node sync.mjs BASE_URL`.
import { RequestScheduler } from "@elysra/tungsten-runtime";
import { RecordsClient } from "./dist/index.js";

const RECORDS = 200;
const base = process.argv[2];

async function server(path, method = "GET") {
  const response = await fetch(base + path, { method });
  return response.json();
}

// One scheduler per quota. Share it between clients that use the same
// quota; give each client a tenant when quotas are per tenant.
const scheduler = new RequestScheduler({ maxActive: 4, maxQueued: RECORDS, maxQueueMs: 60_000, retryRatio: 0.1 });
const client = new RecordsClient({ baseUrl: base, scheduler });

await server("/__reset", "POST");
// Every read starts at once; the scheduler, not the caller, bounds requests.
const results = await Promise.all(
  Array.from({ length: RECORDS }, (_, recordId) => client.records.get({ recordId })),
);
await scheduler.drained();
const failures = results.flatMap((result, recordId) => {
  if (!result.ok) return [result.error.code ?? result.error.category];
  if (result.value.id !== recordId) throw new Error(`record ${recordId} came back as ${result.value.id}`);
  return [];
});
const seen = await server("/__stats");
console.log(`typescript scheduled: ${RECORDS - failures.length}/${RECORDS} read, server saw peak ${seen.peak} and refused ${seen.limited}`);
if (failures.length > 0) throw new Error(`failed reads: ${failures.join(", ")}`);
if (seen.peak > 4 || seen.limited !== 0) throw new Error(`quota exceeded: ${JSON.stringify(seen)}`);
