// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.Map;

/**
 * Observes the request lifecycle. Hooks are synchronous, must not block, and see secrets only as
 * {@code <redacted>}; a hook that throws never changes the call.
 */
public interface Middleware {
    /** Before every attempt. */
    default void onRequest(RequestContext ctx) {}

    /** After every answer. */
    default void onResponse(RequestContext ctx, ResponseContext response) {}

    /** Before a retry, with the reason. */
    default void onRetry(RequestContext ctx, Diagnostic reason) {}

    /**
     * What middleware sees of one attempt.
     *
     * @param operation the operation id
     * @param attempt the attempt number (1-based)
     * @param method the HTTP method
     * @param url the URL (secret query parameters redacted)
     * @param headers the headers (secrets redacted)
     * @param body the body as JSON when it is JSON, a text rendering otherwise
     * @param hasBody whether the request has a body
     */
    record RequestContext(
            String operation,
            int attempt,
            String method,
            String url,
            Map<String, String> headers,
            Object body,
            boolean hasBody) {}

    /**
     * What middleware sees of an answer.
     *
     * @param status the status
     * @param headers the headers, lower-cased names
     */
    record ResponseContext(int status, Map<String, String> headers) {}
}
