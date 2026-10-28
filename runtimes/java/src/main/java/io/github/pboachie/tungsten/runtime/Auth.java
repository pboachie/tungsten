// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.time.Duration;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * Auth resolution: an operation's security is an OR of AND-sets of scheme names; the first
 * alternative the configured credentials fully satisfy is applied. A composite profile satisfies
 * the names in its satisfies list when every part it needs for this request is configured. Also
 * the OAuth2 token requests (client credentials with a single flight, and the stored tokens of
 * the authorization-code flow with refresh before expiry and once after a 401).
 */
final class Auth {
    static final String INSECURE_HTTP_AUTH = "Refusing to send credentials over non-loopback HTTP. Use HTTPS, or set ClientOptions.allowInsecureHttp(true) for intentional insecure transport. Nothing was sent.";

    static boolean credentialHeader(ApiDescriptor api, String name, boolean preservedIdempotency) {
        return (!preservedIdempotency && Text.looksSensitive(name)) || name.equalsIgnoreCase("Authorization") || name.equalsIgnoreCase("Proxy-Authorization") || name.equalsIgnoreCase("Cookie")
                || api.auth().stream().anyMatch(s -> s.kind().equals("api_key") && "header".equals(s.in()) && s.wire().equalsIgnoreCase(name)
                    || s.kind().equals("composite") && s.parts().stream().anyMatch(p -> p.kind().equals("header")
                        && (p.fromConfig() == null || p.fromConfig().isEmpty()) && p.name().equalsIgnoreCase(name)));
    }

    static boolean insecureHttp(String target, boolean allow) {
        if (allow) return false;
        try {
            java.net.URI url = new java.net.URI(target);
            if (!"http".equalsIgnoreCase(url.getScheme())) return false;
            String host = url.getHost();
            if (host == null) return true;
            if (host.equalsIgnoreCase("localhost") || host.equalsIgnoreCase("localhost.")) return false;
            if (host.startsWith("[") && host.endsWith("]")) {
                String literal = host.substring(1, host.length() - 1);
                if (!literal.matches("[0-9a-fA-F:.]+")) return true;
                java.net.InetAddress ip = java.net.InetAddress.getByName(literal);
                return !(ip instanceof java.net.Inet6Address && ip.isLoopbackAddress());
            }
            String[] octets = host.split("\\.", -1);
            if (octets.length != 4 || !octets[0].equals("127")) return true;
            for (String octet : octets) {
                if (!octet.matches("[0-9]{1,3}") || Integer.parseInt(octet) > 255) return true;
            }
            return false;
        } catch (java.net.UnknownHostException error) {
            return true;
        } catch (java.net.URISyntaxException error) {
            return false;
        }
    }
    /** Tokens expiring within this many milliseconds count as expired. */
    static final long EXPIRY_SKEW_MS = 30000;

    private final ClientCore core;
    final OAuthSessions sessions;
    private final java.util.concurrent.locks.ReentrantLock ccLock = new java.util.concurrent.locks.ReentrantLock();
    private final Map<String, Cached> ccTokens = new HashMap<>();

    private record Cached(String token, long expiresAt) {}

    Auth(ClientCore core) {
        this.core = core;
        this.sessions = new OAuthSessions(core, this);
    }

    record PlanHeader(String name, String value, boolean secret) {}

    /** What auth adds to a request. */
    static final class Plan {
        final List<PlanHeader> headers = new ArrayList<>();
        final List<Serialize.Pair> cookies = new ArrayList<>();
        final List<Serialize.Pair> query = new ArrayList<>();
    }

    /** The result of resolving auth. */
    static final class Resolved {
        final Plan plan = new Plan();
        final List<String> secrets = new ArrayList<>();
        /** The authorization-code scheme whose stored token the plan carries. */
        String oauth;
        /** The remediation of an AUTH_FAILED envelope. */
        String failed;
    }

    private enum Kind {
        COMPOSITE,
        SECRET,
        OAUTH_CODE,
        OAUTH
    }

    private static final class Satisfier {
        Kind kind;
        String name;
        List<ApiDescriptor.CompositePart> parts;
        AuthCredential values;
        boolean mutation;
        ApiDescriptor.AuthScheme scheme;
        String secret;
        String clientSecret;
        String clientId;
        String tokenUrl;
        List<String> scopes;
    }

    private record Tried(String key, Satisfier sat, List<String> missing) {}

    static boolean isSafeMethod(String method) {
        switch (method) {
            case "GET":
            case "HEAD":
            case "OPTIONS":
            case "TRACE":
            case "QUERY":
                return true;
            default:
                return false;
        }
    }

    private static String compositeKey(ApiDescriptor.CompositePart p) {
        if (p.kind().equals("cookie")) {
            return p.name();
        }
        if (p.kind().equals("bearer")) {
            return "bearer";
        }
        if (p.equalsCookie() != null && !p.equalsCookie().isEmpty()) {
            return p.equalsCookie();
        }
        if (p.fromConfig() != null && !p.fromConfig().isEmpty()) {
            return p.fromConfig();
        }
        return p.name();
    }

    private static String checkPrefix(String token, String prefix, String what) {
        if (prefix != null && !prefix.isEmpty() && !token.startsWith(prefix)) {
            return "The " + what + " credential must start with \"" + prefix + "\"; check that the right kind of token is configured.";
        }
        return null;
    }

    private static boolean skips(ApiDescriptor.CompositePart p, boolean mutation) {
        return p.kind().equals("header") && p.mutationOnly() && !mutation;
    }

    private static Tried tryComposite(String name, List<ApiDescriptor.CompositePart> parts, AuthCredential cred, String method) {
        if (!cred.isParts()) {
            return new Tried(null, null, List.of("auth." + name + " (an object for the composite profile " + name + ")"));
        }
        boolean mutation = !isSafeMethod(method);
        List<String> missing = new ArrayList<>();
        for (ApiDescriptor.CompositePart p : parts) {
            if (skips(p, mutation)) {
                continue;
            }
            String key = compositeKey(p);
            String v = cred.part(key);
            if (v == null || v.isEmpty()) {
                String role;
                if (p.kind().equals("cookie")) {
                    role = "cookie " + p.name();
                } else if (p.kind().equals("bearer")) {
                    role = "bearer token";
                } else {
                    role = "header " + p.name();
                    if (p.equalsCookie() != null && !p.equalsCookie().isEmpty()) {
                        role += " (equal to cookie " + p.equalsCookie() + ")";
                    }
                }
                missing.add("auth." + name + "[" + Json.jsString(key) + "] (" + role + " of the composite profile " + name + ")");
            }
        }
        if (!missing.isEmpty()) {
            return new Tried(null, null, missing);
        }
        Satisfier s = new Satisfier();
        s.kind = Kind.COMPOSITE;
        s.name = name;
        s.parts = parts;
        s.values = cred;
        s.mutation = mutation;
        return new Tried("composite:" + name, s, null);
    }

    private static Tried tryDirect(ApiDescriptor.AuthScheme scheme, AuthCredential cred) {
        String name = scheme.name();
        if (scheme.kind().equals("oauth2") && cred != null && cred.isParts()) {
            if ("authorizationCode".equals(cred.part("flow"))) {
                if (scheme.authorizationCode() == null) {
                    return new Tried(null, null, List.of("auth." + name + " (sets flow \"authorizationCode\", but the API descriptor has no authorizationCode flow for " + name + ")"));
                }
                String id = cred.part("client_id", "clientId");
                if (id == null || id.isEmpty()) {
                    return new Tried(null, null, List.of("auth." + name + " (a client_id for the authorizationCode flow)"));
                }
                Satisfier s = new Satisfier();
                s.kind = Kind.OAUTH_CODE;
                s.name = name;
                s.clientSecret = cred.part("client_secret", "clientSecret");
                return new Tried("scheme:" + name, s, null);
            }
            String id = cred.part("client_id", "clientId");
            String secret = cred.part("client_secret", "clientSecret");
            if (id != null && secret != null && scheme.tokenUrl() != null && !scheme.tokenUrl().isEmpty()) {
                Satisfier s = new Satisfier();
                s.kind = Kind.OAUTH;
                s.name = name;
                s.tokenUrl = scheme.tokenUrl();
                s.scopes = scheme.scopes();
                s.clientId = id;
                s.clientSecret = secret;
                return new Tried("scheme:" + name, s, null);
            }
            String code = scheme.authorizationCode() != null
                    ? ", or {flow: \"authorizationCode\", client_id, client_secret?, redirect_uri?}"
                    : "";
            return new Tried(null, null, List.of("auth." + name + " (an access token, or {client_id, client_secret} for the token URL" + code + ")"));
        }
        if (cred != null && !cred.isParts() && !cred.secretValue().isEmpty()) {
            Satisfier s = new Satisfier();
            s.kind = Kind.SECRET;
            s.scheme = scheme;
            s.secret = cred.secretValue();
            return new Tried("scheme:" + name, s, null);
        }
        String what;
        switch (scheme.kind()) {
            case "http_basic":
                what = "\"user:password\" for HTTP Basic scheme " + name;
                break;
            case "api_key":
                what = "API key for scheme " + name + " (" + scheme.in() + " " + scheme.wire() + ")";
                break;
            default:
                what = "bearer token for scheme " + name;
        }
        return new Tried(null, null, List.of("auth." + name + " (" + what + ")"));
    }

    private Tried tryScheme(String name, String method) {
        List<String> missing = new ArrayList<>();
        for (ApiDescriptor.AuthScheme s : core.api.auth()) {
            if (!s.kind().equals("composite") || !s.satisfies().contains(name)) {
                continue;
            }
            AuthCredential cred = core.auth.get(s.name());
            if (cred == null) {
                missing.add("auth." + s.name() + " (composite profile " + s.name() + ")");
                continue;
            }
            Tried t = tryComposite(s.name(), s.parts(), cred, method);
            if (t.sat() != null) {
                return t;
            }
            missing.addAll(t.missing());
        }
        ApiDescriptor.AuthScheme direct = null;
        for (ApiDescriptor.AuthScheme s : core.api.auth()) {
            if (s.name().equals(name) && !s.kind().equals("composite")) {
                direct = s;
                break;
            }
        }
        if (direct != null) {
            Tried t = tryDirect(direct, core.auth.get(name));
            if (t.sat() != null) {
                return t;
            }
            // A composite profile covering the name explains the gap better.
            if (missing.isEmpty()) {
                missing.addAll(t.missing());
            }
        } else if (missing.isEmpty()) {
            missing.add("a scheme named " + name + " (the API descriptor does not define it)");
        }
        return new Tried(null, null, missing);
    }

    private boolean configured(String name) {
        if (core.auth.containsKey(name)) {
            return true;
        }
        for (ApiDescriptor.AuthScheme s : core.api.auth()) {
            if (s.kind().equals("composite") && s.satisfies().contains(name) && core.auth.containsKey(s.name())) {
                return true;
            }
        }
        return false;
    }

    private static void bearer(Plan plan, String token) {
        plan.headers.add(new PlanHeader("Authorization", "Bearer " + token, true));
    }

    private String apply(Satisfier s, Resolved out) {
        switch (s.kind) {
            case COMPOSITE:
                for (ApiDescriptor.CompositePart p : s.parts) {
                    if (skips(p, s.mutation)) {
                        continue;
                    }
                    String value = s.values.part(compositeKey(p));
                    if (p.kind().equals("cookie")) {
                        out.plan.cookies.add(new Serialize.Pair(p.name(), value));
                        out.secrets.add(value);
                    } else if (p.kind().equals("bearer")) {
                        String problem = checkPrefix(value, p.prefix(), s.name + " bearer");
                        if (problem != null) {
                            return problem;
                        }
                        bearer(out.plan, value);
                        out.secrets.add(value);
                    } else {
                        boolean secret = p.fromConfig() == null || p.fromConfig().isEmpty();
                        out.plan.headers.add(new PlanHeader(p.name(), value, secret));
                        if (secret) {
                            out.secrets.add(value);
                        }
                    }
                }
                return null;
            case SECRET:
                out.secrets.add(s.secret);
                switch (s.scheme.kind()) {
                    case "api_key":
                        if ("query".equals(s.scheme.in())) {
                            out.plan.query.add(new Serialize.Pair(s.scheme.wire(), s.secret));
                        } else if ("cookie".equals(s.scheme.in())) {
                            out.plan.cookies.add(new Serialize.Pair(s.scheme.wire(), s.secret));
                        } else {
                            out.plan.headers.add(new PlanHeader(s.scheme.wire(), s.secret, true));
                        }
                        return null;
                    case "http_bearer": {
                        String problem = checkPrefix(s.secret, s.scheme.prefix(), s.scheme.name());
                        if (problem != null) {
                            return problem;
                        }
                        bearer(out.plan, s.secret);
                        return null;
                    }
                    case "http_basic": {
                        String encoded = Text.base64(Text.utf8(s.secret));
                        out.secrets.add(encoded);
                        out.plan.headers.add(new PlanHeader("Authorization", "Basic " + encoded, true));
                        return null;
                    }
                    default:
                        bearer(out.plan, s.secret);
                        return null;
                }
            case OAUTH_CODE: {
                if (s.clientSecret != null && !s.clientSecret.isEmpty()) {
                    out.secrets.add(s.clientSecret);
                }
                Token token = storedToken(s.name, false);
                if (token.failure() != null) {
                    return token.failure().message();
                }
                out.secrets.add(token.value());
                bearer(out.plan, token.value());
                if (out.oauth == null) {
                    out.oauth = s.name;
                }
                return null;
            }
            default: {
                out.secrets.add(s.clientSecret);
                Token token = clientCredentialsToken(s);
                if (token.failure() != null) {
                    return token.failure().message();
                }
                out.secrets.add(token.value());
                bearer(out.plan, token.value());
                return null;
            }
        }
    }

    /** Resolves the credentials of an operation; a problem is the result's failed text. */
    Resolved resolve(OperationDescriptor op, String method, String credentialError) {
        Resolved none = new Resolved();
        if (op.security().isEmpty()) {
            return none;
        }
        List<String> best = null;
        boolean bestTouched = false;
        // An empty alternative (anonymous access) is the fallback, never preferred over
        // configured credentials.
        List<List<String>> ordered = new ArrayList<>();
        for (List<String> alt : op.security()) {
            if (!alt.isEmpty()) {
                ordered.add(alt);
            }
        }
        for (List<String> alt : op.security()) {
            if (alt.isEmpty()) {
                ordered.add(alt);
            }
        }
        for (List<String> alternative : ordered) {
            List<String> keys = new ArrayList<>();
            List<Satisfier> sats = new ArrayList<>();
            List<String> missing = new ArrayList<>();
            for (String name : alternative) {
                Tried t = tryScheme(name, method);
                if (t.sat() == null) {
                    missing.addAll(t.missing());
                    continue;
                }
                int at = keys.indexOf(t.key());
                if (at >= 0) {
                    sats.set(at, t.sat());
                } else {
                    keys.add(t.key());
                    sats.add(t.sat());
                }
            }
            if (missing.isEmpty()) {
                if (!sats.isEmpty() && credentialError != null) {
                    Resolved failed = new Resolved();
                    failed.failed = credentialError;
                    return failed;
                }
                Resolved out = new Resolved();
                for (Satisfier s : sats) {
                    String problem = apply(s, out);
                    if (problem != null) {
                        Resolved failed = new Resolved();
                        failed.failed = problem + " Operation " + op.id() + " was not sent.";
                        return failed;
                    }
                }
                return out;
            }
            List<String> unique = new ArrayList<>();
            for (String m : missing) {
                if (!unique.contains(m)) {
                    unique.add(m);
                }
            }
            boolean touched = false;
            for (String name : alternative) {
                if (configured(name)) {
                    touched = true;
                }
            }
            boolean better = best == null || (touched && !bestTouched) || (touched == bestTouched && unique.size() < best.size());
            if (better) {
                best = unique;
                bestTouched = touched;
            }
        }
        List<String> alternatives = new ArrayList<>();
        for (List<String> alt : op.security()) {
            alternatives.add(alt.isEmpty() ? "no credentials" : String.join(" AND ", alt));
        }
        Resolved failed = new Resolved();
        failed.failed = "Operation " + op.id() + " needs " + String.join(" OR ", alternatives) + ". Missing: "
                + (best == null ? "credentials" : String.join("; ", best))
                + ". Configure it in ClientOptions.auth and call again; the request was not sent.";
        return failed;
    }

    // ------------------------------------------------------------- OAuth2

    static String formBody(List<Serialize.Pair> fields) {
        List<String> parts = new ArrayList<>();
        for (Serialize.Pair f : fields) {
            parts.add(Text.percentEncode(f.k()) + "=" + Text.percentEncode(f.v()));
        }
        return String.join("&", parts);
    }

    /** A token request's failure. */
    record Failure(String message, boolean rejected) {}

    /** The client of an authorization-code flow, or the reason it is not configured. */
    record CodeClient(String clientId, String clientSecret, String redirectUri) {
        @Override public String toString() { return "CodeClient{..}"; }
    }

    ApiDescriptor.AuthorizationCode codeFlow(String scheme, String[] reason) {
        for (ApiDescriptor.AuthScheme s : core.api.auth()) {
            if (!s.name().equals(scheme)) {
                continue;
            }
            if (s.kind().equals("oauth2") && s.authorizationCode() != null) {
                return s.authorizationCode();
            }
            reason[0] = "the scheme " + scheme + " has no authorizationCode flow";
            return null;
        }
        reason[0] = "the API descriptor defines no scheme named " + scheme;
        return null;
    }

    CodeClient codeClient(String scheme, String[] reason) {
        String invalid = "auth." + scheme + " must be AuthCredential.parts with flow \"authorizationCode\", client_id, and optionally client_secret and redirect_uri";
        AuthCredential cred = core.auth.get(scheme);
        if (cred == null || !cred.isParts() || !"authorizationCode".equals(cred.part("flow"))) {
            reason[0] = invalid;
            return null;
        }
        String id = cred.part("client_id", "clientId");
        if (id == null || id.isEmpty()) {
            reason[0] = "auth." + scheme + " needs a client_id";
            return null;
        }
        return new CodeClient(id, cred.part("client_secret", "clientSecret"), cred.part("redirect_uri", "redirectUri"));
    }

    /** A token, or why there is none. */
    record Token(String value, Failure failure) {
        @Override public String toString() { return "Token{..}"; }
        static Token of(String value) {
            return new Token(value, null);
        }

        static Token failed(String message) {
            return new Token(null, new Failure(message, false));
        }
    }

    /** A token endpoint's answer: the token to store, or the failure. */
    record TokenResult(TokenStore.StoredToken token, Failure failure) {
        @Override public String toString() { return "TokenResult{..}"; }
    }

    /** Managed read/refresh with generation, mutation-epoch and physical-work fences. */
    Token storedToken(String scheme, boolean force) {
        return sessions.storedToken(scheme, force);
    }

    /** The token to resend once after a 401, or null. */
    String refreshAfterRejection(String scheme, String rejected) {
        return sessions.refreshAfterRejection(scheme, rejected);
    }

    private Duration tokenTimeout() {
        return core.timeout;
    }

    private static boolean isErrorCode(String code) {
        return java.util.Set.of("invalid_request", "invalid_client", "invalid_grant", "unauthorized_client",
                "unsupported_grant_type", "invalid_scope", "temporarily_unavailable", "server_error").contains(code);
    }

    private static String stringAt(Object body, String path) {
        Object v = Json.at(body, path);
        return v instanceof String ? (String) v : "";
    }

    /** Sends a form to a token endpoint. */
    TokenResult tokenRequest(String scheme, String url, CodeClient client, List<Serialize.Pair> fields, String previous) {
        return tokenRequest(scheme, url, client, fields, previous, System.nanoTime(), Long.MAX_VALUE / 2);
    }

    TokenResult tokenRequest(String scheme, String url, CodeClient client, List<Serialize.Pair> fields, String previous, long ownerStarted, long ownerTimeout) {
        long started = System.nanoTime();
        try {
            return tokenRequestOwned(scheme, url, client, fields, previous, started, ownerTimeout - (started - ownerStarted));
        } catch (RuntimeException failure) {
            return tokenJobFailure();
        }
    }

    private static TokenResult tokenJobFailure() {
        return new TokenResult(null, new Failure("The OAuth2 token job did not complete within its bounded response/deadline or could not be delivered; authorize again if a code may have been consumed.", false));
    }

    private TokenResult tokenRequestOwned(String scheme, String url, CodeClient client, List<Serialize.Pair> fields, String previous, long started, long ownerRemaining) {
        long timeout = Math.min(OAuthInputs.timeoutNanos(tokenTimeout()), ownerRemaining);
        OAuthInputs.require(timeout > 0);
        OAuthInputs.require(OAuthInputs.tokenLimit(core.options.maxOAuthTokenResponseBytes));
        if (insecureHttp(url, core.options.allowInsecureHttp)) return new TokenResult(null, new Failure(INSECURE_HTTP_AUTH, false));
        List<Serialize.Header> headers = new ArrayList<>();
        headers.add(new Serialize.Header("Content-Type", "application/x-www-form-urlencoded", false));
        headers.add(new Serialize.Header("Accept", "application/json", false));
        List<Serialize.Pair> all = new ArrayList<>(fields);
        if (client.clientSecret() != null && !client.clientSecret().isEmpty()) {
            String basic = Text.base64(Text.utf8(Text.percentEncode(client.clientId()) + ":" + Text.percentEncode(client.clientSecret())));
            headers.add(new Serialize.Header("Authorization", "Basic " + basic, true));
        } else {
            all.add(new Serialize.Pair("client_id", client.clientId()));
        }
        Serialize.Payload payload = Serialize.Payload.of(Text.utf8(formBody(all)));
        long remaining = timeout - (System.nanoTime() - started);
        if (Thread.currentThread().isInterrupted() || remaining <= 0) return tokenJobFailure();
        Transport.Outcome out = Transport.oauthAttempt(core.http, new Transport.Request(url, "POST", headers,
                payload, Duration.ofNanos(remaining), false, null, core.options.maxOAuthTokenResponseBytes));
        // Socket factories/close and body work may ignore interruption. Check after actual settlement,
        // before classifying either success or rejection (a late rejection must not delete a token).
        if (Thread.currentThread().isInterrupted() || System.nanoTime() - started >= timeout) return tokenJobFailure();
        if (out.kind != Transport.Kind.RESPONSE || !out.bodyOk) {
            return new TokenResult(null, new Failure("The OAuth2 token endpoint for " + scheme
                    + " could not be reached; check network access and the token URL.", false));
        }
        Classify.Decoded decoded = Classify.decode(out.body, out.headers, "application/json");
        Object body = decoded.has ? decoded.value : Json.ABSENT;
        if (System.nanoTime() - started >= timeout) return tokenJobFailure();
        if (out.status < 200 || out.status > 299) {
            String named = "";
            Object err = Json.at(body, "error");
            if (err instanceof String && isErrorCode((String) err)) {
                named = " (" + err + ")";
            }
            return new TokenResult(null, new Failure("The OAuth2 token endpoint for " + scheme + " answered HTTP " + out.status + named
                    + "; check the code or refresh token, client credentials and redirect URI.", out.status >= 400 && out.status < 500));
        }
        String access = stringAt(body, "access_token");
        if (access.isEmpty()) {
            return new TokenResult(null, new Failure("The OAuth2 token endpoint for " + scheme + " returned no access_token.", false));
        }
        String refresh = stringAt(body, "refresh_token");
        if (refresh.isEmpty()) {
            refresh = previous;
        }
        Long expires = null;
        Object e = Json.at(body, "expires_in");
        if (Json.isNumber(e)) {
            expires = core.now() + (long) (Math.max(Json.asDouble(e), 0) * 1000);
        }
        String type = stringAt(body, "token_type");
        String scope = stringAt(body, "scope");
        if (System.nanoTime() - started >= timeout) return tokenJobFailure();
        return new TokenResult(new TokenStore.StoredToken(access, refresh == null || refresh.isEmpty() ? null : refresh, expires,
                type.isEmpty() ? null : type, scope.isEmpty() ? null : scope), null);
    }

    /** An OAuth2 client-credentials token, fetched once and cached (single flight). */
    private Token clientCredentialsToken(Satisfier s) {
        if (insecureHttp(s.tokenUrl, core.options.allowInsecureHttp)) return Token.failed(INSECURE_HTTP_AUTH);
        // A caller with a deadline stops waiting for another call's fetch at its deadline; the
        // fetch itself, shared by every waiter, is never cut short.
        if (Deadline.current() == null) {
            ccLock.lock();
        } else {
            try {
                if (!ccLock.tryLock(Math.max(0, Deadline.remainingNanos()), java.util.concurrent.TimeUnit.NANOSECONDS)) {
                    return Token.failed("The operation deadline expired while another call fetched the OAuth2 token for " + s.name + ".");
                }
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return Token.failed("OAuth2 token acquisition for " + s.name + " was interrupted.");
            }
        }
        try {
            long realNow = System.currentTimeMillis();
            Cached cached = ccTokens.get(s.name);
            if (cached != null && cached.expiresAt() > realNow + 30000) {
                return Token.of(cached.token());
            }
            List<String> form = new ArrayList<>();
            form.add("grant_type=client_credentials");
            if (s.scopes != null && !s.scopes.isEmpty()) {
                form.add("scope=" + Text.encodeFormComponent(String.join(" ", s.scopes)));
            }
            String basic = Text.base64(Text.utf8(Text.encodeComponent(s.clientId) + ":" + Text.encodeComponent(s.clientSecret)));
            List<Serialize.Header> headers = List.of(
                    new Serialize.Header("Content-Type", "application/x-www-form-urlencoded", false),
                    new Serialize.Header("Accept", "application/json", false),
                    new Serialize.Header("Authorization", "Basic " + basic, true));
            Transport.Outcome out = Transport.attempt(core.http, new Transport.Request(s.tokenUrl, "POST", headers,
                    Serialize.Payload.of(Text.utf8(String.join("&", form))), tokenTimeout(), false, null));
            if (out.kind != Transport.Kind.RESPONSE || !out.bodyOk) {
                return Token.failed("The OAuth2 token endpoint for " + s.name + " could not be reached; check network access and the token URL.");
            }
            if (out.status < 200 || out.status > 299) {
                return Token.failed("The OAuth2 token endpoint for " + s.name + " answered HTTP " + out.status + "; check client_id, client_secret and scopes.");
            }
            Classify.Decoded decoded = Classify.decode(out.body, out.headers, "application/json");
            Object body = decoded.has ? decoded.value : Json.ABSENT;
            String token = stringAt(body, "access_token");
            if (token.isEmpty()) {
                return Token.failed("The OAuth2 token endpoint for " + s.name + " returned no access_token.");
            }
            long lifetime = 3600000;
            Object e = Json.at(body, "expires_in");
            if (Json.isNumber(e)) {
                lifetime = (long) Math.max(Json.asDouble(e) * 1000, 0);
            }
            ccTokens.put(s.name, new Cached(token, realNow + lifetime));
            return Token.of(token);
        } finally {
            ccLock.unlock();
        }
    }
}
