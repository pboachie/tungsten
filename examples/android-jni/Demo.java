// SPDX-License-Identifier: AGPL-3.0-only
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
