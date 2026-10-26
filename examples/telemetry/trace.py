# SPDX-License-Identifier: AGPL-3.0-only
"""Collect the telemetry of three reads in memory and print it (Python)."""

import json
import sys
import urllib.request

from records_sdk import ClientOptions, RecordsClient
from tungsten_runtime import MemoryTelemetryCollector, SchedulerOptions, TelemetryEvent


def summary(events: list[TelemetryEvent]) -> list[str]:
    """One line per operation: its phases, attempts and outcome, in order."""
    lines = []
    for call_id in sorted({e["call_id"] for e in events}):
        operation, parts = "", []
        for e in (e for e in events if e["call_id"] == call_id):
            operation = e["operation"]
            if e["name"] == "phase":
                parts.append(f"{e['phase']} {e['reason'] or e['outcome']}")
            elif e["name"] == "attempt.end":
                fields = ("attempt", e["attempt"], e["outcome"], e["category"], e["http_status"])
                parts.append(" ".join(str(f) for f in fields if f is not None))
            elif e["name"] == "operation.end":
                end = " ".join(f for f in (e["outcome"], e["category"]) if f)
                attempts = e["attempts"]
                parts.append(f"{end} after {attempts} attempt{'' if attempts == 1 else 's'}")
        lines.append(f"call {call_id} {operation}: " + "; ".join(parts))
    return lines


def main() -> None:
    base = sys.argv[1]
    urllib.request.urlopen(urllib.request.Request(base + "/__reset", method="POST")).read()
    # Off unless an observer is given; the collector keeps the newest events
    # in memory and counts the ones it had to drop. Nothing is sent anywhere.
    collector = MemoryTelemetryCollector(256)
    client = RecordsClient(
        ClientOptions(base_url=base, telemetry=collector, scheduler=SchedulerOptions(max_active=1))
    )
    for record_id in (7, 13, 404):
        client.records.get(record_id=record_id)
    events = collector.events()
    for line in summary(events):
        print(line)
    # Events carry the operation id and enumerated values, never the URL,
    # arguments or bodies.
    text = json.dumps(events)
    assert not any(secret in text for secret in (base, "/records", "record 13")), text
    print(f"{len(events)} events, {collector.dropped} dropped, no URL or payload in any event")


if __name__ == "__main__":
    main()
