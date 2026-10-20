// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.regex.Pattern;
import java.util.regex.PatternSyntaxException;

/**
 * A schema of the runtime schema form (the descriptor document's {@code request}, {@code
 * response}, {@code page_item} and stream {@code event}): the validator of arguments, bodies,
 * page items and events. The rules are those of the descriptor document specification:
 * structure issues first, then constraint violations, then unknown members.
 */
public final class Schema {
    private final String kind;
    private final String format;
    private final String pattern;
    private final int bits;
    private final Long minLength;
    private final Long maxLength;
    private final Double minimum;
    private final Double maximum;
    private final Double exclusiveMinimum;
    private final Double exclusiveMaximum;
    private final Double multipleOf;
    private final List<Object> values;
    private final Schema items;
    private final Long minItems;
    private final Long maxItems;
    private final boolean unique;
    private final List<Field> fields;
    private final String additional;
    private final Schema extra;
    private final List<Schema> variants;
    private final String tag;
    private final List<String> tags;
    private final Schema inner;
    private final String ref;
    private final Map<String, Schema> defs;

    /**
     * One member of an object schema.
     *
     * @param name the member's name in the value checked
     * @param schema its schema
     * @param required whether it must be present
     * @param nullable whether null is accepted in place of a value
     */
    public record Field(String name, Schema schema, boolean required, boolean nullable) {}

    /**
     * One validation problem.
     *
     * @param path the path of the offending value: member names (String) and indexes (Integer)
     * @param message what is wrong
     */
    public record Issue(List<Object> path, String message) {}

    private Schema(JsonObject o, Map<String, Schema> defs) {
        this.defs = defs;
        this.kind = str(o, "kind", "any");
        this.format = str(o, "format", null);
        this.pattern = str(o, "pattern", null);
        Long b = lng(o, "bits");
        this.bits = b == null ? 0 : (int) (long) b;
        this.minLength = lng(o, "min_length");
        this.maxLength = lng(o, "max_length");
        this.minimum = dbl(o, "minimum");
        this.maximum = dbl(o, "maximum");
        this.exclusiveMinimum = dbl(o, "exclusive_minimum");
        this.exclusiveMaximum = dbl(o, "exclusive_maximum");
        this.multipleOf = dbl(o, "multiple_of");
        Object vals = o.get("values");
        List<Object> copied = new ArrayList<>();
        if (vals instanceof List<?>) {
            copied.addAll((List<?>) vals);
        }
        this.values = Collections.unmodifiableList(copied);
        this.items = o.get("items") instanceof JsonObject ? new Schema((JsonObject) o.get("items"), defs) : null;
        this.minItems = lng(o, "min_items");
        this.maxItems = lng(o, "max_items");
        this.unique = Boolean.TRUE.equals(o.get("unique"));
        List<Field> fs = new ArrayList<>();
        if (o.get("fields") instanceof List<?>) {
            for (Object f : (List<?>) o.get("fields")) {
                if (!(f instanceof JsonObject)) {
                    continue;
                }
                JsonObject fo = (JsonObject) f;
                Schema s = fo.get("schema") instanceof JsonObject
                        ? new Schema((JsonObject) fo.get("schema"), defs)
                        : null;
                fs.add(new Field(str(fo, "name", ""), s, Boolean.TRUE.equals(fo.get("required")),
                        Boolean.TRUE.equals(fo.get("nullable"))));
            }
        }
        this.fields = Collections.unmodifiableList(fs);
        this.additional = str(o, "additional", "open");
        this.extra = o.get("extra") instanceof JsonObject ? new Schema((JsonObject) o.get("extra"), defs) : null;
        List<Schema> vs = new ArrayList<>();
        if (o.get("variants") instanceof List<?>) {
            for (Object v : (List<?>) o.get("variants")) {
                if (v instanceof JsonObject) {
                    vs.add(new Schema((JsonObject) v, defs));
                }
            }
        }
        this.variants = Collections.unmodifiableList(vs);
        this.tag = str(o, "tag", null);
        List<String> ts = new ArrayList<>();
        if (o.get("tags") instanceof List<?>) {
            for (Object t : (List<?>) o.get("tags")) {
                ts.add(String.valueOf(t));
            }
        }
        this.tags = Collections.unmodifiableList(ts);
        this.inner = o.get("inner") instanceof JsonObject ? new Schema((JsonObject) o.get("inner"), defs) : null;
        this.ref = str(o, "ref", null);
    }

    private static String str(JsonObject o, String k, String fallback) {
        Object v = o.get(k);
        return v instanceof String ? (String) v : fallback;
    }

    private static Long lng(JsonObject o, String k) {
        Object v = o.get(k);
        return v instanceof Long ? (Long) v : null;
    }

    private static Double dbl(JsonObject o, String k) {
        Object v = o.get(k);
        return Json.isNumber(v) ? Json.asDouble(v) : null;
    }

    /**
     * The schemas of a descriptor document's {@code defs}, by type id; every {@code ref} of a
     * schema read with them resolves against the returned map.
     */
    static Map<String, Schema> defs(Object json) {
        Map<String, Schema> out = new HashMap<>();
        if (json instanceof JsonObject) {
            JsonObject o = (JsonObject) json;
            for (String k : o.keys()) {
                if (o.get(k) instanceof JsonObject) {
                    out.put(k, new Schema((JsonObject) o.get(k), out));
                }
            }
        }
        return out;
    }

    /** A schema in the runtime schema form, its refs resolved against defs; null for null. */
    static Schema read(Object json, Map<String, Schema> defs) {
        return json instanceof JsonObject ? new Schema((JsonObject) json, defs) : null;
    }

    /** A schema accepting any value. */
    public static Schema any() {
        return new Schema(new JsonObject(), Map.of());
    }

    /** The schema's kind. */
    public String kind() {
        return kind;
    }

    /** A tagged union's discriminator member, or null. */
    public String tag() {
        return tag;
    }

    /** A tagged union's tag value per variant. */
    public List<String> tags() {
        return tags;
    }

    /** The variants of a union. */
    public List<Schema> variants() {
        return variants;
    }

    /** This schema with refs followed. */
    Schema resolved() {
        Schema s = this;
        for (int i = 0; i < 64 && s != null && "ref".equals(s.kind); i++) {
            s = s.defs.get(s.ref);
        }
        return s;
    }

    /** Checks a value: the issues, none when it is valid. */
    public List<Issue> validate(Object value) {
        return validate(value, null);
    }

    /**
     * A schema pattern that could not be checked: the value was accepted without it. It never holds
     * the value.
     */
    record PatternNote(List<Object> path, String pattern, String reason) {}

    /** Checks a value, adding to {@code notes} (when not null) each pattern it could not check, once per path and pattern. */
    List<Issue> validate(Object value, List<PatternNote> notes) {
        Validation v = new Validation(notes);
        v.check(this, value, List.of(), 0);
        List<Issue> out = new ArrayList<>(v.structure);
        out.addAll(v.constraints);
        out.addAll(v.unknown);
        return out;
    }

    /** Whether the schema admits the value without issues. */
    public boolean accepts(Object value) {
        return validate(value).isEmpty();
    }

    // ---------------------------------------------------------- formats

    private static final String DATE =
            "(?:(?:[0-9][0-9][2468][048]|[0-9][0-9][13579][26]|[0-9][0-9]0[48]|[02468][048]00"
                    + "|[13579][26]00)-02-29|[0-9]{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12][0-9]|3[01])"
                    + "|(?:0[469]|11)-(?:0[1-9]|[12][0-9]|30)|(?:02)-(?:0[1-9]|1[0-9]|2[0-8])))";
    private static final String IPV4_PART = "(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])";
    private static final Map<String, Pattern> FORMATS = Map.of(
            "uuid", anchored("[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"),
            "email", anchored("[^\\s@\"]{1,64}@[^\\s@]{1,255}"),
            "date-time", anchored(DATE + "T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](?:\\.[0-9]+)?"
                    + "(?:Z|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9])"),
            "date", anchored(DATE),
            "ipv4", anchored("(?:" + IPV4_PART + "\\.){3}" + IPV4_PART),
            "ipv6", anchored(("(?:(?:H:){7}H|(?:H:){1,7}:|(?:H:){1,6}:H|(?:H:){1,5}(?::H){1,2}"
                    + "|(?:H:){1,4}(?::H){1,3}|(?:H:){1,3}(?::H){1,4}|(?:H:){1,2}(?::H){1,5}"
                    + "|H:(?:(?::H){1,6})|:(?:(?::H){1,7}|:))").replace("H", "[0-9a-fA-F]{1,4}")));

    private static Pattern anchored(String source) {
        return Pattern.compile("^(?:" + source + ")$");
    }

    /** Compiled patterns by source; a pattern Java cannot compile is not checked (and reported). */
    private static final Map<String, Pattern> PATTERNS = new ConcurrentHashMap<>();
    private static final Map<String, String> BAD_PATTERNS = new ConcurrentHashMap<>();

    /** Longest string (in characters) a spec pattern is matched against; longer is not checked. */
    static final int MAX_PATTERN_SUBJECT = 64 * 1024;
    /** Longest a single pattern match may run, in nanoseconds; a timeout means not checked. */
    static final long PATTERN_TIMEOUT_NANOS = 1_000_000_000L;
    /** Marks a pattern that was checked and did not match (compared by identity). */
    private static final String NO_MATCH = new String("no match");

    /** Thrown by {@link DeadlineSequence} once its deadline has passed. */
    private static final class PatternTimeout extends RuntimeException {
        private static final long serialVersionUID = 1L;

        PatternTimeout() {
            super(null, null, false, false);
        }
    }

    /** A CharSequence that fails reads after a deadline, so a runaway match can be abandoned. */
    private static final class DeadlineSequence implements CharSequence {
        private final CharSequence text;
        private final long deadline;
        private int reads;

        DeadlineSequence(CharSequence text, long deadline) {
            this.text = text;
            this.deadline = deadline;
        }

        @Override
        public char charAt(int index) {
            if ((++reads & 0x3FF) == 0 && System.nanoTime() - deadline > 0) {
                throw new PatternTimeout();
            }
            return text.charAt(index);
        }

        @Override
        public int length() {
            return text.length();
        }

        @Override
        public CharSequence subSequence(int start, int end) {
            DeadlineSequence sub = new DeadlineSequence(text.subSequence(start, end), deadline);
            return sub;
        }

        @Override
        public String toString() {
            return text.toString();
        }
    }

    private Pattern compiled() {
        if (pattern == null || BAD_PATTERNS.containsKey(pattern)) {
            return null;
        }
        Pattern p = PATTERNS.get(pattern);
        if (p != null) {
            return p;
        }
        try {
            p = Pattern.compile(pattern);
        } catch (PatternSyntaxException e) {
            BAD_PATTERNS.put(pattern, "it is not a regular expression this runtime can compile (java.util.regex: "
                    + e.getDescription() + ")");
            return null;
        }
        PATTERNS.put(pattern, p);
        return p;
    }

    // ------------------------------------------------------- validation

    static String describe(Object value) {
        if (value == null) {
            return "null";
        }
        if (value instanceof Boolean) {
            return "a boolean";
        }
        if (Json.isNumber(value)) {
            return "a number";
        }
        if (value instanceof String) {
            return "a string";
        }
        if (value instanceof List<?>) {
            return "an array";
        }
        if (value instanceof JsonObject) {
            return "an object";
        }
        return "a value";
    }

    static String boundText(double f) {
        if (f == Math.rint(f) && Math.abs(f) < Json.MAX_SAFE) {
            return Long.toString((long) f);
        }
        return Json.jsNumber(f);
    }

    private static List<Object> with(List<Object> path, Object seg) {
        List<Object> out = new ArrayList<>(path);
        out.add(seg);
        return out;
    }

    private static final class Validation {
        final List<Issue> structure = new ArrayList<>();
        final List<Issue> constraints = new ArrayList<>();
        final List<Issue> unknown = new ArrayList<>();
        private final List<PatternNote> notes;
        private final Set<String> noted = new HashSet<>();

        Validation(List<PatternNote> notes) {
            this.notes = notes;
        }

        void unchecked(List<Object> path, String pattern, String reason) {
            if (notes != null && noted.add(formatPath(path) + "\n" + pattern)) {
                notes.add(new PatternNote(path, pattern, reason));
            }
        }

        void bad(List<Object> path, String message) {
            structure.add(new Issue(path, message));
        }

        void limit(List<Object> path, String message) {
            constraints.add(new Issue(path, message));
        }

        void check(Schema s, Object value, List<Object> path, int depth) {
            if (s == null) {
                return;
            }
            if (depth > 512) {
                bad(path, "the value is nested too deeply");
                return;
            }
            switch (s.kind) {
                case "any":
                    return;
                case "never":
                    bad(path, "no value is allowed here");
                    return;
                case "ref": {
                    Schema target = s.defs.get(s.ref);
                    if (target != null) {
                        check(target, value, path, depth + 1);
                    }
                    return;
                }
                case "nullable":
                    if (value != null) {
                        check(s.inner, value, path, depth + 1);
                    }
                    return;
                case "string":
                    if (!(value instanceof String)) {
                        bad(path, "expected a string, found " + describe(value));
                        return;
                    }
                    stringLimits(s, (String) value, path);
                    return;
                case "bytes":
                    if (Binary.of(value) == null && !(value instanceof String)) {
                        bad(path, "expected binary data, found " + describe(value));
                    }
                    return;
                case "boolean":
                    if (!(value instanceof Boolean)) {
                        bad(path, "expected a boolean, found " + describe(value));
                    }
                    return;
                case "integer": {
                    if (!(value instanceof Long)) {
                        if (value instanceof Double) {
                            bad(path, "expected an integer, found " + Json.jsNumber((Double) value));
                        } else {
                            bad(path, "expected an integer, found " + describe(value));
                        }
                        return;
                    }
                    long n = (Long) value;
                    if (Math.abs((double) n) > Json.MAX_SAFE_INTEGER) {
                        bad(path, "expected a safe integer, found " + n);
                        return;
                    }
                    if (s.bits == 32 && (n < Integer.MIN_VALUE || n > Integer.MAX_VALUE)) {
                        bad(path, "expected a 32-bit integer");
                        return;
                    }
                    numberLimits(s, (double) n, path);
                    return;
                }
                case "number":
                    if (!Json.isNumber(value)) {
                        bad(path, "expected a number, found " + describe(value));
                        return;
                    }
                    numberLimits(s, Json.asDouble(value), path);
                    return;
                case "enum":
                    for (Object allowed : s.values) {
                        if (Json.deepEqual(allowed, value)) {
                            return;
                        }
                    }
                    bad(path, "must be one of " + Json.write(s.values));
                    return;
                case "const":
                    if (!s.values.isEmpty() && !Json.deepEqual(s.values.get(0), value)) {
                        bad(path, "must be " + Json.write(s.values.get(0)));
                    }
                    return;
                case "array": {
                    if (!(value instanceof List<?>)) {
                        bad(path, "expected an array, found " + describe(value));
                        return;
                    }
                    List<?> list = (List<?>) value;
                    for (int i = 0; i < list.size(); i++) {
                        check(s.items, list.get(i), with(path, i), depth + 1);
                    }
                    long n = list.size();
                    if (s.minItems != null && n < s.minItems) {
                        limit(path, "must have at least " + s.minItems + " items");
                    }
                    if (s.maxItems != null && n > s.maxItems) {
                        limit(path, "must have at most " + s.maxItems + " items");
                    }
                    if (s.unique) {
                        for (int i = 0; i < list.size(); i++) {
                            for (int j = 0; j < i; j++) {
                                if (Json.deepEqual(list.get(i), list.get(j))) {
                                    limit(path, "items must be unique");
                                    return;
                                }
                            }
                        }
                    }
                    return;
                }
                case "map": {
                    if (!(value instanceof JsonObject)) {
                        bad(path, "expected an object, found " + describe(value));
                        return;
                    }
                    JsonObject o = (JsonObject) value;
                    for (String k : o.keys()) {
                        check(s.items, o.get(k), with(path, k), depth + 1);
                    }
                    return;
                }
                case "object":
                    if (!(value instanceof JsonObject)) {
                        bad(path, "expected an object, found " + describe(value));
                        return;
                    }
                    object(s, (JsonObject) value, path, depth);
                    return;
                case "union":
                    union(s, value, path, depth);
                    return;
                case "all":
                    for (Schema member : s.variants) {
                        check(member, value, path, depth + 1);
                    }
                    return;
                case "limits":
                    if (value instanceof String) {
                        stringLimits(s, (String) value, path);
                    } else if (Json.isNumber(value)) {
                        numberLimits(s, Json.asDouble(value), path);
                    }
                    return;
                default:
                    bad(path, "unknown schema kind " + s.kind);
            }
        }

        void stringLimits(Schema s, String text, List<Object> path) {
            // JSON Schema counts code points: a surrogate pair is one, a lone surrogate is one.
            long n = text.codePointCount(0, text.length());
            if (s.minLength != null && n < s.minLength) {
                limit(path, "must have at least " + s.minLength + " characters");
            }
            if (s.maxLength != null && n > s.maxLength) {
                limit(path, "must have at most " + s.maxLength + " characters");
            }
            if (s.pattern != null) {
                Pattern re = s.compiled();
                if (re == null) {
                    unchecked(path, s.pattern, BAD_PATTERNS.get(s.pattern));
                } else {
                    String outcome = patternOutcome(re, text);
                    if (outcome == NO_MATCH) {
                        limit(path, "must match the pattern /" + s.pattern + "/");
                    } else if (outcome != null) {
                        unchecked(path, s.pattern, outcome);
                    }
                }
            }
            Pattern f = s.format == null ? null : FORMATS.get(s.format);
            if (f != null && !f.matcher(text).matches()) {
                limit(path, "must be a valid " + s.format);
            }
        }

        /** Null when the pattern matches, {@link #NO_MATCH} when it does not, else why it could not be checked. */
        private String patternOutcome(Pattern re, String text) {
            if (text.length() > MAX_PATTERN_SUBJECT) {
                return "the value is longer than " + MAX_PATTERN_SUBJECT + " characters, the most a pattern is matched against";
            }
            try {
                return re.matcher(new DeadlineSequence(text, System.nanoTime() + PATTERN_TIMEOUT_NANOS)).find() ? null : NO_MATCH;
            } catch (PatternTimeout e) {
                return "matching took longer than " + PATTERN_TIMEOUT_NANOS / 1_000_000_000L + " s";
            } catch (StackOverflowError e) {
                // java.util.regex recurses per repetition of some groups, so a long subject can
                // overflow the stack; that is as unbounded as a timeout and reads as not checked.
                return "matching the value overflowed the stack of java.util.regex";
            }
        }

        void numberLimits(Schema s, double f, List<Object> path) {
            if (s.minimum != null && f < s.minimum) {
                limit(path, "must be at least " + boundText(s.minimum));
            }
            if (s.maximum != null && f > s.maximum) {
                limit(path, "must be at most " + boundText(s.maximum));
            }
            if (s.exclusiveMinimum != null && f <= s.exclusiveMinimum) {
                limit(path, "must be greater than " + boundText(s.exclusiveMinimum));
            }
            if (s.exclusiveMaximum != null && f >= s.exclusiveMaximum) {
                limit(path, "must be less than " + boundText(s.exclusiveMaximum));
            }
            if (s.multipleOf != null && s.multipleOf != 0) {
                double q = f / s.multipleOf;
                if (Math.abs(q - Math.rint(q)) > 1e-9 * Math.max(Math.abs(q), 1)) {
                    limit(path, "must be a multiple of " + boundText(s.multipleOf));
                }
            }
        }

        void object(Schema s, JsonObject obj, List<Object> path, int depth) {
            Set<String> known = new HashSet<>();
            for (Field f : s.fields) {
                known.add(f.name());
                List<Object> at = with(path, f.name());
                if (!obj.has(f.name())) {
                    if (f.required()) {
                        bad(at, "missing required member " + f.name());
                    }
                } else if (obj.get(f.name()) == null && f.nullable()) {
                    continue;
                } else {
                    check(f.schema(), obj.get(f.name()), at, depth + 1);
                }
            }
            for (String k : obj.keys()) {
                if (known.contains(k)) {
                    continue;
                }
                if (s.extra != null) {
                    check(s.extra, obj.get(k), with(path, k), depth + 1);
                } else if ("closed".equals(s.additional)) {
                    unknown.add(new Issue(with(path, k), "unknown member " + k));
                }
            }
        }

        void union(Schema s, Object value, List<Object> path, int depth) {
            if (s.tag != null) {
                if (!(value instanceof JsonObject)) {
                    bad(path, "expected an object, found " + describe(value));
                    return;
                }
                JsonObject obj = (JsonObject) value;
                Object tagValue = obj.get(s.tag);
                if (!(tagValue instanceof String)) {
                    bad(with(path, s.tag), "missing the discriminator " + s.tag);
                    return;
                }
                for (int i = 0; i < s.tags.size(); i++) {
                    if (s.tags.get(i).equals(tagValue) && i < s.variants.size()) {
                        check(s.variants.get(i), value, path, depth + 1);
                        return;
                    }
                }
                bad(with(path, s.tag), "must be one of " + Json.write(new ArrayList<Object>(s.tags)));
                return;
            }
            for (Schema variant : s.variants) {
                if (variant.accepts(value)) {
                    return;
                }
            }
            bad(path, "does not match any variant");
        }
    }

    // ------------------------------------------------------------ paths

    /** An issue path as .key and [index] segments. */
    static String formatPath(List<Object> path) {
        StringBuilder b = new StringBuilder();
        for (Object seg : path) {
            String s = String.valueOf(seg);
            if (seg instanceof Integer || Json.isIndex(s)) {
                b.append('[').append(s).append(']');
            } else {
                b.append('.').append(s);
            }
        }
        return b.toString();
    }

    static List<String> segments(List<Object> path) {
        List<String> out = new ArrayList<>();
        for (Object seg : path) {
            out.add(String.valueOf(seg));
        }
        return out;
    }
}
