// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.io.ByteArrayOutputStream;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.function.UnaryOperator;

/** Parameter serialization, request bodies and headers. */
final class Serialize {
    private Serialize() {}

    /** A name and a value. */
    record Pair(String k, String v) {}

    /** A value that cannot be encoded: VALIDATION_FAILED. */
    static final class Problem extends Exception {
        private static final long serialVersionUID = 1L;
        final String parameter;
        final String expected;
        final transient Object value;

        Problem(String parameter, String expected, Object value) {
            super(expected, null, false, false);
            this.parameter = parameter;
            this.expected = expected;
            this.value = value;
        }
    }

    /** String(value) for a scalar, JSON text for anything else. */
    static String scalar(Object v) {
        if (v instanceof String) {
            return (String) v;
        }
        if (Json.isNumber(v)) {
            return Json.numberText(v);
        }
        if (v instanceof Boolean) {
            return v.toString();
        }
        return Json.write(v);
    }

    private static List<String> encodedItems(List<?> items) {
        List<String> out = new ArrayList<>();
        for (Object v : items) {
            if (v != null) {
                out.add(Text.encodeComponent(scalar(v)));
            }
        }
        return out;
    }

    private static List<Pair> encodedPairs(JsonObject o) {
        List<Pair> out = new ArrayList<>();
        for (String k : o.keys()) {
            Object v = o.get(k);
            if (v != null) {
                out.add(new Pair(Text.encodeComponent(k), Text.encodeComponent(scalar(v))));
            }
        }
        return out;
    }

    private static List<String> flat(List<Pair> pairs) {
        List<String> out = new ArrayList<>();
        for (Pair p : pairs) {
            out.add(p.k);
            out.add(p.v);
        }
        return out;
    }

    private static List<String> joined(List<Pair> pairs) {
        List<String> out = new ArrayList<>();
        for (Pair p : pairs) {
            out.add(p.k + "=" + p.v);
        }
        return out;
    }

    /** One path parameter value, percent-encoded. */
    static String pathParam(OperationDescriptor.Param p, Object v) {
        String name = Text.encodeComponent(p.wire());
        String style = "label".equals(p.style()) || "matrix".equals(p.style()) ? p.style() : "simple";
        if (v instanceof List<?>) {
            List<String> items = encodedItems((List<?>) v);
            if (style.equals("label")) {
                return "." + String.join(p.explode() ? "." : ",", items);
            }
            if (style.equals("matrix") && p.explode()) {
                StringBuilder b = new StringBuilder();
                for (String i : items) {
                    b.append(';').append(name).append('=').append(i);
                }
                return b.toString();
            }
            if (style.equals("matrix")) {
                return ";" + name + "=" + String.join(",", items);
            }
            return String.join(",", items);
        }
        if (v instanceof JsonObject) {
            List<Pair> pairs = encodedPairs((JsonObject) v);
            List<String> exploded = joined(pairs);
            List<String> flat = flat(pairs);
            if (style.equals("label") && p.explode()) {
                return "." + String.join(".", exploded);
            }
            if (style.equals("label")) {
                return "." + String.join(",", flat);
            }
            if (style.equals("matrix") && p.explode()) {
                StringBuilder b = new StringBuilder();
                for (String e : exploded) {
                    b.append(';').append(e);
                }
                return b.toString();
            }
            if (style.equals("matrix")) {
                return ";" + name + "=" + String.join(",", flat);
            }
            return String.join(",", p.explode() ? exploded : flat);
        }
        String val = Text.encodeComponent(scalar(v));
        if (style.equals("label")) {
            return "." + val;
        }
        if (style.equals("matrix")) {
            return ";" + name + "=" + val;
        }
        return val;
    }

    private static void deepObjectPairs(String prefix, Object v, List<String> out, int depth) {
        if (depth > 16 || v == null) {
            return;
        }
        if (v instanceof List<?>) {
            for (Object item : (List<?>) v) {
                deepObjectPairs(prefix, item, out, depth + 1);
            }
        } else if (v instanceof JsonObject) {
            JsonObject o = (JsonObject) v;
            for (String k : o.keys()) {
                if (o.get(k) != null) {
                    deepObjectPairs(prefix + "[" + Text.encodeComponent(k) + "]", o.get(k), out, depth + 1);
                }
            }
        } else {
            out.add(prefix + "=" + Text.encodeComponent(scalar(v)));
        }
    }

    /** The query parts (name=value, encoded) of one parameter. */
    static List<String> queryParam(OperationDescriptor.Param p, Object v) {
        List<String> out = new ArrayList<>();
        if (v == null) {
            return out;
        }
        String name = Text.encodeComponent(p.wire());
        String delimiter = ",";
        if ("space_delimited".equals(p.style())) {
            delimiter = "%20";
        } else if ("pipe_delimited".equals(p.style())) {
            delimiter = "|";
        }
        if (v instanceof JsonObject && "deep_object".equals(p.style())) {
            deepObjectPairs(name, v, out, 0);
            return out;
        }
        if (v instanceof List<?>) {
            List<String> items = encodedItems((List<?>) v);
            if (items.isEmpty()) {
                return out;
            }
            if (p.explode() || "deep_object".equals(p.style())) {
                for (String item : items) {
                    out.add(name + "=" + item);
                }
                return out;
            }
            out.add(name + "=" + String.join(delimiter, items));
            return out;
        }
        if (v instanceof JsonObject) {
            List<Pair> pairs = encodedPairs((JsonObject) v);
            if (p.explode()) {
                return joined(pairs);
            }
            out.add(name + "=" + String.join(delimiter, flat(pairs)));
            return out;
        }
        out.add(name + "=" + Text.encodeComponent(scalar(v)));
        return out;
    }

    /** A header parameter (simple style, not encoded). */
    static String headerParam(OperationDescriptor.Param p, Object v) {
        List<String> parts = new ArrayList<>();
        if (v instanceof List<?>) {
            for (Object item : (List<?>) v) {
                if (item != null) {
                    parts.add(scalar(item));
                }
            }
            return String.join(",", parts);
        }
        if (v instanceof JsonObject) {
            JsonObject o = (JsonObject) v;
            for (String k : o.keys()) {
                Object item = o.get(k);
                if (item == null) {
                    continue;
                }
                if (p.explode()) {
                    parts.add(k + "=" + scalar(item));
                } else {
                    parts.add(k);
                    parts.add(scalar(item));
                }
            }
            return String.join(",", parts);
        }
        return scalar(v);
    }

    /** name=value of a cookie parameter. */
    static String cookieParam(OperationDescriptor.Param p, Object v) {
        if (v instanceof List<?>) {
            return p.wire() + "=" + String.join(",", encodedItems((List<?>) v));
        }
        if (v instanceof JsonObject) {
            return p.wire() + "=" + String.join(",", flat(encodedPairs((JsonObject) v)));
        }
        return p.wire() + "=" + Text.encodeComponent(scalar(v));
    }

    // ------------------------------------------------------------ bodies

    /** One part of a multipart body. */
    record Part(String name, String kind, String text, byte[] data, String filename, String contentType) {}

    /** What goes on the wire: nothing, bytes, or multipart parts. */
    static final class Payload {
        static final Payload EMPTY = new Payload(true, null, null);
        final boolean empty;
        final byte[] bytes;
        final List<Part> parts;

        Payload(boolean empty, byte[] bytes, List<Part> parts) {
            this.empty = empty;
            this.bytes = bytes;
            this.parts = parts;
        }

        static Payload of(byte[] bytes) {
            return new Payload(false, bytes, null);
        }
    }

    /** An encoded body with its display form and the bytes it is hashed by. */
    record Encoded(Payload payload, String contentType, Object display, byte[] hashed) {}

    /**
     * The body before encoding: merged fields picked from the arguments (under their wire
     * names), or args[arg]; wrapped in the rpc envelope when the operation has one. Absent
     * values are {@link Json#ABSENT}.
     */
    static Object bodyValue(OperationDescriptor op, JsonObject args, java.util.Set<String> paramNames) {
        Object value = Json.ABSENT;
        OperationDescriptor.Body b = op.body();
        if (b != null) {
            if (b.arg() != null) {
                if (args.has(b.arg())) {
                    value = args.get(b.arg());
                }
            } else {
                JsonObject picked = new JsonObject();
                for (OperationDescriptor.MergedField f : b.fields()) {
                    if (args.has(f.arg())) {
                        picked.set(f.wire(), args.get(f.arg()));
                    }
                }
                if (!picked.isEmpty() || b.required() || op.rpc() != null) {
                    value = picked;
                }
            }
        }
        OperationDescriptor.Rpc rpc = op.rpc();
        if (rpc == null) {
            return value;
        }
        Object params;
        if (b == null) {
            JsonObject o = new JsonObject();
            for (String k : args.keys()) {
                if (!paramNames.contains(k)) {
                    o.set(k, args.get(k));
                }
            }
            params = o;
        } else if (value != Json.ABSENT) {
            params = value;
        } else {
            params = new JsonObject();
        }
        JsonObject envelope = new JsonObject();
        if (rpc.constants() != null) {
            for (String k : rpc.constants().keys()) {
                envelope.set(k, rpc.constants().get(k));
            }
        }
        envelope.set(rpc.field(), rpc.value());
        envelope.set(rpc.paramsField(), params);
        return envelope;
    }

    private static List<Pair> formPairs(Object v) throws Problem {
        if (!(v instanceof JsonObject)) {
            throw new Problem("body", "an object of form fields", v);
        }
        JsonObject o = (JsonObject) v;
        List<Pair> out = new ArrayList<>();
        for (String k : o.keys()) {
            Object item = o.get(k);
            if (item == null) {
                continue;
            }
            if (item instanceof List<?>) {
                for (Object x : (List<?>) item) {
                    if (x != null) {
                        out.add(new Pair(k, scalar(x)));
                    }
                }
            } else {
                out.add(new Pair(k, scalar(item)));
            }
        }
        return out;
    }

    static String binaryDisplay(int n, String media) {
        return media == null || media.isEmpty() ? "<" + n + " bytes>" : "<" + n + " bytes of " + media + ">";
    }

    /** Encodes the value per the body descriptor; value is {@link Json#ABSENT} without a body. */
    static Encoded encode(OperationDescriptor.Body d, Object v, UnaryOperator<Object> redact) throws Problem {
        if (v == Json.ABSENT) {
            return new Encoded(Payload.EMPTY, null, null, null);
        }
        String encoding = d == null ? "json" : d.encoding();
        String media = d == null ? "application/json" : d.mediaType();
        switch (encoding) {
            case "form": {
                List<Pair> pairs = formPairs(v);
                List<String> parts = new ArrayList<>();
                JsonObject raw = new JsonObject();
                for (Pair p : pairs) {
                    parts.add(Text.encodeFormComponent(p.k) + "=" + Text.encodeFormComponent(p.v));
                    raw.set(p.k, p.v);
                }
                byte[] text = Text.utf8(String.join("&", parts));
                return new Encoded(Payload.of(text), media, redact.apply(raw), text);
            }
            case "multipart": {
                if (!(v instanceof JsonObject)) {
                    throw new Problem("body", "an object of multipart fields", v);
                }
                JsonObject o = (JsonObject) v;
                List<Part> parts = new ArrayList<>();
                JsonObject display = new JsonObject();
                for (String k : o.keys()) {
                    Object item = o.get(k);
                    if (item == null) {
                        continue;
                    }
                    List<Object> list = new ArrayList<>();
                    boolean isArray = item instanceof List<?>;
                    if (isArray) {
                        for (Object x : (List<?>) item) {
                            if (x != null) {
                                list.add(x);
                            }
                        }
                    } else {
                        list.add(item);
                    }
                    List<Object> shown = new ArrayList<>();
                    for (Object x : list) {
                        Binary bin = Binary.of(x);
                        if (bin != null) {
                            shown.add(binaryDisplay(bin.length(), bin.contentType()));
                            parts.add(new Part(k, "file", null, bin.data(),
                                    bin.filename() != null ? bin.filename() : k,
                                    bin.contentType() != null ? bin.contentType() : "application/octet-stream"));
                        } else if (x instanceof JsonObject) {
                            parts.add(new Part(k, "json", Json.write(x), null, null, null));
                            shown.add(x);
                        } else {
                            parts.add(new Part(k, "text", scalar(x), null, null, null));
                            shown.add(scalar(x));
                        }
                    }
                    if (isArray) {
                        display.set(k, shown);
                    } else {
                        display.set(k, shown.isEmpty() ? null : shown.get(0));
                    }
                }
                return new Encoded(new Payload(false, null, parts), null, redact.apply(display),
                        Text.utf8(Json.canonical(v)));
            }
            case "bytes": {
                byte[] data;
                if (v instanceof String) {
                    data = Text.utf8((String) v);
                } else {
                    Binary bin = Binary.of(v);
                    if (bin == null) {
                        throw new Problem("body", "binary data (a Binary value)", v);
                    }
                    data = bin.data();
                }
                return new Encoded(Payload.of(data), media, binaryDisplay(data.length, media), data);
            }
            case "text": {
                if (!(v instanceof String || Json.isNumber(v) || v instanceof Boolean)) {
                    throw new Problem("body", "a string", v);
                }
                String text = scalar(v);
                return new Encoded(Payload.of(Text.utf8(text)), media, text, Text.utf8(text));
            }
            default: {
                String text = Json.write(v);
                Object raw;
                try {
                    raw = Json.parse(text);
                } catch (Json.JsonException e) {
                    throw new Problem("body", "a JSON-serializable value", v);
                }
                return new Encoded(Payload.of(Text.utf8(text)), media, redact.apply(raw),
                        Text.utf8(Json.canonical(raw)));
            }
        }
    }

    /** A multipart body and its content type. */
    static Map.Entry<byte[], String> multipart(List<Part> parts) {
        String boundary = "tungsten-" + Text.hex(Text.randomBytes(12));
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        for (Part p : parts) {
            StringBuilder head = new StringBuilder();
            head.append("--").append(boundary).append("\r\n");
            switch (p.kind()) {
                case "file":
                    head.append("Content-Disposition: form-data; name=\"").append(escapeQuotes(p.name()))
                            .append("\"; filename=\"").append(escapeQuotes(p.filename())).append("\"\r\n");
                    head.append("Content-Type: ").append(p.contentType()).append("\r\n");
                    break;
                case "json":
                    head.append("Content-Disposition: form-data; name=\"").append(escapeQuotes(p.name())).append("\"\r\n");
                    head.append("Content-Type: application/json\r\n");
                    break;
                default:
                    head.append("Content-Disposition: form-data; name=\"").append(escapeQuotes(p.name())).append("\"\r\n");
            }
            head.append("\r\n");
            out.writeBytes(Text.utf8(head.toString()));
            out.writeBytes(p.kind().equals("file") ? p.data() : Text.utf8(p.text()));
            out.writeBytes(Text.utf8("\r\n"));
        }
        out.writeBytes(Text.utf8("--" + boundary + "--\r\n"));
        return Map.entry(out.toByteArray(), "multipart/form-data; boundary=" + boundary);
    }

    private static String escapeQuotes(String s) {
        return s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\r", "%0D").replace("\n", "%0A");
    }

    // ----------------------------------------------------------- headers

    /** A header with whether its value is a secret. */
    record Header(String name, String value, boolean secret) {}

    /** A case-insensitive header collection that remembers which values are secrets. */
    static final class Headers {
        final List<Header> entries = new ArrayList<>();

        void set(String name, String value, boolean secret) {
            for (int i = 0; i < entries.size(); i++) {
                if (entries.get(i).name().equalsIgnoreCase(name)) {
                    entries.set(i, new Header(name, value, secret));
                    return;
                }
            }
            entries.add(new Header(name, value, secret));
        }

        String get(String name) {
            for (Header h : entries) {
                if (h.name().equalsIgnoreCase(name)) {
                    return h.value();
                }
            }
            return null;
        }

        Map<String, String> toMap(boolean redact) {
            Map<String, String> out = new LinkedHashMap<>();
            for (Header h : entries) {
                out.put(h.name(), redact && h.secret() ? Text.REDACTED : h.value());
            }
            return out;
        }

        List<String> secrets() {
            List<String> out = new ArrayList<>();
            for (Header h : entries) {
                if (h.secret()) {
                    out.add(h.value());
                }
            }
            return out;
        }
    }
}
