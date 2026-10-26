// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.time.Duration;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;

/** Options of one call. {@link #toString} never shows keys or tokens. */
public final class CallOptions {
    private static final CallOptions DEFAULTS = builder().build();

    private final String idempotencyKey;
    private final Confirm confirm;
    private final boolean refuseConfirmYes;
    private final Duration timeout;
    private final Duration deadline;
    private final Long maxResponseBytes;
    private final boolean verify;
    private final Map<String, String> headers;

    private CallOptions(Builder b) {
        idempotencyKey = b.idempotencyKey;
        confirm = b.confirm;
        refuseConfirmYes = b.refuseConfirmYes;
        timeout = b.timeout;
        deadline = b.deadline;
        maxResponseBytes = b.maxResponseBytes;
        verify = b.verify;
        headers = Collections.unmodifiableMap(new LinkedHashMap<>(b.headers));
    }

    /** No options. */
    public static CallOptions defaults() {
        return DEFAULTS;
    }

    /** A builder. */
    public static Builder builder() {
        return new Builder();
    }

    /** A builder starting from these options. */
    public Builder toBuilder() {
        Builder b = new Builder();
        b.idempotencyKey = idempotencyKey;
        b.confirm = confirm;
        b.refuseConfirmYes = refuseConfirmYes;
        b.timeout = timeout;
        b.deadline = deadline;
        b.maxResponseBytes = maxResponseBytes;
        b.verify = verify;
        b.headers.putAll(headers);
        return b;
    }

    /** The idempotency key, or null. */
    public String idempotencyKey() {
        return idempotencyKey;
    }

    /** The confirmation. */
    public Confirm confirm() {
        return confirm;
    }

    /** Whether {@link Confirm#YES} is refused too. */
    public boolean refuseConfirmYes() {
        return refuseConfirmYes;
    }

    /** The per-attempt timeout, or null for the client's. */
    public Duration timeout() {
        return timeout;
    }

    /** The total deadline of this call, or null for the client's. */
    public Duration deadline() {
        return deadline;
    }

    /** Maximum ordinary response bytes; null uses the client's limit. */
    public Long maxResponseBytes() {
        return maxResponseBytes;
    }

    /** Whether the verification hook runs after success. */
    public boolean verify() {
        return verify;
    }

    /** Headers sent on this call. */
    public Map<String, String> headers() {
        return headers;
    }

    boolean hasKey() {
        return idempotencyKey != null && !idempotencyKey.isEmpty();
    }

    @Override
    public String toString() {
        return "CallOptions{idempotencyKey=" + (hasKey() ? Text.REDACTED : "")
                + ", confirm=" + confirm + ", timeout=" + timeout + ", deadline=" + deadline + ", verify=" + verify
                + ", headers=" + headers.keySet() + "}";
    }

    /** Builds {@link CallOptions}. */
    public static final class Builder {
        private String idempotencyKey;
        private Confirm confirm = Confirm.NONE;
        private boolean refuseConfirmYes;
        private Duration timeout;
        private Duration deadline;
        private Long maxResponseBytes;
        private boolean verify;
        private final Map<String, String> headers = new LinkedHashMap<>();

        private Builder() {}

        /**
         * The idempotency key: required for caller_owned operations with a required persisted
         * key; reuse the same key on every retry of one intent.
         */
        public Builder idempotencyKey(String key) {
            idempotencyKey = key == null || key.isEmpty() ? null : key;
            return this;
        }

        /**
         * The confirmation: a token authorizes one intent. The first call that sends spends it,
         * and it is accepted again only for a retry with the same idempotency key (or the
         * identical body of a content_identity operation).
         */
        public Builder confirm(Confirm value) {
            confirm = value == null ? Confirm.NONE : value;
            return this;
        }

        /**
         * {@link Confirm#YES} does not confirm a destructive operation either, and remediation
         * never offers it (callers that only forward preview tokens, such as a tool server).
         */
        public Builder refuseConfirmYes(boolean value) {
            refuseConfirmYes = value;
            return this;
        }

        /** The per-attempt timeout. */
        public Builder timeout(Duration value) {
            timeout = value;
            return this;
        }

        /**
         * The total deadline of this call, overriding the client's; null, zero or negative uses
         * the client's (none by default). Measured on a monotonic clock from the call, it bounds
         * authentication, idempotency-key reservation, every attempt (each one's timeout is cut to
         * the time left), retry waits, response bodies and verification; for pages, poll and
         * runMacro it bounds the whole traversal, poll or macro, and nested work cannot restart
         * it. No attempt starts after it expires (code {@code DEADLINE_EXCEEDED}); a mutation
         * already sent reports {@code OUTCOME_UNKNOWN}, never a blind replay. For a stream it
         * bounds connecting and each reconnect, not an open stream (see the idle timeout).
         * Blocking work the runtime cannot interrupt (a token request this call owns, a shared
         * client-credentials fetch, an idempotency store) is checked when it returns.
         */
        public Builder deadline(Duration value) {
            deadline = value;
            return this;
        }

        /** Runs the operation's verification hook after success. */
        public Builder verify(boolean value) {
            verify = value;
            return this;
        }

        /** A header sent on this call. */
        public Builder header(String name, String value) {
            headers.put(name, value);
            return this;
        }

        /** Headers sent on this call, replacing the ones set before. */
        public Builder headers(Map<String, String> values) {
            headers.clear();
            headers.putAll(values);
            return this;
        }

        /** Overrides the client ordinary response limit with a positive safe integer. */
        public Builder maxResponseBytes(long value) {
            return maxResponseBytes(Long.valueOf(value));
        }

        /** Overrides the client ordinary response limit, or null to use its limit. */
        public Builder maxResponseBytes(Long value) {
            maxResponseBytes = value;
            return this;
        }

        public CallOptions build() {
            return new CallOptions(this);
        }
    }
}
