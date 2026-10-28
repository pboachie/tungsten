// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.Objects;

/**
 * Who the runtime says it is, and how it spells its own options in the prose of an envelope: the
 * {@code (language)} token of {@code User-Agent}, the {@code tungsten-<runtime>/<version>} of
 * {@code X-Tungsten-Runtime}, and the names of the call and client options that remediation
 * texts mention. A language built on this runtime (Kotlin) passes its own identity in {@link
 * ClientOptions.Builder#identity}.
 */
public final class RuntimeIdentity {
    /** The version of this runtime. */
    public static final String VERSION = "0.1.4";

    /** The Java runtime's identity. */
    public static final RuntimeIdentity JAVA =
            builder("java", "java")
                    .idempotencyKey("idempotencyKey")
                    .confirmYes("Confirm.YES")
                    .baseUrl("baseUrl")
                    .timeout("`timeout`")
                    .deadline("`deadline`")
                    .idleTimeout("`idleTimeout`")
                    .maxEventBytes("maxEventBytes")
                    .build();

    private final String language;
    private final String runtime;
    private final String version;
    private final String idempotencyKey;
    private final String confirmYes;
    private final String baseUrl;
    private final String timeout;
    private final String deadline;
    private final String idleTimeout;
    private final String maxEventBytes;

    private RuntimeIdentity(Builder b) {
        this.language = b.language;
        this.runtime = b.runtime;
        this.version = b.version;
        this.idempotencyKey = b.idempotencyKey;
        this.confirmYes = b.confirmYes;
        this.baseUrl = b.baseUrl;
        this.timeout = b.timeout;
        this.deadline = b.deadline;
        this.idleTimeout = b.idleTimeout;
        this.maxEventBytes = b.maxEventBytes;
    }

    /** A builder; the spellings default to the Java runtime's. */
    public static Builder builder(String language, String runtime) {
        return new Builder(language, runtime);
    }

    /** The User-Agent language token ({@code java}). */
    public String language() {
        return language;
    }

    /** The runtime id of X-Tungsten-Runtime ({@code java}). */
    public String runtime() {
        return runtime;
    }

    /** The runtime version of X-Tungsten-Runtime. */
    public String version() {
        return version;
    }

    /** The call option carrying an idempotency key ({@code idempotencyKey}). */
    public String idempotencyKey() {
        return idempotencyKey;
    }

    /** The confirmation without a token ({@code Confirm.YES}). */
    public String confirmYes() {
        return confirmYes;
    }

    /** The client option of the base URL ({@code baseUrl}). */
    public String baseUrl() {
        return baseUrl;
    }

    /** The per-attempt timeout option, as written in prose ({@code `timeout`}). */
    public String timeout() {
        return timeout;
    }

    /** The total operation deadline option, as written in prose ({@code `deadline`}). */
    public String deadline() {
        return deadline;
    }

    /** The stream idle timeout option, as written in prose ({@code `idleTimeout`}). */
    public String idleTimeout() {
        return idleTimeout;
    }

    /** The client option bounding one event ({@code maxEventBytes}). */
    public String maxEventBytes() {
        return maxEventBytes;
    }

    /** Builds a {@link RuntimeIdentity}. */
    public static final class Builder {
        private final String language;
        private final String runtime;
        private String version = VERSION;
        private String idempotencyKey = "idempotencyKey";
        private String confirmYes = "Confirm.YES";
        private String baseUrl = "baseUrl";
        private String timeout = "`timeout`";
        private String deadline = "`deadline`";
        private String idleTimeout = "`idleTimeout`";
        private String maxEventBytes = "maxEventBytes";

        private Builder(String language, String runtime) {
            this.language = Objects.requireNonNull(language, "language");
            this.runtime = Objects.requireNonNull(runtime, "runtime");
        }

        public Builder version(String value) {
            version = Objects.requireNonNull(value);
            return this;
        }

        public Builder idempotencyKey(String value) {
            idempotencyKey = Objects.requireNonNull(value);
            return this;
        }

        public Builder confirmYes(String value) {
            confirmYes = Objects.requireNonNull(value);
            return this;
        }

        public Builder baseUrl(String value) {
            baseUrl = Objects.requireNonNull(value);
            return this;
        }

        public Builder timeout(String value) {
            timeout = Objects.requireNonNull(value);
            return this;
        }

        public Builder deadline(String value) {
            deadline = Objects.requireNonNull(value);
            return this;
        }

        public Builder idleTimeout(String value) {
            idleTimeout = Objects.requireNonNull(value);
            return this;
        }

        public Builder maxEventBytes(String value) {
            maxEventBytes = Objects.requireNonNull(value);
            return this;
        }

        public RuntimeIdentity build() {
            return new RuntimeIdentity(this);
        }
    }
}
