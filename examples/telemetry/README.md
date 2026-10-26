# Trace calls with local telemetry

Collect correlated operation, attempt and phase telemetry in memory from the TypeScript, Python and Rust SDKs, with nothing sent anywhere.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; Python venv and pip, Node 22 or later with npm, and the pinned Rust toolchain and Cargo.

From the repository root:

```sh
python3 examples/run.py --only telemetry
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: the TypeScript, Python and Rust clients print the same correlated operation, attempt and phase events for a read, a retried read and a missing record, with no URL or payload in any event.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

## Problem: where did the time go?

A slow or failed call can hide an authentication refresh, a wait for request
scheduler capacity, a retry after a `503` or an error status. The recipe
starts a loopback record store ([server.py](server.py)) where record 13 is
briefly unavailable once and record 404 does not exist, generates TypeScript,
Python and Rust SDKs for it, and reads records 7, 13 and 404 in each language
with an in-memory telemetry collector:

- [trace.py](trace.py), [trace.mjs](trace.mjs) and [trace.rs](trace.rs) each
  print one line per operation from the collected events, then check that no
  event holds the URL or a payload.

Expected output, identical in the three languages:

```text
call 1 api.getRecord: auth ok; queue admitted; attempt 1 ok 200; ok after 1 attempt
call 2 api.getRecord: auth ok; queue admitted; attempt 1 error UPSTREAM_UNAVAILABLE 503; retry_backoff ok; queue admitted; attempt 2 ok 200; ok after 2 attempts
call 3 api.getRecord: auth ok; queue admitted; attempt 1 error NOT_FOUND 404; error NOT_FOUND after 1 attempt
22 events, 0 dropped, no URL or payload in any event
```

## The configuration

Telemetry is off unless the client is given an observer; nothing is sent
anywhere by the runtime:

```python
collector = MemoryTelemetryCollector(256)
client = RecordsClient(ClientOptions(base_url=base, telemetry=collector))
```

```js
const collector = new MemoryTelemetryCollector(256);
const client = new RecordsClient({ baseUrl: base, telemetry: collector });
```

```rust
let collector = Arc::new(MemoryTelemetryCollector::new(256));
let client = RecordsClient::new(ClientOptions { telemetry: Some(collector.clone()), ..Default::default() })?;
```

- Every event has the same fields in the three runtimes (`schema_version` 1):
  `call_id` ties an operation's events together, `attempt` numbers what was
  sent, and `phase` events time authentication (`auth`), scheduler admission
  (`queue`) and the wait before a retry (`retry_backoff`), each with a
  `duration_ms` and an `outcome`.
- Events hold the API name, the operation id, enumerated outcomes, categories
  and reasons, HTTP status codes, counts and durations: never URLs, headers,
  bodies, argument values, credentials or error text.
- The collector keeps at most its capacity of events and counts what it
  dropped. Any function (or an object with `onEvent` / `on_event`, or a Rust
  `TelemetryObserver`) can observe instead; it is called synchronously and its
  failures never change the call. TypeScript and Python also offer an
  OpenTelemetry adapter (`openTelemetryObserver`, `open_telemetry_observer`)
  that takes a tracer without adding a dependency.
