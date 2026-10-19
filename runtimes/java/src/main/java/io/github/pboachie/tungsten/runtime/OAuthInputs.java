// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.io.ByteArrayOutputStream;
import java.net.URI;
import java.net.URISyntaxException;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** Bounded input rules shared by the managed browser flow and its refresh dispatch. */
final class OAuthInputs {
    static final int CAPACITY = 32;
    static final int MAX_ITEMS = 64;
    static final int MAX_CAPTURE_BYTES = 32768;
    static final int MAX_CALLBACK_BYTES = 16384;
    static final long LIFETIME_NANOS = 600_000_000_000L;
    private static final Set<String> PROTECTED = Set.of(
            "response_type", "client_id", "client_secret", "redirect_uri", "scope", "resource", "audience",
            "state", "code_challenge", "code_challenge_method", "code_verifier", "grant_type", "code", "iss",
            "error", "error_description", "error_uri", "request", "request_uri", "response_mode");

    private OAuthInputs() {}

    static final class Invalid extends RuntimeException {
        private static final long serialVersionUID = 1L;
    }

    static void require(boolean allowed) {
        if (!allowed) throw new Invalid();
    }

    /** Counts without allocating a replacement-encoded copy or accepting unpaired surrogates. */
    static int bytes(String value, int limit) {
        require(value != null && value.length() <= limit);
        int count = 0;
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (c <= 0x7f) count++;
            else if (c <= 0x7ff) count += 2;
            else if (Character.isHighSurrogate(c)) {
                require(i + 1 < value.length() && Character.isLowSurrogate(value.charAt(++i)));
                count += 4;
            } else {
                require(!Character.isLowSurrogate(c));
                count += 3;
            }
            require(count <= limit);
        }
        return count;
    }

    static final class Budget {
        private int used;

        void charge(String value) {
            used += bytes(value, MAX_CAPTURE_BYTES - used);
        }

        List<String> scopes(List<String> values) {
            require(values != null && values.size() <= MAX_ITEMS);
            List<String> copy = new ArrayList<>();
            for (String value : values) {
                require(copy.size() < MAX_ITEMS);
                charge(value);
                copy.add(value);
            }
            return List.copyOf(copy);
        }

        Map<String, String> extra(Map<String, String> values) {
            Map<String, String> copy = new LinkedHashMap<>();
            if (values == null) return copy;
            require(values.size() <= MAX_ITEMS);
            for (Map.Entry<String, String> entry : values.entrySet()) {
                require(copy.size() < MAX_ITEMS);
                String key = entry.getKey();
                String value = entry.getValue();
                charge(key);
                charge(value);
                require(!key.isEmpty() && !PROTECTED.contains(key) && !copy.containsKey(key));
                copy.put(key, value);
            }
            return copy;
        }
    }

    static boolean tokenLimit(long limit) {
        return limit > 0 && limit <= 9_007_199_254_740_991L;
    }

    static long timeoutNanos(Duration timeout) {
        require(timeout != null);
        long value;
        try {
            value = timeout.toNanos();
        } catch (ArithmeticException invalid) {
            throw new Invalid();
        }
        require(value > 0 && value <= Long.MAX_VALUE / 2);
        return value;
    }

    private static int hex(char c) {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        throw new Invalid();
    }

    private static String decode(String value) {
        byte[] raw = value.getBytes(StandardCharsets.UTF_8);
        ByteArrayOutputStream decoded = new ByteArrayOutputStream(raw.length);
        for (int i = 0; i < raw.length; i++) {
            int c = raw[i] & 255;
            if (c == '%') {
                require(i + 2 < raw.length);
                decoded.write((hex((char) raw[i + 1]) << 4) | hex((char) raw[i + 2]));
                i += 2;
            } else decoded.write(c == '+' ? ' ' : c);
        }
        try {
            return StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(decoded.toByteArray())).toString();
        } catch (CharacterCodingException invalid) {
            throw new Invalid();
        }
    }

    static Map<String, String> fields(String query) {
        bytes(query, MAX_CALLBACK_BYTES);
        int count = query.isEmpty() ? 0 : 1;
        for (int i = 0; i < query.length(); i++) {
            if (query.charAt(i) == '&') require(++count <= MAX_ITEMS);
        }
        Map<String, String> fields = new LinkedHashMap<>();
        if (query.isEmpty()) return fields;
        int decodedBytes = 0;
        int start = 0;
        while (start <= query.length()) {
            int end = query.indexOf('&', start);
            if (end < 0) end = query.length();
            int equal = query.indexOf('=', start);
            require(equal > start && equal < end);
            String key = decode(query.substring(start, equal));
            String value = decode(query.substring(equal + 1, end));
            decodedBytes += bytes(key, MAX_CALLBACK_BYTES) + bytes(value, MAX_CALLBACK_BYTES);
            require(!key.isEmpty() && decodedBytes <= MAX_CALLBACK_BYTES && !fields.containsKey(key));
            fields.put(key, value);
            if (end == query.length()) break;
            start = end + 1;
        }
        return fields;
    }

    static void endpoint(String value, boolean redirect) {
        bytes(value, MAX_CAPTURE_BYTES);
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            require(c > 0x20 && c != 0x7f);
        }
        require(value.indexOf('#') < 0);
        URI uri;
        try {
            uri = new URI(value);
        } catch (URISyntaxException invalid) {
            throw new Invalid();
        }
        require(uri.isAbsolute() && uri.getRawUserInfo() == null && uri.getPort() <= 65535);
        int query = value.indexOf('?');
        if (redirect) require(query < 0);
        else {
            require(("http".equalsIgnoreCase(uri.getScheme()) || "https".equalsIgnoreCase(uri.getScheme()))
                    && uri.getHost() != null && !uri.getHost().isEmpty());
            if (query >= 0) {
                for (String key : fields(value.substring(query + 1)).keySet()) require(!PROTECTED.contains(key));
            }
        }
    }

    static boolean sameScopes(List<String> current, List<String> captured) {
        if (current == null || current.size() > MAX_ITEMS) return false;
        int index = 0;
        for (String scope : current) {
            if (index >= captured.size() || !captured.get(index++).equals(scope)) return false;
        }
        return index == captured.size();
    }
}
