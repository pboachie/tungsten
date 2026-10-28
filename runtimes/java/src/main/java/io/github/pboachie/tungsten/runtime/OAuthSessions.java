// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.lang.ref.WeakReference;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/** Per-client managed browser sessions. No store or application callbacks run under lock. */
final class OAuthSessions {
    private final ClientCore core;
    private final Auth auth;
    private final Object lock = new Object();
    private final Map<String, Session> sessions = new HashMap<>();
    private final Map<String, Attempt> attempts = new HashMap<>();
    private final Map<FlightKey, CompletableFuture<RefreshResult>> refreshes = new HashMap<>();
    private int admitted;
    private int tokenJobs;

    OAuthSessions(ClientCore core, Auth auth) {
        this.core = core;
        this.auth = auth;
    }

    private static final class Session {
        long generation;
        long epoch;
        int loggingOut;
        boolean closed;
    }

    private record Version(long generation, long epoch) {}
    private record FlightKey(String scheme, Version version) {}
    private record RefreshResult(Auth.Token token, Version committed) {
        @Override public String toString() { return "RefreshResult{..}"; }
    }

    private record Capture(String scheme, Auth.CodeClient client, String authorizationUrl, String tokenUrl,
            String refreshUrl, List<String> configuredScopes, List<String> requestedScopes,
            String redirect, String binding, String resource, String audience, String accountContext,
            OAuthFlow.IssuerDefense defense, String state, String verifier, String url) {
        @Override public String toString() { return "Capture{..}"; }
    }

    private record RefreshCapture(String scheme, Auth.CodeClient client, String authorizationUrl, String tokenUrl,
            String refreshUrl, List<String> scopes, String selectedUrl) {
        @Override public String toString() { return "RefreshCapture{..}"; }
    }

    /** Admission precedes arbitrary store/clock work and lasts through physical settlement. */
    private final class TokenJob implements AutoCloseable {
        final long started = System.nanoTime();
        final long timeout;

        TokenJob() {
            timeout = OAuthInputs.timeoutNanos(core.timeout);
            OAuthInputs.require(OAuthInputs.tokenLimit(core.options.maxOAuthTokenResponseBytes));
            synchronized (lock) {
                OAuthInputs.require(tokenJobs < OAuthInputs.CAPACITY);
                tokenJobs++;
            }
        }

        private boolean handedOff;

        boolean timely() { return System.nanoTime() - started < timeout && !Thread.currentThread().isInterrupted(); }
        long remaining() { return Math.max(1, timeout - (System.nanoTime() - started)); }
        /** The admission now ends when the work handed off settles, not when the caller returns. */
        void handOff() { handedOff = true; }
        void release() { synchronized (lock) { tokenJobs--; } }
        @Override public void close() { if (!handedOff) release(); }
    }

    private static final class Attempt {
        final String scheme;
        final String handle;
        final String binding;
        final long generation;
        final long deadline;
        Capture capture;
        ScheduledFuture<?> timer;
        boolean claimed;
        boolean invalid;

        Attempt(Capture capture, String handle, long generation) {
            this.scheme = capture.scheme();
            this.binding = capture.binding();
            this.handle = handle;
            this.generation = generation;
            this.capture = capture;
            this.deadline = System.nanoTime() + OAuthInputs.LIFETIME_NANOS;
        }
    }

    /** One lazy daemon for all clients; canceled tasks are removed, and owners are weak. */
    private static final class Expiry {
        static final ScheduledThreadPoolExecutor TIMER = timer();

        private static ScheduledThreadPoolExecutor timer() {
            ScheduledThreadPoolExecutor timer = new ScheduledThreadPoolExecutor(1, runnable -> {
                Thread thread = new Thread(runnable, "tungsten-oauth-expiry");
                thread.setDaemon(true);
                return thread;
            });
            timer.setRemoveOnCancelPolicy(true);
            return timer;
        }
    }

    private static final class Expire implements Runnable {
        private final WeakReference<OAuthSessions> owner;
        private final WeakReference<Attempt> attempt;

        Expire(OAuthSessions owner, Attempt attempt) {
            this.owner = new WeakReference<>(owner);
            this.attempt = new WeakReference<>(attempt);
        }

        @Override public void run() {
            OAuthSessions manager = owner.get();
            Attempt pending = attempt.get();
            if (manager != null && pending != null) synchronized (manager.lock) {
                manager.invalidateLocked(pending);
            }
        }
    }

    private Session sessionLocked(String scheme) {
        return sessions.computeIfAbsent(scheme, ignored -> new Session());
    }

    private void requireScheme(String scheme) {
        OAuthInputs.bytes(scheme, OAuthInputs.MAX_CAPTURE_BYTES);
        OAuthInputs.require(auth.codeFlow(scheme, new String[1]) != null);
    }

    private Version version(String scheme) {
        synchronized (lock) {
            Session session = sessionLocked(scheme);
            return new Version(session.generation, session.epoch);
        }
    }

    private boolean activeLocked(String scheme, long generation) {
        Session session = sessionLocked(scheme);
        return !session.closed && session.loggingOut == 0 && session.generation == generation;
    }

    private boolean currentLocked(String scheme, Version version) {
        return activeLocked(scheme, version.generation()) && sessionLocked(scheme).epoch == version.epoch();
    }

    private boolean current(String scheme, Version version) {
        synchronized (lock) { return currentLocked(scheme, version); }
    }

    /** Invoked only by a capable store's final serialized predicate. */
    private Version mutateLocked(String scheme) {
        Session session = sessionLocked(scheme);
        if (session.epoch == Long.MAX_VALUE) {
            session.closed = true;
            return null;
        }
        session.epoch++;
        return new Version(session.generation, session.epoch);
    }

    private boolean liveLocked(Attempt attempt) {
        return !attempt.invalid && attempts.get(attempt.handle) == attempt
                && System.nanoTime() - attempt.deadline < 0 && activeLocked(attempt.scheme, attempt.generation);
    }

    private void removeLocked(Attempt attempt) {
        if (attempts.remove(attempt.handle, attempt)) admitted--;
        attempt.invalid = true;
        attempt.capture = null;
        if (attempt.timer != null) {
            attempt.timer.cancel(false);
            attempt.timer = null;
        }
    }

    private void invalidateLocked(Attempt attempt) {
        attempt.invalid = true;
        attempt.capture = null;
        if (attempt.timer != null) {
            attempt.timer.cancel(false);
            attempt.timer = null;
        }
        if (!attempt.claimed) removeLocked(attempt);
    }

    private void sweepLocked() {
        for (Attempt attempt : new ArrayList<>(attempts.values())) {
            if (!liveLocked(attempt)) invalidateLocked(attempt);
        }
    }

    private <T> Result<T> failed(String scheme, String helper, String message) {
        return core.diag("oauth." + scheme + "." + helper, Category.AUTH_FAILED).remedy(message).fail().toResult();
    }

    private static <T> Result<T> ok(T value) {
        return new Result.Ok<>(value, value != null, null, ResponseMeta.local(), null);
    }

    private static String optional(OAuthInputs.Budget budget, String value) {
        if (value != null) budget.charge(value);
        return value;
    }

    private Capture capture(String scheme, OAuthFlow.AuthorizationParams parameters) {
        OAuthInputs.require(parameters != null && parameters.s256Supported() && parameters.issuerDefense() != null);
        OAuthInputs.Budget budget = new OAuthInputs.Budget();
        budget.charge(scheme);
        String binding = parameters.sessionBinding();
        budget.charge(binding);
        OAuthInputs.require(!binding.isEmpty());
        String[] reason = new String[1];
        ApiDescriptor.AuthorizationCode flow = auth.codeFlow(scheme, reason);
        Auth.CodeClient client = auth.codeClient(scheme, reason);
        OAuthInputs.require(flow != null && client != null);
        // Capture borrowed descriptor scopes before any cryptography or caller callback can run.
        List<String> configuredScopes = budget.scopes(flow.scopes());
        String authorizationUrl = flow.authorizationUrl();
        String tokenUrl = flow.tokenUrl();
        String refreshUrl = flow.refreshUrl();
        budget.charge(authorizationUrl);
        budget.charge(tokenUrl);
        optional(budget, refreshUrl);
        budget.charge(client.clientId());
        optional(budget, client.clientSecret());
        optional(budget, client.redirectUri());
        String redirect = parameters.redirectUri() != null ? parameters.redirectUri() : client.redirectUri();
        budget.charge(redirect);
        List<String> scopes = parameters.scopes() == null ? configuredScopes : budget.scopes(parameters.scopes());
        String resource = optional(budget, parameters.resource());
        String audience = optional(budget, parameters.audience());
        String context = optional(budget, parameters.accountContext());
        OAuthFlow.IssuerDefense defense = parameters.issuerDefense();
        budget.charge(defense.value());
        OAuthInputs.require(!defense.value().isEmpty());
        Map<String, String> extra = budget.extra(parameters.extra());
        OAuthInputs.endpoint(authorizationUrl, false);
        OAuthInputs.endpoint(tokenUrl, false);
        if (refreshUrl != null && !refreshUrl.isEmpty()) OAuthInputs.endpoint(refreshUrl, false);
        OAuthInputs.endpoint(redirect, true);
        if (defense.response()) OAuthInputs.endpoint(defense.value(), false);
        OAuthInputs.timeoutNanos(core.timeout);
        String state = Text.base64Url(Text.randomBytes(32));
        String verifier = Text.base64Url(Text.randomBytes(32));
        List<Serialize.Pair> fields = new ArrayList<>();
        fields.add(new Serialize.Pair("response_type", "code"));
        fields.add(new Serialize.Pair("client_id", client.clientId()));
        fields.add(new Serialize.Pair("redirect_uri", redirect));
        if (!scopes.isEmpty()) fields.add(new Serialize.Pair("scope", String.join(" ", scopes)));
        if (resource != null) fields.add(new Serialize.Pair("resource", resource));
        if (audience != null) fields.add(new Serialize.Pair("audience", audience));
        fields.add(new Serialize.Pair("state", state));
        fields.add(new Serialize.Pair("code_challenge", OAuthFlow.challenge(verifier)));
        fields.add(new Serialize.Pair("code_challenge_method", OAuthFlow.PKCE_METHOD));
        extra.forEach((key, value) -> fields.add(new Serialize.Pair(key, value)));
        String separator = authorizationUrl.contains("?")
                ? (authorizationUrl.endsWith("?") || authorizationUrl.endsWith("&") ? "" : "&") : "?";
        String url = authorizationUrl + separator + Auth.formBody(fields);
        return new Capture(scheme, client, authorizationUrl, tokenUrl, refreshUrl, configuredScopes, scopes,
                redirect, binding, resource, audience, context, defense, state, verifier, url);
    }

    /** Does not hold lock: even hostile descriptor collections cannot execute under manager lock. */
    private boolean configurationCurrent(Capture capture) {
        try {
            String[] reason = new String[1];
            ApiDescriptor.AuthorizationCode flow = auth.codeFlow(capture.scheme(), reason);
            Auth.CodeClient client = auth.codeClient(capture.scheme(), reason);
            return flow != null && Objects.equals(client, capture.client())
                    && Objects.equals(flow.authorizationUrl(), capture.authorizationUrl())
                    && Objects.equals(flow.tokenUrl(), capture.tokenUrl())
                    && Objects.equals(flow.refreshUrl(), capture.refreshUrl())
                    && OAuthInputs.sameScopes(flow.scopes(), capture.configuredScopes());
        } catch (RuntimeException invalid) { return false; }
    }

    private RefreshCapture captureRefresh(String scheme, TokenStore.StoredToken previous) {
        OAuthInputs.Budget budget = new OAuthInputs.Budget();
        budget.charge(scheme);
        long tokenBytes = core.options.maxOAuthTokenResponseBytes;
        OAuthInputs.bytes(previous.accessToken(), (int) Math.min(tokenBytes, Integer.MAX_VALUE));
        OAuthInputs.bytes(previous.refreshToken(), (int) Math.min(tokenBytes, Integer.MAX_VALUE));
        String[] reason = new String[1];
        ApiDescriptor.AuthorizationCode flow = auth.codeFlow(scheme, reason);
        Auth.CodeClient client = auth.codeClient(scheme, reason);
        OAuthInputs.require(flow != null && client != null);
        List<String> scopes = budget.scopes(flow.scopes());
        String authorization = flow.authorizationUrl();
        String token = flow.tokenUrl();
        String refresh = flow.refreshUrl();
        budget.charge(authorization);
        budget.charge(token);
        optional(budget, refresh);
        budget.charge(client.clientId());
        optional(budget, client.clientSecret());
        optional(budget, client.redirectUri());
        OAuthInputs.endpoint(authorization, false);
        OAuthInputs.endpoint(token, false);
        if (refresh != null && !refresh.isEmpty()) OAuthInputs.endpoint(refresh, false);
        return new RefreshCapture(scheme, client, authorization, token, refresh, scopes,
                refresh == null || refresh.isEmpty() ? token : refresh);
    }

    private boolean configurationCurrent(RefreshCapture capture) {
        try {
            String[] reason = new String[1];
            ApiDescriptor.AuthorizationCode flow = auth.codeFlow(capture.scheme(), reason);
            return flow != null && Objects.equals(auth.codeClient(capture.scheme(), reason), capture.client())
                    && Objects.equals(flow.authorizationUrl(), capture.authorizationUrl())
                    && Objects.equals(flow.tokenUrl(), capture.tokenUrl())
                    && Objects.equals(flow.refreshUrl(), capture.refreshUrl())
                    && OAuthInputs.sameScopes(flow.scopes(), capture.scopes());
        } catch (RuntimeException invalid) { return false; }
    }

    private boolean authorizationCurrent(Attempt attempt, Capture capture) {
        if (!configurationCurrent(capture)) return false;
        synchronized (lock) { return liveLocked(attempt); }
    }

    Result<OAuthFlow.AuthorizationAttempt> begin(String scheme, OAuthFlow.AuthorizationParams parameters) {
        boolean reserved = false;
        boolean begun = false;
        Attempt pending = null;
        try {
            OAuthInputs.require(core.tokenStore instanceof TokenStore.Fenced
                    && OAuthInputs.tokenLimit(core.options.maxOAuthTokenResponseBytes));
            requireScheme(scheme);
            long generation;
            synchronized (lock) {
                sweepLocked();
                generation = sessionLocked(scheme).generation;
                OAuthInputs.require(activeLocked(scheme, generation) && admitted < OAuthInputs.CAPACITY);
                admitted++;
                reserved = true;
            }
            Capture captured = capture(scheme, parameters);
            for (int collision = 0; collision < 4; collision++) {
                String handle = Text.base64Url(Text.randomBytes(32));
                synchronized (lock) {
                    OAuthInputs.require(activeLocked(scheme, generation));
                    if (attempts.containsKey(handle)) continue;
                    pending = new Attempt(captured, handle, generation);
                    attempts.put(handle, pending);
                    reserved = false;
                    break;
                }
            }
            OAuthInputs.require(pending != null);
            ScheduledFuture<?> timer = Expiry.TIMER.schedule(new Expire(this, pending),
                    OAuthInputs.LIFETIME_NANOS, TimeUnit.NANOSECONDS);
            synchronized (lock) {
                if (!liveLocked(pending)) {
                    timer.cancel(false);
                    throw new OAuthInputs.Invalid();
                }
                pending.timer = timer;
            }
            Result<OAuthFlow.AuthorizationAttempt> result = ok(new OAuthFlow.AuthorizationAttempt(pending.handle, captured.url(), 600_000));
            begun = true;
            return result;
        } catch (RuntimeException invalid) {
            return failed(scheme, "authorizationUrl", "Cannot begin authorization: qualify S256 and issuer defense, supply an owning session binding, bounded valid endpoints and an atomic fenced token store. Finish or cancel existing attempts if capacity is exhausted.");
        } finally {
            if (!begun && pending != null) synchronized (lock) { removeLocked(pending); }
            if (reserved) synchronized (lock) { admitted--; }
        }
    }

    Result<OAuthFlow.TokenInfo> complete(String scheme, OAuthFlow.ExchangeParams parameters) {
        final String refusal = "Authorization callback refused: invalid, expired, consumed or mismatched attempt. Begin a new managed authorization attempt if needed.";
        Attempt pending = null;
        boolean claimed = false;
        // The attempt already owns admission. This deadline also fences custom-store settlement.
        long started = System.nanoTime();
        try {
            long timeout = OAuthInputs.timeoutNanos(core.timeout);
            OAuthInputs.require(parameters != null);
            OAuthInputs.bytes(parameters.attemptHandle(), 128);
            OAuthInputs.bytes(parameters.sessionBinding(), OAuthInputs.MAX_CAPTURE_BYTES);
            OAuthInputs.bytes(parameters.callbackUrl(), OAuthInputs.MAX_CALLBACK_BYTES);
            Capture captured;
            synchronized (lock) {
                pending = attempts.get(parameters.attemptHandle());
                OAuthInputs.require(pending != null && pending.scheme.equals(scheme) && liveLocked(pending)
                        && !pending.claimed && pending.binding.equals(parameters.sessionBinding()));
                captured = pending.capture;
            }
            OAuthInputs.require(captured != null && authorizationCurrent(pending, captured)
                    && System.nanoTime() - started < timeout && !Thread.currentThread().isInterrupted());
            String callback = parameters.callbackUrl();
            OAuthInputs.require(callback.indexOf('#') < 0);
            int query = callback.indexOf('?');
            OAuthInputs.require(query > 0 && callback.substring(0, query).equals(captured.redirect()));
            Map<String, String> fields = OAuthInputs.fields(callback.substring(query + 1));
            OAuthInputs.require(captured.state().equals(fields.get("state"))
                    && (!captured.defense().response() || captured.defense().value().equals(fields.get("iss"))));
            String code = fields.get("code");
            String denied = fields.get("error");
            OAuthInputs.require((code != null) != (denied != null) && (code == null || !code.isEmpty())
                    && (denied == null || !denied.isEmpty())
                    && (code == null || (!fields.containsKey("error_description") && !fields.containsKey("error_uri"))));
            synchronized (lock) {
                OAuthInputs.require(liveLocked(pending) && !pending.claimed && System.nanoTime() - started < timeout);
                pending.claimed = true;
                pending.capture = null;
                claimed = true;
            }
            if (denied != null) return failed(scheme, "exchangeCode", "Authorization was denied. Begin a new managed authorization attempt to authorize again.");
            OAuthInputs.require(authorizationCurrent(pending, captured));
            List<Serialize.Pair> tokenFields = List.of(new Serialize.Pair("grant_type", "authorization_code"),
                    new Serialize.Pair("code", code), new Serialize.Pair("redirect_uri", captured.redirect()),
                    new Serialize.Pair("code_verifier", captured.verifier()));
            Auth.TokenResult result = auth.tokenRequest(scheme, captured.tokenUrl(), captured.client(), tokenFields, null,
                    started, timeout);
            OAuthInputs.require(result.failure() == null && result.token() != null);
            OAuthInputs.require(core.tokenStore instanceof TokenStore.Fenced);
            Attempt owned = pending;
            Version[] committed = new Version[1];
            boolean changed = ((TokenStore.Fenced) core.tokenStore).setIfCurrent(scheme, result.token(), ignored -> {
                if (!configurationCurrent(captured)) return false;
                synchronized (lock) {
                    if (!liveLocked(owned) || System.nanoTime() - started >= timeout || Thread.currentThread().isInterrupted()) return false;
                    committed[0] = mutateLocked(scheme);
                    return committed[0] != null;
                }
            });
            TokenStore.StoredToken token = result.token();
            Result<OAuthFlow.TokenInfo> success = ok(new OAuthFlow.TokenInfo(scheme, token.expiresAt(), token.scope(),
                    token.refreshToken() != null && !token.refreshToken().isEmpty()));
            OAuthInputs.require(changed && committed[0] != null && configurationCurrent(captured));
            synchronized (lock) {
                OAuthInputs.require(liveLocked(pending) && currentLocked(scheme, committed[0])
                        && System.nanoTime() - started < timeout && !Thread.currentThread().isInterrupted());
                removeLocked(pending);
                claimed = false;
            }
            return success;
        } catch (RuntimeException invalid) {
            return failed(scheme, "exchangeCode", refusal);
        } finally {
            if (claimed && pending != null) synchronized (lock) { removeLocked(pending); }
        }
    }

    Result<Void> cancel(String scheme, String handle, String binding) {
        try {
            OAuthInputs.bytes(handle, 128);
            OAuthInputs.bytes(binding, OAuthInputs.MAX_CAPTURE_BYTES);
        } catch (RuntimeException invalid) {
            return failed(scheme, "cancelAuthorization", "Authorization attempt is unavailable or belongs to a different session.");
        }
        boolean changed = false;
        synchronized (lock) {
            Attempt pending = attempts.get(handle);
            if (pending != null && pending.scheme.equals(scheme) && pending.binding.equals(binding)) {
                invalidateLocked(pending);
                changed = true;
            }
        }
        return changed ? ok(null) : failed(scheme, "cancelAuthorization", "Authorization attempt is unavailable or belongs to a different session.");
    }

    Result<Void> logout(String scheme) {
        try { requireScheme(scheme); }
        catch (RuntimeException invalid) { return failed(scheme, "logout", "Local authorization-code session is unavailable."); }
        synchronized (lock) {
            Session session = sessionLocked(scheme);
            if (session.generation == Long.MAX_VALUE) session.closed = true;
            else session.generation++;
            session.loggingOut++;
            for (Attempt attempt : new ArrayList<>(attempts.values())) {
                if (attempt.scheme.equals(scheme)) invalidateLocked(attempt);
            }
        }
        try {
            core.tokenStore.delete(scheme);
            return ok(null);
        } catch (RuntimeException failure) {
            return failed(scheme, "logout", "Local credential removal did not complete; check the token store before authorizing again.");
        } finally {
            synchronized (lock) { sessionLocked(scheme).loggingOut--; }
        }
    }

    TokenStore.StoredToken readToken(String scheme) {
        try (TokenJob job = new TokenJob()) {
            requireScheme(scheme);
            return readTokenOwned(scheme, job);
        }
        catch (RuntimeException failure) { return null; }
    }

    private TokenStore.StoredToken readTokenOwned(String scheme, TokenJob job) {
        Version seen = version(scheme);
        if (!job.timely() || !current(scheme, seen)) return null;
        try {
            TokenStore.StoredToken stored = core.tokenStore.get(scheme);
            return stored != null && stored.accessToken() != null && !stored.accessToken().isEmpty()
                    && job.timely() && current(scheme, seen) ? stored : null;
        } catch (RuntimeException failure) { return null; }
    }

    private static Auth.Token changed() {
        return Auth.Token.failed("The OAuth session changed while token work was pending; obtain credentials for the current session.");
    }

    Auth.Token storedToken(String scheme, boolean force) {
        try (TokenJob job = new TokenJob()) {
            requireScheme(scheme);
            return storedTokenOwned(scheme, force, job);
        }
        catch (RuntimeException failure) { return Auth.Token.failed("OAuth token admission, storage or refresh did not complete within its bounds."); }
    }

    private Auth.Token storedTokenOwned(String scheme, boolean force, TokenJob job) {
        Version seen = version(scheme);
        TokenStore.StoredToken stored = readTokenOwned(scheme, job);
        if (!job.timely() || !current(scheme, seen)) return changed();
        if (stored == null) return Auth.Token.failed("No OAuth2 token is stored for " + scheme
                + "; begin a managed authorizationUrl attempt, then complete exchangeCode with its handle, full callback URL and owning session binding.");
        boolean fresh;
        try { fresh = stored.expiresAt() == null || stored.expiresAt() > core.now() + Auth.EXPIRY_SKEW_MS; }
        catch (RuntimeException failure) { return changed(); }
        if (!job.timely() || !current(scheme, seen)) return changed();
        if (fresh && !force) return Auth.Token.of(stored.accessToken());
        FlightKey key = new FlightKey(scheme, seen);
        CompletableFuture<RefreshResult> flight;
        boolean owner = false;
        synchronized (lock) {
            if (!currentLocked(scheme, seen)) return changed();
            flight = refreshes.get(key);
            if (flight == null) {
                if (refreshes.size() >= OAuthInputs.CAPACITY) return Auth.Token.failed("OAuth refresh capacity is exhausted; wait for owned work to settle.");
                flight = new CompletableFuture<>();
                refreshes.put(key, flight);
                owner = true;
            }
        }
        if (owner) {
            // The refresh runs beside its owner, bounded by its own timeout, so the owner waits for
            // it like any other caller and stops at its own deadline.
            CompletableFuture<RefreshResult> shared = flight;
            Thread worker = new Thread(() -> {
                try {
                    RefreshResult result;
                    try { result = refreshStored(scheme, seen, stored, force, job); }
                    catch (RuntimeException failure) { result = new RefreshResult(Auth.Token.failed("OAuth token storage or refresh did not complete; authorize again."), null); }
                    catch (Error failure) {
                        synchronized (lock) { refreshes.remove(key, shared); }
                        shared.completeExceptionally(failure);
                        throw failure;
                    }
                    synchronized (lock) { refreshes.remove(key, shared); }
                    // CompletableFuture dependents can execute inline; never complete under manager lock.
                    shared.complete(result);
                } finally {
                    job.release();
                }
            }, "tungsten-oauth-refresh");
            worker.setDaemon(true);
            job.handOff();
            try { worker.start(); }
            catch (RuntimeException | Error failure) {
                job.handedOff = false;
                synchronized (lock) { refreshes.remove(key, flight); }
                flight.complete(new RefreshResult(Auth.Token.failed("OAuth token storage or refresh did not complete; authorize again."), null));
                throw failure;
            }
        }
        try {
            // A caller's deadline stops its own wait, never the shared refresh.
            RefreshResult result = flight.get(Math.max(1, Math.min(job.remaining(), Deadline.remainingNanos())), TimeUnit.NANOSECONDS);
            if (result.token().failure() != null) return result.token();
            return job.timely() && result.committed() != null && current(scheme, result.committed()) ? result.token() : changed();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return Auth.Token.failed("OAuth token acquisition was interrupted; owned work may still be settling.");
        } catch (ExecutionException | TimeoutException failure) { return changed(); }
    }

    String refreshAfterRejection(String scheme, String rejected) {
        try (TokenJob job = new TokenJob()) {
            requireScheme(scheme);
            return refreshAfterRejectionOwned(scheme, rejected, job);
        }
        catch (RuntimeException failure) { return null; }
    }

    private String refreshAfterRejectionOwned(String scheme, String rejected, TokenJob job) {
        Version seen = version(scheme);
        TokenStore.StoredToken stored = readTokenOwned(scheme, job);
        if (stored == null || !job.timely() || !current(scheme, seen)) return null;
        if (!stored.accessToken().equals(rejected)) return stored.accessToken();
        if (stored.refreshToken() == null || stored.refreshToken().isEmpty()) return null;
        Auth.Token token = storedTokenOwned(scheme, true, job);
        return token.failure() == null ? token.value() : null;
    }

    private static boolean sameToken(TokenStore.StoredToken current, TokenStore.StoredToken previous) {
        return current != null && Objects.equals(current.accessToken(), previous.accessToken())
                && Objects.equals(current.refreshToken(), previous.refreshToken());
    }

    private RefreshResult refreshStored(String scheme, Version seen, TokenStore.StoredToken previous, boolean force, TokenJob job) {
        if (!job.timely() || !current(scheme, seen)) return new RefreshResult(changed(), null);
        if (!(core.tokenStore instanceof TokenStore.Fenced fenced)) return new RefreshResult(Auth.Token.failed("OAuth refresh requires an atomic fenced token store."), null);
        if (previous.refreshToken() == null || previous.refreshToken().isEmpty()) {
            String message = "The OAuth2 token has no refresh credential; begin a new managed authorization attempt.";
            if (!force) message = "The OAuth2 token expired without a refresh credential; begin a new managed authorization attempt.";
            return new RefreshResult(Auth.Token.failed(message), null);
        }
        RefreshCapture capture = captureRefresh(scheme, previous);
        if (!job.timely() || !current(scheme, seen) || !configurationCurrent(capture)) return new RefreshResult(changed(), null);
        // Validate the immutable actual selected refresh endpoint immediately before dispatch.
        OAuthInputs.endpoint(capture.selectedUrl(), false);
        Auth.TokenResult renewed = auth.tokenRequest(scheme, capture.selectedUrl(), capture.client(), List.of(new Serialize.Pair("grant_type", "refresh_token"),
                new Serialize.Pair("refresh_token", previous.refreshToken())), previous.refreshToken(), job.started, job.timeout);
        if (renewed.failure() != null || renewed.token() == null) {
            if (renewed.failure() != null && renewed.failure().rejected()) fenced.deleteIfCurrent(scheme, token -> {
                if (!configurationCurrent(capture)) return false;
                synchronized (lock) { return job.timely() && currentLocked(scheme, seen) && sameToken(token, previous) && mutateLocked(scheme) != null; }
            });
            return new RefreshResult(new Auth.Token(null, renewed.failure() != null ? renewed.failure()
                    : new Auth.Failure("OAuth refresh returned no token; authorize again.", false)), null);
        }
        Version[] committed = new Version[1];
        boolean changed = fenced.setIfCurrent(scheme, renewed.token(), token -> {
            if (!configurationCurrent(capture)) return false;
            synchronized (lock) {
                if (!job.timely() || !currentLocked(scheme, seen) || !sameToken(token, previous)) return false;
                committed[0] = mutateLocked(scheme);
                return committed[0] != null;
            }
        });
        if (!changed || committed[0] == null || !job.timely() || !configurationCurrent(capture) || !current(scheme, committed[0])) return new RefreshResult(changed(), null);
        return new RefreshResult(Auth.Token.of(renewed.token().accessToken()), committed[0]);
    }
}
