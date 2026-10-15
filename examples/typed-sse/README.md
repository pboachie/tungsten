# Typed Python server-sent events

An endpoint returning a FastAPI `StreamingResponse` needs an explicit response
contract before a generator can type its events. This local example emits one
named `step` event carrying `{index, step}`, then one named `done` event carrying
`{run_id, steps}`. These are illustrative, AOBench-shaped payloads; this is not an
AOBench server integration or a claim that its response schema has been fixed.

Prerequisites: build/install Tungsten from the repository root as described in the
[Python quickstart](../../README.md#quickstart), Python 3.12+, uv, and Bash
(Linux, macOS or WSL). The runner installs the matching runtime source and a
freshly generated SDK; it does not assume a published Python runtime exists.
It downloads Python dependencies but makes API requests only to loopback.

```sh
export PATH="/path/to/tungsten/target/debug:$PATH"
bash examples/typed-sse/run.sh /tmp/run-events-example
```

Use a **new** workspace directory each time. The runner checks and generates the
SDK, checks generation drift, exports the IR, strictly type-checks the consumer,
starts the local authenticated server and runs sync and async consumers. Expected
output includes `sync step 0 started`, `sync done demo 1` and matching `async`
lines. The server is stopped even when the consumer fails. Port 8098 must be free.
Dependencies and a Python interpreter can be installed by uv on the first run.

## Declare the upstream response, or review an overlay

The preferred correction belongs in the upstream application: see the explicit
`responses={200: {"content": {"text/event-stream": {"schema": ...}}}}` declaration
in [server.py](server.py). Running that file exports `app.openapi()` without
starting the application. Inline `oneOf` variants need no invented JSON tag;
in production, named Pydantic schemas may be referenced instead.

[prepare.py](prepare.py) writes three concrete before/after documents:

- `before.json` has no declared media type. `tungsten ir dump before.json`
  has no stream plan: endpoint names and descriptions cannot reveal a stream.
- `missing-schema.json` declares `text/event-stream` without `schema`.
  `tungsten check missing-schema.json` emits TG0530; event data is untyped JSON.
- `typed.json` declares `responses['200'].content['text/event-stream'].schema`
  as `oneOf` references to `StepEvent` and `DoneEvent`. Both have required,
  distinct properties and reject extra members.

For an upstream document you cannot immediately correct, the generated
`events.overlay.json` is a standard OpenAPI Overlay 1.0 document. Its explicit
JSONPath targets only the reviewed response and adds the media type/schema.
`tungsten.yml` applies this overlay to `before.json`. Review the actual payloads
and error responses before adopting it; overlays do not discover a contract or
verify a remote server. No upstream source code or requests are executed during
contract preparation.

Inspect `ir.json` for the `api.watchRun` operation's `stream` plan, event union
and `done: null`. Inspect generated
`run_events_sdk/resources/runs_events.py`: `list_stream(run_id=...)` returns
`EventStream[WatchRunResponse]` or `AsyncEventStream[WatchRunResponse]`.
The ordinary `list()` method is not the streaming interface. The schema union
intentionally produces TG0301 (untagged union); adding a discriminator the
server does not send would be incorrect.

## Handle the event name and payload separately

[consume.py](consume.py) passes the API key through the `ApiKey` auth scheme,
enables strict response validation, handles `Err` before accessing payloads,
then combines the wire event name with `isinstance(payload, StepEvent)` or
`isinstance(payload, DoneEvent)`. Checking `item.event == 'step'` alone does not
statically narrow the JSON payload union. An unexpected name/payload pair fails
explicitly. Never print credentials or an envelope's potentially sensitive
`received_value` to diagnose a failed request.

A named `event: done` containing JSON is delivered to the application. It is
**not** the raw `data: [DONE]` sentinel: `x-tungsten-stream-done` compares an exact
data value, not the SSE event name. This example deliberately sets no sentinel;
the consumer recognizes the terminal event, breaks and explicitly closes the
stream. EOF before that event is treated as incomplete work.

Reconnect is disabled (`max_reconnects=0`): this example's endpoint promises no
event IDs or replay semantics. Enable resumption only for an endpoint with a
documented replay contract. A `finally` block calls `close()`/`await aclose()` on
early exit and errors; client context managers release the HTTP pool. To stop a
pending async read from another task, call `cancel()`, join the reader and then
`aclose()`; do not close an async generator while another task is reading it.

This recipe exercises local success and strict consumer typing. Fragmented-frame,
malformed-payload, HTTP-error and cancellation regressions belong to the
maintainer contract suite, per [CONTRIBUTING](../../CONTRIBUTING.md#tests-are-maintained-privately).
Python 3.10 compatibility is tracked separately in
[issue #157](https://github.com/pboachie/tungsten/issues/157).
