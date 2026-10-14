// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * Confirmation tokens: {@code tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, operation \n args
 * digest \n expiry))>}, valid only for the operation and the exact arguments until the expiry.
 */
final class Tokens {
    /** The lifetime of a confirmation token. */
    static final long TTL_MS = 5 * 60 * 1000;

    private static final String PREFIX = "tgc1";

    private Tokens() {}

    enum Check {
        VALID,
        EXPIRED,
        MISMATCH,
        MALFORMED
    }

    /** The SHA-256 of the canonical JSON of the arguments. */
    static String argsDigest(Object args) {
        return Text.sha256Hex(Text.utf8(Json.canonical(args)));
    }

    private static String signature(byte[] key, String operation, String digest, long expiry) {
        return Text.base64Url(Text.hmacSha256(key, Text.utf8(operation + "\n" + digest + "\n" + expiry)));
    }

    static String issue(byte[] key, String operation, Object args, long now) {
        long expiry = now + TTL_MS;
        return PREFIX + "." + expiry + "." + signature(key, operation, argsDigest(args), expiry);
    }

    /** The expiry a token claims when it has the token shape, else null. */
    static Long expiry(String token) {
        if (!token.startsWith(PREFIX + ".")) {
            return null;
        }
        String rest = token.substring(PREFIX.length() + 1);
        int dot = rest.indexOf('.');
        if (dot < 0) {
            return null;
        }
        String expiry = rest.substring(0, dot);
        String sig = rest.substring(dot + 1);
        if (expiry.isEmpty() || expiry.length() > 16 || !Text.allDigits(expiry) || sig.length() != 43) {
            return null;
        }
        for (int i = 0; i < sig.length(); i++) {
            char c = sig.charAt(i);
            if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '-')) {
                return null;
            }
        }
        long parsed = Long.parseLong(expiry);
        return Long.toString(parsed).equals(expiry) ? parsed : null;
    }

    static Check check(byte[] key, String token, String operation, Object args, long now) {
        Long expiry = expiry(token);
        if (expiry == null) {
            return Check.MALFORMED;
        }
        String given = token.substring(token.lastIndexOf('.') + 1);
        if (!Text.timingSafeEqual(signature(key, operation, argsDigest(args), expiry), given)) {
            return Check.MISMATCH;
        }
        return expiry > now ? Check.VALID : Check.EXPIRED;
    }
}
