// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;

/**
 * The authorization-code helpers of one OAuth2 scheme, as generated clients expose them
 * ({@code client.oauth()}). Configure the scheme with {@code AuthCredential.parts({flow:
 * "authorizationCode", client_id, client_secret?, redirect_uri?})}. Failures are AUTH_FAILED
 * envelopes.
 */
public final class OAuthFlow {
    /** The PKCE method; always S256. */
    public static final String PKCE_METHOD = "S256";

    private final ClientCore core;
    private final String scheme;

    OAuthFlow(ClientCore core, String scheme) {
        this.core = core;
        this.scheme = scheme;
    }

    /**
     * A PKCE pair (RFC 7636, S256). The verifier is a secret until the exchange.
     *
     * @param verifier the code verifier
     * @param challenge base64url(SHA-256(verifier))
     */
    public record Pkce(String verifier, String challenge) {
        @Override
        public String toString() {
            return "Pkce{challenge=" + challenge + "}";
        }
    }

    /** Explicit application-qualified issuer defense; missing iss never chooses a fallback. */
    public static final class IssuerDefense {
        private final boolean response;
        private final String value;

        private IssuerDefense(boolean response, String value) {
            this.response = response;
            this.value = value;
        }

        /** Require the exact decoded provider issuer, including on denial. */
        public static IssuerDefense issuerResponse(String issuer) {
            return new IssuerDefense(true, issuer);
        }

        /** Qualify a redirect that isolates this issuer from other issuers. */
        public static IssuerDefense distinctRedirect(String qualification) {
            return new IssuerDefense(false, qualification);
        }

        boolean response() { return response; }
        String value() { return value; }

        @Override
        public String toString() { return "IssuerDefense{..}"; }
    }

    /**
     * Managed authorization inputs. Collections are bounded and copied by the managed begin,
     * after admission; avoid concurrent application mutation while begin captures them.
     */
    public record AuthorizationParams(String sessionBinding, boolean s256Supported, IssuerDefense issuerDefense,
            String redirectUri, List<String> scopes, String resource, String audience, String accountContext,
            Map<String, String> extra) {
        /** A builder; owning binding and explicit provider qualifications are required at begin. */
        public static Builder builder() { return new Builder(); }

        @Override
        public String toString() { return "AuthorizationParams{..}"; }

        /** Builds managed inputs without cloning unbounded borrowed collections. */
        public static final class Builder {
            private String sessionBinding = "";
            private boolean s256Supported;
            private IssuerDefense issuerDefense;
            private String redirectUri;
            private List<String> scopes;
            private String resource;
            private String audience;
            private String accountContext;
            private Map<String, String> extra;

            private Builder() {}
            public Builder sessionBinding(String value) { sessionBinding = value; return this; }
            public Builder s256Supported(boolean value) { s256Supported = value; return this; }
            public Builder issuerDefense(IssuerDefense value) { issuerDefense = value; return this; }
            public Builder redirectUri(String value) { redirectUri = value; return this; }
            public Builder scopes(List<String> value) { scopes = value; return this; }
            public Builder resource(String value) { resource = value; return this; }
            public Builder audience(String value) { audience = value; return this; }
            public Builder accountContext(String value) { accountContext = value; return this; }
            public Builder extra(Map<String, String> value) { extra = value; return this; }
            public AuthorizationParams build() {
                return new AuthorizationParams(sessionBinding, s256Supported, issuerDefense, redirectUri,
                        scopes, resource, audience, accountContext, extra);
            }
        }
    }

    /** Handle and URL for the owning listener; neither appears in toString. */
    public record AuthorizationAttempt(String attemptHandle, String url, long expiresInMs) {
        @Override
        public String toString() { return "AuthorizationAttempt{expiresInMs=" + expiresInMs + "}"; }
    }

    /** Complete one managed attempt with its entire callback and owning listener/session binding. */
    public record ExchangeParams(String attemptHandle, String callbackUrl, String sessionBinding) {
        @Override
        public String toString() { return "ExchangeParams{..}"; }
    }

    /** Begin a bounded managed S256/state attempt and retain its handle with the owning listener. */
    public Result<AuthorizationAttempt> authorizationUrl(AuthorizationParams parameters) {
        return core.authResolver.sessions.begin(scheme, parameters);
    }

    /** Validate and claim a managed callback once; an ambiguous exchange never reopens its code. */
    public Result<TokenInfo> exchangeCode(ExchangeParams parameters) {
        return core.authResolver.sessions.complete(scheme, parameters);
    }

    /** Invalidate one owned attempt; claimed work retains capacity until physical settlement. */
    public Result<Void> cancelAuthorization(String attemptHandle, String sessionBinding) {
        return core.authResolver.sessions.cancel(scheme, attemptHandle, sessionBinding);
    }

    /** Invalidate local attempts before token removal; no remote revocation is performed. */
    public Result<Void> logout() {
        return core.authResolver.sessions.logout(scheme);
    }

    /**
     * What exchangeCode and refresh report: no secrets.
     *
     * @param scheme the scheme
     * @param expiresAt the expiry in epoch milliseconds, or null
     * @param scope the granted scope, or null
     * @param refreshable whether a refresh token is stored
     */
    public record TokenInfo(String scheme, Long expiresAt, String scope, boolean refreshable) {}

    /** base64url(SHA-256(ASCII(verifier))). */
    public static String challenge(String verifier) {
        return Text.base64Url(Text.sha256(Text.utf8(verifier)));
    }

    /** The scheme the helpers belong to. */
    public String scheme() {
        return scheme;
    }

    /** The tokens' store. */
    public TokenStore tokenStore() {
        return core.tokenStore;
    }

    private String operation(String helper) {
        return "oauth." + scheme + "." + helper;
    }

    private <T> Result<T> failed(String helper, String remediation) {
        return core.diag(operation(helper), Category.AUTH_FAILED).remedy(remediation).fail().toResult();
    }

    /** Low-level PKCE utility. Managed authorizationUrl generates and retains its own pair. */
    public Pkce pkce() {
        String verifier = Text.base64Url(Text.randomBytes(32));
        return new Pkce(verifier, challenge(verifier));
    }

    /**
     * The URL to send the user to.
     *
     * @param redirectUri the redirect URI, or null for the credential's redirect_uri
     * @param scopes the scopes, or null for the flow's; an empty list sends none
     * @param state the state, or null
     * @param codeChallenge the PKCE challenge, or null
     * @param extra further query parameters, added in key order; they never override the others
     */
    public Result<String> lowLevelAuthorizationUrl(String redirectUri, List<String> scopes, String state, String codeChallenge,
            Map<String, String> extra) {
        String[] reason = new String[1];
        ApiDescriptor.AuthorizationCode flow = core.authResolver.codeFlow(scheme, reason);
        if (flow == null) {
            return failed("authorizationUrl", "Cannot build an authorization URL: " + reason[0] + ".");
        }
        Auth.CodeClient client = core.authResolver.codeClient(scheme, reason);
        if (client == null) {
            return failed("authorizationUrl", "Cannot build an authorization URL: " + reason[0] + ".");
        }
        String redirect = redirectUri != null && !redirectUri.isEmpty() ? redirectUri : client.redirectUri();
        if (redirect == null || redirect.isEmpty()) {
            return failed("authorizationUrl", "Pass redirect_uri, or set redirect_uri in auth." + scheme + "; the authorization URL needs one.");
        }
        String scope = String.join(" ", scopes != null ? scopes : flow.scopes());
        List<Serialize.Pair> fields = new ArrayList<>();
        fields.add(new Serialize.Pair("response_type", "code"));
        fields.add(new Serialize.Pair("client_id", client.clientId()));
        fields.add(new Serialize.Pair("redirect_uri", redirect));
        if (!scope.isEmpty()) {
            fields.add(new Serialize.Pair("scope", scope));
        }
        if (state != null && !state.isEmpty()) {
            fields.add(new Serialize.Pair("state", state));
        }
        if (codeChallenge != null && !codeChallenge.isEmpty()) {
            fields.add(new Serialize.Pair("code_challenge", codeChallenge));
            fields.add(new Serialize.Pair("code_challenge_method", PKCE_METHOD));
        }
        Set<String> reserved = new HashSet<>();
        for (Serialize.Pair f : fields) {
            reserved.add(f.k());
        }
        if (extra != null) {
            for (Map.Entry<String, String> e : new TreeMap<>(extra).entrySet()) {
                if (!reserved.contains(e.getKey())) {
                    fields.add(new Serialize.Pair(e.getKey(), e.getValue()));
                }
            }
        }
        String base = flow.authorizationUrl() == null ? "" : flow.authorizationUrl();
        String sep = "?";
        if (base.contains("?")) {
            sep = base.endsWith("?") || base.endsWith("&") ? "" : "&";
        }
        String url = base + sep + Auth.formBody(fields);
        return new Result.Ok<>(url, true, url, ResponseMeta.local(), null);
    }

    /** Trades the code from the redirect for tokens, stored for later calls. */
    public Result<TokenInfo> lowLevelExchangeCode(String code, String redirectUri, String codeVerifier) {
        String[] reason = new String[1];
        ApiDescriptor.AuthorizationCode flow = core.authResolver.codeFlow(scheme, reason);
        if (flow == null) {
            return failed("exchangeCode", "Cannot exchange the code: " + reason[0] + ".");
        }
        Auth.CodeClient client = core.authResolver.codeClient(scheme, reason);
        if (client == null) {
            return failed("exchangeCode", "Cannot exchange the code: " + reason[0] + ".");
        }
        if (code == null || code.isEmpty()) {
            return failed("exchangeCode", "Pass the authorization code the server redirected back with as code.");
        }
        String redirect = redirectUri != null && !redirectUri.isEmpty() ? redirectUri : client.redirectUri();
        if (redirect == null || redirect.isEmpty()) {
            return failed("exchangeCode", "Pass redirect_uri, or set redirect_uri in auth." + scheme
                    + "; it must equal the one used for the authorization URL.");
        }
        List<Serialize.Pair> fields = new ArrayList<>();
        fields.add(new Serialize.Pair("grant_type", "authorization_code"));
        fields.add(new Serialize.Pair("code", code));
        fields.add(new Serialize.Pair("redirect_uri", redirect));
        if (codeVerifier != null && !codeVerifier.isEmpty()) {
            fields.add(new Serialize.Pair("code_verifier", codeVerifier));
        }
        Auth.TokenResult r = core.authResolver.tokenRequest(scheme, flow.tokenUrl(), client, fields, null);
        if (r.failure() != null) {
            return failed("exchangeCode", r.failure().message());
        }
        core.tokenStore.set(scheme, r.token());
        TokenInfo info = info(r.token());
        return new Result.Ok<>(info, true, null, ResponseMeta.local(), null);
    }

    /** Refreshes the stored token now. Calls refresh on their own when it expires. */
    public Result<TokenInfo> refresh() {
        Auth.Token t = core.authResolver.storedToken(scheme, true);
        if (t.failure() != null) {
            return failed("refresh", t.failure().message());
        }
        TokenStore.StoredToken stored = core.authResolver.sessions.readToken(scheme);
        if (stored == null || stored.accessToken() == null || stored.accessToken().isEmpty()) {
            return failed("refresh", "No token is stored for " + scheme + ".");
        }
        TokenInfo info = info(stored);
        return new Result.Ok<>(info, true, null, ResponseMeta.local(), null);
    }

    private TokenInfo info(TokenStore.StoredToken t) {
        return new TokenInfo(scheme, t.expiresAt(), t.scope(), t.refreshToken() != null && !t.refreshToken().isEmpty());
    }
}
