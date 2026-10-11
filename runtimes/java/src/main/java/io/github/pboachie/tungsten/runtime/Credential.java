// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.Map;
import java.util.TreeMap;

/**
 * The credential of one auth scheme: a secret string for api_key, http_bearer and http_basic
 * ("user:password") schemes, or parts keyed by cookie name or config key for a composite profile
 * (or {client_id, client_secret} of an OAuth2 scheme, {flow: "authorizationCode", client_id, ...}
 * for its authorization-code flow). {@link #toString} never shows it.
 */
public final class Credential {
    private final String secret;
    private final Map<String, String> parts;

    private Credential(String secret, Map<String, String> parts) {
        this.secret = secret;
        this.parts = parts;
    }

    /** A secret string credential. */
    public static Credential secret(String value) {
        return new Credential(value == null ? "" : value, null);
    }

    /** A credential made of named parts. */
    public static Credential parts(Map<String, String> parts) {
        return new Credential(null, new TreeMap<>(parts));
    }

    boolean isParts() {
        return parts != null;
    }

    String secretValue() {
        return secret;
    }

    String part(String key) {
        return parts == null ? null : parts.get(key);
    }

    /** A part by its snake_case or camelCase key. */
    String part(String snake, String camel) {
        String v = part(snake);
        return v != null ? v : part(camel);
    }

    @Override
    public String toString() {
        if (parts != null) {
            return "Credential.parts(" + parts.size() + " parts, " + Text.REDACTED + ")";
        }
        return "Credential.secret(" + Text.REDACTED + ")";
    }
}
