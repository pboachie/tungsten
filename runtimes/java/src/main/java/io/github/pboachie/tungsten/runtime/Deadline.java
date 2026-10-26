// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.time.Duration;
import java.util.function.Supplier;

/**
 * The total deadline of one logical call: an absolute {@link System#nanoTime} instant, scoped to
 * the calling thread while a public entry point runs. Nested work (pages, polls, verification,
 * macro steps) runs inside that scope and can only shorten it, never restart it.
 */
final class Deadline {
    private static final ThreadLocal<Long> CURRENT = new ThreadLocal<>();

    private Deadline() {}

    /** The absolute deadline of a call starting now, or null without one. */
    static Long resolve(Duration client, CallOptions opts) {
        Duration d = positive(opts.deadline()) ? opts.deadline() : client;
        if (!positive(d)) {
            return null;
        }
        long nanos;
        try {
            nanos = d.toNanos();
        } catch (ArithmeticException e) {
            return null;
        }
        long now = System.nanoTime();
        // A deadline beyond about 146 years is no deadline.
        return nanos > Long.MAX_VALUE / 2 ? null : now + nanos;
    }

    static boolean positive(Duration d) {
        return d != null && !d.isZero() && !d.isNegative();
    }

    /** Runs body with at as the call deadline (the earlier of at and an inherited one). */
    static <T> T within(Long at, Supplier<T> body) {
        if (at == null) {
            return body.get();
        }
        Long inherited = CURRENT.get();
        CURRENT.set(inherited != null && inherited - at < 0 ? inherited : at);
        try {
            return body.get();
        } finally {
            if (inherited == null) {
                CURRENT.remove();
            } else {
                CURRENT.set(inherited);
            }
        }
    }

    /** The current call deadline, or null. */
    static Long current() {
        return CURRENT.get();
    }

    /** Nanoseconds left before the call deadline; {@link Long#MAX_VALUE} without one. */
    static long remainingNanos() {
        Long at = CURRENT.get();
        return at == null ? Long.MAX_VALUE : at - System.nanoTime();
    }

    /** Whether the call deadline has passed. */
    static boolean expired() {
        return remainingNanos() <= 0;
    }

    /** The error of a call whose deadline expired before its request was sent. */
    static TungstenException failure(ClientCore c, String op, String during) {
        String name = c.identity.deadline();
        return c.diag(op, Category.TRANSPORT_FAILED).code("DEADLINE_EXCEEDED").retry(Retryable.NEVER)
                .remedy("The operation deadline (" + name + ") expired " + during
                        + ", so the request was not sent. Call again with a larger " + name + ".")
                .fail();
    }
}
