// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.time.Duration;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Consumer;
import java.util.function.DoubleSupplier;
import java.util.function.LongSupplier;
import javax.net.ssl.SSLSocketFactory;

/** Options of a client. Build them with {@link #builder}. */
public final class ClientOptions {
    /** The per-attempt timeout by default. */
    public static final Duration DEFAULT_TIMEOUT = Duration.ofSeconds(30);

    /** The largest event a stream accepts by default, in UTF-8 bytes. */
    public static final int DEFAULT_MAX_EVENT_BYTES = 1024 * 1024;

    /** The most JSON a stream collection keeps by default. */
    public static final int DEFAULT_MAX_COLLECT_BYTES = 4 * 1024 * 1024;

    /** The longest a stream collection runs by default. */
    public static final Duration DEFAULT_MAX_COLLECT_TIME = Duration.ofSeconds(60);

    /** How often a dropped stream reconnects by default. */
    public static final int DEFAULT_MAX_RECONNECTS = 3;

    /** The longest wait before a reconnect by default. */
    public static final Duration DEFAULT_RECONNECT_MAX = Duration.ofSeconds(30);

    /** How success bodies are checked. */
    public enum ValidateResponses {
        /** Reported through onDiagnostic; never fails the call (the default). */
        WARN,
        /** Not checked. */
        OFF,
        /** A body that does not match fails the call. */
        STRICT
    }

    final String baseUrl;
    final Map<String, Credential> auth;
    final TokenStore tokenStore;
    final Duration timeout;
    final RetryOptions retries;
    final IdempotencyStore idempotencyStore;
    final List<Middleware> middleware;
    final ValidateResponses validateResponses;
    final Consumer<Diagnostic> onDiagnostic;
    final Map<String, String> headers;
    final int maxEventBytes;
    final int maxCollectBytes;
    final Duration maxCollectTime;
    final int maxReconnects;
    final Duration reconnectMax;
    final Duration idleTimeout;
    final byte[] confirmationKey;
    final SSLSocketFactory sslSocketFactory;
    final LongSupplier clock;
    final DoubleSupplier random;
    final RuntimeIdentity identity;

    private ClientOptions(Builder b) {
        baseUrl = b.baseUrl;
        auth = Collections.unmodifiableMap(new LinkedHashMap<>(b.auth));
        tokenStore = b.tokenStore != null ? b.tokenStore : new TokenStore.Memory();
        timeout = positive(b.timeout, DEFAULT_TIMEOUT);
        retries = b.retries != null ? b.retries : RetryOptions.none();
        idempotencyStore = b.idempotencyStore != null ? b.idempotencyStore : new IdempotencyStore.Memory();
        middleware = Collections.unmodifiableList(new ArrayList<>(b.middleware));
        validateResponses = b.validateResponses != null ? b.validateResponses : ValidateResponses.WARN;
        onDiagnostic = b.onDiagnostic;
        headers = Collections.unmodifiableMap(new LinkedHashMap<>(b.headers));
        maxEventBytes = b.maxEventBytes > 0 ? b.maxEventBytes : DEFAULT_MAX_EVENT_BYTES;
        maxCollectBytes = b.maxCollectBytes > 0 ? b.maxCollectBytes : DEFAULT_MAX_COLLECT_BYTES;
        maxCollectTime = positive(b.maxCollectTime, DEFAULT_MAX_COLLECT_TIME);
        maxReconnects = b.maxReconnects == null ? DEFAULT_MAX_RECONNECTS : Math.min(Math.max(b.maxReconnects, 0), 1000);
        reconnectMax = positive(b.reconnectMax, DEFAULT_RECONNECT_MAX);
        idleTimeout = b.idleTimeout != null && !b.idleTimeout.isNegative() && !b.idleTimeout.isZero()
                ? (b.idleTimeout.toMillis() < 1 ? Duration.ofMillis(1) : b.idleTimeout)
                : null;
        confirmationKey = b.confirmationKey != null && b.confirmationKey.length > 0
                ? b.confirmationKey.clone()
                : Text.randomBytes(32);
        sslSocketFactory = b.sslSocketFactory;
        clock = b.clock;
        random = b.random;
        identity = b.identity != null ? b.identity : RuntimeIdentity.JAVA;
    }

    private static Duration positive(Duration d, Duration fallback) {
        return d == null || d.isNegative() || d.isZero() ? fallback : d;
    }

    /** A builder with the defaults. */
    public static Builder builder() {
        return new Builder();
    }

    /** Builds {@link ClientOptions}. */
    public static final class Builder {
        private String baseUrl;
        private final Map<String, Credential> auth = new LinkedHashMap<>();
        private TokenStore tokenStore;
        private Duration timeout;
        private RetryOptions retries;
        private IdempotencyStore idempotencyStore;
        private final List<Middleware> middleware = new ArrayList<>();
        private ValidateResponses validateResponses;
        private Consumer<Diagnostic> onDiagnostic;
        private final Map<String, String> headers = new LinkedHashMap<>();
        private int maxEventBytes;
        private int maxCollectBytes;
        private Duration maxCollectTime;
        private Integer maxReconnects;
        private Duration reconnectMax;
        private Duration idleTimeout;
        private byte[] confirmationKey;
        private SSLSocketFactory sslSocketFactory;
        private LongSupplier clock;
        private DoubleSupplier random;
        private RuntimeIdentity identity;

        private Builder() {}

        /** The base URL; the API's first server by default. */
        public Builder baseUrl(String value) {
            baseUrl = value;
            return this;
        }

        /** The credential of an auth scheme (or composite profile) by name. */
        public Builder auth(String scheme, Credential credential) {
            auth.put(scheme, credential);
            return this;
        }

        /**
         * Where the tokens of authorization-code schemes are kept (configure such a scheme with
         * {@code Credential.parts({flow: "authorizationCode", client_id: ...})}); in memory by
         * default.
         */
        public Builder tokenStore(TokenStore value) {
            tokenStore = value;
            return this;
        }

        /** The per-attempt timeout (30 s by default). */
        public Builder timeout(Duration value) {
            timeout = value;
            return this;
        }

        /** Overrides of the API's per-tier retry defaults, for every tier. */
        public Builder retries(RetryOptions value) {
            retries = value;
            return this;
        }

        /** The store of automatic idempotency keys; in memory by default. */
        public Builder idempotencyStore(IdempotencyStore value) {
            idempotencyStore = value;
            return this;
        }

        /** Adds an observer of the request lifecycle. */
        public Builder middleware(Middleware value) {
            middleware.add(value);
            return this;
        }

        /** How success bodies are checked ({@link ValidateResponses#WARN} by default). */
        public Builder validateResponses(ValidateResponses value) {
            validateResponses = value;
            return this;
        }

        /** Receives warnings (a response that does not match in WARN mode). */
        public Builder onDiagnostic(Consumer<Diagnostic> value) {
            onDiagnostic = value;
            return this;
        }

        /** A header sent on every request. */
        public Builder header(String name, String value) {
            headers.put(name, value);
            return this;
        }

        /** The largest server-sent event a stream accepts, in UTF-8 bytes (1 MiB by default). */
        public Builder maxEventBytes(int value) {
            maxEventBytes = value;
            return this;
        }

        /** The most JSON a stream collection keeps (4 MiB by default). */
        public Builder maxCollectBytes(int value) {
            maxCollectBytes = value;
            return this;
        }

        /** The longest a stream collection runs (60 s by default). */
        public Builder maxCollectTime(Duration value) {
            maxCollectTime = value;
            return this;
        }

        /**
         * How often a stream reconnects after it dropped (a lost connection, or a clean end
         * before the done event the operation declares) with Last-Event-ID set to the last event
         * id it delivered; an event whose id was delivered is never delivered twice. Only for
         * calls that are safe to repeat and only once an event id is known (or before the first
         * event). 3 by default; 0 disables it.
         */
        public Builder maxReconnects(int value) {
            maxReconnects = value;
            return this;
        }

        /**
         * The longest wait before a reconnect: the server's retry value (250 ms when it sent
         * none), shortened by up to 25 % at random. 30 s by default.
         */
        public Builder reconnectMax(Duration value) {
            reconnectMax = value;
            return this;
        }

        /**
         * A stream that delivers no bytes (comments count) for this long is idle; it is
         * reconnected like a dropped one, else ends with an error whose code is STREAM_IDLE. Unset:
         * the per-attempt timeout ends a silent stream, without a reconnect.
         */
        public Builder idleTimeout(Duration value) {
            idleTimeout = value;
            return this;
        }

        /** The key confirmation tokens are signed with; random per client by default. */
        public Builder confirmationKey(byte[] value) {
            confirmationKey = value == null ? null : value.clone();
            return this;
        }

        /**
         * The TLS sockets of https requests (a custom trust store, for instance). Default: the
         * JVM's default {@link SSLSocketFactory}. Every attempt opens its own HTTP/1.1
         * connection.
         */
        public Builder sslSocketFactory(SSLSocketFactory value) {
            sslSocketFactory = value;
            return this;
        }

        /** The clock in epoch milliseconds. */
        public Builder clock(LongSupplier value) {
            clock = value;
            return this;
        }

        /** A number in [0, 1) for retry and reconnect jitter. */
        public Builder random(DoubleSupplier value) {
            random = value;
            return this;
        }

        /** The identity the runtime reports (another language built on this runtime). */
        public Builder identity(RuntimeIdentity value) {
            identity = value;
            return this;
        }

        public ClientOptions build() {
            return new ClientOptions(this);
        }
    }
}
