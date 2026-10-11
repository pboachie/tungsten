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
 * ({@code client.oauth()}). Configure the scheme with {@code Credential.parts({flow:
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

    /** A fresh pair: pass the challenge to authorizationUrl and keep the verifier for exchangeCode. */
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
    public Result<String> authorizationUrl(String redirectUri, List<String> scopes, String state, String codeChallenge,
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
    public Result<TokenInfo> exchangeCode(String code, String redirectUri, String codeVerifier) {
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
        TokenStore.StoredToken stored = core.tokenStore.get(scheme);
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
