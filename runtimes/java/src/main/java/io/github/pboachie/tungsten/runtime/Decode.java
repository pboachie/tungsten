// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.Base64;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.function.Function;

/**
 * The decoding and encoding helpers generated models are written with. A decoder turns a JSON
 * value of this runtime into a model and throws {@link Failure} with the path of the value that
 * did not fit.
 */
public final class Decode {
    private Decode() {}

    /** A JSON value that does not fit the SDK's type. */
    public static final class Failure extends RuntimeException {
        private static final long serialVersionUID = 1L;
        private final transient List<Object> path;

        /** A failure at the value itself. */
        public Failure(String message) {
            this(List.of(), message);
        }

        private Failure(List<Object> path, String message) {
            super(message, null, false, false);
            this.path = path;
        }

        /** The path of the value: member names and indexes. */
        public List<Object> path() {
            return path;
        }

        /** The same failure one level deeper, below seg. */
        public Failure below(Object seg) {
            List<Object> p = new ArrayList<>();
            p.add(seg);
            p.addAll(path);
            return new Failure(Collections.unmodifiableList(p), getMessage());
        }
    }

    private static Failure expected(String what, Object v) {
        return new Failure("expected " + what + ", found " + Schema.describe(v));
    }

    /** An object. */
    public static JsonObject object(Object v) {
        if (v instanceof JsonObject) {
            return (JsonObject) v;
        }
        throw expected("an object", v);
    }

    /** A string. */
    public static String string(Object v) {
        if (v instanceof String) {
            return (String) v;
        }
        throw expected("a string", v);
    }

    /** A boolean. */
    public static boolean bool(Object v) {
        if (v instanceof Boolean) {
            return (Boolean) v;
        }
        throw expected("a boolean", v);
    }

    /** A 64-bit integer. */
    public static long int64(Object v) {
        if (v instanceof Long) {
            return (Long) v;
        }
        throw expected("an integer", v);
    }

    /** A 32-bit integer. */
    public static int int32(Object v) {
        long n = int64(v);
        if (n < Integer.MIN_VALUE || n > Integer.MAX_VALUE) {
            throw new Failure("expected a 32-bit integer, found " + n);
        }
        return (int) n;
    }

    /** A number. */
    public static double number(Object v) {
        if (Json.isNumber(v)) {
            return Json.asDouble(v);
        }
        throw expected("a number", v);
    }

    /** Base64 bytes. */
    public static byte[] bytes(Object v) {
        String text = string(v);
        try {
            return Base64.getDecoder().decode(text);
        } catch (IllegalArgumentException e) {
            throw new Failure("expected base64 data");
        }
    }

    /** Any JSON value (a copy). */
    public static Object any(Object v) {
        return Json.deepCopy(v);
    }

    /** Exactly this JSON value. */
    public static Object constant(Object v, Object expected) {
        if (Json.deepEqual(v, expected)) {
            return Json.deepCopy(v);
        }
        throw new Failure("expected " + Json.write(expected) + ", found " + Schema.describe(v));
    }

    /** null, or the decoded value. */
    public static <T> T nullable(Object v, Function<Object, T> decode) {
        return v == null ? null : decode.apply(v);
    }

    /** An array of decoded items. */
    public static <T> List<T> list(Object v, Function<Object, T> item) {
        if (!(v instanceof List<?>)) {
            throw expected("an array", v);
        }
        List<?> in = (List<?>) v;
        List<T> out = new ArrayList<>(in.size());
        for (int i = 0; i < in.size(); i++) {
            try {
                out.add(item.apply(in.get(i)));
            } catch (Failure f) {
                throw f.below(i);
            }
        }
        return Collections.unmodifiableList(out);
    }

    /** An object of decoded member values. */
    public static <T> Map<String, T> map(Object v, Function<Object, T> item) {
        JsonObject o = object(v);
        Map<String, T> out = new LinkedHashMap<>();
        for (String k : o.keys()) {
            try {
                out.put(k, item.apply(o.get(k)));
            } catch (Failure f) {
                throw f.below(k);
            }
        }
        return Collections.unmodifiableMap(out);
    }

    /** A decoder applied to a value. */
    public static <T> T apply(Object v, Function<Object, T> decode) {
        return decode.apply(v);
    }

    /** An encoder applied to a value. */
    public static <T> Object encode(T value, Function<T, Object> encode) {
        return encode.apply(value);
    }

    /** Binary data: the runtime's tagged binary object, or base64 text. */
    public static Binary binary(Object v) {
        Binary b = Binary.of(v);
        if (b != null) {
            return b;
        }
        if (v instanceof String) {
            try {
                return Binary.of(Base64.getDecoder().decode((String) v));
            } catch (IllegalArgumentException e) {
                throw new Failure("expected base64 data");
            }
        }
        throw expected("binary data", v);
    }

    /**
     * A member of a model: null when absent or null, else decoded. Presence is checked by the
     * response schema, so a decoder never refuses a body the schema check only warned about.
     */
    public static <T> T member(JsonObject o, String name, Function<Object, T> decode) {
        if (!o.has(name) || o.get(name) == null) {
            return null;
        }
        try {
            return decode.apply(o.get(name));
        } catch (Failure f) {
            throw f.below(name);
        }
    }

    /** A read-only copy of additional members (none for null), keeping null values and order. */
    public static <T> Map<String, T> frozen(Map<String, T> members) {
        return members == null ? Map.of() : Collections.unmodifiableMap(new LinkedHashMap<>(members));
    }

    /** Writes the additional members that are not already set, in order. */
    public static <T> void putExtra(JsonObject o, Map<String, T> members, Function<T, Object> encode) {
        for (Map.Entry<String, T> e : members.entrySet()) {
            if (!o.has(e.getKey())) {
                o.set(e.getKey(), e.getValue() == null ? null : encode.apply(e.getValue()));
            }
        }
    }

    /** A required member. */
    public static <T> T required(JsonObject o, String name, Function<Object, T> decode) {
        if (!o.has(name)) {
            throw new Failure("missing required member " + name).below(name);
        }
        try {
            return decode.apply(o.get(name));
        } catch (Failure f) {
            throw f.below(name);
        }
    }

    /** An optional member: null when absent (or null). */
    public static <T> T optional(JsonObject o, String name, Function<Object, T> decode) {
        if (!o.has(name) || o.get(name) == null) {
            return null;
        }
        try {
            return decode.apply(o.get(name));
        } catch (Failure f) {
            throw f.below(name);
        }
    }

    /** An optional nullable member. */
    public static <T> Patch<T> patch(JsonObject o, String name, Function<Object, T> decode) {
        if (!o.has(name)) {
            return Patch.unset();
        }
        if (o.get(name) == null) {
            return Patch.ofNull();
        }
        try {
            return Patch.of(decode.apply(o.get(name)));
        } catch (Failure f) {
            throw f.below(name);
        }
    }

    /** Refuses members outside known (a closed record). */
    public static void closed(JsonObject o, Set<String> known) {
        for (String k : o.keys()) {
            if (!known.contains(k)) {
                throw new Failure("unknown member " + k).below(k);
            }
        }
    }

    /** The members outside known, decoded. */
    public static <T> Map<String, T> extra(JsonObject o, Set<String> known, Function<Object, T> decode) {
        Map<String, T> out = new LinkedHashMap<>();
        for (String k : o.keys()) {
            if (known.contains(k)) {
                continue;
            }
            try {
                out.put(k, decode.apply(o.get(k)));
            } catch (Failure f) {
                throw f.below(k);
            }
        }
        return Collections.unmodifiableMap(out);
    }

    /** The index of the variant of a union schema the value matches (by tag, else the first that accepts), or -1. */
    public static int variant(Schema union, Object v) {
        Schema s = union.resolved();
        if (s == null) {
            return -1;
        }
        if (s.tag() != null) {
            if (!(v instanceof JsonObject)) {
                return -1;
            }
            Object tag = ((JsonObject) v).get(s.tag());
            return s.tags().indexOf(tag);
        }
        for (int i = 0; i < s.variants().size(); i++) {
            if (s.variants().get(i).accepts(v)) {
                return i;
            }
        }
        return -1;
    }

    // ------------------------------------------------------------ encode

    /** A list as JSON. */
    public static <T> List<Object> encodeList(List<T> items, Function<T, Object> encode) {
        List<Object> out = new ArrayList<>(items.size());
        for (T item : items) {
            out.add(item == null ? null : encode.apply(item));
        }
        return out;
    }

    /** A map as JSON, in its order. */
    public static <T> JsonObject encodeMap(Map<String, T> members, Function<T, Object> encode) {
        JsonObject out = new JsonObject();
        for (Map.Entry<String, T> e : members.entrySet()) {
            out.set(e.getKey(), e.getValue() == null ? null : encode.apply(e.getValue()));
        }
        return out;
    }

    /** Bytes as base64 JSON text. */
    public static String encodeBytes(byte[] data) {
        return Base64.getEncoder().encodeToString(data);
    }

    /** Sets a member when the value is not null. */
    public static <T> void put(JsonObject o, String name, T value, Function<T, Object> encode) {
        if (value != null) {
            o.set(name, encode.apply(value));
        }
    }

    /** Sets a member of a patch field when it is set. */
    public static <T> void putPatch(JsonObject o, String name, Patch<T> value, Function<T, Object> encode) {
        if (value != null && value.isSet()) {
            o.set(name, value.toJson(encode));
        }
    }
}
