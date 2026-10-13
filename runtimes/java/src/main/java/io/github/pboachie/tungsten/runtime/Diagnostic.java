// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.List;

/**
 * The error envelope. It serializes ({@link #toJson}) to exactly the fourteen members of
 * specs/envelope.schema.json, in order, every member present (null when it has no value),
 * identical across the runtimes.
 *
 * @param category the error category
 * @param operation the operation id or macro name
 * @param httpStatus the HTTP status, or null
 * @param code the API error code, or null
 * @param failedParameter the argument, option or response path at fault, or null
 * @param receivedValue the offending value (redacted when sensitive, strings cut to 200
 *     characters), or null
 * @param expected what was expected, or null
 * @param remediation what to do
 * @param retryable whether and how the call may be repeated
 * @param retryAfterMs the delay the server asked for, or null
 * @param nextAction the next call to make, or null
 * @param requestId the server's request id, or null
 * @param attempts the number of attempts made
 */
public record Diagnostic(
        Category category,
        String operation,
        Integer httpStatus,
        String code,
        String failedParameter,
        Object receivedValue,
        String expected,
        String remediation,
        Retryable retryable,
        Long retryAfterMs,
        String nextAction,
        String requestId,
        int attempts) {

    /** The envelope as a JSON object in the documented member order. */
    public JsonObject toJson() {
        JsonObject o = new JsonObject();
        o.set("status", "error");
        o.set("category", category.name());
        o.set("operation", operation);
        o.set("http_status", httpStatus == null ? null : (long) (int) httpStatus);
        o.set("code", code);
        o.set("failed_parameter", failedParameter);
        o.set("received_value", receivedValue);
        o.set("expected", expected);
        o.set("remediation", remediation);
        o.set("retryable", retryable.wire());
        o.set("retry_after_ms", retryAfterMs);
        o.set("next_action", nextAction);
        o.set("request_id", requestId);
        o.set("trace", JsonObject.of("attempts", (long) attempts));
        return o;
    }

    /** The same envelope for another operation (a macro). */
    public Diagnostic withOperation(String name) {
        return new Diagnostic(category, name, httpStatus, code, failedParameter, receivedValue,
                expected, remediation, retryable, retryAfterMs, nextAction, requestId, attempts);
    }

    /** The same envelope with another remediation, retryable and next action. */
    public Diagnostic withAdvice(String text, Retryable r, String next) {
        return new Diagnostic(category, operation, httpStatus, code, failedParameter,
                receivedValue, expected, text, r, retryAfterMs, next, requestId, attempts);
    }

    /** Every occurrence of the secrets replaced in the text members: a last line of defence. */
    Diagnostic scrubbed(List<String> secrets) {
        if (secrets.isEmpty()) {
            return this;
        }
        return new Diagnostic(category, operation, httpStatus, Text.scrub(code, secrets),
                Text.scrub(failedParameter, secrets), Text.scrubValue(receivedValue, secrets),
                Text.scrub(expected, secrets), Text.scrub(remediation, secrets), retryable,
                retryAfterMs, Text.scrub(nextAction, secrets), Text.scrub(requestId, secrets),
                attempts);
    }

    /** "category in operation: remediation". */
    @Override
    public String toString() {
        return category + " in " + operation + ": " + remediation;
    }
}
