// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/** Response decoding and the classification of failures into envelopes. */
final class Classify {
    private Classify() {}

    private static final String[] REQUEST_ID_HEADERS = {
        "x-request-id", "request-id", "x-correlation-id", "x-amzn-requestid", "cf-ray"
    };

    static String requestId(Map<String, String> headers) {
        for (String name : REQUEST_ID_HEADERS) {
            String v = headers.get(name);
            if (v != null && !v.isEmpty()) {
                return Text.takeUnits(v, 200);
            }
        }
        return null;
    }

    /** The media type without parameters, lower-cased; "" without one. */
    static String mediaType(Map<String, String> headers) {
        String v = headers.get("content-type");
        if (v == null) {
            return "";
        }
        return v.split(";", 2)[0].trim().toLowerCase(Locale.ROOT);
    }

    static boolean isJsonMedia(String media) {
        return media.equals("application/json") || media.endsWith("+json") || media.equals("text/json");
    }

    static boolean isJsonlMedia(String media) {
        switch (media) {
            case "application/jsonl":
            case "application/x-jsonl":
            case "application/ndjson":
            case "application/x-ndjson":
                return true;
            default:
                return false;
        }
    }

    static OperationDescriptor.Response match(List<OperationDescriptor.Response> responses, int status) {
        for (OperationDescriptor.Response r : responses) {
            if (r.statusKind().equals("exact") && r.statusCode() == status) {
                return r;
            }
        }
        for (OperationDescriptor.Response r : responses) {
            if (r.statusKind().equals("class") && r.statusCode() == status / 100) {
                return r;
            }
        }
        for (OperationDescriptor.Response r : responses) {
            if (r.statusKind().equals("default")) {
                return r;
            }
        }
        return null;
    }

    /** A decoded response body. */
    static final class Decoded {
        /** Parsed JSON, text as a string, bytes as the tagged binary object. */
        Object value;
        boolean has;
        boolean json;
        boolean invalidJson;
        boolean empty;
        boolean jsonl;
        int badLine;
    }

    static Decoded decode(byte[] data, Map<String, String> headers, String declared) {
        Decoded d = new Decoded();
        String announced = mediaType(headers);
        if (data.length == 0 && !isJsonlMedia(announced)) {
            d.empty = true;
            return d;
        }
        String media = announced.isEmpty() && declared != null ? declared.toLowerCase(Locale.ROOT) : announced;
        String jsonlMedia = announced.isEmpty() ? media.split(";", 2)[0].trim() : announced;
        if (isJsonlMedia(jsonlMedia)) {
            String raw = Text.decodeBody(data);
            List<Object> values = new ArrayList<>();
            String[] lines = raw.split("\n", -1);
            for (int i = 0; i < lines.length; i++) {
                if (lines[i].trim().isEmpty()) {
                    continue;
                }
                try {
                    values.add(Json.parse(lines[i]));
                } catch (Json.JsonException e) {
                    d.value = raw;
                    d.has = true;
                    d.invalidJson = true;
                    d.jsonl = true;
                    d.badLine = i + 1;
                    return d;
                }
            }
            d.value = values;
            d.has = true;
            d.jsonl = true;
            return d;
        }
        if (isJsonMedia(media)) {
            String raw = Text.decodeBody(data);
            d.has = true;
            try {
                d.value = Json.parse(raw);
                d.json = true;
            } catch (Json.JsonException e) {
                d.value = raw;
                d.invalidJson = true;
            }
            return d;
        }
        if (media.isEmpty() || media.startsWith("text/") || media.equals("application/problem+xml")
                || media.endsWith("+xml") || media.equals("application/xml")) {
            String raw = Text.decodeBody(data);
            d.has = true;
            String trimmed = raw.replaceFirst("^[ \\t\\r\\n\\u000B\\f]+", "");
            if (media.isEmpty() && (trimmed.startsWith("[") || trimmed.startsWith("{"))) {
                try {
                    d.value = Json.parse(raw);
                    d.json = true;
                    return d;
                } catch (Json.JsonException e) {
                    // text below
                }
            }
            d.value = raw;
            return d;
        }
        d.value = Binary.of(data).toJson();
        d.has = true;
        return d;
    }

    private static final String[] MESSAGE_FIELDS = {
        "message", "error.message", "detail", "error_description", "title", "error"
    };

    static String serverMessage(Object body) {
        for (String field : MESSAGE_FIELDS) {
            Object v = Json.at(body, field);
            if (v instanceof String) {
                String text = ((String) v).trim();
                if (!text.isEmpty()) {
                    return Text.takeUnits(text, 200);
                }
            }
        }
        return "";
    }

    static String errorCode(OperationDescriptor op, Object body) {
        if (op.errorCodeField() == null || op.errorCodeField().isEmpty()) {
            return null;
        }
        Object v = Json.at(body, op.errorCodeField());
        if (v instanceof String && !((String) v).isEmpty()) {
            return Text.takeUnits((String) v, 200);
        }
        if (Json.isNumber(v)) {
            return Json.numberText(v);
        }
        return null;
    }

    /** How, in words, to find out whether an unprotected mutation took effect. */
    record OutcomeCheck(String call, String shows) {}

    /** Shared by every classification of one call. */
    record Context(
            RuntimeIdentity identity,
            ApiDescriptor api,
            OperationDescriptor op,
            String key,
            boolean hasKey,
            String keyHeader,
            int attempts,
            OutcomeCheck check) {
        Diag diag(Category c) {
            return new Diag(identity, op.id(), c);
        }
    }

    static boolean refersToResponse(Object node, int depth) {
        if (depth > 32) {
            return true;
        }
        if (node instanceof String) {
            String t = (String) node;
            return t.equals("$response") || t.startsWith("$response.");
        }
        if (node instanceof List<?>) {
            for (Object item : (List<?>) node) {
                if (refersToResponse(item, depth + 1)) {
                    return true;
                }
            }
        } else if (node instanceof JsonObject) {
            for (Object item : ((JsonObject) node).asMap().values()) {
                if (refersToResponse(item, depth + 1)) {
                    return true;
                }
            }
        }
        return false;
    }

    /** After a lost answer there is no body, so a hook whose args reference $response cannot run. */
    static boolean verifyCallable(OperationDescriptor.Verify v) {
        return !refersToResponse(v.args(), 0);
    }

    static String nextActionHint(Context ctx) {
        OperationDescriptor op = ctx.op();
        OperationDescriptor.Verify v = op.agent().verify();
        if (v != null && !v.operation().isEmpty() && verifyCallable(v)) {
            String with = "";
            if (v.args() instanceof JsonObject && !((JsonObject) v.args()).isEmpty()) {
                with = " with " + String.join(", ", ((JsonObject) v.args()).keys());
            }
            return "Call " + v.operation() + with + " to check whether " + op.id() + " took effect before doing anything else.";
        }
        if (op.agent().idempotency().policy().equals("content_identity")) {
            return "Call " + op.id() + " again with the identical body; the body is its own identity, so the server answers the original result instead of applying it twice.";
        }
        if (ctx.hasKey()) {
            return "Call " + op.id() + " again with the same " + ctx.keyHeader() + " value and identical arguments; the server answers the original result instead of applying it twice.";
        }
        return null;
    }

    /** The change an operation makes, in words. */
    static String changeOf(OperationDescriptor op) {
        String summary = op.summary() == null ? "" : op.summary().trim();
        if (summary.endsWith(".")) {
            summary = summary.substring(0, summary.length() - 1);
        }
        if (summary.isEmpty()) {
            return "the change " + op.id() + " makes";
        }
        return "the change " + Json.jsString(summary);
    }

    static boolean hasReplayProtection(OperationDescriptor op, boolean hasKey) {
        return op.agent().idempotency().policy().equals("content_identity") || hasKey;
    }

    /**
     * OUTCOME_UNKNOWN for a mutation whose effect cannot be known. Without replay protection a
     * repeat can apply the effect twice, so the envelope never offers one.
     */
    static Diagnostic outcomeUnknown(
            Context ctx, String cause, String nextAction, Integer status, String code, String requestId, Long retryAfter) {
        OperationDescriptor op = ctx.op();
        String rule;
        String hint = null;
        if (op.agent().idempotency().policy().equals("content_identity")) {
            rule = "If you retry, resend the identical bytes only; the body is its own identity.";
        } else if (ctx.hasKey()) {
            rule = "If you retry, reuse the SAME " + ctx.keyHeader() + " value; a new key can apply the effect twice.";
        } else if (ctx.check() != null) {
            OutcomeCheck c = ctx.check();
            rule = "This operation has no idempotency key, so repeating it can apply the effect twice. Before calling "
                    + op.id() + " again, call " + c.call() + " and check " + c.shows() + ": if it does, " + op.id()
                    + " took effect and must not be repeated; call it again only if it did not.";
            hint = "Call " + c.call() + " and check " + c.shows() + "; call " + op.id() + " again only if it did not take effect.";
        } else {
            String change = changeOf(op);
            rule = "This operation has no idempotency key and no registered way to verify it, so " + op.id()
                    + " may have taken effect and a repeat can apply the effect twice: do not retry it blindly. Check the outcome by other means if you can (look for "
                    + change + "); if you cannot, ask whoever owns the task before calling it again.";
            hint = "Do not retry " + op.id() + " blindly: it may have taken effect. Check for " + change
                    + " if you can; call " + op.id() + " again only if it is not there, or after the task owner accepts that it may apply twice.";
        }
        Retryable retryable = hasReplayProtection(op, ctx.hasKey()) ? Retryable.SAME_KEY_ONLY : Retryable.AFTER_REMEDIATION;
        String next = nextAction != null ? nextAction : hint;
        if (next == null) {
            next = nextActionHint(ctx);
        }
        return ctx.diag(Category.OUTCOME_UNKNOWN)
                .remedy(cause + " The server may or may not have applied " + op.id() + ". " + rule)
                .retry(retryable)
                .next(next)
                .status(status)
                .code(code)
                .requestId(requestId)
                .retryAfter(retryAfter)
                .attempts(ctx.attempts())
                .build();
    }

    /**
     * A manifest entry with one of these categories (or retryable never / after_remediation) is a
     * definite answer even for an ambiguous status.
     */
    static boolean definite(Category c, Retryable r) {
        if (r == Retryable.NEVER || r == Retryable.AFTER_REMEDIATION) {
            return true;
        }
        if (c == null) {
            return false;
        }
        switch (c) {
            case VALIDATION_FAILED:
            case MALFORMED_REQUEST:
            case REQUEST_TOO_LARGE:
            case AUTH_FAILED:
            case NOT_FOUND:
            case CONFLICT:
            case PRECONDITION_FAILED:
            case RATE_LIMITED:
            case GATE_DISABLED:
                return true;
            default:
                return false;
        }
    }

    private static String optText(String s) {
        return s == null || s.isEmpty() ? null : s;
    }

    /** Classifies an HTTP error status. */
    static Diagnostic error(Context ctx, int status, Map<String, String> headers, Decoded decoded, Long retryAfter) {
        ApiDescriptor api = ctx.api();
        OperationDescriptor op = ctx.op();
        String requestId = requestId(headers);
        String code = decoded.json ? errorCode(op, decoded.value) : null;
        OperationDescriptor.Gate g = op.gate();
        if (g != null && status == g.disabledStatus() && code == null) {
            String text = api.gates().get(g.envVar());
            if (text == null) {
                text = "This deployment disables " + op.id() + " because " + g.envVar() + " is off (HTTP " + status
                        + "). It is a deployment setting, not a missing resource; do not retry.";
            }
            return ctx.diag(Category.GATE_DISABLED).status(status).requestId(requestId).retryAfter(retryAfter)
                    .attempts(ctx.attempts()).code(code).remedy(text).retry(Retryable.NEVER).build();
        }
        OperationDescriptor.Remediation entry = code == null ? null : op.agent().remediation().get(code);
        if (entry == null && code != null) {
            entry = api.errorCodes().get(code);
        }
        String media = "none";
        if (!decoded.empty) {
            String m = mediaType(headers);
            if (!m.isEmpty()) {
                media = m;
            }
        }
        ApiDescriptor.NonJson nonJson = null;
        if (!decoded.json) {
            for (ApiDescriptor.NonJson n : api.nonJson()) {
                if (n.status() == status && (n.media().equals(media) || (n.media().equals("none") && decoded.empty))) {
                    nonJson = n;
                    break;
                }
            }
        }
        OperationDescriptor.Response declared = match(op.responses(), status);
        boolean ambiguous = api.ambiguousStatuses().contains(status)
                || (declared != null && declared.kind().equals("ambiguous"))
                || (status >= 500 && status <= 599);
        boolean definiteAnswer = false;
        if (entry != null) {
            definiteAnswer = definite(entry.category(), entry.retryable());
        } else if (nonJson != null) {
            definiteAnswer = definite(nonJson.category(), nonJson.retryable());
        }
        if (ambiguous && op.isMutation() && !definiteAnswer) {
            String said = "";
            if (entry != null && entry.text() != null && !entry.text().isEmpty()) {
                said = entry.text();
            } else if (nonJson != null && nonJson.text() != null && !nonJson.text().isEmpty()) {
                said = nonJson.text();
            }
            String cause = "HTTP " + status + " leaves the outcome of this call unknown.";
            if (!said.isEmpty()) {
                cause += " " + said;
            }
            String next = entry != null ? optText(entry.nextAction()) : null;
            return outcomeUnknown(ctx, cause, next, status, code, requestId, retryAfter);
        }
        Category category;
        Retryable retryable = null;
        String text = null;
        String next = null;
        if (entry != null) {
            category = entry.category() != null ? entry.category() : Diag.forStatus(status, decoded.json);
            retryable = entry.retryable();
            text = entry.text();
            next = optText(entry.nextAction());
        } else if (nonJson != null) {
            category = nonJson.category();
            retryable = nonJson.retryable();
            text = nonJson.text();
        } else {
            category = Diag.forStatus(status, decoded.json);
        }
        if (category == Category.OUTCOME_UNKNOWN && !op.isMutation()) {
            category = Category.UPSTREAM_UNAVAILABLE;
            if (retryable == null || retryable == Retryable.SAME_KEY_ONLY) {
                retryable = Retryable.AFTER_DELAY;
            }
        }
        if (text == null || text.isEmpty()) {
            text = Diag.generic(ctx.identity(), category);
            if (decoded.json) {
                String said = serverMessage(decoded.value);
                if (!said.isEmpty()) {
                    text += " Server message: " + Json.jsString(said);
                }
            }
        }
        Diag d = ctx.diag(category).status(status).requestId(requestId).retryAfter(retryAfter)
                .attempts(ctx.attempts()).code(code).remedy(text).next(next);
        if (retryable != null) {
            d.retry(retryable);
        }
        return d.build();
    }
}
