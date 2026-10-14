// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.HashMap;
import java.util.Map;

/**
 * Holds the tokens of the authorization-code schemes, by scheme name. Implement it to persist
 * tokens; the default is {@link Memory}, per client.
 */
public interface TokenStore {
    /** The token stored for the scheme, or null. */
    StoredToken get(String scheme);

    /** Stores the scheme's token. */
    void set(String scheme, StoredToken token);

    /** Forgets the scheme's token. */
    void delete(String scheme);

    /**
     * What a store keeps for one scheme. {@link #toString} shows no secret.
     *
     * @param accessToken the access token
     * @param refreshToken the refresh token, or null
     * @param expiresAt the expiry in epoch milliseconds, or null when the server gave no lifetime
     * @param tokenType the token type, or null
     * @param scope the granted scope, or null
     */
    record StoredToken(
            String accessToken, String refreshToken, Long expiresAt, String tokenType, String scope) {
        @Override
        public String toString() {
            return "StoredToken{" + Text.REDACTED + "}";
        }
    }

    /** Keeps tokens in memory. */
    final class Memory implements TokenStore {
        private final Map<String, StoredToken> tokens = new HashMap<>();

        @Override
        public synchronized StoredToken get(String scheme) {
            return tokens.get(scheme);
        }

        @Override
        public synchronized void set(String scheme, StoredToken token) {
            tokens.put(scheme, token);
        }

        @Override
        public synchronized void delete(String scheme) {
            tokens.remove(scheme);
        }
    }
}
