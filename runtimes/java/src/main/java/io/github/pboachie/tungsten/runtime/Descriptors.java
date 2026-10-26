// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.io.IOException;
import java.io.InputStream;
import java.io.UncheckedIOException;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

/**
 * The SDK descriptor document v1 ({@code tungsten-sdk-descriptors/1}): the API, every callable
 * operation by id and every macro by name, with their schemas in the runtime schema form. A
 * generated SDK ships it as the resource {@code tungsten-descriptors.json} and loads it once.
 */
public final class Descriptors {
    /** The format this runtime reads. */
    public static final String FORMAT = "tungsten-sdk-descriptors/1";

    private final String target;
    private final ApiDescriptor api;
    private final Map<String, OperationDescriptor> operations;
    private final Map<String, MacroDescriptor> macros;

    private Descriptors(
            String target,
            ApiDescriptor api,
            Map<String, OperationDescriptor> operations,
            Map<String, MacroDescriptor> macros) {
        this.target = target;
        this.api = api;
        this.operations = Collections.unmodifiableMap(operations);
        this.macros = Collections.unmodifiableMap(macros);
    }

    /**
     * Reads a document.
     *
     * @throws IllegalArgumentException when the stream does not hold a v1 document
     * @throws UncheckedIOException when the stream cannot be read
     */
    public static Descriptors load(InputStream in) {
        if (in == null) {
            throw new IllegalArgumentException("the descriptor document is missing");
        }
        try (InputStream stream = in) {
            return parse(Text.decodeUtf8(stream.readAllBytes()));
        } catch (IOException e) {
            throw new UncheckedIOException("the descriptor document could not be read", e);
        }
    }

    /**
     * Reads a document from its text.
     *
     * @throws IllegalArgumentException when the text is not a v1 document
     */
    public static Descriptors parse(String text) {
        Object doc;
        try {
            doc = Json.parse(text);
        } catch (Json.JsonException e) {
            throw new IllegalArgumentException("the descriptor document is not JSON: " + e.getMessage(), e);
        }
        if (!(doc instanceof JsonObject) || !FORMAT.equals(((JsonObject) doc).get("format"))) {
            throw new IllegalArgumentException("not a " + FORMAT + " document");
        }
        JsonObject d = (JsonObject) doc;
        Map<String, Schema> defs = Schema.defs(d.get("defs"));
        ApiDescriptor api = api(obj(d.get("api")));
        Map<String, OperationDescriptor> ops = new TreeMap<>();
        JsonObject opsJson = obj(d.get("operations"));
        for (String id : opsJson.keys()) {
            ops.put(id, operation(obj(opsJson.get(id)), defs));
        }
        Map<String, MacroDescriptor> macros = new TreeMap<>();
        JsonObject macrosJson = obj(d.get("macros"));
        for (String name : macrosJson.keys()) {
            macros.put(name, macro(obj(macrosJson.get(name))));
        }
        return new Descriptors(str(d.get("target")), api, ops, macros);
    }

    /** The target the names are spelled for. */
    public String target() {
        return target;
    }

    /** The API. */
    public ApiDescriptor api() {
        return api;
    }

    /** Every callable operation by id, sorted by id. */
    public Map<String, OperationDescriptor> operations() {
        return operations;
    }

    /** Every macro by name, sorted by name. */
    public Map<String, MacroDescriptor> macros() {
        return macros;
    }

    /**
     * The operation with this id.
     *
     * @throws IllegalArgumentException when the document has none
     */
    public OperationDescriptor operation(String id) {
        OperationDescriptor op = operations.get(id);
        if (op == null) {
            throw new IllegalArgumentException("the descriptor document has no operation " + id);
        }
        return op;
    }

    /**
     * The macro with this name.
     *
     * @throws IllegalArgumentException when the document has none
     */
    public MacroDescriptor macro(String name) {
        MacroDescriptor m = macros.get(name);
        if (m == null) {
            throw new IllegalArgumentException("the descriptor document has no macro " + name);
        }
        return m;
    }

    // ------------------------------------------------------------ readers

    private static JsonObject obj(Object v) {
        return v instanceof JsonObject ? (JsonObject) v : new JsonObject();
    }

    private static String str(Object v) {
        return v instanceof String ? (String) v : null;
    }

    private static String str(Object v, String fallback) {
        return v instanceof String ? (String) v : fallback;
    }

    private static boolean bool(Object v) {
        return Boolean.TRUE.equals(v);
    }

    private static Long lng(Object v) {
        return v instanceof Long ? (Long) v : null;
    }

    private static List<String> strings(Object v) {
        List<String> out = new ArrayList<>();
        if (v instanceof List<?>) {
            for (Object item : (List<?>) v) {
                if (item instanceof String) {
                    out.add((String) item);
                }
            }
        }
        return Collections.unmodifiableList(out);
    }

    private static List<JsonObject> objects(Object v) {
        List<JsonObject> out = new ArrayList<>();
        if (v instanceof List<?>) {
            for (Object item : (List<?>) v) {
                if (item instanceof JsonObject) {
                    out.add((JsonObject) item);
                }
            }
        }
        return out;
    }

    private static OperationDescriptor.Remediation remediation(JsonObject o) {
        return new OperationDescriptor.Remediation(
                Category.fromWire(str(o.get("category"))),
                str(o.get("text")),
                Retryable.fromWire(str(o.get("retryable"))),
                str(o.get("next_action")));
    }

    private static Map<String, OperationDescriptor.Remediation> remediations(Object v) {
        Map<String, OperationDescriptor.Remediation> out = new LinkedHashMap<>();
        JsonObject o = obj(v);
        for (String k : o.keys()) {
            out.put(k, remediation(obj(o.get(k))));
        }
        return Collections.unmodifiableMap(out);
    }

    private static RetryOptions retry(JsonObject o) {
        RetryOptions.Builder b = RetryOptions.builder();
        if (o.get("max") instanceof Long) {
            b.max((int) Math.min(Integer.MAX_VALUE, (Long) o.get("max")));
        }
        if (o.get("base_ms") instanceof Long) {
            b.baseMs((Long) o.get("base_ms"));
        }
        if (o.get("max_ms") instanceof Long) {
            b.maxMs((Long) o.get("max_ms"));
        }
        if (o.get("jitter") instanceof String) {
            b.jitter(RetryOptions.Jitter.fromWire((String) o.get("jitter")));
        }
        if (o.get("honor_retry_after") instanceof Boolean) {
            b.honorRetryAfter((Boolean) o.get("honor_retry_after"));
        }
        return b.build();
    }

    private static ApiDescriptor api(JsonObject o) {
        List<ApiDescriptor.AuthScheme> auth = new ArrayList<>();
        for (JsonObject s : objects(o.get("auth"))) {
            ApiDescriptor.AuthorizationCode code = null;
            if (s.get("authorization_code") instanceof JsonObject) {
                JsonObject c = (JsonObject) s.get("authorization_code");
                code = new ApiDescriptor.AuthorizationCode(
                        str(c.get("authorization_url")), str(c.get("token_url")),
                        str(c.get("refresh_url")), strings(c.get("scopes")));
            }
            List<ApiDescriptor.CompositePart> parts = new ArrayList<>();
            for (JsonObject p : objects(s.get("parts"))) {
                parts.add(new ApiDescriptor.CompositePart(
                        str(p.get("kind"), ""), str(p.get("name")), str(p.get("equals_cookie")),
                        str(p.get("from_config")), bool(p.get("mutation_only")), str(p.get("prefix"))));
            }
            auth.add(new ApiDescriptor.AuthScheme(
                    str(s.get("kind"), ""), str(s.get("name"), ""), str(s.get("in")),
                    str(s.get("wire")), str(s.get("prefix")), str(s.get("token_url")),
                    strings(s.get("scopes")), code, strings(s.get("satisfies")),
                    Collections.unmodifiableList(parts)));
        }
        List<Integer> ambiguous = new ArrayList<>();
        if (o.get("ambiguous_statuses") instanceof List<?>) {
            for (Object n : (List<?>) o.get("ambiguous_statuses")) {
                if (n instanceof Long) {
                    ambiguous.add((int) (long) (Long) n);
                }
            }
        }
        List<ApiDescriptor.NonJson> nonJson = new ArrayList<>();
        for (JsonObject n : objects(o.get("non_json"))) {
            Category c = Category.fromWire(str(n.get("category")));
            Retryable r = Retryable.fromWire(str(n.get("retryable")));
            Long status = lng(n.get("status"));
            if (c == null || status == null) {
                continue;
            }
            nonJson.add(new ApiDescriptor.NonJson(
                    (int) (long) status, str(n.get("media"), "none"), c,
                    r == null ? c.defaultRetryable() : r, str(n.get("text"))));
        }
        Map<String, String> gates = new LinkedHashMap<>();
        JsonObject g = obj(o.get("gates"));
        for (String k : g.keys()) {
            gates.put(k, String.valueOf(g.get(k)));
        }
        JsonObject retries = obj(o.get("retries"));
        return new ApiDescriptor(
                str(o.get("name"), "api"),
                str(o.get("version"), "0"),
                str(o.get("tungsten_version"), RuntimeIdentity.VERSION),
                strings(o.get("servers")),
                Collections.unmodifiableList(auth),
                remediations(o.get("error_codes")),
                Collections.unmodifiableList(ambiguous),
                Collections.unmodifiableList(nonJson),
                Collections.unmodifiableMap(gates),
                retry(obj(retries.get("read_only"))),
                retry(obj(retries.get("mutating"))), RedirectPolicy.parse(o.get("redirects")));
    }

    private static OperationDescriptor operation(JsonObject o, Map<String, Schema> defs) {
        JsonObject agentJson = obj(o.get("agent"));
        List<String> sensitiveRequest = strings(agentJson.get("sensitive_request_fields"));
        List<OperationDescriptor.Param> params = new ArrayList<>();
        for (JsonObject p : objects(o.get("params"))) {
            String key = str(p.get("key"), str(p.get("name"), ""));
            params.add(new OperationDescriptor.Param(
                    key, str(p.get("wire"), ""), str(p.get("in"), "query"), bool(p.get("required")),
                    str(p.get("style"), "form"), bool(p.get("explode")), str(p.get("role"), "plain"),
                    str(p.get("constant")), sensitiveRequest.contains(key)));
        }
        OperationDescriptor.Body body = null;
        if (o.get("body") instanceof JsonObject) {
            JsonObject b = (JsonObject) o.get("body");
            JsonObject shape = obj(b.get("shape"));
            List<OperationDescriptor.MergedField> fields = new ArrayList<>();
            String arg = null;
            if ("arg".equals(shape.get("kind"))) {
                arg = str(shape.get("arg"), "body");
            } else {
                for (JsonObject f : objects(shape.get("fields"))) {
                    fields.add(new OperationDescriptor.MergedField(
                            str(f.get("key"), ""), str(f.get("wire"), "")));
                }
            }
            body = new OperationDescriptor.Body(
                    str(b.get("media_type"), "application/json"), str(b.get("encoding"), "json"),
                    bool(b.get("required")), Collections.unmodifiableList(fields), arg);
        }
        List<OperationDescriptor.Response> responses = new ArrayList<>();
        for (JsonObject r : objects(o.get("responses"))) {
            Object status = r.get("status");
            String kind;
            int code;
            if (status instanceof Long) {
                kind = "exact";
                code = (int) (long) (Long) status;
            } else if ("default".equals(status)) {
                kind = "default";
                code = 0;
            } else if (status instanceof String && ((String) status).matches("[1-5]XX")) {
                kind = "class";
                code = ((String) status).charAt(0) - '0';
            } else {
                continue;
            }
            responses.add(new OperationDescriptor.Response(
                    kind, code, str(r.get("kind"), "success"), str(r.get("media_type"))));
        }
        List<List<String>> security = new ArrayList<>();
        if (o.get("security") instanceof List<?>) {
            for (Object alt : (List<?>) o.get("security")) {
                security.add(strings(alt));
            }
        }
        OperationDescriptor.Pagination pagination = null;
        if (o.get("pagination") instanceof JsonObject) {
            JsonObject p = (JsonObject) o.get("pagination");
            pagination = new OperationDescriptor.Pagination(
                    str(p.get("style"), ""), str(p.get("request_param")), str(p.get("response_field")),
                    str(p.get("items_field"), ""), str(p.get("page_size_param")),
                    str(p.get("has_more_field")), str(p.get("cursor_item_field")),
                    str(p.get("offset_param")), str(p.get("limit_param")), str(p.get("page_param")),
                    str(p.get("size_param")));
        }
        OperationDescriptor.Rpc rpc = null;
        if (o.get("rpc") instanceof JsonObject) {
            JsonObject r = (JsonObject) o.get("rpc");
            rpc = new OperationDescriptor.Rpc(
                    str(r.get("field"), ""), str(r.get("value"), ""), str(r.get("params_field"), ""),
                    r.get("constants") instanceof JsonObject ? (JsonObject) r.get("constants") : null);
        }
        OperationDescriptor.Gate gate = null;
        JsonObject status = obj(o.get("status"));
        if ("gated".equals(status.get("kind")) && status.get("disabled_status") instanceof Long) {
            gate = new OperationDescriptor.Gate(
                    str(status.get("env_var"), ""), (int) (long) (Long) status.get("disabled_status"));
        }
        OperationDescriptor.Stream stream = null;
        if (o.get("stream") instanceof JsonObject) {
            JsonObject s = (JsonObject) o.get("stream");
            stream = new OperationDescriptor.Stream(
                    Schema.read(s.get("event"), defs), str(s.get("done")), str(s.get("flag")));
        }
        return new OperationDescriptor(
                str(o.get("id"), ""),
                str(o.get("method"), "GET"),
                str(o.get("path"), "/"),
                Collections.unmodifiableList(params),
                body,
                Collections.unmodifiableList(responses),
                Collections.unmodifiableList(security),
                pagination,
                rpc,
                str(o.get("error_code_field")),
                gate,
                agent(agentJson, sensitiveRequest),
                Schema.read(o.get("request"), defs),
                Schema.read(o.get("response"), defs),
                Schema.read(o.get("page_item"), defs),
                stream,
                str(o.get("summary")));
    }

    private static OperationDescriptor.Agent agent(JsonObject a, List<String> sensitiveRequest) {
        JsonObject i = obj(a.get("idempotency"));
        OperationDescriptor.Idempotency idempotency = new OperationDescriptor.Idempotency(
                str(i.get("policy"), "none"), str(i.get("header")), str(i.get("format")),
                bool(i.get("persist_required")), str(i.get("note")));
        JsonObject p = obj(a.get("preview"));
        OperationDescriptor.Preview preview = new OperationDescriptor.Preview(
                str(p.get("mode"), "local"), str(p.get("header")), str(p.get("value")),
                str(p.get("operation")));
        OperationDescriptor.Confirmation confirmation = null;
        if (a.get("confirmation") instanceof JsonObject) {
            JsonObject c = (JsonObject) a.get("confirmation");
            confirmation = new OperationDescriptor.Confirmation(
                    strings(c.get("summary_fields")), str(c.get("message")));
        }
        OperationDescriptor.Verify verify = null;
        if (a.get("verify") instanceof JsonObject) {
            JsonObject v = (JsonObject) a.get("verify");
            verify = new OperationDescriptor.Verify(
                    str(v.get("operation"), ""), v.get("args"), v.get("expect"), v.get("terminal"),
                    lng(v.get("poll_interval_ms")), lng(v.get("poll_budget_ms")));
        }
        return new OperationDescriptor.Agent(
                Safety.fromWire(str(a.get("safety"), "mutating")),
                idempotency,
                preview,
                confirmation,
                verify,
                remediations(a.get("remediation")),
                str(a.get("remediation_note")),
                strings(a.get("sensitive_response_fields")),
                sensitiveRequest,
                bool(a.get("shown_once")),
                importPolicy(a.get("import_policy")));
    }

    private static OperationDescriptor.ImportPolicy importPolicy(Object raw) {
        if (!(raw instanceof JsonObject)) {
            return null;
        }
        JsonObject p = (JsonObject) raw;
        // Only a literal true counts as reviewed; anything else fails closed.
        return new OperationDescriptor.ImportPolicy(
                strings(p.get("allowed_origins")),
                Safety.fromWire(str(p.get("inferred_safety"), "irreversible")),
                Boolean.TRUE.equals(p.get("reviewed")),
                str(p.get("source"), ""));
    }

    private static MacroDescriptor macro(JsonObject m) {
        List<MacroDescriptor.Step> steps = new ArrayList<>();
        for (JsonObject s : objects(m.get("steps"))) {
            steps.add(new MacroDescriptor.Step(
                    str(s.get("kind"), "call"), str(s.get("operation"), ""), s.get("args"),
                    str(s.get("as")), s.get("until"), lng(s.get("interval_ms")), s.get("budget_ms"),
                    lng(s.get("max_pages"))));
        }
        JsonObject input = obj(m.get("input"));
        List<MacroDescriptor.InputField> fields = new ArrayList<>();
        for (JsonObject f : objects(input.get("fields"))) {
            fields.add(new MacroDescriptor.InputField(
                    str(f.get("key"), ""), str(f.get("name"), ""), bool(f.get("required"))));
        }
        JsonObject add = input.get("add") instanceof JsonObject ? (JsonObject) input.get("add") : null;
        if (add != null) {
            JsonObject sorted = new JsonObject();
            List<String> keys = add.keys();
            Collections.sort(keys);
            for (String k : keys) {
                sorted.set(k, add.get(k));
            }
            add = sorted;
        }
        return new MacroDescriptor(
                str(m.get("name"), ""),
                str(m.get("summary"), ""),
                Safety.fromWire(str(m.get("safety"), "read_only")),
                Collections.unmodifiableList(steps),
                m.get("output"),
                new MacroDescriptor.Input(str(input.get("extends")), add, Collections.unmodifiableList(fields)),
                strings(m.get("sensitive_response_fields")),
                bool(m.get("shown_once")),
                str(m.get("cluster")));
    }
}
