// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.math.BigDecimal;
import java.math.BigInteger;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collection;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

/**
 * JSON values of this runtime and their text forms.
 *
 * <p>A JSON value is {@code null}, a {@link Boolean}, a {@link String}, a {@link Long} or a
 * {@link Double} (numbers), a {@link List} (arrays) or a {@link JsonObject} (objects, which keep
 * the order of their members). A number without a fractional part and below 2^53 in magnitude is
 * always a {@code Long}, as the other runtimes read it; other numbers are {@code Double}s.
 * {@link #parse} produces such values and {@link #normalize} converts other Java values (maps,
 * integers, generated models) to them.
 */
public final class Json {
    /** 2^53, the bound of the integers a JavaScript number holds exactly. */
    static final double MAX_SAFE = 9007199254740992.0;

    /** 2^53 - 1. */
    static final double MAX_SAFE_INTEGER = 9007199254740991.0;

    private static final int MAX_PARSE_DEPTH = 2000;

    private Json() {}

    /** A value that converts itself to a JSON value (generated models and requests). */
    public interface Encodable {
        /** The value as JSON (a value of this runtime, or one {@link #normalize} accepts). */
        Object toJson();
    }

    // ------------------------------------------------------------- parse

    /**
     * Parses one JSON document.
     *
     * @throws JsonException when the text is not JSON
     */
    public static Object parse(String text) {
        Parser p = new Parser(text);
        p.skipSpace();
        Object v = p.value(0);
        p.skipSpace();
        if (p.at < text.length()) {
            throw new JsonException("trailing data after the JSON value at offset " + p.at);
        }
        return v;
    }

    /** Parses UTF-8 JSON bytes. */
    public static Object parse(byte[] utf8) {
        return parse(Text.decodeUtf8(utf8));
    }

    /** Thrown for text that is not JSON. */
    public static final class JsonException extends RuntimeException {
        private static final long serialVersionUID = 1L;

        JsonException(String message) {
            super(message);
        }
    }

    private static final class Parser {
        private final String s;
        private int at;

        Parser(String s) {
            this.s = s;
        }

        void skipSpace() {
            while (at < s.length()) {
                char c = s.charAt(at);
                if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
                    at++;
                } else {
                    break;
                }
            }
        }

        JsonException fail(String what) {
            return new JsonException(what + " at offset " + at);
        }

        Object value(int depth) {
            if (depth > MAX_PARSE_DEPTH) {
                throw fail("JSON nested too deeply");
            }
            if (at >= s.length()) {
                throw fail("unexpected end of JSON");
            }
            char c = s.charAt(at);
            switch (c) {
                case '{':
                    return object(depth);
                case '[':
                    return array(depth);
                case '"':
                    return string();
                case 't':
                    return literal("true", Boolean.TRUE);
                case 'f':
                    return literal("false", Boolean.FALSE);
                case 'n':
                    return literal("null", null);
                default:
                    if (c == '-' || (c >= '0' && c <= '9')) {
                        return number();
                    }
                    throw fail("unexpected character '" + c + "'");
            }
        }

        Object literal(String word, Object value) {
            if (!s.startsWith(word, at)) {
                throw fail("invalid literal");
            }
            at += word.length();
            return value;
        }

        JsonObject object(int depth) {
            at++;
            JsonObject out = new JsonObject();
            skipSpace();
            if (at < s.length() && s.charAt(at) == '}') {
                at++;
                return out;
            }
            while (true) {
                skipSpace();
                if (at >= s.length() || s.charAt(at) != '"') {
                    throw fail("expected a member name");
                }
                String key = string();
                skipSpace();
                if (at >= s.length() || s.charAt(at) != ':') {
                    throw fail("expected ':'");
                }
                at++;
                skipSpace();
                out.set(key, value(depth + 1));
                skipSpace();
                if (at >= s.length()) {
                    throw fail("unterminated object");
                }
                char c = s.charAt(at++);
                if (c == '}') {
                    return out;
                }
                if (c != ',') {
                    throw fail("expected ',' or '}'");
                }
            }
        }

        List<Object> array(int depth) {
            at++;
            List<Object> out = new ArrayList<>();
            skipSpace();
            if (at < s.length() && s.charAt(at) == ']') {
                at++;
                return out;
            }
            while (true) {
                skipSpace();
                out.add(value(depth + 1));
                skipSpace();
                if (at >= s.length()) {
                    throw fail("unterminated array");
                }
                char c = s.charAt(at++);
                if (c == ']') {
                    return out;
                }
                if (c != ',') {
                    throw fail("expected ',' or ']'");
                }
            }
        }

        String string() {
            at++;
            StringBuilder b = new StringBuilder();
            while (true) {
                if (at >= s.length()) {
                    throw fail("unterminated string");
                }
                char c = s.charAt(at++);
                if (c == '"') {
                    return b.toString();
                }
                if (c < 0x20) {
                    throw fail("control character in a string");
                }
                if (c != '\\') {
                    b.append(c);
                    continue;
                }
                if (at >= s.length()) {
                    throw fail("unterminated escape");
                }
                char e = s.charAt(at++);
                switch (e) {
                    case '"':
                        b.append('"');
                        break;
                    case '\\':
                        b.append('\\');
                        break;
                    case '/':
                        b.append('/');
                        break;
                    case 'b':
                        b.append('\b');
                        break;
                    case 'f':
                        b.append('\f');
                        break;
                    case 'n':
                        b.append('\n');
                        break;
                    case 'r':
                        b.append('\r');
                        break;
                    case 't':
                        b.append('\t');
                        break;
                    case 'u':
                        if (at + 4 > s.length()) {
                            throw fail("invalid \\u escape");
                        }
                        int code = 0;
                        for (int i = 0; i < 4; i++) {
                            int d = Character.digit(s.charAt(at + i), 16);
                            if (d < 0) {
                                throw fail("invalid \\u escape");
                            }
                            code = code * 16 + d;
                        }
                        at += 4;
                        b.append((char) code);
                        break;
                    default:
                        throw fail("invalid escape");
                }
            }
        }

        Object number() {
            int start = at;
            if (s.charAt(at) == '-') {
                at++;
            }
            if (at >= s.length()) {
                throw fail("invalid number");
            }
            if (s.charAt(at) == '0') {
                at++;
            } else if (s.charAt(at) >= '1' && s.charAt(at) <= '9') {
                while (at < s.length() && Character.isDigit(s.charAt(at)) && s.charAt(at) < 128) {
                    at++;
                }
            } else {
                throw fail("invalid number");
            }
            if (at < s.length() && s.charAt(at) == '.') {
                at++;
                int digits = at;
                while (at < s.length() && s.charAt(at) >= '0' && s.charAt(at) <= '9') {
                    at++;
                }
                if (at == digits) {
                    throw fail("invalid number");
                }
            }
            if (at < s.length() && (s.charAt(at) == 'e' || s.charAt(at) == 'E')) {
                at++;
                if (at < s.length() && (s.charAt(at) == '+' || s.charAt(at) == '-')) {
                    at++;
                }
                int digits = at;
                while (at < s.length() && s.charAt(at) >= '0' && s.charAt(at) <= '9') {
                    at++;
                }
                if (at == digits) {
                    throw fail("invalid number");
                }
            }
            Object n = parseNumber(s.substring(start, at));
            if (n == null) {
                throw fail("number out of range");
            }
            return n;
        }
    }

    /** A number's text as a JSON value of this runtime; null when it is not finite. */
    static Object parseNumber(String text) {
        boolean integral = text.indexOf('.') < 0 && text.indexOf('e') < 0 && text.indexOf('E') < 0;
        if (integral) {
            try {
                return Long.parseLong(text);
            } catch (NumberFormatException ignored) {
                // falls through to a double
            }
        }
        double d;
        try {
            d = Double.parseDouble(text);
        } catch (NumberFormatException e) {
            return null;
        }
        if (Double.isInfinite(d) || Double.isNaN(d)) {
            return null;
        }
        return normalizeDouble(d);
    }

    /** A finite double as a JSON number: an integral value below 2^53 is a Long. */
    static Object normalizeDouble(double d) {
        if (d == Math.rint(d) && Math.abs(d) < MAX_SAFE) {
            return (long) d;
        }
        return d;
    }

    // ------------------------------------------------------- conversion

    /**
     * Converts a Java value to a JSON value of this runtime: maps (members sorted by name),
     * collections and arrays, integers and floats, {@link Encodable} values, {@link Binary}.
     * JSON values pass through (objects and arrays are copied).
     *
     * @throws IllegalArgumentException for a value that is not JSON (a non-finite number, an
     *     unknown type)
     */
    public static Object normalize(Object v) {
        return normalize(v, 0);
    }

    private static Object normalize(Object v, int depth) {
        if (depth > MAX_PARSE_DEPTH) {
            throw new IllegalArgumentException("the value is nested too deeply");
        }
        if (v == null || v instanceof Boolean || v instanceof String || v instanceof Long) {
            return v;
        }
        if (v instanceof Integer || v instanceof Short || v instanceof Byte) {
            return ((Number) v).longValue();
        }
        if (v instanceof Double || v instanceof Float) {
            double d = ((Number) v).doubleValue();
            if (Double.isNaN(d) || Double.isInfinite(d)) {
                throw new IllegalArgumentException("a non-finite number is not JSON");
            }
            return normalizeDouble(d);
        }
        if (v instanceof BigInteger) {
            BigInteger b = (BigInteger) v;
            if (b.bitLength() < 64) {
                return b.longValue();
            }
            return normalize(b.doubleValue(), depth);
        }
        if (v instanceof BigDecimal) {
            BigDecimal b = (BigDecimal) v;
            try {
                return b.longValueExact();
            } catch (ArithmeticException notIntegral) {
                return normalize(b.doubleValue(), depth);
            }
        }
        if (v instanceof Character) {
            return v.toString();
        }
        if (v instanceof Binary) {
            return ((Binary) v).toJson();
        }
        if (v instanceof Encodable) {
            return normalize(((Encodable) v).toJson(), depth + 1);
        }
        if (v instanceof Enum<?>) {
            return ((Enum<?>) v).name();
        }
        if (v instanceof JsonObject) {
            JsonObject o = (JsonObject) v;
            JsonObject out = new JsonObject();
            for (Map.Entry<String, Object> e : o.asMap().entrySet()) {
                out.set(e.getKey(), normalize(e.getValue(), depth + 1));
            }
            return out;
        }
        if (v instanceof Map<?, ?>) {
            TreeMap<String, Object> sorted = new TreeMap<>();
            for (Map.Entry<?, ?> e : ((Map<?, ?>) v).entrySet()) {
                sorted.put(String.valueOf(e.getKey()), e.getValue());
            }
            JsonObject out = new JsonObject();
            for (Map.Entry<String, Object> e : sorted.entrySet()) {
                out.set(e.getKey(), normalize(e.getValue(), depth + 1));
            }
            return out;
        }
        if (v instanceof Collection<?>) {
            List<Object> out = new ArrayList<>();
            for (Object item : (Collection<?>) v) {
                out.add(normalize(item, depth + 1));
            }
            return out;
        }
        if (v instanceof Object[]) {
            return normalize(Arrays.asList((Object[]) v), depth);
        }
        if (v instanceof byte[]) {
            return Text.base64((byte[]) v);
        }
        throw new IllegalArgumentException(
                "a " + v.getClass().getSimpleName() + " is not a JSON value");
    }

    /** A deep copy of a JSON value. */
    public static Object deepCopy(Object v) {
        if (v instanceof List<?>) {
            List<Object> out = new ArrayList<>();
            for (Object item : (List<?>) v) {
                out.add(deepCopy(item));
            }
            return out;
        }
        if (v instanceof JsonObject) {
            JsonObject out = new JsonObject();
            for (Map.Entry<String, Object> e : ((JsonObject) v).asMap().entrySet()) {
                out.set(e.getKey(), deepCopy(e.getValue()));
            }
            return out;
        }
        return v;
    }

    // ----------------------------------------------------------- equality

    static boolean isNumber(Object v) {
        return v instanceof Long || v instanceof Double;
    }

    static double asDouble(Object v) {
        return ((Number) v).doubleValue();
    }

    /** Structural equality: 1 equals 1.0, member order does not matter. */
    public static boolean deepEqual(Object a, Object b) {
        if (a == null || b == null) {
            return a == null && b == null;
        }
        if (isNumber(a) && isNumber(b)) {
            if (a instanceof Long && b instanceof Long) {
                return ((Long) a).longValue() == ((Long) b).longValue();
            }
            return asDouble(a) == asDouble(b);
        }
        if (a instanceof List<?> && b instanceof List<?>) {
            List<?> x = (List<?>) a;
            List<?> y = (List<?>) b;
            if (x.size() != y.size()) {
                return false;
            }
            for (int i = 0; i < x.size(); i++) {
                if (!deepEqual(x.get(i), y.get(i))) {
                    return false;
                }
            }
            return true;
        }
        if (a instanceof JsonObject && b instanceof JsonObject) {
            JsonObject x = (JsonObject) a;
            JsonObject y = (JsonObject) b;
            if (x.size() != y.size()) {
                return false;
            }
            for (Map.Entry<String, Object> e : x.asMap().entrySet()) {
                if (!y.has(e.getKey()) || !deepEqual(e.getValue(), y.get(e.getKey()))) {
                    return false;
                }
            }
            return true;
        }
        if (a instanceof Boolean || a instanceof String) {
            return a.equals(b);
        }
        return false;
    }

    // ---------------------------------------------------------- text forms

    /** JSON.stringify(value): members in insertion order, no whitespace. */
    public static String write(Object v) {
        StringBuilder b = new StringBuilder();
        write(b, v, false, 0);
        return b.toString();
    }

    /**
     * The canonical JSON every tungsten runtime digests (confirmation tokens, automatic
     * idempotency keys, content hashes): object members sorted by the UTF-16 code units of their
     * names, no whitespace, numbers as JavaScript writes them, binary values as {"$bytes":
     * base64} (or {"$file": {"bytes", "name", "type"}} with a name or type).
     */
    public static String canonical(Object v) {
        StringBuilder b = new StringBuilder();
        write(b, v, true, 0);
        return b.toString();
    }

    /** UTF-8 bytes of {@link #write}. */
    static byte[] bytes(Object v) {
        return write(v).getBytes(StandardCharsets.UTF_8);
    }

    private static void write(StringBuilder b, Object v, boolean canonical, int depth) {
        if (depth > MAX_PARSE_DEPTH) {
            b.append("null");
            return;
        }
        if (v == null) {
            b.append("null");
        } else if (v instanceof Boolean) {
            b.append(((Boolean) v) ? "true" : "false");
        } else if (v instanceof Long) {
            b.append(((Long) v).longValue());
        } else if (v instanceof Double) {
            b.append(numberText(v));
        } else if (v instanceof String) {
            writeString(b, (String) v);
        } else if (v instanceof List<?>) {
            b.append('[');
            boolean first = true;
            for (Object item : (List<?>) v) {
                if (!first) {
                    b.append(',');
                }
                first = false;
                write(b, item, canonical, depth + 1);
            }
            b.append(']');
        } else if (v instanceof JsonObject) {
            JsonObject o = (JsonObject) v;
            if (canonical) {
                Binary bin = Binary.of(o);
                if (bin != null) {
                    writeBinary(b, bin);
                    return;
                }
            }
            List<String> keys = o.keys();
            if (canonical) {
                keys.sort(String::compareTo);
            }
            b.append('{');
            boolean first = true;
            for (String k : keys) {
                if (!first) {
                    b.append(',');
                }
                first = false;
                writeString(b, k);
                b.append(':');
                write(b, o.get(k), canonical, depth + 1);
            }
            b.append('}');
        } else {
            Object converted;
            try {
                converted = normalize(v);
            } catch (IllegalArgumentException e) {
                b.append("null");
                return;
            }
            write(b, converted, canonical, depth + 1);
        }
    }

    private static void writeBinary(StringBuilder b, Binary bin) {
        String encoded = Text.base64(bin.data());
        if (bin.filename() == null && bin.contentType() == null) {
            b.append("{\"$bytes\":");
            writeString(b, encoded);
            b.append('}');
            return;
        }
        b.append("{\"$file\":{\"bytes\":");
        writeString(b, encoded);
        b.append(",\"name\":");
        if (bin.filename() != null) {
            writeString(b, bin.filename());
        } else {
            b.append("null");
        }
        b.append(",\"type\":");
        if (bin.contentType() != null) {
            writeString(b, bin.contentType());
        } else {
            b.append("null");
        }
        b.append("}}");
    }

    private static final char[] HEX = "0123456789abcdef".toCharArray();

    static void writeString(StringBuilder b, String text) {
        b.append('"');
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            switch (c) {
                case '"':
                    b.append("\\\"");
                    break;
                case '\\':
                    b.append("\\\\");
                    break;
                case '\b':
                    b.append("\\b");
                    break;
                case '\f':
                    b.append("\\f");
                    break;
                case '\n':
                    b.append("\\n");
                    break;
                case '\r':
                    b.append("\\r");
                    break;
                case '\t':
                    b.append("\\t");
                    break;
                default:
                    if (c < 0x20) {
                        b.append("\\u00").append(HEX[c >> 4]).append(HEX[c & 15]);
                    } else {
                        b.append(c);
                    }
            }
        }
        b.append('"');
    }

    /** A string literal as JSON.stringify writes it. */
    public static String jsString(String text) {
        StringBuilder b = new StringBuilder();
        writeString(b, text);
        return b.toString();
    }

    /** A JSON number as JavaScript writes it; "null" for anything else. */
    static String numberText(Object v) {
        if (v instanceof Long) {
            return Long.toString((Long) v);
        }
        if (v instanceof Double) {
            double d = (Double) v;
            if (Double.isNaN(d) || Double.isInfinite(d)) {
                return "null";
            }
            return jsNumber(d);
        }
        return "null";
    }

    /**
     * String(value) for a JavaScript number (ECMAScript Number::toString): 1.0 is "1", 1e-7 is
     * "1e-7", 1e21 is "1e+21".
     */
    public static String jsNumber(double value) {
        if (Double.isNaN(value)) {
            return "NaN";
        }
        if (Double.isInfinite(value)) {
            return value > 0 ? "Infinity" : "-Infinity";
        }
        if (value == 0) {
            return "0";
        }
        String sign = value < 0 ? "-" : "";
        BigDecimal exact = shortest(Math.abs(value));
        String digits = exact.unscaledValue().toString();
        int k = digits.length();
        int n = k - exact.scale();
        String body;
        if (k <= n && n <= 21) {
            body = digits + "0".repeat(n - k);
        } else if (0 < n && n <= 21) {
            body = digits.substring(0, n) + "." + digits.substring(n);
        } else if (-6 < n && n <= 0) {
            body = "0." + "0".repeat(-n) + digits;
        } else {
            int e = n - 1;
            String mark = e < 0 ? "-" : "+";
            String head = k > 1 ? digits.charAt(0) + "." + digits.substring(1) : digits;
            body = head + "e" + mark + Math.abs(e);
        }
        return sign + body;
    }

    /** The shortest decimal that reads back as the double (ECMAScript's choice of digits). */
    private static BigDecimal shortest(double d) {
        BigDecimal exact = new BigDecimal(d);
        for (int p = 1; p < 17; p++) {
            BigDecimal r = exact.round(new java.math.MathContext(p, java.math.RoundingMode.HALF_EVEN));
            if (r.doubleValue() == d) {
                return r.stripTrailingZeros();
            }
        }
        return exact.round(new java.math.MathContext(17, java.math.RoundingMode.HALF_EVEN)).stripTrailingZeros();
    }

    // ------------------------------------------------------------- paths

    /** Splits a field path (a.b, items[0].id, items.0.id). */
    static List<String> splitPath(String path) {
        List<String> out = new ArrayList<>();
        if (path == null || path.isEmpty() || path.equals(".")) {
            return out;
        }
        StringBuilder dotted = new StringBuilder();
        int i = 0;
        while (i < path.length()) {
            char c = path.charAt(i);
            if (c == '[') {
                int j = i + 1;
                while (j < path.length() && path.charAt(j) >= '0' && path.charAt(j) <= '9') {
                    j++;
                }
                if (j > i + 1 && j < path.length() && path.charAt(j) == ']') {
                    dotted.append('.').append(path, i + 1, j);
                    i = j + 1;
                    continue;
                }
            }
            dotted.append(c);
            i++;
        }
        for (String s : dotted.toString().split("\\.", -1)) {
            if (!s.isEmpty()) {
                out.add(s);
            }
        }
        return out;
    }

    static boolean isIndex(String s) {
        if (s.isEmpty()) {
            return false;
        }
        for (int i = 0; i < s.length(); i++) {
            if (s.charAt(i) < '0' || s.charAt(i) > '9') {
                return false;
            }
        }
        return true;
    }

    /** The marker of an absent value in path lookups. */
    static final Object ABSENT = new Object();

    /** The value at a field path, or {@link #ABSENT}. */
    static Object at(Object v, List<String> segments) {
        if (v == ABSENT) {
            return ABSENT;
        }
        Object cur = v;
        for (String seg : segments) {
            if (cur instanceof List<?>) {
                List<?> list = (List<?>) cur;
                if (!isIndex(seg) || seg.length() > 9) {
                    return ABSENT;
                }
                int n = Integer.parseInt(seg);
                if (n >= list.size()) {
                    return ABSENT;
                }
                cur = list.get(n);
            } else if (cur instanceof JsonObject) {
                JsonObject o = (JsonObject) cur;
                if (!o.has(seg)) {
                    return ABSENT;
                }
                cur = o.get(seg);
            } else {
                return ABSENT;
            }
        }
        return cur;
    }

    /** The value at a dotted path, or {@link #ABSENT}. */
    static Object at(Object v, String path) {
        return at(v, splitPath(path));
    }

    /** Whether a value nests arrays and objects deeper than limit. */
    static boolean nestsDeeperThan(Object v, int limit) {
        if (v instanceof List<?>) {
            if (limit == 0) {
                return true;
            }
            for (Object item : (List<?>) v) {
                if (nestsDeeperThan(item, limit - 1)) {
                    return true;
                }
            }
        } else if (v instanceof JsonObject) {
            if (limit == 0) {
                return true;
            }
            for (Object item : ((JsonObject) v).asMap().values()) {
                if (nestsDeeperThan(item, limit - 1)) {
                    return true;
                }
            }
        }
        return false;
    }
}
