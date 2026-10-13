// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/** Helpers about an operation's arguments: paths, sensitivity, names. */
final class Args {
    private Args() {}

    /** A structural problem that makes the descriptor unusable, or null. */
    static String descriptorProblem(OperationDescriptor op) {
        if (op.id() == null || op.id().isEmpty()) {
            return "`id` must be a non-empty string";
        }
        for (OperationDescriptor.Param p : op.params()) {
            if (p.name().isEmpty() || p.wire().isEmpty()) {
                return "every parameter needs a `name` and a `wire`";
            }
        }
        if (op.rpc() != null && (op.rpc().field().isEmpty() || op.rpc().paramsField().isEmpty())) {
            return "`rpc` needs `field` and `params_field`";
        }
        return null;
    }

    /**
     * The JSON path of an argument issue: body.&lt;wire&gt; for a merged body field, body for the
     * whole-body argument, else args.&lt;name&gt;.
     */
    static String argumentPath(OperationDescriptor op, List<Object> path) {
        if (op.body() != null && !path.isEmpty()) {
            String head = String.valueOf(path.get(0));
            List<Object> rest = path.subList(1, path.size());
            if (op.body().arg() != null) {
                if (op.body().arg().equals(head)) {
                    return "body" + Schema.formatPath(rest);
                }
            } else {
                for (OperationDescriptor.MergedField f : op.body().fields()) {
                    if (f.arg().equals(head)) {
                        return "body." + f.wire() + Schema.formatPath(rest);
                    }
                }
            }
        }
        return path.isEmpty() ? "args" : "args" + Schema.formatPath(path);
    }

    static List<String> sensitiveRequestPaths(OperationDescriptor op) {
        List<String> out = new ArrayList<>();
        for (String p : op.agent().sensitiveRequestFields()) {
            if (!p.isEmpty()) {
                out.add(p);
            }
        }
        return out;
    }

    private static boolean digitsOnly(String s) {
        for (int i = 0; i < s.length(); i++) {
            if (s.charAt(i) < '0' || s.charAt(i) > '9') {
                return false;
            }
        }
        return true;
    }

    /** An argument path without array indexes. */
    static String dotted(List<Object> path) {
        List<String> parts = new ArrayList<>();
        for (Object seg : path) {
            if (seg instanceof String && !digitsOnly((String) seg)) {
                parts.add((String) seg);
            }
        }
        return String.join(".", parts);
    }

    static boolean sensitiveArg(OperationDescriptor op, List<Object> path) {
        if (!path.isEmpty()) {
            String head = String.valueOf(path.get(0));
            for (OperationDescriptor.Param p : op.params()) {
                if (p.sensitive() && p.name().equals(head)) {
                    return true;
                }
            }
        }
        String d = dotted(path);
        if (!d.isEmpty()) {
            for (String f : sensitiveRequestPaths(op)) {
                if (d.equals(f) || d.startsWith(f + ".")) {
                    return true;
                }
            }
        }
        for (Object seg : path) {
            if (seg instanceof String && Text.looksSensitive((String) seg)) {
                return true;
            }
        }
        return false;
    }

    /** The value with the sensitive fields below the argument at path redacted. */
    static Object redactBelow(OperationDescriptor op, List<Object> path, Object value) {
        String d = dotted(path);
        String prefix = d.isEmpty() ? "" : d + ".";
        List<String> below = new ArrayList<>();
        for (String f : sensitiveRequestPaths(op)) {
            if (f.startsWith(prefix)) {
                below.add(f.substring(prefix.length()));
            }
        }
        return below.isEmpty() ? value : Text.redactPaths(value, below);
    }

    /** The sensitive argument paths relative to the body as sent; "" is the whole body. */
    static List<String> sensitiveBodyPaths(OperationDescriptor op) {
        List<String> paths = sensitiveRequestPaths(op);
        List<String> relative = new ArrayList<>();
        OperationDescriptor.Body body = op.body();
        if (body != null && body.arg() == null) {
            for (String p : paths) {
                List<String> segs = Json.splitPath(p);
                if (segs.isEmpty()) {
                    continue;
                }
                for (OperationDescriptor.MergedField f : body.fields()) {
                    if (f.arg().equals(segs.get(0))) {
                        String rest = String.join(".", segs.subList(1, segs.size()));
                        relative.add(rest.isEmpty() ? f.wire() : f.wire() + "." + rest);
                        break;
                    }
                }
            }
        } else if (body != null) {
            for (String p : paths) {
                if (p.equals(body.arg())) {
                    relative.add("");
                } else if (p.startsWith(body.arg() + ".")) {
                    relative.add(p.substring(body.arg().length() + 1));
                }
            }
        } else if (op.rpc() != null) {
            relative.addAll(paths);
        }
        if (op.rpc() == null) {
            return relative;
        }
        List<String> out = new ArrayList<>();
        for (String p : relative) {
            out.add(p.isEmpty() ? op.rpc().paramsField() : op.rpc().paramsField() + "." + p);
        }
        return out;
    }

    /** Every string or number found at a path, through arrays. */
    static void valuesAt(Object v, List<String> segments, List<String> out, int depth) {
        if (depth > 64) {
            return;
        }
        if (v instanceof List<?>) {
            for (Object item : (List<?>) v) {
                valuesAt(item, segments, out, depth + 1);
            }
            return;
        }
        if (segments.isEmpty()) {
            if (v instanceof String) {
                out.add((String) v);
            } else if (Json.isNumber(v)) {
                out.add(Json.numberText(v));
            } else if (v instanceof JsonObject) {
                for (Object item : ((JsonObject) v).asMap().values()) {
                    valuesAt(item, List.of(), out, depth + 1);
                }
            }
            return;
        }
        if (v instanceof JsonObject && ((JsonObject) v).has(segments.get(0))) {
            valuesAt(((JsonObject) v).get(segments.get(0)), segments.subList(1, segments.size()), out, depth + 1);
        }
    }

    /**
     * The arguments with each parameter's value also under its wire name, so references
     * written against the API find arguments whose generated name differs.
     */
    static JsonObject withWireNames(OperationDescriptor op, JsonObject args) {
        JsonObject out = args.copy();
        for (OperationDescriptor.Param p : op.params()) {
            if (args.has(p.name()) && !out.has(p.wire())) {
                out.set(p.wire(), args.get(p.name()));
            }
        }
        return out;
    }

    /** A path whose leading parameter wire name is rewritten to its argument name. */
    static List<String> argPath(OperationDescriptor op, List<String> path) {
        if (path.isEmpty()) {
            return path;
        }
        for (OperationDescriptor.Param p : op.params()) {
            if (p.wire().equals(path.get(0)) && !p.name().equals(path.get(0))) {
                List<String> out = new ArrayList<>();
                out.add(p.name());
                out.addAll(path.subList(1, path.size()));
                return out;
            }
        }
        return path;
    }

    /** The argument name of a parameter key given by name or wire. */
    static String argName(OperationDescriptor op, String key) {
        if (key == null) {
            return null;
        }
        for (OperationDescriptor.Param p : op.params()) {
            if (p.name().equals(key)) {
                return p.name();
            }
        }
        for (OperationDescriptor.Param p : op.params()) {
            if (p.wire().equals(key)) {
                return p.name();
            }
        }
        return key;
    }

    private static final Pattern PLACEHOLDER_REF = Pattern.compile("\\{([^{}]+)\\}");

    /** {field} references filled from the arguments; sensitive ones redacted, unset ones <unset>. */
    static String interpolate(String template, JsonObject args, OperationDescriptor op) {
        Matcher m = PLACEHOLDER_REF.matcher(template);
        StringBuilder b = new StringBuilder();
        while (m.find()) {
            String inner = m.group(1).trim();
            List<String> segments = argPath(op, List.of(inner.split("\\.", -1)));
            Object value = Json.at(args, segments);
            String replacement;
            if (value == Json.ABSENT || value == null) {
                replacement = "<unset>";
            } else if (sensitiveArg(op, new ArrayList<Object>(segments))) {
                replacement = Text.REDACTED;
            } else {
                Object shown = Text.envelopeValue(value, false);
                replacement = shown instanceof String ? (String) shown : Json.write(shown);
            }
            m.appendReplacement(b, Matcher.quoteReplacement(replacement));
        }
        m.appendTail(b);
        return b.toString();
    }

    /** The placeholders of args in a preview URL as written instead of percent-encoded. */
    static String unescapePlaceholders(String url, Object args) {
        List<String> found = new ArrayList<>();
        Expr.placeholders(args, found, 0);
        Set<String> seen = new HashSet<>();
        for (String text : found) {
            if (seen.add(text)) {
                url = url.replace(Text.encodeComponent(text), text);
            }
        }
        return url;
    }

    /** JSON for remediation text, cut to 80 characters. */
    static String shortJson(Object v) {
        String text = Json.write(v);
        int[] cps = text.codePoints().toArray();
        if (cps.length > 80) {
            return new String(cps, 0, 77) + "...";
        }
        return text;
    }

    /** " with name value, ..." for remediation text. */
    static String withArguments(Object args) {
        if (!(args instanceof JsonObject) || ((JsonObject) args).isEmpty()) {
            return "";
        }
        JsonObject o = (JsonObject) args;
        List<String> shown = new ArrayList<>();
        for (String k : o.keys()) {
            shown.add(k + " " + shortJson(o.get(k)));
        }
        return " with " + String.join(", ", shown);
    }

    /**
     * " (name value, ...)": the scalar, non-sensitive arguments a call was made with (at most
     * four, by wire name).
     */
    static String sentArguments(OperationDescriptor op, JsonObject args) {
        List<String> shown = new ArrayList<>();
        for (String k : args.keys()) {
            if (shown.size() == 4) {
                break;
            }
            Object v = args.get(k);
            if (!(v instanceof String || Json.isNumber(v) || v instanceof Boolean)) {
                continue;
            }
            if (sensitiveArg(op, List.of(k))) {
                continue;
            }
            String wire = k;
            for (OperationDescriptor.Param p : op.params()) {
                if (p.name().equals(k)) {
                    wire = p.wire();
                    break;
                }
            }
            shown.add(wire + " " + shortJson(v));
        }
        return shown.isEmpty() ? "" : " (" + String.join(", ", shown) + ")";
    }

    /** A finite number within [min, max], else the fallback; ABSENT is the fallback. */
    static double bounded(Object v, double fallback, double min, double max) {
        if (!Json.isNumber(v)) {
            return fallback;
        }
        double f = Json.asDouble(v);
        if (Double.isNaN(f) || Double.isInfinite(f)) {
            return fallback;
        }
        return Math.max(min, Math.min(max, f));
    }
}
