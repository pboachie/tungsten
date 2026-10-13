// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/** Builds an envelope with every member present (generic remediation and retryable by default). */
final class Diag {
    private final RuntimeIdentity identity;
    private final Category category;
    private final String operation;
    private Integer httpStatus;
    private String code;
    private String failedParameter;
    private Object receivedValue;
    private String expected;
    private String remediation;
    private Retryable retryable;
    private Long retryAfterMs;
    private String nextAction;
    private String requestId;
    private int attempts;

    Diag(RuntimeIdentity identity, String operation, Category category) {
        this.identity = identity;
        this.operation = operation;
        this.category = category;
    }

    /** The remediation used when no manifest entry applies. */
    static String generic(RuntimeIdentity id, Category c) {
        switch (c) {
            case VALIDATION_FAILED:
                return "The server rejected the arguments. Fix the parameter the API error names and call again; the same request fails the same way.";
            case MALFORMED_REQUEST:
                return "The server could not parse the request. Check the format of path and query parameters (for example canonical UUIDs); do not retry unchanged.";
            case REQUEST_TOO_LARGE:
                return "The request body is larger than the server accepts. Send a smaller body; do not retry unchanged.";
            case AUTH_FAILED:
                return "The credential is missing, expired, revoked or lacks permission. Do not retry; fix the credential configured in ClientOptions.auth.";
            case NOT_FOUND:
                return "The resource does not exist or is not visible to this credential. Check the identifiers; do not retry unchanged.";
            case CONFLICT:
                return "The request conflicts with the resource's current state. Read the current state before deciding what to do; do not retry unchanged.";
            case PRECONDITION_FAILED:
                return "A precondition of this operation is not met (account, billing or resource state). Resolve it first; do not retry unchanged.";
            case RATE_LIMITED:
                return "Rate limited. Wait retry_after_ms (or a few seconds when it is null) before calling again.";
            case UPSTREAM_UNAVAILABLE:
                return "The service is temporarily unavailable. Wait, then call again.";
            case OUTCOME_UNKNOWN:
                return "The server may or may not have applied this call; do not retry it blindly. Check whether it took effect before doing anything else.";
            case TRANSPORT_FAILED:
                return "The request could not be delivered (DNS, TLS or connection failure), so the server did not receive it. Check "
                        + id.baseUrl()
                        + " and network access, then call again.";
            case CONFIRMATION_REQUIRED:
                return "This operation needs confirmation: call preview(...) and pass its confirmation_token.";
            case GATE_DISABLED:
                return "This operation is disabled on this deployment. It is a deployment setting; do not retry.";
            case UNEXPECTED_RESPONSE:
                return "The server's response did not match the API description. Do not retry blindly; report it.";
            default:
                return "The call failed.";
        }
    }

    /** The category of an HTTP error status without a manifest entry. */
    static Category forStatus(int status, boolean jsonBody) {
        switch (status) {
            case 401:
            case 403:
            case 407:
                return Category.AUTH_FAILED;
            case 404:
            case 410:
                return Category.NOT_FOUND;
            case 409:
                return Category.CONFLICT;
            case 402:
            case 412:
            case 428:
                return Category.PRECONDITION_FAILED;
            case 413:
                return Category.REQUEST_TOO_LARGE;
            case 422:
                return Category.VALIDATION_FAILED;
            case 429:
                return Category.RATE_LIMITED;
            case 408:
            case 502:
            case 503:
            case 504:
                return Category.UPSTREAM_UNAVAILABLE;
            default:
                break;
        }
        if (status >= 500 && status <= 599) {
            return Category.UPSTREAM_UNAVAILABLE;
        }
        if (status >= 400 && status <= 499) {
            return jsonBody ? Category.VALIDATION_FAILED : Category.MALFORMED_REQUEST;
        }
        return Category.UNEXPECTED_RESPONSE;
    }

    Diag status(Integer code) {
        httpStatus = code;
        return this;
    }

    Diag code(String value) {
        code = value;
        return this;
    }

    Diag param(String path) {
        failedParameter = path;
        return this;
    }

    Diag received(Object v) {
        receivedValue = v;
        return this;
    }

    Diag expected(String text) {
        expected = text;
        return this;
    }

    Diag remedy(String text) {
        remediation = text;
        return this;
    }

    Diag retry(Retryable r) {
        retryable = r;
        return this;
    }

    Diag retryAfter(Long ms) {
        retryAfterMs = ms;
        return this;
    }

    Diag next(String text) {
        nextAction = text;
        return this;
    }

    Diag requestId(String id) {
        requestId = id;
        return this;
    }

    Diag attempts(int n) {
        attempts = n;
        return this;
    }

    Diagnostic build() {
        return new Diagnostic(
                category,
                operation,
                httpStatus,
                code,
                failedParameter,
                receivedValue,
                expected,
                remediation != null ? remediation : generic(identity, category),
                retryable != null ? retryable : category.defaultRetryable(),
                retryAfterMs,
                nextAction,
                requestId,
                attempts);
    }

    TungstenException fail() {
        return new TungstenException(build());
    }
}
