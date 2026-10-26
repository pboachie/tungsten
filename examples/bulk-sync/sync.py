# SPDX-License-Identifier: AGPL-3.0-only
"""Read many records at once, first unbounded, then through a request scheduler."""

import json
import sys
import urllib.request
from concurrent.futures import ThreadPoolExecutor

from records_sdk import ClientOptions, Err, RecordsClient
from tungsten_runtime import RequestScheduler, SchedulerOptions

RECORDS = 200
WORKERS = 32  # application threads; the scheduler, not this number, bounds requests


def server(base: str, path: str, method: str = "GET") -> dict[str, int]:
    request = urllib.request.Request(base + path, method=method)
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def sync(client: RecordsClient) -> tuple[int, list[str]]:
    def read(record_id: int) -> str | None:
        result = client.records.get(record_id=record_id)
        if isinstance(result, Err):
            # The envelope's code says why: QUEUE_FULL, RATE_LIMITED, ...
            return result.error["code"] or result.error["category"]
        assert result.value.id == record_id
        return None

    with ThreadPoolExecutor(WORKERS) as pool:
        failures = [f for f in pool.map(read, range(RECORDS)) if f is not None]
    return RECORDS - len(failures), failures


def main(base: str) -> None:
    server(base, "/__reset", "POST")
    with RecordsClient(base_url=base) as client:
        ok, failures = sync(client)
    seen = server(base, "/__stats")
    print(f"unbounded: {ok}/{RECORDS} read, server saw peak {seen['peak']} and refused {seen['limited']}")

    # One scheduler per quota. Share it between clients that use the same
    # quota; give each client a tenant when quotas are per tenant.
    scheduler = RequestScheduler(
        SchedulerOptions(max_active=4, max_queued=RECORDS, max_queue_ms=60_000, retry_ratio=0.1)
    )
    server(base, "/__reset", "POST")
    with RecordsClient(ClientOptions(scheduler=scheduler), base_url=base) as client:
        ok, failures = sync(client)
    assert scheduler.drained(5_000), "requests were still in flight"
    seen = server(base, "/__stats")
    print(f"scheduled: {ok}/{RECORDS} read, server saw peak {seen['peak']} and refused {seen['limited']}")
    assert not failures, failures
    assert seen["peak"] <= 4 and seen["limited"] == 0, seen
    print("bulk sync stayed within the quota")


if __name__ == "__main__":
    main(sys.argv[1])
