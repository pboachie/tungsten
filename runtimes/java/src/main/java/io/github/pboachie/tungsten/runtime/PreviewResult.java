// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

/**
 * What preview returns.
 *
 * @param operation the operation id or macro name
 * @param safety the tier
 * @param request the request (for a macro: the first step's)
 * @param effects the side effects in words
 * @param confirmationToken the token of a mutating operation, bound to the exact arguments, or
 *     null
 * @param expiresInMs the token's lifetime, or null
 * @param serverPreview the server's answer for header and endpoint previews
 * @param hasServerPreview whether the server answered with a body
 * @param steps the steps of a macro preview, or null
 */
public record PreviewResult(
        String operation,
        Safety safety,
        RenderedRequest request,
        List<String> effects,
        String confirmationToken,
        Long expiresInMs,
        Object serverPreview,
        boolean hasServerPreview,
        List<StepPreview> steps) {

    /**
     * A request rendered without sending it; secrets show as {@code <redacted>}.
     *
     * @param method the method
     * @param url the URL
     * @param headers the headers
     * @param body the body as JSON when it is JSON, a text rendering otherwise, or null
     */
    public record RenderedRequest(String method, String url, Map<String, String> headers, Object body) {
        /** The request as JSON (headers sorted by name). */
        public JsonObject toJson() {
            JsonObject h = new JsonObject();
            for (Map.Entry<String, String> e : new TreeMap<>(headers).entrySet()) {
                h.set(e.getKey(), e.getValue());
            }
            return JsonObject.of("method", method, "url", url, "headers", h, "body", body);
        }
    }

    /**
     * One step of a macro preview.
     *
     * @param step the 1-based position
     * @param kind call, poll or paginate
     * @param operation the operation id
     * @param as the name of its result, or null
     * @param safety the step's tier
     * @param request the request, or null when it depends on an earlier result
     * @param effects the step's effects
     */
    public record StepPreview(
            int step, String kind, String operation, String as, Safety safety, RenderedRequest request, List<String> effects) {}

    /** The preview as the JSON the other runtimes hand out. */
    public JsonObject toJson() {
        JsonObject out = JsonObject.of("operation", operation, "safety", safety.wire(), "request", request.toJson());
        out.set("effects", new ArrayList<Object>(effects));
        out.set("confirmation_token", confirmationToken);
        out.set("expires_in_ms", confirmationToken != null ? expiresInMs : null);
        if (hasServerPreview) {
            out.set("server_preview", serverPreview);
        }
        if (steps != null) {
            List<Object> list = new ArrayList<>();
            for (StepPreview s : steps) {
                list.add(JsonObject.of("step", (long) s.step(), "kind", s.kind(), "operation", s.operation(), "as", s.as(),
                        "safety", s.safety().wire(), "request", s.request() == null ? null : s.request().toJson(),
                        "effects", new ArrayList<Object>(s.effects())));
            }
            out.set("steps", list);
        }
        return out;
    }
}
