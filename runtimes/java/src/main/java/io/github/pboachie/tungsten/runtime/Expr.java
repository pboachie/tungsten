// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Predicates and expressions.
 *
 * <p>Predicates (verify expect and terminal, poll until): field path to {"in": [...]} |
 * {"equals": x} | {"contains": ...}; any other value is compared for equality. Every entry must
 * hold.
 *
 * <p>Expressions: JSON in which a string starting with "$" is a reference resolved against a
 * scope ($input.a.b, $name.a, $response.x, $args.y), an object {"expr": "&lt;ref&gt; in [..]" |
 * "&lt;ref&gt; == x" | "&lt;ref&gt; != x"} is a boolean, anything else a literal. A dry
 * evaluation (macro previews) knows the input but not the earlier results: a reference to one is
 * the placeholder "&lt;from step NAME: path&gt;". Absent values are {@link Json#ABSENT}.
 */
final class Expr {
    private Expr() {}

    /** Whether every entry of the predicate holds on the value; null always holds. */
    static boolean predicate(Object predicate, Object value) {
        if (predicate == null) {
            return true;
        }
        if (!(predicate instanceof JsonObject)) {
            return false;
        }
        JsonObject p = (JsonObject) predicate;
        for (String path : p.keys()) {
            if (!holds(p.get(path), Json.at(value, path))) {
                return false;
            }
        }
        return true;
    }

    private static boolean equalOpt(Object a, Object b) {
        if (a == Json.ABSENT || b == Json.ABSENT) {
            return a == b;
        }
        return Json.deepEqual(a, b);
    }

    private static boolean holds(Object test, Object actual) {
        if (test instanceof JsonObject && ((JsonObject) test).size() == 1) {
            JsonObject m = (JsonObject) test;
            if (m.has("in")) {
                if (!(m.get("in") instanceof List<?>)) {
                    return false;
                }
                for (Object c : (List<?>) m.get("in")) {
                    if (equalOpt(c, actual)) {
                        return true;
                    }
                }
                return false;
            }
            if (m.has("equals")) {
                return equalOpt(m.get("equals"), actual);
            }
            if (m.has("contains")) {
                Object pattern = m.get("contains");
                if (pattern instanceof String) {
                    if (actual instanceof String) {
                        return ((String) actual).contains((String) pattern);
                    }
                    if (actual instanceof List<?>) {
                        for (Object item : (List<?>) actual) {
                            if (pattern.equals(item)) {
                                return true;
                            }
                        }
                    }
                    return false;
                }
                if (actual instanceof List<?> && !(pattern instanceof List<?>)) {
                    for (Object item : (List<?>) actual) {
                        if (containsSubset(item, pattern)) {
                            return true;
                        }
                    }
                    return false;
                }
                return containsSubset(actual, pattern);
            }
        }
        return equalOpt(test, actual);
    }

    /**
     * Every field of the pattern appears in the value with an equal (recursively contained)
     * value; arrays match when each pattern element is contained by some element.
     */
    static boolean containsSubset(Object value, Object pattern) {
        if (value == Json.ABSENT) {
            return false;
        }
        if (pattern instanceof JsonObject) {
            if (!(value instanceof JsonObject)) {
                return false;
            }
            JsonObject have = (JsonObject) value;
            JsonObject p = (JsonObject) pattern;
            for (String k : p.keys()) {
                if (!have.has(k) || !containsSubset(have.get(k), p.get(k))) {
                    return false;
                }
            }
            return true;
        }
        if (pattern instanceof List<?>) {
            if (!(value instanceof List<?>)) {
                return false;
            }
            for (Object want : (List<?>) pattern) {
                boolean found = false;
                for (Object h : (List<?>) value) {
                    if (containsSubset(h, want)) {
                        found = true;
                        break;
                    }
                }
                if (!found) {
                    return false;
                }
            }
            return true;
        }
        return Json.deepEqual(value, pattern);
    }

    /** One $name.path reference, or {@link Json#ABSENT}. */
    static Object ref(String reference, JsonObject scope) {
        List<String> segments = Json.splitPath(reference.substring(1));
        if (segments.isEmpty() || !scope.has(segments.get(0))) {
            return Json.ABSENT;
        }
        return Json.at(scope.get(segments.get(0)), segments.subList(1, segments.size()));
    }

    /** Evaluates an expression; unknown references are absent (dropped from objects). */
    static Object evaluate(Object expr, JsonObject scope, int depth) {
        if (depth > 64) {
            return null;
        }
        if (expr instanceof String) {
            String t = (String) expr;
            return t.startsWith("$") ? ref(t, scope) : t;
        }
        if (expr instanceof List<?>) {
            List<Object> out = new ArrayList<>();
            for (Object item : (List<?>) expr) {
                Object v = evaluate(item, scope, depth + 1);
                out.add(v == Json.ABSENT ? null : v);
            }
            return out;
        }
        if (expr instanceof JsonObject) {
            JsonObject t = (JsonObject) expr;
            if (t.size() == 1 && t.get("expr") instanceof String) {
                return bool((String) t.get("expr"), scope);
            }
            JsonObject out = new JsonObject();
            for (String k : t.keys()) {
                Object v = evaluate(t.get(k), scope, depth + 1);
                if (v != Json.ABSENT) {
                    out.set(k, v);
                }
            }
            return out;
        }
        return expr;
    }

    static String placeholder(String name, String path) {
        return path.isEmpty() ? "<from step " + name + ">" : "<from step " + name + ": " + path + ">";
    }

    private static final Pattern PLACEHOLDER = Pattern.compile("^<from step [^<>]+>$");

    static void placeholders(Object v, List<String> out, int depth) {
        if (depth > 64) {
            return;
        }
        if (v instanceof String) {
            if (PLACEHOLDER.matcher((String) v).matches()) {
                out.add((String) v);
            }
        } else if (v instanceof List<?>) {
            for (Object item : (List<?>) v) {
                placeholders(item, out, depth + 1);
            }
        } else if (v instanceof JsonObject) {
            for (Object item : ((JsonObject) v).asMap().values()) {
                placeholders(item, out, depth + 1);
            }
        }
    }

    static boolean containsPlaceholder(Object v) {
        List<String> found = new ArrayList<>();
        placeholders(v, found, 0);
        return !found.isEmpty();
    }

    /** [name, path] of the pending binding a reference names, or null. */
    private static String[] pendingRef(String reference, Set<String> pending) {
        if (!reference.startsWith("$")) {
            return null;
        }
        List<String> segs = Json.splitPath(reference.substring(1));
        if (segs.isEmpty()) {
            return null;
        }
        String name = segs.get(0);
        if (!pending.contains(name) || !reference.startsWith("$" + name)) {
            return null;
        }
        String rest = reference.substring(name.length() + 1);
        return new String[] {name, rest.startsWith(".") ? rest.substring(1) : rest};
    }

    private static final Pattern DRY_REF = Pattern.compile("^\\$[^\\s=!]+");

    /** A dry evaluation: references to pending results become placeholders. */
    static Object dry(Object expr, JsonObject scope, Set<String> pending, int depth) {
        if (depth > 64) {
            return null;
        }
        if (expr instanceof String) {
            String t = (String) expr;
            if (t.startsWith("$")) {
                String[] p = pendingRef(t, pending);
                if (p != null) {
                    return placeholder(p[0], p[1]);
                }
                return ref(t, scope);
            }
            return t;
        }
        if (expr instanceof List<?>) {
            List<Object> out = new ArrayList<>();
            for (Object item : (List<?>) expr) {
                Object v = dry(item, scope, pending, depth + 1);
                out.add(v == Json.ABSENT ? null : v);
            }
            return out;
        }
        if (expr instanceof JsonObject) {
            JsonObject t = (JsonObject) expr;
            if (t.size() == 1 && t.get("expr") instanceof String) {
                String source = ((String) t.get("expr")).trim();
                Matcher head = DRY_REF.matcher(source);
                if (head.find()) {
                    String[] p = pendingRef(head.group(), pending);
                    if (p != null) {
                        String rest = source.substring(p[0].length() + 1);
                        return placeholder(p[0], rest.startsWith(".") ? rest.substring(1) : rest);
                    }
                }
                return bool(source, scope);
            }
            JsonObject out = new JsonObject();
            for (String k : t.keys()) {
                Object v = dry(t.get(k), scope, pending, depth + 1);
                if (v != Json.ABSENT) {
                    out.set(k, v);
                }
            }
            return out;
        }
        return expr;
    }

    /** A predicate in words: state in ["a", "b"], endpoints contains {...}, a = 1, joined with "and". */
    static String describe(Object predicate) {
        if (!(predicate instanceof JsonObject)) {
            return "";
        }
        JsonObject p = (JsonObject) predicate;
        List<String> parts = new ArrayList<>();
        for (String path : p.keys()) {
            Object test = p.get(path);
            if (test instanceof JsonObject && ((JsonObject) test).size() == 1) {
                JsonObject m = (JsonObject) test;
                if (m.get("in") instanceof List<?>) {
                    List<String> shown = new ArrayList<>();
                    for (Object v : (List<?>) m.get("in")) {
                        shown.add(Json.write(v));
                    }
                    parts.add(path + " in [" + String.join(", ", shown) + "]");
                    continue;
                }
                if (m.has("equals")) {
                    parts.add(path + " = " + Json.write(m.get("equals")));
                    continue;
                }
                if (m.has("contains")) {
                    parts.add(path + " contains " + Json.write(m.get("contains")));
                    continue;
                }
            }
            parts.add(path + " = " + Json.write(test));
        }
        return String.join(" and ", parts);
    }

    private static final Pattern SINGLE_QUOTED = Pattern.compile("'((?:[^'\\\\]|\\\\.)*)'");

    private static Object literal(String text) {
        String trimmed = text.trim();
        try {
            return Json.parse(trimmed);
        } catch (Json.JsonException e) {
            // single-quoted strings below
        }
        Matcher m = SINGLE_QUOTED.matcher(trimmed);
        StringBuilder b = new StringBuilder();
        while (m.find()) {
            m.appendReplacement(b, Matcher.quoteReplacement(Json.jsString(m.group(1).replace("\\'", "'"))));
        }
        m.appendTail(b);
        try {
            return Json.parse(b.toString());
        } catch (Json.JsonException e) {
            return Json.ABSENT;
        }
    }

    private static final Pattern BOOLEAN =
            Pattern.compile("^\\s*(\\$[^\\s=!]+)\\s+(in|==|!=)\\s+(.+)$", Pattern.DOTALL);

    /** A boolean expression; null when it is malformed. */
    static Object bool(String source, JsonObject scope) {
        Matcher m = BOOLEAN.matcher(source);
        if (!m.matches()) {
            return null;
        }
        Object lit = literal(m.group(3));
        if (lit == Json.ABSENT) {
            return null;
        }
        Object actual = ref(m.group(1), scope);
        if (m.group(2).equals("in")) {
            if (!(lit instanceof List<?>)) {
                return null;
            }
            for (Object v : (List<?>) lit) {
                if (equalOpt(v, actual)) {
                    return true;
                }
            }
            return false;
        }
        if (actual == Json.ABSENT) {
            actual = null;
        }
        boolean equal = Json.deepEqual(actual, lit);
        return m.group(2).equals("==") ? equal : !equal;
    }
}
