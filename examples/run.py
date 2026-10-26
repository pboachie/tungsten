#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Run the committed gallery recipes against matching local runtime source."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time
import urllib.request

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
CATALOG = json.loads((HERE / "gallery.json").read_text())


# The Java side of the android-jni recipe. Java sources live only under
# runtimes/java/, so the runner writes these into the recipe's .work/ and the
# recipe README shows them.
JNI_JAVA = {
    "PetstoreNative.java": r"""package example.petstore;

import java.util.concurrent.locks.ReentrantReadWriteLock;

/**
 * A Petstore client backed by the native library {@code petstore_jni}. Calls block the calling
 * thread, so call them from a background thread or coroutine dispatcher, never the Android main
 * thread. Calls may run concurrently; {@link #close()} cancels calls in flight, waits for them to
 * return and frees the native handle, after which every call throws {@link IllegalStateException}.
 */
public final class PetstoreNative implements AutoCloseable {
    static {
        System.loadLibrary("petstore_jni");
    }

    private final ReentrantReadWriteLock lock = new ReentrantReadWriteLock();
    private long handle;

    /**
     * @param baseUrl the API's base URL
     * @param deadlineMs the total deadline of one call (authentication, retries and the body)
     * @param maxResponseBytes the largest response body accepted
     */
    public PetstoreNative(String baseUrl, long deadlineMs, long maxResponseBytes) {
        handle = create(baseUrl, deadlineMs, maxResponseBytes);
    }

    /** The pets as a JSON array; throws {@link PetstoreException} with the diagnostic envelope. */
    public String listPets() {
        lock.readLock().lock();
        try {
            return listPets(live());
        } finally {
            lock.readLock().unlock();
        }
    }

    @Override
    public void close() {
        // Cancel first under the read lock, so calls in flight return promptly...
        lock.readLock().lock();
        try {
            if (handle != 0) {
                cancel(handle);
            }
        } finally {
            lock.readLock().unlock();
        }
        // ...then free the handle once no call holds it.
        lock.writeLock().lock();
        try {
            if (handle != 0) {
                destroy(handle);
                handle = 0;
            }
        } finally {
            lock.writeLock().unlock();
        }
    }

    private long live() {
        if (handle == 0) {
            throw new IllegalStateException("the client is closed");
        }
        return handle;
    }

    private static native long create(String baseUrl, long deadlineMs, long maxResponseBytes);

    private static native String listPets(long handle);

    private static native void cancel(long handle);

    private static native void destroy(long handle);
}
""",
    "PetstoreException.java": r"""package example.petstore;

/** A failed call: the message is the runtime's diagnostic envelope as JSON. */
public final class PetstoreException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    public PetstoreException(String envelope) {
        super(envelope);
    }

    /** The diagnostic envelope as JSON (category, code, remediation, ...). */
    public String envelope() {
        return getMessage();
    }
}
""",
    "Demo.java": r"""package example.petstore;

import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Exercises the bridge on the development host, the way an Android app would: a typed read, error
 * mapping, cancellation, use after close and repeated create/call/close. Usage: {@code Demo
 * BASE_URL UNREACHABLE_URL}.
 */
public final class Demo {
    private static final long DEADLINE_MS = 5_000;
    private static final long MAX_RESPONSE_BYTES = 1 << 20;

    public static void main(String[] args) throws Exception {
        String base = args[0];
        String unreachable = args[1];

        try (PetstoreNative client = new PetstoreNative(base, DEADLINE_MS, MAX_RESPONSE_BYTES)) {
            String pets = client.listPets();
            check(pets.startsWith("[") && pets.endsWith("]"), "listPets returns a JSON array");
            System.out.println("listPets: a JSON array of pets");
        }

        try (PetstoreNative client = new PetstoreNative(unreachable, DEADLINE_MS, MAX_RESPONSE_BYTES)) {
            client.listPets();
            throw new AssertionError("an unreachable server must fail");
        } catch (PetstoreException e) {
            check(e.envelope().contains("\"category\":\"TRANSPORT_FAILED\""), "envelope " + e.envelope());
            System.out.println("unreachable server: PetstoreException TRANSPORT_FAILED");
        }

        // A server that accepts the connection and never answers: close() from
        // another thread cancels the call in flight long before its deadline.
        try (ServerSocket silent = new ServerSocket(0, 50, InetAddress.getLoopbackAddress())) {
            String url = "http://127.0.0.1:" + silent.getLocalPort();
            PetstoreNative client = new PetstoreNative(url, 60_000, MAX_RESPONSE_BYTES);
            AtomicReference<Throwable> outcome = new AtomicReference<>();
            Thread call = new Thread(() -> {
                try {
                    client.listPets();
                } catch (Throwable t) {
                    outcome.set(t);
                }
            });
            long start = System.nanoTime();
            call.start();
            Thread.sleep(300);
            client.close();
            call.join(10_000);
            long ms = (System.nanoTime() - start) / 1_000_000;
            check(!call.isAlive() && ms < 10_000, "the cancelled call returned after " + ms + " ms");
            check(outcome.get() instanceof PetstoreException
                    && ((PetstoreException) outcome.get()).envelope().contains("CANCELLED"),
                    "cancelled with " + outcome.get());
            System.out.println("close during a call: the call ends with CANCELLED");
        }

        PetstoreNative closed = new PetstoreNative(base, DEADLINE_MS, MAX_RESPONSE_BYTES);
        closed.close();
        closed.close();
        try {
            closed.listPets();
            throw new AssertionError("a closed client must refuse calls");
        } catch (IllegalStateException e) {
            System.out.println("after close: IllegalStateException, the native handle is never reused");
        }

        long before = nativeThreads();
        for (int i = 0; i < 50; i++) {
            try (PetstoreNative client = new PetstoreNative(base, DEADLINE_MS, MAX_RESPONSE_BYTES)) {
                client.listPets();
            }
        }
        long after = nativeThreads();
        check(before < 0 || after <= before, "threads before " + before + ", after " + after);
        System.out.println("50 create/call/close cycles: no native threads left behind");
    }

    /** The process's thread count on Linux (Android included), else -1. */
    private static long nativeThreads() throws IOException {
        Path status = Path.of("/proc/self/status");
        if (!Files.isReadable(status)) {
            return -1;
        }
        for (String line : Files.readAllLines(status)) {
            if (line.startsWith("Threads:")) {
                return Long.parseLong(line.substring("Threads:".length()).trim());
            }
        }
        return -1;
    }

    private static void check(boolean condition, String what) {
        if (!condition) {
            throw new AssertionError(what);
        }
    }
}
""",
}


TUTORIALS = {
    "mock-scenarios": """
## Saved scenarios for retry behavior

This recipe compiles the committed `openapi.json` directly; it does not write a
project manifest or generate an SDK. Each file in `scenarios/` is version 1 JSON:
responses queued on the existing mock controls, plus the calls and effects the
integration should cause. Files are data only. The integration is the separate,
stdlib-only `integration.py`, selected explicitly after `--`:

```sh
tungsten scenario validate openapi.json --scenario scenarios/lost-write.json
tungsten scenario run openapi.json --scenario scenarios/lost-write.json \\
  --timeout-ms 30000 -- python3 -I integration.py lost-write
```

The integration reads its fresh loopback URL from `TUNGSTEN_MOCK_BASE_URL`. Each
run binds its own port and mock state, so runs never share responses or counts.

| Scenario | Correct integration | `--broken` variant that must fail |
|---|---|---|
| `rate-limit.json` | retries a 429 after `Retry-After` | gives up after the 429 |
| `stream-interrupted.json` | resumes with `Last-Event-ID` after a cut stream | stops at the cut |
| `malformed.json` | reports an invalid JSON body once (`--expect-exit 3`) | retries the same request |
| `lost-write.json` | retries a dropped write with the same `Idempotency-Key` | uses a new key for the retry |

The runner validates all four files, runs each correct integration, and
requires each broken variant to fail. The lost-write broken variant matches
every call count; only `expect.effects` (`applied` 2, `replayed` 0) rejects it.
`scenario run` works on Linux, macOS and Windows; on any other platform this
recipe fails explicitly after validation. Assertions describe the mock's
observations and its own processing, not a real provider's behavior.
""",
    "input-postman": """
## Problem: a collection is not yet a client contract

You have saved requests, but want a typed client without replaying those requests
or running collection scripts. Start with the small committed `collection.json`:
it records one `GET /pets` and a saved JSON response. Its `example.invalid` URL is
a placeholder, not a service to call. The sample response is evidence of one
shape, not a promise that every response has that shape.

### Select the matching compiler in this shell

The snippets below use POSIX `sh` syntax. Replace the placeholder with the
absolute path to the compiler built for this matching checkout, then export it:

```sh
export TUNGSTEN="/absolute/path/to/matching/tungsten"
```

Use that same shell for the following commands, including the canonical runner
command above. `TUNGSTEN` selects the runner's compiler; it does not change shell
`PATH`. The standalone commands below invoke `"$TUNGSTEN"` explicitly. For a
Windows compiler used from a POSIX shell, select its absolute executable path
including `.exe`. These snippets are not PowerShell syntax or a claim of native
platform qualification.

### 1. Import offline and inspect the result

Use the canonical command above from the repository root. The recipe initializes
the **docs target**, checks the project, generates twice, compares the generated
documentation and checks freshness. It does not build or invoke a Python SDK.
Expected results here describe those source-defined checks, not a captured run.

Open `examples/input-postman/.work/project/` and review:

| File | What to check |
|---|---|
| `imported.openapi.json` | Accepted operations, parameter and response shapes, and imported-operation annotations. |
| `import-report.json` | Omitted items, skipped test scripts and unknown contract facts. An omitted request is not a supported endpoint. |
| `tungsten.yml` | Input reference, selected targets and generated output paths. |
| `agent.yml` | Authentication-related policy, safety and allowed origins before any live use. Importing cannot approve its own safety or origins. |

The importer is structural: it never executes requests, pre-request scripts,
tests or dynamic variables. Unsupported items such as script-dependent requests,
non-JSON bodies/responses and unsupported authentication helpers can be omitted;
structural or limit errors can refuse the import. Read the report even when
initialization succeeds. See [supported inputs](../../README.md#supported-inputs)
for the conversion limits and supported targets.

### 2. Review the contract before choosing a client

Compare the normalized contract with the API owner's documentation. Supply the
missing success and error responses, required and optional fields, nullability,
pagination, authentication and authorization requirements. Confirm the origin
and operation safety policy explicitly. A saved `200` example cannot establish
these facts, and generation cannot supply server-side authorization.

Keep reviewed work outside the recipe's disposable `.work/` directory. For a
separate Python project, this is the source-supported initialization route from
the repository root; choose a fresh destination:

```sh
"$TUNGSTEN" init --from examples/input-postman/collection.json --dir postman-python --targets python --python-runtime runtimes/python --no-interactive
```

Review the new project's normalized input, report and manifests as above. Then:

```sh
"$TUNGSTEN" check postman-python
"$TUNGSTEN" generate postman-python
```

The generated package is under `postman-python/generated/python`. Follow its
installation guidance and the [Python runtime source guide](../../runtimes/python/README.md)
with the matching checkout. Inspect the generated names and methods; the
`PetstoreClient` used by another gallery fixture is not a promised name for an
arbitrary imported collection. These commands do not demonstrate a successful
Postman-derived client call.

### 3. Learn the call boundary with the existing local mock

For a separate, declared OpenAPI fixture and a strict typed loopback call, use:

```sh
python3 examples/run.py --only local-mock
```

That [recipe](../local-mock/README.md) installs its generated Python client and
matching runtime, starts a loopback mock, and checks for a nonempty typed Pet
list. Its expected client message is `mock returned typed pets`; this is the
message in the recipe source, not a transcript supplied here. It uses its own
contract, not the Postman conversion above. The [first Python package](../first-python/README.md)
recipe isolates installation/import; [preview and confirmation](../preview-confirmation/README.md)
adds a separate destructive-call example. A mock call does not validate a real
API, its credentials or production readiness.

### Prerequisites, recovery and limits

Use a compiler and runtime source from the same checkout, not an unrelated
binary found on PATH. The runner accepts an absolute compiler path through
`TUNGSTEN`; building that compiler uses the repository's `rust-toolchain.toml`.
Python 3.12+ is needed for this runner; the local-call step also needs venv/pip
and may download dependencies. See the [target matrix](../../README.md#targets)
for client-specific requirements and acceptance limits.

- **Missing compiler:** select the matching compiler through `TUNGSTEN`, or build
  it as described in the [gallery prerequisites](../README.md). Installing the
  Python runtime alone does not install the compiler.
- **Omitted or refused input:** inspect the diagnostic and import report; replace
  script-dependent behavior with an explicitly reviewed contract. Do not treat
  fewer imported operations as a complete conversion.
- **Existing destination or interrupted setup:** inspect partial files and keep
  reviewed changes; use a fresh directory rather than forcing an overwrite.
- **Unexpected client failure:** inspect the returned error and contract first.
  Do not retry a mutation merely because its outcome is unknown.

All commands are terminal/text-first and require no screenshot or recording.
`--no-interactive` avoids setup prompts in the separate-project command. Before
rerunning a recipe, save anything needed from `.work/`: the runner replaces it.
If interrupted, inspect the command's processes and partial artifacts before
starting another run; this guide does not promise cross-platform cancellation
or recovery qualification. No real API, proxy/CA setup or credential flow is
exercised by the offline import.
""",
    "webhook-receiver": """
## Problem: trusting an incoming webhook

An API that calls your service back (an order shipped, a payment settled) sends
a signed request. Before acting on it, a receiver must check the signature
against the raw bytes, refuse stale and replayed deliveries, and only then
trust the payload's shape. The recipe's [openapi.json](openapi.json) declares
one OpenAPI 3.1 webhook, `order.shipped`; the generated TypeScript, Python and
Rust SDKs each get a `webhooks` module with its typed payload and a
`parseWebhook` / `parse_webhook` helper. [receive.py](receive.py),
[receive.mjs](receive.mjs) and [receive.rs](receive.rs) sign six example
deliveries locally with example secrets and pass them through that helper, with
no web framework and no network:

```text
accepted order.shipped evt_1001: ord_42 via parcelpost
rejected duplicate
rejected signature_mismatch
rejected timestamp_out_of_tolerance
rejected payload_invalid
accepted order.shipped evt_1003: ord_45 via parcelpost
```

Every language prints exactly these lines: the first delivery is accepted, its
replay is refused by the application's `claim` hook, a body changed after
signing fails the signature, a delivery signed ten minutes ago is outside the
default five-minute tolerance, a payload without `carrier` does not match the
declared schema, and a delivery signed with the previous secret is accepted
while both secrets are configured (rotation).

## Wiring it into a service

Give the helper the raw request body (bytes, before any JSON parsing), the
`Stripe-Signature` header value and the endpoint secrets:

```python
outcome = webhooks.parse_webhook("order.shipped", raw_body, signature, [current, previous], claim=claim)
```

```js
const outcome = await webhooks.parseWebhook("order.shipped", { body: rawBody, signatureHeader: signature, secrets: [current, previous], claim });
```

```rust
let outcome = parse_webhook(WebhookName::OrderShipped, &raw_body, signature, &options)?;
```

The signature scheme is the Stripe-compatible `t=<unix>,v1=<hex HMAC-SHA256 of
"t.body">` header. The helper bounds the body (1 MiB by default), compares
signatures in constant time and never puts secrets or payloads in its rejection
messages. Replay tracking, queues and the acknowledgement are the
application's: `claim` is called only for a verified, well-formed delivery, so
answer `2xx` once the event is durably handed off, and a non-`2xx` status lets
the provider redeliver. See each runtime README's Limits section.
""",
    "android-jni": """
## Problem: calling a Rust SDK from an Android app

An Android app can use a generated Rust SDK as a native library behind a small
JNI layer. That keeps one client implementation, but the app then owns a
native handle: its async runtime, its threads, how long a call may block, what
an error looks like on the Java side, and when the memory is freed. The recipe
generates the Rust SDK for the committed Petstore contract, builds
[bridge.rs](bridge.rs) as a `cdylib`, and drives it from the Java classes
listed [below](#the-java-side) against a loopback `tungsten mock`. It runs on the development host; the same
bridge cross-compiles for Android.

Expected output:

```text
listPets: a JSON array of pets
unreachable server: PetstoreException TRANSPORT_FAILED
close during a call: the call ends with CANCELLED
after close: IllegalStateException, the native handle is never reused
50 create/call/close cycles: no native threads left behind
android: cross-check not requested (set TUNGSTEN_ANDROID_TARGETS)
```

## The bridge

- **Compiler and runtime are separate.** `tungsten` runs on the development
  host and writes Rust source; only the generated SDK, the Rust runtime and
  the bridge are compiled into the app's `.so`. Nothing in the app depends on
  the compiler.
- **One handle, one runtime.** `create` builds the client and a Tokio runtime
  with one worker thread and returns a pointer as a `long`. Each call blocks
  the calling Java thread with `block_on`, so call it from a background thread
  or an `Dispatchers.IO` coroutine, never the main thread. Several threads may
  call at once.
- **Bounded calls.** The client's total `deadline` bounds each call as a whole
  (authentication, retries and backoff, the body), and `max_response_bytes`
  bounds a response. Use the runtime's own options for these rather than
  timers in Java.
- **Errors are values.** A failure throws `PetstoreException` whose message is
  the runtime's diagnostic envelope as JSON (`category`, `code`,
  `remediation`, ...), so the app can branch on `category` and show the
  remediation. A Rust panic is caught at the boundary and becomes an
  `IllegalStateException`; it never unwinds into the JVM.
- **Cancellation and cleanup.** `close()` first cancels every call in flight on
  the handle (each returns `CANCELLED` at once; the request is abandoned),
  then waits for them under a write lock and frees the handle, which stops the
  runtime's threads. After that every call throws `IllegalStateException`, so
  a late call can never reach a freed handle. Tie `close()` to the owning
  component (a `ViewModel`'s `onCleared`, a service's `onDestroy`). Android
  can kill the process without any callback; nothing here survives that, and a
  mutation that was sent when the call was cancelled or the process died keeps
  the runtime's `OUTCOME_UNKNOWN` contract: retry it only with the same
  idempotency key.

## Building for Android

Install the Rust targets and an Android NDK, then build the bridge per ABI,
for example with [cargo-ndk](https://github.com/bbqsrc/cargo-ndk):

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo ndk -t arm64-v8a -t x86_64 --platform 28 -o app/src/main/jniLibs build --release
```

This places `libpetstore_jni.so` under `jniLibs/<abi>/`, where Gradle packages
it. Copy `PetstoreNative.java` and `PetstoreException.java` from
[below](#the-java-side) into the app (they work from Kotlin unchanged) and keep their package, or rename the
`Java_example_petstore_...` functions in the bridge to match a new one.

- **ABIs.** Only `arm64-v8a` (devices) and `x86_64` (emulators) are in scope.
  `armeabi-v7a` and `x86` are not covered.
- **16 KB pages.** Android 15 devices can use 16 KB memory pages, and Google
  Play requires native libraries aligned for them. NDK r28 and later link with
  16 KB alignment by default; with an older NDK, pass
  `-C link-arg=-Wl,-z,max-page-size=16384`.
- **Cross-check here.** With the targets and an NDK compiler configured
  (`CC_aarch64_linux_android` and so on, which `ring` needs), set
  `TUNGSTEN_ANDROID_TARGETS=aarch64-linux-android,x86_64-linux-android` and the
  recipe also runs `cargo check` for each. That is compile evidence only: it
  does not link, package or run anything on a device.

## TLS and trust

The Rust runtime uses `reqwest` with `rustls` and the bundled Mozilla root
store (`webpki-roots`). It does **not** read Android's system trust store,
user-installed CAs or the app's network security configuration, and it does
not apply certificate pinning declared there. To trust a private CA, or to
pin, build a `reqwest::Client` with your roots
(`tls_built_in_root_certs(false)`, `add_root_certificate(...)`), with
`redirect(reqwest::redirect::Policy::none())`, and pass it as
`ClientOptions::http_client`.

## Credentials

The example calls an API without credentials. For OAuth, keep tokens in
application-owned secure storage: implement the runtime's `TokenStore` over
storage the app protects (for example values encrypted with an Android
Keystore key, reached through JNI) and pass it as `ClientOptions::token_store`.
Do not persist tokens in plain files or `SharedPreferences`.

## What this does not establish

The recipe proves that the bridge, the SDK and the runtime build and behave as
described on the development host, and, when requested, that they type-check
for the Android targets. It is not device or emulator qualification: network
behaviour on a device, the platform TLS stack, lifecycle interruption and
packaging are not exercised here. Qualified Android combinations are recorded
by the maintainer in the capability contract (`tungsten capabilities`) once
they exist; until then treat Android as unqualified. See the Rust runtime
README's Limits section.
""",
    "file-transfer": """
## Problem: files too large to hold in memory

Media, backups and exports should move between disk and an API without the
whole file in memory, and without leaving a half-written file behind when the
transfer fails. The recipe starts a loopback file store
([server.py](server.py)), generates TypeScript, Python and Rust SDKs for it
([openapi.json](openapi.json)) and, in each language
([transfer.py](transfer.py), [transfer.mjs](transfer.mjs),
[transfer.rs](transfer.rs)):

- writes a 16 MiB input file and uploads it from disk with a recreatable
  `UploadSource` that declares its length and SHA-256;
- downloads it with the generated download method and saves it to
  `copy.bin`, checking the SHA-256;
- shows that saving again without overwrite is refused, that a byte limit
  refuses the body from its `Content-Length`, and that a missing file is
  `NOT_FOUND`.

Expected output, the same for each language:

```text
python: uploaded 16777216 bytes, sha256 matches
python: downloaded 16777216 bytes to copy.bin, sha256 matches
python: second save refused: DESTINATION_EXISTS
python: capped download refused: RESPONSE_TOO_LARGE
python: missing file: NOT_FOUND
```

## The calls

```python
source = UploadSource.recreatable(chunks, length=SIZE, sha256=sha256)
client.files.update(name="report.bin", body=source)
client.files.get_download(name="report.bin").value.save_to("copy.bin")
```

```js
const source = UploadSource.recreatable(() => Readable.toWeb(createReadStream("input.bin")), { length: SIZE, sha256 });
await client.files.update({ name: "report.bin", body: source });
await saveDownload((await client.files.getDownload({ name: "report.bin" })).value, "copy.bin");
```

```rust
client.files().update_upload(request, source, &opts).await?;
client.files().get_download(get(), &opts, DownloadOptions::default()).await?.save_to("copy.bin", false).await?;
```

- An upload source is **one-shot** (a stream read once: a request whose body
  was read is never sent again) or **recreatable** (opened again for every
  attempt, as here). A declared length and SHA-256 are checked against the
  bytes; the last chunk is held back until they match, so a file that changed
  under the upload never completes its request.
- A download is read in chunks with backpressure, bounded by `max_bytes`
  (`maxBytes`), the idle timeout and the call deadline. Saving writes to a
  temporary `.part` file next to the destination and renames it only after
  the whole body arrived; a failure removes it and leaves the destination as
  it was. The server's suggested `filename` never chooses the destination.
- Range requests and resumable uploads are not supported. See each runtime
  README's Limits section.
""",
    "first-mcp-python": """
## An experimental native Python MCP server

The recipe generates the `python` SDK and the experimental `mcp-python` server
from the same contract, installs both with the `tungsten-mcp` runtime into a
virtual environment, and drives the server over stdio with the official MCP
Python client (`mcp==2.3.0`) against a loopback `tungsten mock`. Node is not
involved. Expected output:

```text
tools/list: pets_pets_delete, pets_pets_list, preview
pets_pets_list: typed pets as structuredContent
pets_pets_delete without a token: CONFIRMATION_REQUIRED, nothing sent
the preview token for pet-1 does not confirm pet-2
pets_pets_delete with the preview token: deleted
```

The server serves the same manifest as the `mcp` target, but only discrete
tools over stdio: progressive disclosure, Streamable HTTP, streamed results
and `run_script` are refused, never weakened (TG0729 reports them at generate
time). See the `tungsten-mcp` runtime README's Limits section and
[#154](https://github.com/pboachie/tungsten/issues/154).
""",
    "bulk-sync": """
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
""",
    "telemetry": """
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
""",
    "ci-drift": """
## Problem: the contract changed but generated output did not

You want a review or CI gate to catch a forgotten regeneration, rather than
discovering stale methods in a consumer. This recipe uses a committed OpenAPI
contract and generated **docs output** to make that failure and repair small.
Expected results below describe the recipe's checks, not a recorded CI run.

### Select the matching compiler in this shell

The snippets below use POSIX `sh` syntax. Replace the placeholder with the
absolute path to the compiler built for this matching checkout, then export it:

```sh
export TUNGSTEN="/absolute/path/to/matching/tungsten"
```

Use that same shell for the following commands, including the canonical runner
command above. `TUNGSTEN` selects the runner's compiler; it does not change shell
`PATH`. The standalone commands below invoke `"$TUNGSTEN"` explicitly. For a
Windows compiler used from a POSIX shell, select its absolute executable path
including `.exe`. These snippets are not PowerShell syntax or a claim of native
platform qualification.

### 1. Follow the stale-output cycle

Run the canonical command above from the repository root. In
`examples/ci-drift/.work/`, the runner:

1. Copies `openapi.json`, writes a docs-target manifest and validates it.
2. Generates twice, compares file hashes and checks that output is fresh.
3. Adds `GET /status` with operation ID `getStatus` and a `204` response to the
   **copied** contract. The committed input remains unchanged.
4. Requires `generate --check` to exit `1` with diagnostic `TG0901`.
5. Regenerates and requires the same freshness check to succeed.

The runner handles the deliberately failing middle command. A nonzero exit of
the whole recipe is a failure to investigate, not the expected stale stage.
Generated `sdk/llms.txt` and `sdk/tools.json` are part of this docs example;
the directory name `sdk` does not make this an SDK build or an API call.

After the recipe completes, inspect the final copied contract and output. For
your own initialized project, the corresponding repair commands are:

```sh
"$TUNGSTEN" check path/to/project
"$TUNGSTEN" generate --check path/to/project
```

If the second command reports `TG0901`, review the contract change, then run:

```sh
"$TUNGSTEN" generate path/to/project
"$TUNGSTEN" generate --check path/to/project
```

Replace `path/to/project` with the directory containing your `tungsten.yml`.
Review and commit the intended generated diff with the contract change. Do not
remove generated files from the freshness check just to make it pass. See
[surface diff](../surface-diff/README.md) for the existing semver/migration
review recipe; a fresh tree alone does not establish backward compatibility.

### 2. Compare SDK, CLI and MCP generation honestly

The committed contracts in `first-python`, `first-rust`, `first-mcp` and this
directory have identical bytes. You can use the existing recipes to inspect
how those target configurations differ:

```sh
python3 examples/run.py --only first-python,first-rust,first-mcp
```

| Recipe | What its source checks |
|---|---|
| [Python SDK](../first-python/README.md) | Installs the generated package with matching Python runtime source and imports `PetstoreClient`. |
| [Rust SDK and CLI](../first-rust/README.md) | Builds the generated workspace and runs the generated CLI's `--help`. |
| [MCP server](../first-mcp/README.md) | Builds the matching TypeScript runtime, adapter, generated SDK and server package. |

These are separate projects and `.work/` directories, not one shared manifest
or one run of the changed contract through every consumer. CLI help is not an
API operation, and an MCP package build is not a tool invocation. The docs-only
staleness cycle above does not prove SDK/CLI/MCP behavioral parity. For an
actual loopback client step defined in source, use the separate
[local-mock recipe](../local-mock/README.md); it does not call a real API.

Use Python 3.12+ with venv/pip, the repository's pinned Rust toolchain with
Cargo, and Node with npm. For MCP, Node 22 requires **22.13 or newer**, and
Node 23 requires **23.2 or newer**; consult the
[MCP runtime guide](../../runtimes/mcp/README.md) and
[target matrix](../../README.md#targets) for the precise version ranges.
The runner uses matching runtime source and can download package dependencies.
`--generate-only` omits these package builds; it is not equivalent validation.

### 3. Put the same freshness rule in your repository

The [copyable GitHub Actions workflow](../github-actions/README.md) validates
`TUNGSTEN_PROJECT` and runs `tungsten generate --check` without regenerating
files in CI. Follow that guide's copy commands and upgrade instructions. It
pins the compiler release, installer and checkout action; upgrade them together
deliberately. Its current platform is Ubuntu 24.04 x86_64, not a promise for all
hosts. It disables external emitter commands for pull-request safety, so read
its trust and skipped-target limitations before adopting it. No hosted run or
successful installation is demonstrated by this page.

### Troubleshooting and safe reruns

- **`TG0901` in your project:** inspect missing, changed or extra generated files,
  regenerate with the matching compiler, review the diff and rerun the check.
- **Different results across machines:** compare compiler/runtime source and
  toolchain versions before accepting a new output baseline. See the
  [gallery prerequisites](../README.md) for compiler selection through `TUNGSTEN`.
- **Missing package toolchain:** install the prerequisites for the selected
  recipe. Choosing `--generate-only` changes the evidence; it does not turn a
  missing build or invocation into a pass.
- **Interrupted run or local edits:** preserve useful artifacts before rerunning.
  Each recipe replaces its own `.work/`; do not keep hand-maintained code there.
  Inspect any remaining child processes before another run. No universal
  cancellation or platform-recovery result is claimed here.

The steps are text-first and use normal terminal commands. Generation freshness,
package builds, installed-consumer behavior, live API acceptance and production
readiness are separate checks; keep the [target limits](../../README.md#targets)
visible when sharing a result.
""",
}


TUTORIALS["android-jni"] += """
## The Java side

The runner writes these classes into `.work/java-src/` and compiles them with
`javac`. `Demo.java` is the host check whose output is shown above.
""" + "".join(f"\n`{name}`:\n\n```java\n{text}```\n" for name, text in JNI_JAVA.items())

def command(argv: list[str], cwd: Path, expected: int = 0) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(argv, cwd=cwd, text=True, capture_output=True, timeout=900)
    if result.returncode != expected:
        raise RuntimeError(f"{argv!r} exited {result.returncode}, expected {expected}\n{result.stdout}\n{result.stderr}")
    return result


def compiler() -> str:
    value = os.environ.get("TUNGSTEN") or shutil.which("tungsten")
    if not value:
        local = REPO / "target/debug" / ("tungsten.exe" if os.name == "nt" else "tungsten")
        if local.is_file():
            value = str(local)
    if not value:
        raise RuntimeError("Set TUNGSTEN to the matching compiler, or run cargo build -p tungsten-cli")
    return str(Path(value).resolve())


def tungsten(binary: str, cwd: Path, *args: str, expected: int = 0) -> dict:
    return json.loads(command([binary, *args, "--json"], cwd, expected).stdout)


def relative(path: Path, output: Path) -> str:
    return Path(os.path.relpath(path, output)).as_posix()


def fresh_work(example: Path) -> Path:
    work = example / ".work"
    # Verify the absolute target before replacing generated artifacts.
    if work.is_symlink() or work.resolve() != example.resolve() / ".work":
        raise RuntimeError(f"refusing redirected work directory: {work}")
    if work.exists():
        shutil.rmtree(work)
    work.mkdir()
    return work


def target_options(target: str, work: Path) -> dict:
    out = work / "sdk"
    runtime = REPO / "runtimes"
    options: dict = {"out": "sdk"}
    if target == "typescript":
        options.update(package="@gallery/petstore", runtime_path=relative(runtime / "ts", out))
    elif target == "rust":
        options.update(runtime_path=relative(runtime / "rust/runtime", out / "petstore-sdk"),
                       cli={"kit_path": relative(runtime / "rust/cli-kit", out / "petstore-cli")})
    elif target in {"java", "csharp", "ruby"}:
        options["runtime_path"] = relative(runtime / target, out)
    elif target == "go":
        emitter = HERE / ".cache" / ("tungsten-emit-go.exe" if os.name == "nt" else "tungsten-emit-go")
        emitter.parent.mkdir(exist_ok=True)
        command(["go", "build", "-o", str(emitter), "."], REPO / "emitters/go")
        options.update(external=f'"{emitter}"', module="example.com/gallery/petstore",
                       package="petstore", runtime_path=relative(runtime / "go", out))
    elif target in {"mcp", "webmcp"}:
        options.update(package=f"@gallery/petstore-{target}", sdk="@gallery/petstore",
                       sdk_path="../typescript", runtime_path=relative(runtime / target, out),
                       sdk_runtime_path=relative(runtime / "ts", out))
    return options


def manifest(target: str, work: Path) -> dict:
    targets = {target: target_options(target, work)}
    if target in {"mcp", "webmcp"}:
        ts = target_options("typescript", work)
        ts.update(out="typescript", runtime_path=relative(REPO / "runtimes/ts", work / "typescript"))
        targets = {"typescript": ts, **targets}
    return {"tungsten": 1, "api": {"name": "petstore", "title": "Gallery Petstore"},
            "inputs": [{"namespace": "pets", "spec": "openapi.json"}],
            "servers": {"default": "http://127.0.0.1:4010", "allow_override": True},
            "targets": targets}


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n")


def python_environment(work: Path) -> Path:
    venv = work / "venv"
    python = venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    uv = shutil.which("uv")
    bootstrap = REPO / "crates/tungsten-cli/src/commands/bootstrap_python.py"
    if uv:
        command([sys.executable, "-I", str(bootstrap), "--check-uv", uv], work)
        command([uv, "venv", "--python", sys.executable, str(venv)], work)
        install = [uv, "pip", "install", "--python", str(python)]
    else:
        command([sys.executable, "-m", "venv", str(venv)], work)
        command([str(python), "-I", str(bootstrap)], work)
        install = [str(python), "-m", "pip", "install", "--disable-pip-version-check"]
    command([*install, "-e", str(REPO / "runtimes/python"), "-e", str(work / "sdk")], work)
    return python


def npm_build(path: Path) -> None:
    action = "ci" if (path / "package-lock.json").is_file() else "install"
    command(["npm", action, "--ignore-scripts", "--no-audit", "--no-fund"], path)
    command(["npm", "run", "build"], path)


def build(target: str, work: Path) -> None:
    sdk = work / "sdk"
    if target == "python":
        python = python_environment(work)
        command([str(python), "-c", "from petstore_sdk import PetstoreClient; print(PetstoreClient.__name__)"], work)
    elif target in {"typescript", "mcp", "webmcp"}:
        npm_build(REPO / "runtimes/ts")
        if target != "typescript":
            npm_build(REPO / "runtimes" / target)
            npm_build(work / "typescript")
        npm_build(sdk)
        if target == "typescript":
            command(["node", "--input-type=module", "-e", "import {PetstoreClient} from './dist/index.js'; console.log(PetstoreClient.name)"], sdk)
    elif target == "rust":
        command(["cargo", "build", "--workspace"], sdk)
        command(["cargo", "run", "--quiet", "-p", "petstore-cli", "--", "--help"], sdk)
    elif target == "go":
        command(["go", "build", "./..."], sdk)
    elif target == "java":
        command(["mvn", "-B", "-q", "package", "-DskipTests"], sdk)
    elif target == "csharp":
        command(["dotnet", "build", str(next(sdk.glob("*.csproj"))), "--nologo",
                 "--artifacts-path", str(work / "artifacts")], sdk)
    elif target == "ruby":
        command(["bundle", "install"], sdk)
        command(["bundle", "exec", "ruby", "-Ilib", "-e", "require 'petstore_sdk'"], sdk)


@contextlib.contextmanager
def mock(binary: str, work: Path):
    with (work / "mock.log").open("w+") as log:
        process = subprocess.Popen([binary, "mock", "tungsten.yml", "--port", "0", "--json"],
                                   cwd=work, stdin=subprocess.PIPE, stdout=log, stderr=log, text=True)
        try:
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                log.seek(0)
                for line in log.read().splitlines():
                    if line.startswith('{"tool"'):
                        yield json.loads(line)["result"]["base_url"]
                        return
                if process.poll() is not None:
                    raise RuntimeError("mock exited before listening")
                time.sleep(0.05)
            raise RuntimeError("mock did not listen within 20 seconds")
        finally:
            if process.stdin:
                process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)


def local_client(binary: str, work: Path, preview: bool) -> None:
    python = python_environment(work)
    script = '''from petstore_sdk import PetstoreClient
from tungsten_runtime import Err
import sys
with PetstoreClient(base_url=sys.argv[1], validate_responses="strict") as client:
    result = client.pets.list()
    if isinstance(result, Err):
        raise RuntimeError(str(result.error))
    assert len(result.value) > 0
    print("mock returned typed pets")
'''
    if preview:
        script += '''    rejected = client.pets.delete(pet_id="pet-1")
    assert isinstance(rejected, Err) and rejected.error["category"] == "CONFIRMATION_REQUIRED"
    preview = client.pets.preview_delete(pet_id="pet-1")
    if isinstance(preview, Err):
        raise RuntimeError(str(preview.error))
    token = preview.value["confirmation_token"]
    assert isinstance(token, str) and token
    result = client.pets.delete(pet_id="pet-1", opts={"confirm": token})
    assert not isinstance(result, Err), result
    print("preview confirmed the same destructive arguments")
'''
    (work / "client.py").write_text(script)
    with mock(binary, work) as base:
        command([str(python), str(work / "client.py"), base], work)
        with urllib.request.urlopen(base + "/__tungsten/calls", timeout=10) as response:
            calls = json.load(response)
        deletes = [call for call in calls if call["method"] == "DELETE"]
        assert len(deletes) == (1 if preview else 0), "preview or missing confirmation executed a mutation"


SCENARIOS = {"rate-limit": 0, "stream-interrupted": 0, "malformed": 3, "lost-write": 0}


def scenario_run(binary: str, work: Path, name: str, broken: bool) -> dict:
    # --json precedes the subcommand: everything after `--` is the integration argv.
    argv = [binary, "--json", "scenario", "run", "openapi.json", "--scenario", f"scenarios/{name}.json",
            "--timeout-ms", "30000", "--expect-exit", str(SCENARIOS[name]),
            "--", sys.executable, "-I", str(work / "integration.py"), name]
    result = command(argv + (["--broken"] if broken else []), work, 1 if broken else 0)
    return json.loads(result.stdout)["result"]["run"]


def scenarios(binary: str, work: Path) -> None:
    for name in SCENARIOS:
        tungsten(binary, work, "scenario", "validate", "openapi.json", "--scenario", f"scenarios/{name}.json")
    if not sys.platform.startswith(("linux", "darwin", "win32")):
        raise RuntimeError("scenario files validated, but scenario run is available on Linux, macOS and Windows only")
    for name in SCENARIOS:
        run = scenario_run(binary, work, name, broken=False)
        assert run["passed"] and run["reason"] == "completed", (name, run)
        broken = scenario_run(binary, work, name, broken=True)
        assert not broken["passed"] and not broken["scenario"]["passed"], (name, broken)
    # A new idempotency key on the retry matches every call count; only the
    # effect assertion distinguishes it from a correct replay.
    lost = scenario_run(binary, work, "lost-write", broken=True)["scenario"]
    assert lost["total_calls"]["passed"] and all(c["passed"] for c in lost["calls"]), lost
    assert lost["effects"][0]["applied"]["actual"] == 2, lost


@contextlib.contextmanager
def quota_server(example: Path, work: Path):
    """An example's loopback server on a free port; yields its base URL."""
    with (work / "server.log").open("w+") as log:
        process = subprocess.Popen([sys.executable, "-I", str(example / "server.py"), "0"],
                                   cwd=work, stdout=subprocess.PIPE, stderr=log, text=True)
        try:
            line = process.stdout.readline() if process.stdout else ""
            if not line.startswith("listening on "):
                raise RuntimeError(f"the {example.name} server exited before listening")
            yield line.removeprefix("listening on ").strip()
        finally:
            process.terminate()
            process.wait(timeout=10)


def bulk_sync(binary: str, example: Path, work: Path, build_packages: bool) -> None:
    runtime = REPO / "runtimes"
    shutil.copyfile(example / "openapi.json", work / "openapi.json")
    write_json(work / "tungsten.yml", {
        "tungsten": 1, "api": {"name": "records", "title": "Records"},
        "inputs": [{"namespace": "api", "spec": "openapi.json"}],
        "targets": {
            "python": {"package": "records-sdk", "module": "records_sdk", "out": "sdk"},
            "typescript": {"package": "@gallery/records", "out": "typescript",
                           "runtime_path": relative(runtime / "ts", work / "typescript")},
            "rust": {"out": "rust", "runtime_path": relative(runtime / "rust/runtime", work / "rust/records-sdk"),
                     "cli": {"kit_path": relative(runtime / "rust/cli-kit", work / "rust/records-cli")}},
        },
    })
    outputs = ("sdk", "typescript", "rust")
    tungsten(binary, work, "check")
    tungsten(binary, work, "generate")
    first = {name: digest_tree(work / name) for name in outputs}
    assert all(first.values()), "a target produced no files; inspect skipped/refused target diagnostics"
    tungsten(binary, work, "generate")
    assert first == {name: digest_tree(work / name) for name in outputs}, "regeneration was not deterministic"
    tungsten(binary, work, "generate", "--check")
    if not build_packages:
        return
    python = python_environment(work)
    npm_build(REPO / "runtimes/ts")
    npm_build(work / "typescript")
    shutil.copyfile(example / "sync.mjs", work / "typescript/sync.mjs")
    consumer = work / "consumer"
    consumer.mkdir()
    shutil.copyfile(example / "sync.rs", consumer / "sync.rs")
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "bulk-sync"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "bulk-sync"
path = "sync.rs"

[dependencies]
records-sdk = {{ path = "../rust/records-sdk" }}
tungsten-runtime = {{ path = "{relative(runtime / "rust/runtime", consumer)}" }}
reqwest = {{ version = "0.12", default-features = false, features = ["json"] }}
serde_json = "1"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}

[workspace]
''')
    command(["cargo", "build", "--quiet"], consumer)
    with quota_server(example, work) as base:
        for argv, cwd in (([str(python), str(example / "sync.py"), base], work),
                          (["node", "sync.mjs", base], work / "typescript"),
                          (["cargo", "run", "--quiet", "--", base], consumer)):
            print(command(argv, cwd).stdout, end="", flush=True)


FILE_TRANSFER_OUTCOME = """python: uploaded 16777216 bytes, sha256 matches
python: downloaded 16777216 bytes to copy.bin, sha256 matches
python: second save refused: DESTINATION_EXISTS
python: capped download refused: RESPONSE_TOO_LARGE
python: missing file: NOT_FOUND
"""


def file_transfer(binary: str, example: Path, work: Path, build_packages: bool) -> None:
    runtime = REPO / "runtimes"
    shutil.copyfile(example / "openapi.json", work / "openapi.json")
    write_json(work / "tungsten.yml", {
        "tungsten": 1, "api": {"name": "files", "title": "Files"},
        "inputs": [{"namespace": "files", "spec": "openapi.json"}],
        "targets": {
            "python": {"package": "files-sdk", "module": "files_sdk", "out": "sdk"},
            "typescript": {"package": "@gallery/files", "out": "typescript",
                           "runtime_path": relative(runtime / "ts", work / "typescript")},
            "rust": {"out": "rust", "runtime_path": relative(runtime / "rust/runtime", work / "rust/files-sdk"),
                     "cli": {"kit_path": relative(runtime / "rust/cli-kit", work / "rust/files-cli")}},
        },
    })
    outputs = ("sdk", "typescript", "rust")
    tungsten(binary, work, "check")
    tungsten(binary, work, "generate")
    first = {name: digest_tree(work / name) for name in outputs}
    assert all(first.values()), "a target produced no files; inspect skipped/refused target diagnostics"
    tungsten(binary, work, "generate")
    assert first == {name: digest_tree(work / name) for name in outputs}, "regeneration was not deterministic"
    tungsten(binary, work, "generate", "--check")
    if not build_packages:
        return
    python = python_environment(work)
    npm_build(REPO / "runtimes/ts")
    npm_build(work / "typescript")
    shutil.copyfile(example / "transfer.mjs", work / "typescript/transfer.mjs")
    consumer = work / "consumer"
    consumer.mkdir()
    shutil.copyfile(example / "transfer.rs", consumer / "transfer.rs")
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "file-transfer"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "file-transfer"
path = "transfer.rs"

[dependencies]
files-sdk = {{ path = "../rust/files-sdk" }}
tungsten-runtime = {{ path = "{relative(runtime / "rust/runtime", consumer)}" }}
serde_json = "1"
sha2 = "0.10"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}

[workspace]
''')
    command(["cargo", "build", "--quiet"], consumer)
    scratch = {"python": work / "python-run", "typescript": work / "typescript", "rust": consumer}
    scratch["python"].mkdir()
    with quota_server(example, work) as base:
        outputs = {}
        for language, argv in (("python", [str(python), str(example / "transfer.py"), base]),
                               ("typescript", ["node", "transfer.mjs", base]),
                               ("rust", ["cargo", "run", "--quiet", "--", base])):
            outputs[language] = command(argv, scratch[language]).stdout
            print(outputs[language], end="", flush=True)
    for language, output in outputs.items():
        expected = FILE_TRANSFER_OUTCOME.replace("python:", f"{language}:")
        assert output == expected, f"{language} printed {output!r}, expected {expected!r}"


TELEMETRY_OUTCOME = """call 1 api.getRecord: auth ok; queue admitted; attempt 1 ok 200; ok after 1 attempt
call 2 api.getRecord: auth ok; queue admitted; attempt 1 error UPSTREAM_UNAVAILABLE 503; retry_backoff ok; \
queue admitted; attempt 2 ok 200; ok after 2 attempts
call 3 api.getRecord: auth ok; queue admitted; attempt 1 error NOT_FOUND 404; error NOT_FOUND after 1 attempt
22 events, 0 dropped, no URL or payload in any event
"""


def telemetry_trace(binary: str, example: Path, work: Path, build_packages: bool) -> None:
    runtime = REPO / "runtimes"
    shutil.copyfile(example / "openapi.json", work / "openapi.json")
    write_json(work / "tungsten.yml", {
        "tungsten": 1, "api": {"name": "records", "title": "Records"},
        "inputs": [{"namespace": "api", "spec": "openapi.json"}],
        "targets": {
            "python": {"package": "records-sdk", "module": "records_sdk", "out": "sdk"},
            "typescript": {"package": "@gallery/records", "out": "typescript",
                           "runtime_path": relative(runtime / "ts", work / "typescript")},
            "rust": {"out": "rust", "runtime_path": relative(runtime / "rust/runtime", work / "rust/records-sdk"),
                     "cli": {"kit_path": relative(runtime / "rust/cli-kit", work / "rust/records-cli")}},
        },
    })
    outputs = ("sdk", "typescript", "rust")
    tungsten(binary, work, "check")
    tungsten(binary, work, "generate")
    first = {name: digest_tree(work / name) for name in outputs}
    assert all(first.values()), "a target produced no files; inspect skipped/refused target diagnostics"
    tungsten(binary, work, "generate")
    assert first == {name: digest_tree(work / name) for name in outputs}, "regeneration was not deterministic"
    tungsten(binary, work, "generate", "--check")
    if not build_packages:
        return
    python = python_environment(work)
    npm_build(REPO / "runtimes/ts")
    npm_build(work / "typescript")
    shutil.copyfile(example / "trace.mjs", work / "typescript/trace.mjs")
    consumer = work / "consumer"
    consumer.mkdir()
    shutil.copyfile(example / "trace.rs", consumer / "trace.rs")
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "telemetry-trace"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "telemetry-trace"
path = "trace.rs"

[dependencies]
records-sdk = {{ path = "../rust/records-sdk" }}
tungsten-runtime = {{ path = "{relative(runtime / "rust/runtime", consumer)}" }}
reqwest = {{ version = "0.12", default-features = false }}
serde_json = "1"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}

[workspace]
''')
    command(["cargo", "build", "--quiet"], consumer)
    with quota_server(example, work) as base:
        for language, argv, cwd in (("python", [str(python), str(example / "trace.py"), base], work),
                                    ("typescript", ["node", "trace.mjs", base], work / "typescript"),
                                    ("rust", ["cargo", "run", "--quiet", "--", base], consumer)):
            printed = command(argv, cwd).stdout
            assert printed == TELEMETRY_OUTCOME, f"{language} trace printed:\n{printed}"
            print(f"{language}:\n{printed}", end="", flush=True)


WEBHOOK_OUTCOME = """accepted order.shipped evt_1001: ord_42 via parcelpost
rejected duplicate
rejected signature_mismatch
rejected timestamp_out_of_tolerance
rejected payload_invalid
accepted order.shipped evt_1003: ord_45 via parcelpost
"""


def webhook_receiver(binary: str, example: Path, work: Path, build_packages: bool) -> None:
    runtime = REPO / "runtimes"
    shutil.copyfile(example / "openapi.json", work / "openapi.json")
    write_json(work / "tungsten.yml", {
        "tungsten": 1, "api": {"name": "orders", "title": "Orders"},
        "inputs": [{"namespace": "api", "spec": "openapi.json"}],
        "targets": {
            "python": {"package": "orders-sdk", "module": "orders_sdk", "out": "sdk"},
            "typescript": {"package": "@gallery/orders", "out": "typescript",
                           "runtime_path": relative(runtime / "ts", work / "typescript")},
            "rust": {"out": "rust", "runtime_path": relative(runtime / "rust/runtime", work / "rust/orders-sdk"),
                     "cli": {"kit_path": relative(runtime / "rust/cli-kit", work / "rust/orders-cli")}},
        },
    })
    outputs = ("sdk", "typescript", "rust")
    tungsten(binary, work, "check")
    tungsten(binary, work, "generate")
    first = {name: digest_tree(work / name) for name in outputs}
    assert all(first.values()), "a target produced no files; inspect skipped/refused target diagnostics"
    tungsten(binary, work, "generate")
    assert first == {name: digest_tree(work / name) for name in outputs}, "regeneration was not deterministic"
    tungsten(binary, work, "generate", "--check")
    if not build_packages:
        return
    python = python_environment(work)
    npm_build(REPO / "runtimes/ts")
    npm_build(work / "typescript")
    shutil.copyfile(example / "receive.mjs", work / "typescript/receive.mjs")
    consumer = work / "consumer"
    consumer.mkdir()
    shutil.copyfile(example / "receive.rs", consumer / "receive.rs")
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "webhook-receiver"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "webhook-receiver"
path = "receive.rs"

[dependencies]
orders-sdk = {{ path = "../rust/orders-sdk" }}
tungsten-runtime = {{ path = "{relative(runtime / "rust/runtime", consumer)}" }}

[workspace]
''')
    command(["cargo", "build", "--quiet"], consumer)
    for language, argv, cwd in (("python", [str(python), str(example / "receive.py")], work),
                                ("typescript", ["node", "receive.mjs"], work / "typescript"),
                                ("rust", ["cargo", "run", "--quiet"], consumer)):
        printed = command(argv, cwd).stdout
        assert printed == WEBHOOK_OUTCOME, f"{language} receiver printed:\n{printed}"
        print(f"{language}: " + printed.replace("\n", "; ").rstrip("; "), flush=True)


def android_jni(binary: str, example: Path, work: Path, build_packages: bool) -> None:
    shutil.copyfile(example / "openapi.json", work / "openapi.json")
    write_json(work / "tungsten.yml", manifest("rust", work))
    tungsten(binary, work, "check")
    tungsten(binary, work, "generate")
    first = digest_tree(work / "sdk")
    assert first, "the rust target produced no files; inspect skipped/refused target diagnostics"
    tungsten(binary, work, "generate")
    assert first == digest_tree(work / "sdk"), "regeneration was not deterministic"
    tungsten(binary, work, "generate", "--check")
    if not build_packages:
        return
    bridge = work / "bridge"
    bridge.mkdir()
    shutil.copyfile(example / "bridge.rs", bridge / "bridge.rs")
    (bridge / "Cargo.toml").write_text(f'''[package]
name = "petstore-jni"
version = "0.0.0"
edition = "2024"
publish = false

[lib]
name = "petstore_jni"
path = "bridge.rs"
crate-type = ["cdylib"]

[dependencies]
petstore-sdk = {{ path = "../sdk/petstore-sdk" }}
jni = "0.21"
serde_json = "1"
tokio = {{ version = "1", features = ["rt-multi-thread", "sync", "macros"] }}

[workspace]
''')
    command(["cargo", "build", "--quiet"], bridge)
    java = work / "java"
    package = work / "java-src/example/petstore"
    package.mkdir(parents=True)
    for name, text in JNI_JAVA.items():
        (package / name).write_text(text)
    command(["javac", "-d", str(java), *sorted(str(p) for p in package.iterdir())], work)
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        unreachable = f"http://127.0.0.1:{probe.getsockname()[1]}"
    with mock(binary, work) as base:
        demo = command(["java", f"-Djava.library.path={bridge / 'target/debug'}", "-cp", str(java),
                        "example.petstore.Demo", base, unreachable], work)
        print(demo.stdout, end="", flush=True)
    # Cross-checking for Android needs the Rust targets and an NDK C compiler
    # (for ring); it runs only when requested.
    targets = [t for t in os.environ.get("TUNGSTEN_ANDROID_TARGETS", "").split(",") if t]
    for target in targets:
        command(["cargo", "check", "--quiet", "--target", target], bridge)
        print(f"android: the SDK, runtime and bridge type-check for {target}", flush=True)
    if not targets:
        print("android: cross-check not requested (set TUNGSTEN_ANDROID_TARGETS)", flush=True)


PYTHON_MCP_CLIENT = r'''import json, sys
import anyio
from mcp.client.session import ClientSession
from mcp.client.stdio import StdioServerParameters, stdio_client


async def main(command: str, base: str) -> None:
    server = StdioServerParameters(command=command, env={"PETSTORE_BASE_URL": base, "PETSTORE_MCP_DEADLINE_MS": "10000"})
    async with stdio_client(server) as (read, write), ClientSession(read, write) as session:
        await session.initialize()
        tools = {tool.name: tool for tool in (await session.list_tools()).tools}
        print("tools/list:", ", ".join(sorted(tools)))
        listed = await session.call_tool("pets_pets_list", {})
        assert not listed.is_error and listed.structured_content["value"], listed
        print("pets_pets_list: typed pets as structuredContent")
        refused = await session.call_tool("pets_pets_delete", {"pet_id": "pet-1"})
        assert refused.is_error and refused.structured_content["category"] == "CONFIRMATION_REQUIRED", refused
        print("pets_pets_delete without a token: CONFIRMATION_REQUIRED, nothing sent")
        preview = await session.call_tool("preview", {"tool": "pets_pets_delete", "arguments": {"pet_id": "pet-1"}})
        token = preview.structured_content["confirmation_token"]
        assert isinstance(token, str) and token, preview
        other = await session.call_tool("pets_pets_delete", {"pet_id": "pet-2", "confirmation_token": token})
        assert other.is_error and other.structured_content["category"] == "CONFIRMATION_REQUIRED", other
        print("the preview token for pet-1 does not confirm pet-2")
        done = await session.call_tool("pets_pets_delete", {"pet_id": "pet-1", "confirmation_token": token})
        assert not done.is_error, done
        print("pets_pets_delete with the preview token: deleted")


anyio.run(main, sys.argv[1], sys.argv[2])
'''


def python_mcp(binary: str, example: Path, work: Path, build_packages: bool) -> None:
    shutil.copyfile(example / "openapi.json", work / "openapi.json")
    runtime = REPO / "runtimes"
    write_json(work / "tungsten.yml", {
        "tungsten": 1, "api": {"name": "petstore", "title": "Gallery Petstore"},
        "inputs": [{"namespace": "pets", "spec": "openapi.json"}],
        "servers": {"default": "http://127.0.0.1:4010", "allow_override": True},
        "targets": {
            "python": {"package": "petstore-sdk", "module": "petstore_sdk", "out": "sdk"},
            "mcp-python": {"out": "mcp-python",
                           "runtime_path": relative(runtime / "mcp-python", work / "mcp-python"),
                           "sdk_path": "../sdk",
                           "sdk_runtime_path": relative(runtime / "python", work / "mcp-python")},
        },
    })
    outputs = ("sdk", "mcp-python")
    tungsten(binary, work, "check")
    tungsten(binary, work, "generate")
    first = {name: digest_tree(work / name) for name in outputs}
    assert all(first.values()), "a target produced no files; inspect skipped/refused target diagnostics"
    tungsten(binary, work, "generate")
    assert first == {name: digest_tree(work / name) for name in outputs}, "regeneration was not deterministic"
    tungsten(binary, work, "generate", "--check")
    if not build_packages:
        return
    python = python_environment(work)
    # Installed from the local runtime and the generated package; no Node.
    uv = shutil.which("uv")
    install = ([uv, "pip", "install", "--python", str(python)] if uv
               else [str(python), "-m", "pip", "install", "--disable-pip-version-check"])
    command([*install, str(runtime / "mcp-python"), str(work / "mcp-python")], work)
    (work / "mcp_client.py").write_text(PYTHON_MCP_CLIENT)
    server = python.parent / ("petstore-mcp.exe" if os.name == "nt" else "petstore-mcp")
    with mock(binary, work) as base:
        print(command([str(python), str(work / "mcp_client.py"), str(server), base], work).stdout, end="", flush=True)
        with urllib.request.urlopen(base + "/__tungsten/calls", timeout=10) as response:
            calls = json.load(response)
        deletes = [call for call in calls if call["method"] == "DELETE"]
        assert len(deletes) == 1, f"expected exactly one confirmed DELETE, the server saw {len(deletes)}"


def digest_tree(root: Path) -> dict[str, str]:
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob("*") if p.is_file()}


def execute(entry: dict, binary: str, build_packages: bool) -> None:
    example = HERE / entry["name"]
    work = fresh_work(example)
    mode = entry["mode"]
    if mode == "bulk-sync":
        bulk_sync(binary, example, work, build_packages)
        return
    if mode == "file-transfer":
        file_transfer(binary, example, work, build_packages)
        return
    if mode == "webhooks":
        webhook_receiver(binary, example, work, build_packages)
        return
    if mode == "telemetry":
        telemetry_trace(binary, example, work, build_packages)
        return
    if mode == "mcp-python":
        python_mcp(binary, example, work, build_packages)
        return
    if mode == "android-jni":
        android_jni(binary, example, work, build_packages)
        return
    if mode == "input":
        args = ["init", "--from", str(example / entry["input"]), "--dir", str(work / "project"),
                "--targets", "docs", "--no-interactive", *entry.get("args", [])]
        tungsten(binary, work, *args)
        project = work / "project"
        assert (project / "tungsten.yml").is_file() and (project / "agent.yml").is_file()
        tungsten(binary, project, "check")
        tungsten(binary, project, "generate")
        docs = next(project.rglob("llms.txt")).parent
        first = digest_tree(docs)
        tungsten(binary, project, "generate")
        assert first == digest_tree(docs), "input regeneration was not deterministic"
        tungsten(binary, project, "generate", "--check")
        assert list(project.rglob("llms.txt")), "docs were not generated"
        return
    if mode == "scenario":
        for name in ("openapi.json", "integration.py"):
            shutil.copyfile(example / name, work / name)
        shutil.copytree(example / "scenarios", work / "scenarios")
        scenarios(binary, work)
        return
    target = entry.get("target", "python" if mode in {"mock", "preview"} else "docs")
    shutil.copyfile(example / "openapi.json", work / "openapi.json")
    write_json(work / "tungsten.yml", manifest(target, work))
    if mode == "preview":
        shutil.copyfile(example / "agent.yml", work / "agent.yml")
    tungsten(binary, work, "check")
    tungsten(binary, work, "generate")
    first = digest_tree(work / "sdk")
    assert first, "the selected target produced no files; inspect skipped/refused target diagnostics"
    tungsten(binary, work, "generate")
    assert first == digest_tree(work / "sdk"), "regeneration was not deterministic"
    tungsten(binary, work, "generate", "--check")
    if target == "docs":
        assert (work / "sdk/llms.txt").is_file() and (work / "sdk/tools.json").is_file()
    if mode in {"drift", "diff"}:
        changed = json.loads((work / "openapi.json").read_text())
        changed["paths"]["/status"] = {"get": {"operationId": "getStatus", "responses": {"204": {"description": "healthy"}}}}
        write_json(work / "openapi.json", changed)
        if mode == "drift":
            stale = tungsten(binary, work, "generate", "--check", expected=1)
            assert any(d["code"] == "TG0901" for d in stale["diagnostics"])
            tungsten(binary, work, "generate")
            tungsten(binary, work, "generate", "--check")
        else:
            result = tungsten(binary, work, "diff", "--semver")
            assert result["result"]["level"] == "minor", result
            migration = tungsten(binary, work, "diff", "--migration")
            assert any(c["semver"]["migration"]["actions"] for c in migration["result"]["changes"])
    elif mode in {"mock", "preview"}:
        local_client(binary, work, mode == "preview")
    elif build_packages:
        build(target, work)


def example_readme(entry: dict) -> str:
    return f'''# {entry["title"]}

{entry["description"]}

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; {entry["prerequisites"]}.

From the repository root:

```sh
python3 examples/run.py --only {entry["name"]}
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: {entry["outcome"]}.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.
''' + TUTORIALS.get(entry["name"], "")


def index_readme() -> str:
    rows = "\n".join(f'| [{e["title"]}]({e["name"]}/README.md) | {e["description"]} |' for e in CATALOG)
    return f'''# Runnable example gallery

Start with a matching compiler and source checkout (`cargo build -p tungsten-cli`
or `TUNGSTEN=/absolute/path/to/tungsten`). Each recipe has a committed small
Petstore-shaped input, complete prerequisites and a checked outcome. Run one
from its README, or run all recipes from the repository root:

```sh
python3 examples/run.py --check
```

The full run needs Python 3.12 with venv/pip, Node 22 with npm, the pinned Rust
toolchain, Go 1.22, Java 17 with Maven, .NET 10 and Ruby 3.3 with Bundler. It builds
generated packages against matching runtime source, including the Rust CLI.
It contacts package registries and loopback mocks, never an external API.
For compiler/input/workflow smoke coverage without first-SDK package builds,
use `--generate-only`. Use `--only NAME` (repeatable or comma-separated) to select
recipes. Missing tools fail explicitly; no entry is silently skipped.

The runner checks this index and every recipe README against the catalog,
then executes the described outcomes. CI runs all gallery recipes with package
builds enabled. Generated work stays below each recipe's ignored `.work/`;
canonical inputs are never modified.

| Recipe | What it demonstrates |
|---|---|
{rows}

JSON Schema needs explicit HTTP routing: its recipe uses a versioned endpoint
manifest with declared models, rather than guessing an API from a bare schema.
Imported observations need review before live use; see
[supported inputs](../README.md#supported-inputs).

The [target matrix](../README.md#targets) and [#61](https://github.com/pboachie/tungsten/issues/61)
own readiness claims. A sample does not establish registry publication, complete
validation parity or live acceptance.

Additional examples have their own runners and prerequisites:

- [Manual release-video walkthrough](release-video/README.md): version-audited Windows terminal recording, local narration, chapters and review-only delivery artifacts.
- [Six public APIs](showcase/README.md): pinned specifications and regenerated showcase.
- [External Markdown emitter](external-emitter-markdown/README.md): implement the emitter protocol.
- [Typed Python SSE](typed-sse/README.md): declare and consume named streams with explicit cleanup.
- [Response-contract quality](response-contract-audit/README.md): audit weak responses and apply reviewed schema overlays.
- [GitHub Actions workflow](github-actions/README.md): a copyable workflow, inline or through the reusable [`actions/tungsten`](../actions/tungsten/README.md) action, that installs a pinned release, validates the contract and fails on stale generated output or a stale lock.
- [Reuse one configuration profile](profile-reuse/README.md): share reviewed defaults across an app and a multi-target library, with explicit per-project overrides.
- [Restricted agent integration](restricted-agent/README.md): an MCP server that reads freely, confirms writes and never offers deletion, with transport references from a shared profile.
'''


def documentation(write: bool) -> None:
    outputs = {HERE / "README.md": index_readme()}
    outputs.update({HERE / e["name"] / "README.md": example_readme(e) for e in CATALOG})
    for path, expected in outputs.items():
        if write:
            path.write_text(expected)
        elif not path.is_file() or path.read_text() != expected:
            raise RuntimeError(f"gallery README drift: {path}; run python3 examples/run.py --write-index")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--only", action="append", default=[])
    parser.add_argument("--generate-only", action="store_true")
    parser.add_argument("--check", action="store_true", help="check README drift and run recipes (the default)")
    parser.add_argument("--write-index", action="store_true", help="rewrite gallery READMEs only")
    parser.add_argument("--list", action="store_true")
    args = parser.parse_args()
    if args.list:
        print("\n".join(e["name"] for e in CATALOG))
        return
    documentation(args.write_index)
    if args.write_index:
        return
    selected = {name for group in args.only for name in group.split(",")}
    known = {e["name"] for e in CATALOG}
    if selected - known:
        raise RuntimeError(f"unknown recipes: {', '.join(sorted(selected - known))}")
    binary = compiler()
    for entry in CATALOG:
        if selected and entry["name"] not in selected:
            continue
        print(f"Running {entry['name']}", flush=True)
        execute(entry, binary, not args.generate_only)
        if args.generate_only and entry["mode"] == "target" and entry["target"] != "docs":
            print(f"OK: {entry['target']} generation is deterministic and fresh; package build was not run", flush=True)
        elif args.generate_only and entry["mode"] in ("bulk-sync", "file-transfer", "webhooks", "telemetry", "android-jni", "mcp-python"):
            print("OK: generation is deterministic and fresh; the clients were not run", flush=True)
        else:
            print(f"OK: {entry['outcome']}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, RuntimeError, subprocess.SubprocessError, OSError) as error:
        print(f"gallery: {error}", file=sys.stderr)
        raise SystemExit(1)
