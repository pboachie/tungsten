// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * Retry settings, field by field; null fields are unset and fall back to the layer below (the
 * runtime defaults, then the API's defaults for the operation's tier, then {@link
 * ClientOptions.Builder#retries}).
 */
public final class RetryOptions {
    /** The backoff jitter. */
    public enum Jitter {
        NONE("none"),
        FULL("full"),
        EQUAL("equal");

        private final String wire;

        Jitter(String wire) {
            this.wire = wire;
        }

        /** The wire name. */
        public String wire() {
            return wire;
        }

        /** The jitter with this wire name, or null. */
        public static Jitter fromWire(String name) {
            for (Jitter j : values()) {
                if (j.wire.equals(name)) {
                    return j;
                }
            }
            return null;
        }
    }

    private final Integer max;
    private final Long baseMs;
    private final Long maxMs;
    private final Jitter jitter;
    private final Boolean honorRetryAfter;

    private RetryOptions(Builder b) {
        max = b.max;
        baseMs = b.baseMs;
        maxMs = b.maxMs;
        jitter = b.jitter;
        honorRetryAfter = b.honorRetryAfter;
    }

    /** A builder with every field unset. */
    public static Builder builder() {
        return new Builder();
    }

    /** No overrides. */
    public static RetryOptions none() {
        return new Builder().build();
    }

    /** Attempts after the first, or null. */
    public Integer max() {
        return max;
    }

    /** The base delay in milliseconds, or null. */
    public Long baseMs() {
        return baseMs;
    }

    /** The largest delay in milliseconds, or null. */
    public Long maxMs() {
        return maxMs;
    }

    /** The jitter, or null. */
    public Jitter jitter() {
        return jitter;
    }

    /** Whether Retry-After is honoured, or null. */
    public Boolean honorRetryAfter() {
        return honorRetryAfter;
    }

    /** Builds {@link RetryOptions}. */
    public static final class Builder {
        private Integer max;
        private Long baseMs;
        private Long maxMs;
        private Jitter jitter;
        private Boolean honorRetryAfter;

        private Builder() {}

        /** Attempts after the first (0 to 100). */
        public Builder max(int value) {
            max = value;
            return this;
        }

        public Builder baseMs(long value) {
            baseMs = value;
            return this;
        }

        public Builder maxMs(long value) {
            maxMs = value;
            return this;
        }

        public Builder jitter(Jitter value) {
            jitter = value;
            return this;
        }

        public Builder honorRetryAfter(boolean value) {
            honorRetryAfter = value;
            return this;
        }

        public RetryOptions build() {
            return new RetryOptions(this);
        }
    }
}
