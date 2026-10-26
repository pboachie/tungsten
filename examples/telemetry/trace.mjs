// SPDX-License-Identifier: AGPL-3.0-only
// Collect the telemetry of three reads in memory and print it (TypeScript).
// The runner copies this file next to the generated package and runs
// `node trace.mjs BASE_URL`.
import { MemoryTelemetryCollector } from "@elysra/tungsten-runtime";
import { RecordsClient } from "./dist/index.js";

/** One line per operation: its phases, attempts and outcome, in order. */
function summary(events) {
  const lines = [];
  for (const callId of [...new Set(events.map((e) => e.call_id))].sort((a, b) => a - b)) {
    let operation = "";
    const parts = [];
    for (const e of events.filter((e) => e.call_id === callId)) {
      operation = e.operation;
      if (e.name === "phase") parts.push(`${e.phase} ${e.reason ?? e.outcome}`);
      else if (e.name === "attempt.end") parts.push(["attempt", e.attempt, e.outcome, e.category, e.http_status].filter((f) => f !== null).join(" "));
      else if (e.name === "operation.end") {
        const end = [e.outcome, e.category].filter((f) => f !== null).join(" ");
        parts.push(`${end} after ${e.attempts} attempt${e.attempts === 1 ? "" : "s"}`);
      }
    }
    lines.push(`call ${callId} ${operation}: ${parts.join("; ")}`);
  }
  return lines;
}

const base = process.argv[2];
await fetch(`${base}/__reset`, { method: "POST" });
// Off unless an observer is given; the collector keeps the newest events in
// memory and counts the ones it had to drop. Nothing is sent anywhere.
const collector = new MemoryTelemetryCollector(256);
const client = new RecordsClient({ baseUrl: base, telemetry: collector, scheduler: { maxActive: 1 } });
for (const recordId of [7, 13, 404]) await client.records.get({ recordId });
const events = collector.events();
for (const line of summary(events)) console.log(line);
// Events carry the operation id and enumerated values, never the URL,
// arguments or bodies.
const text = JSON.stringify(events);
if ([base, "/records", "record 13"].some((secret) => text.includes(secret))) throw new Error(text);
console.log(`${events.length} events, ${collector.dropped} dropped, no URL or payload in any event`);
