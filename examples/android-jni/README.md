# Embed a Rust SDK through JNI

Call a generated Rust SDK from Java through a small JNI bridge with a bounded deadline and response size, error envelopes, cancellation and cleanup, as an Android app would.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; the pinned Rust toolchain and Cargo, and Java 17 or later; for the optional Android cross-check, the Android Rust targets and an NDK.

From the repository root:

```sh
python3 examples/run.py --only android-jni
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: Java reads pets through the bridge from a loopback mock, gets TRANSPORT_FAILED for an unreachable server, cancels a call in flight on close, refuses use after close and leaves no native threads after 50 create/call/close cycles.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

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

## The Java side

The runner writes these classes into `.work/java-src/` and compiles them with
`javac`. `Demo.java` is the host check whose output is shown above.

`PetstoreNative.java`:

```java
package example.petstore;

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
```

`PetstoreException.java`:

```java
package example.petstore;

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
```

`Demo.java`:

```java
package example.petstore;

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
```
