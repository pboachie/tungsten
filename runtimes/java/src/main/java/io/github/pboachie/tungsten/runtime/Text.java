// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CharsetDecoder;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.Locale;
import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;

/** Text, encoding and hashing helpers shared by the runtime. */
final class Text {
    /** Written wherever a secret or sensitive value would appear. */
    static final String REDACTED = "<redacted>";

    /** The longest string kept in received_value. */
    static final int MAX_RECEIVED_CHARS = 200;

    private static final SecureRandom RANDOM = new SecureRandom();
    private static final char[] HEX = "0123456789abcdef".toCharArray();
    private static final char[] UPPER_HEX = "0123456789ABCDEF".toCharArray();

    private Text() {}

    // ------------------------------------------------------------ bytes

    /** UTF-8 bytes as text, invalid sequences replaced with U+FFFD. */
    static String decodeUtf8(byte[] data) {
        CharsetDecoder d =
                StandardCharsets.UTF_8
                        .newDecoder()
                        .onMalformedInput(CodingErrorAction.REPLACE)
                        .onUnmappableCharacter(CodingErrorAction.REPLACE);
        try {
            return d.decode(ByteBuffer.wrap(data)).toString();
        } catch (CharacterCodingException e) {
            return new String(data, StandardCharsets.UTF_8);
        }
    }

    /** UTF-8 text without a leading byte order mark. */
    static String decodeBody(byte[] data) {
        int start = 0;
        if (data.length >= 3
                && (data[0] & 0xff) == 0xEF
                && (data[1] & 0xff) == 0xBB
                && (data[2] & 0xff) == 0xBF) {
            start = 3;
        }
        byte[] rest = new byte[data.length - start];
        System.arraycopy(data, start, rest, 0, rest.length);
        return decodeUtf8(rest);
    }

    static byte[] utf8(String text) {
        return text.getBytes(StandardCharsets.UTF_8);
    }

    static String base64(byte[] data) {
        return Base64.getEncoder().encodeToString(data);
    }

    static String base64Url(byte[] data) {
        return Base64.getUrlEncoder().withoutPadding().encodeToString(data);
    }

    static String hex(byte[] data) {
        char[] out = new char[data.length * 2];
        for (int i = 0; i < data.length; i++) {
            out[2 * i] = HEX[(data[i] >> 4) & 15];
            out[2 * i + 1] = HEX[data[i] & 15];
        }
        return new String(out);
    }

    static byte[] sha256(byte[] data) {
        try {
            return MessageDigest.getInstance("SHA-256").digest(data);
        } catch (GeneralSecurityException e) {
            throw new IllegalStateException("SHA-256 is not available", e);
        }
    }

    static String sha256Hex(byte[] data) {
        return hex(sha256(data));
    }

    static byte[] hmacSha256(byte[] key, byte[] message) {
        try {
            Mac mac = Mac.getInstance("HmacSHA256");
            // An empty HMAC key is padded with zeros like any short key (RFC 2104), so one zero
            // byte is the same key; SecretKeySpec refuses an empty one.
            mac.init(new SecretKeySpec(key.length == 0 ? new byte[1] : key, "HmacSHA256"));
            return mac.doFinal(message);
        } catch (GeneralSecurityException e) {
            throw new IllegalStateException("HMAC-SHA256 is not available", e);
        }
    }

    static boolean timingSafeEqual(String a, String b) {
        return MessageDigest.isEqual(utf8(a), utf8(b)) && a.length() == b.length();
    }

    static byte[] randomBytes(int n) {
        byte[] out = new byte[n];
        RANDOM.nextBytes(out);
        return out;
    }

    static double randomUnit() {
        return RANDOM.nextDouble();
    }

    /** A random UUID version 4 in canonical lowercase form. */
    static String uuidV4() {
        byte[] b = randomBytes(16);
        b[6] = (byte) ((b[6] & 0x0f) | 0x40);
        b[8] = (byte) ((b[8] & 0x3f) | 0x80);
        String h = hex(b);
        return h.substring(0, 8)
                + "-"
                + h.substring(8, 12)
                + "-"
                + h.substring(12, 16)
                + "-"
                + h.substring(16, 20)
                + "-"
                + h.substring(20);
    }

    // ---------------------------------------------------------- UTF-16

    /** The first max UTF-16 code units of text, never splitting a surrogate pair. */
    static String takeUnits(String text, int max) {
        if (text.length() <= max) {
            return text;
        }
        int end = max;
        if (end > 0 && Character.isHighSurrogate(text.charAt(end - 1))) {
            end--;
        }
        return text.substring(0, end);
    }

    static String cut(String text) {
        if (text.length() > MAX_RECEIVED_CHARS) {
            return takeUnits(text, MAX_RECEIVED_CHARS - 1) + "\u2026";
        }
        return text;
    }

    // -------------------------------------------------------- encoding

    private static boolean unreserved(int b, String extra) {
        return (b >= 'a' && b <= 'z')
                || (b >= 'A' && b <= 'Z')
                || (b >= '0' && b <= '9')
                || extra.indexOf(b) >= 0;
    }

    private static String encode(String text, String extra, boolean plus) {
        StringBuilder b = new StringBuilder();
        for (byte raw : utf8(text)) {
            int c = raw & 0xff;
            if (unreserved(c, extra)) {
                b.append((char) c);
            } else if (plus && c == ' ') {
                b.append('+');
            } else {
                b.append('%').append(UPPER_HEX[c >> 4]).append(UPPER_HEX[c & 15]);
            }
        }
        return b.toString();
    }

    /** encodeURIComponent. */
    static String encodeComponent(String text) {
        return encode(text, "-_.!~*'()", false);
    }

    /** One component of a form body (URLSearchParams). */
    static String encodeFormComponent(String text) {
        return encode(text, "*-._", true);
    }

    /** RFC 3986 percent-encoding. */
    static String percentEncode(String text) {
        return encode(text, "-._~", false);
    }

    /** Decodes percent escapes and '+' (a query component). */
    static String decodeQueryComponent(String text) {
        byte[] out = new byte[text.length() * 3];
        int n = 0;
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c == '+') {
                out[n++] = ' ';
            } else if (c == '%' && i + 2 < text.length()
                    && Character.digit(text.charAt(i + 1), 16) >= 0
                    && Character.digit(text.charAt(i + 2), 16) >= 0) {
                out[n++] =
                        (byte)
                                (Character.digit(text.charAt(i + 1), 16) * 16
                                        + Character.digit(text.charAt(i + 2), 16));
                i += 2;
            } else {
                for (byte x : utf8(String.valueOf(c))) {
                    out[n++] = x;
                }
            }
        }
        byte[] exact = new byte[n];
        System.arraycopy(out, 0, exact, 0, n);
        return decodeUtf8(exact);
    }

    /** An RFC 9110 token. */
    static boolean validHeaderName(String name) {
        if (name.isEmpty()) {
            return false;
        }
        for (int i = 0; i < name.length(); i++) {
            char c = name.charAt(i);
            if (c > 127 || !unreserved(c, "!#$%&'*+-.^_`|~")) {
                return false;
            }
        }
        return true;
    }

    /** Visible ASCII, tab, space and Latin-1 characters. */
    static boolean validHeaderValue(String value) {
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (!(c == '\t' || (c >= 0x20 && c <= 0x7e) || (c >= 0x80 && c <= 0xff))) {
                return false;
            }
        }
        return true;
    }

    /** {@code type/subtype} tokens, optionally followed by {@code ;} parameters, in visible ASCII, space and tab. */
    static boolean validMediaType(String value) {
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (!(c == '\t' || (c >= 0x20 && c <= 0x7e))) {
                return false;
            }
        }
        int semi = value.indexOf(';');
        String head = (semi < 0 ? value : value.substring(0, semi)).strip();
        int slash = head.indexOf('/');
        return slash > 0 && validHeaderName(head.substring(0, slash)) && validHeaderName(head.substring(slash + 1));
    }

    static boolean allDigits(String s) {
        return Json.isIndex(s);
    }

    // -------------------------------------------------------- redaction

    private static final String[] SENSITIVE_WORDS = {
        "secret", "password", "passwd", "passphrase", "token", "apikey", "api-key", "api_key",
        "privatekey", "private-key", "private_key", "credential", "authorization", "cookie",
        "session",
    };

    /** Whether a name very likely holds a secret. */
    static boolean looksSensitive(String name) {
        String lower = name.toLowerCase(Locale.ROOT);
        for (String w : SENSITIVE_WORDS) {
            if (lower.contains(w)) {
                return true;
            }
        }
        return false;
    }

    static Object redactSensitiveKeys(Object v, int depth) {
        if (depth > 32) {
            return null;
        }
        if (v instanceof List<?>) {
            List<Object> out = new ArrayList<>();
            for (Object item : (List<?>) v) {
                out.add(redactSensitiveKeys(item, depth + 1));
            }
            return out;
        }
        if (v instanceof JsonObject) {
            JsonObject o = (JsonObject) v;
            JsonObject out = new JsonObject();
            for (String k : o.keys()) {
                out.set(k, looksSensitive(k) ? REDACTED : redactSensitiveKeys(o.get(k), depth + 1));
            }
            return out;
        }
        return v;
    }

    private static Object redactAt(Object v, List<String> segments, int from) {
        if (from == segments.size()) {
            return REDACTED;
        }
        if (v instanceof List<?>) {
            List<Object> out = new ArrayList<>();
            for (Object item : (List<?>) v) {
                out.add(redactAt(item, segments, from));
            }
            return out;
        }
        if (v instanceof JsonObject) {
            JsonObject o = (JsonObject) v;
            String head = segments.get(from);
            if (!o.has(head)) {
                return o;
            }
            JsonObject out = o.copy();
            out.set(head, redactAt(o.get(head), segments, from + 1));
            return out;
        }
        return v;
    }

    /** A copy of v with every listed field path redacted; arrays on the way fan out. */
    static Object redactPaths(Object v, List<String> paths) {
        Object out = Json.deepCopy(v);
        for (String p : paths) {
            List<String> segs = Json.splitPath(p);
            if (!segs.isEmpty()) {
                out = redactAt(out, segs, 0);
            }
        }
        return out;
    }

    /** A value made safe for received_value. */
    static Object envelopeValue(Object v, boolean sensitive) {
        if (sensitive) {
            return REDACTED;
        }
        if (v instanceof String) {
            return cut((String) v);
        }
        if (v instanceof List<?> || v instanceof JsonObject) {
            Binary bin = Binary.of(v);
            if (bin != null) {
                return "<" + bin.length() + " bytes>";
            }
            String text = Json.write(redactSensitiveKeys(v, 0));
            if (text.length() > MAX_RECEIVED_CHARS) {
                return cut(text);
            }
            try {
                return Json.parse(text);
            } catch (Json.JsonException e) {
                return null;
            }
        }
        return v;
    }

    /** Every occurrence of the secrets (four characters or more) replaced. */
    static String scrub(String text, List<String> secrets) {
        if (text == null) {
            return null;
        }
        for (String secret : secrets) {
            if (secret.codePointCount(0, secret.length()) >= 4 && text.contains(secret)) {
                text = text.replace(secret, REDACTED);
            }
        }
        return text;
    }

    static Object scrubValue(Object v, List<String> secrets) {
        if (v instanceof String) {
            return scrub((String) v, secrets);
        }
        if (v instanceof List<?> || v instanceof JsonObject) {
            try {
                return Json.parse(scrub(Json.write(v), secrets));
            } catch (Json.JsonException e) {
                return REDACTED;
            }
        }
        return v;
    }

    /**
     * The Accept of a stream call: the caller's media ranges in order without repeats,
     * text/event-stream first when they do not name it.
     */
    static String mergeAccept(String value) {
        List<String> ranges = new ArrayList<>();
        for (String part : value.split(",", -1)) {
            String r = part.trim();
            if (!r.isEmpty() && !ranges.contains(r)) {
                ranges.add(r);
            }
        }
        boolean names = false;
        for (String r : ranges) {
            String essence = r.split(";", 2)[0].trim();
            if (essence.equalsIgnoreCase("text/event-stream")) {
                names = true;
            }
        }
        if (!names) {
            ranges.add(0, "text/event-stream");
        }
        return String.join(", ", ranges);
    }

    // ------------------------------------------------------ retry-after

    private static long daysFromCivil(long year, long month, long day) {
        long y = month <= 2 ? year - 1 : year;
        long era = (y >= 0 ? y : y - 399) / 400;
        long yoe = y - era * 400;
        long mp = (month + 9) % 12;
        long doy = (153 * mp + 2) / 5 + day - 1;
        long doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        return era * 146097 + doe - 719468;
    }

    private static final String[] MONTHS = {
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"
    };
    private static final String[] MONTHS_FULL = {
        "january", "february", "march", "april", "may", "june", "july", "august", "september",
        "october", "november", "december"
    };

    private static long monthNumber(String token) {
        String lower = token.toLowerCase(Locale.ROOT);
        if (lower.length() < 3) {
            return -1;
        }
        for (int i = 0; i < 12; i++) {
            if (lower.startsWith(MONTHS[i])
                    && (lower.length() == 3 || lower.equals(MONTHS_FULL[i]) || lower.equals("sept"))) {
                return i + 1;
            }
        }
        return -1;
    }

    private static long digits(String s) {
        if (!allDigits(s) || s.length() > 18) {
            return -1;
        }
        return Long.parseLong(s);
    }

    private static long clock(String token) {
        String[] parts = token.split(":", -1);
        if (parts.length < 2 || parts.length > 3) {
            return -1;
        }
        long hour = digits(parts[0]);
        long minute = digits(parts[1]);
        if (hour < 0 || hour >= 24 || minute < 0 || minute >= 60) {
            return -1;
        }
        double second = 0;
        if (parts.length == 3) {
            try {
                second = Double.parseDouble(parts[2]);
            } catch (NumberFormatException e) {
                return -1;
            }
            if (!(second >= 0 && second < 61) || parts[2].isEmpty()) {
                return -1;
            }
        }
        return (hour * 3600 + minute * 60) * 1000 + (long) (second * 1000 + 0.5);
    }

    private static final long NO_ZONE = Long.MIN_VALUE;

    private static long zone(String token) {
        switch (token.toUpperCase(Locale.ROOT)) {
            case "GMT":
            case "UTC":
            case "UT":
            case "Z":
                return 0;
            default:
                break;
        }
        if (token.isEmpty()) {
            return NO_ZONE;
        }
        long sign;
        if (token.charAt(0) == '+') {
            sign = 1;
        } else if (token.charAt(0) == '-') {
            sign = -1;
        } else {
            return NO_ZONE;
        }
        String d = token.substring(1).replace(":", "");
        if (d.length() != 4) {
            return NO_ZONE;
        }
        long hours = digits(d.substring(0, 2));
        long minutes = digits(d.substring(2));
        if (hours < 0 || minutes < 0) {
            return NO_ZONE;
        }
        return sign * (hours * 60 + minutes) * 60000;
    }

    private static Long isoDate(String text) {
        String date = text;
        String rest = null;
        for (int i = 0; i < text.length(); i++) {
            if (text.charAt(i) == 'T' || text.charAt(i) == ' ') {
                date = text.substring(0, i);
                rest = text.substring(i + 1);
                break;
            }
        }
        String[] fields = date.split("-", -1);
        if (fields.length != 3) {
            return null;
        }
        long year = digits(fields[0]);
        long month = digits(fields[1]);
        long day = digits(fields[2]);
        if (year < 0 || year > 9999 || month < 1 || month > 12 || day < 1 || day > 31) {
            return null;
        }
        long time = 0;
        long offset = 0;
        if (rest != null) {
            int split = rest.length();
            for (int i = 0; i < rest.length(); i++) {
                char c = rest.charAt(i);
                if (c == 'Z' || c == 'z' || c == '+' || c == '-') {
                    split = i;
                    break;
                }
            }
            time = clock(rest.substring(0, split));
            if (time < 0) {
                return null;
            }
            if (split < rest.length()) {
                offset = zone(rest.substring(split));
                if (offset == NO_ZONE) {
                    return null;
                }
            }
        }
        return daysFromCivil(year, month, day) * 86400000L + time - offset;
    }

    /** An HTTP date (IMF-fixdate, RFC 850, asctime) or ISO 8601 timestamp in epoch ms. */
    static Long parseHttpDate(String raw) {
        String text = raw.trim();
        if (text.length() >= 8
                && text.charAt(0) >= '0'
                && text.charAt(0) <= '9'
                && text.charAt(4) == '-') {
            return isoDate(text);
        }
        long day = 0;
        long month = 0;
        long year = 0;
        long time = 0;
        long offset = 0;
        boolean hasDay = false;
        boolean hasMonth = false;
        boolean hasYear = false;
        for (String token : text.split("[ ,]+")) {
            if (token.isEmpty()) {
                continue;
            }
            if (token.contains(":")) {
                time = clock(token);
                if (time < 0) {
                    return null;
                }
            } else if (token.contains("-") && token.charAt(0) >= '0' && token.charAt(0) <= '9') {
                String[] parts = token.split("-", -1);
                if (parts.length < 3) {
                    return null;
                }
                long d = digits(parts[0]);
                long m = monthNumber(parts[1]);
                long y = digits(parts[2]);
                if (d < 0 || m < 0 || y < 0) {
                    return null;
                }
                if (y < 50) {
                    y += 2000;
                } else if (y < 100) {
                    y += 1900;
                }
                day = d;
                month = m;
                year = y;
                hasDay = true;
                hasMonth = true;
                hasYear = true;
            } else if (allDigits(token)) {
                long n = digits(token);
                if (!hasDay && token.length() <= 2) {
                    day = n;
                    hasDay = true;
                } else {
                    year = n;
                    hasYear = true;
                }
            } else {
                long m = monthNumber(token);
                if (m > 0) {
                    month = m;
                    hasMonth = true;
                } else {
                    long z = zone(token);
                    if (z != NO_ZONE) {
                        offset = z;
                    } else {
                        for (int i = 0; i < token.length(); i++) {
                            char c = token.charAt(i);
                            if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '.')) {
                                return null;
                            }
                        }
                    }
                }
            }
        }
        if (!hasDay || !hasMonth || !hasYear || day < 1 || day > 31 || year < 0 || year > 9999) {
            return null;
        }
        return daysFromCivil(year, month, day) * 86400000L + time - offset;
    }

    /** Retry-After in milliseconds (delta seconds or an HTTP date); null when absent or bad. */
    static Long parseRetryAfter(String value, long nowMs) {
        if (value == null) {
            return null;
        }
        String trimmed = value.trim();
        if (allDigits(trimmed)) {
            if (trimmed.length() > 15) {
                return 1L << 62;
            }
            long n = Long.parseLong(trimmed);
            return Math.min(n * 1000, 1L << 62);
        }
        Long date = parseHttpDate(trimmed);
        if (date == null) {
            return null;
        }
        return Math.max(0, date - nowMs);
    }

    /** The next page URL of a Link header (rel="next"), or null. */
    static String nextLink(String header) {
        if (header == null || header.isEmpty()) {
            return null;
        }
        List<String> parts = new ArrayList<>();
        int start = 0;
        for (int i = 0; i < header.length(); i++) {
            if (header.charAt(i) == ',' && header.substring(i + 1).stripLeading().startsWith("<")) {
                parts.add(header.substring(start, i));
                start = i + 1;
            }
        }
        parts.add(header.substring(start));
        java.util.regex.Pattern target =
                java.util.regex.Pattern.compile("^\\s*<([^>]*)>(.*)$", java.util.regex.Pattern.DOTALL);
        java.util.regex.Pattern relNext =
                java.util.regex.Pattern.compile(
                        "(?i);\\s*rel\\s*=\\s*\"?([^\";]*\\s)?next(\\s[^\";]*)?\"?\\s*(;|$)");
        for (String part : parts) {
            java.util.regex.Matcher m = target.matcher(part);
            if (!m.find()) {
                continue;
            }
            if (relNext.matcher(m.group(2)).find()) {
                return m.group(1);
            }
        }
        return null;
    }
}
