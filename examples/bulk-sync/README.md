# Bulk sync within a quota

Bound concurrent calls, queue the rest and share 429 backoff with a request scheduler in TypeScript, Python and Rust.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; Python venv and pip, Node 22 or later with npm, and the pinned Rust toolchain and Cargo.

From the repository root:

```sh
python3 examples/run.py --only bulk-sync
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: the TypeScript, Python and Rust clients each read 200 records through a request scheduler while the loopback server sees at most 4 concurrent requests and refuses none.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

## Problem: a bulk job turns into a storm of 429s

A sync job that reads many records at once can exceed an API's concurrency
quota and turn into a burst of `429` responses and retries. The recipe starts a
loopback record store ([server.py](server.py)) that allows 4 concurrent
requests and answers `429` with `Retry-After: 1` beyond that, generates
TypeScript, Python and Rust SDKs for it, and reads 200 records in each
language through a request scheduler:

- [sync.py](sync.py) reads with 32 threads, first without a scheduler (the
  server refuses many requests and some reads may fail after their retries),
  then through one.
- [sync.mjs](sync.mjs) starts all 200 reads at once with `Promise.all`.
- [sync.rs](sync.rs) spawns all 200 reads as Tokio tasks.

Expected output:

```text
unbounded: 199/200 read, server saw peak 5 and refused 52
scheduled: 200/200 read, server saw peak 4 and refused 0
bulk sync stayed within the quota
typescript scheduled: 200/200 read, server saw peak 4 and refused 0
rust scheduled: 200/200 read, server saw peak 4 and refused 0
```

The unbounded counts vary from run to run; the scheduled lines do not.

## The configuration

Each language creates one scheduler for the quota and passes it to the client:

```python
scheduler = RequestScheduler(
    SchedulerOptions(max_active=4, max_queued=200, max_queue_ms=60_000, retry_ratio=0.1)
)
client = RecordsClient(ClientOptions(scheduler=scheduler), base_url=base)
```

```js
const scheduler = new RequestScheduler({ maxActive: 4, maxQueued: 200, maxQueueMs: 60_000, retryRatio: 0.1 });
const client = new RecordsClient({ baseUrl: base, scheduler });
```

```rust
let mut limits = SchedulerOptions::new(4);
limits.max_queued = 200;
limits.max_queue = Duration::from_secs(60);
limits.retry_ratio = Some(0.1);
let scheduler = RequestScheduler::new(limits)?;
let client = RecordsClient::new(ClientOptions { scheduler: Some(scheduler.clone()), ..Default::default() })?;
```

- `max_active` bounds ordinary requests in flight per scope (by default the
  client's `tenant` plus the request origin); event streams have a separate
  `max_streams` limit, and optional `max_active_total` / `max_streams_total`
  caps apply across every scope.
- `max_queued` bounds the requests waiting for capacity. A request beyond it
  fails at once with `RATE_LIMITED`, code `QUEUE_FULL`, and is never sent; so
  does one that waited longer than `max_queue_ms` (`QUEUE_TIMEOUT`).
- After a `429`, the scope pauses for its `Retry-After` and then lets one
  request probe the quota before resuming, so callers do not retry in a burst.
- `retry_ratio` caps the retries all callers of the scope make together (here
  about one per ten calls, after a reserve of ten).

Share one scheduler between clients that use the same quota, and give each
client a `tenant` when quotas are per tenant or principal. Closing a scheduler
rejects waiting work at shutdown; `drained` waits for requests in flight.
Nothing is coordinated across processes: run one scheduler per process and size
`max_active` for that. See each runtime README's Limits section.
