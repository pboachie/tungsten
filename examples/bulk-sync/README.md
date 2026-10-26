# Bulk sync within a quota

A sync job that reads many records at once can exceed an API's concurrency
quota and turn into a storm of `429` responses and retries. This example reads
200 records with 32 application threads from a local record store that allows 4
concurrent requests and answers `429` with `Retry-After: 1` beyond that. It
runs the job twice with a generated Python SDK:

1. Unbounded: every thread sends at once, so the server refuses many requests
   and some reads may fail after their retries.
2. Through a request scheduler: `ClientOptions(scheduler=...)` allows at most 4
   requests in flight, queues the rest in order, and shares any `Retry-After`
   pause between all callers of the API. Every read succeeds and the server
   never refuses one.

Prerequisites: build/install Tungsten from the repository root as described in
the [Python quickstart](../../README.md#quickstart), Python 3.12+, uv, and Bash
(Linux, macOS or WSL). The runner installs the matching runtime source and a
freshly generated SDK; it downloads Python dependencies but makes API requests
only to loopback. Port 8099 must be free (or set `BULK_SYNC_PORT`).

```sh
export PATH="/path/to/tungsten/target/debug:$PATH"
bash examples/bulk-sync/run.sh /tmp/bulk-sync-example
```

Use a **new** workspace directory each time. Expected output ends with:

```text
unbounded: 199/200 read, server saw peak 5 and refused 52
scheduled: 200/200 read, server saw peak 4 and refused 0
bulk sync stayed within the quota
```

The unbounded counts vary from run to run; the scheduled line does not.

## The configuration

[sync.py](sync.py) creates one scheduler for the quota:

```python
scheduler = RequestScheduler(
    SchedulerOptions(max_active=4, max_queued=200, max_queue_ms=60_000, retry_ratio=0.1)
)
client = RecordsClient(ClientOptions(scheduler=scheduler), base_url=base)
```

- `max_active` bounds ordinary requests in flight per scope (by default the
  client's `tenant` plus the request origin); event streams have a separate
  `max_streams` limit.
- `max_queued` bounds the requests waiting for capacity. A request beyond it
  fails at once with `RATE_LIMITED`, code `QUEUE_FULL`, and is never sent;
  so does one that waited longer than `max_queue_ms` (`QUEUE_TIMEOUT`).
- After a `429`, the scope pauses for its `Retry-After` and then lets one
  request probe the quota before resuming, so callers do not retry in a burst.
- `retry_ratio` caps the retries all callers of the scope may make together
  (here about one per ten calls, after a reserve of ten).

Share one scheduler between clients that use the same quota, and give each
client a `tenant` when quotas are per tenant or principal. `scheduler.close()`
rejects waiting work at shutdown and `scheduler.drained()` waits for requests in
flight. Nothing is coordinated across processes: run one scheduler per process
and size `max_active` for that. The TypeScript and Rust runtimes take the same
options; see each runtime README's Limits section.
