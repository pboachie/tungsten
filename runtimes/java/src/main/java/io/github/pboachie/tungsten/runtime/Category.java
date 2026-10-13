// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/** The closed set of error categories shared by every tungsten runtime. */
public enum Category {
    VALIDATION_FAILED,
    MALFORMED_REQUEST,
    REQUEST_TOO_LARGE,
    AUTH_FAILED,
    NOT_FOUND,
    CONFLICT,
    PRECONDITION_FAILED,
    RATE_LIMITED,
    UPSTREAM_UNAVAILABLE,
    OUTCOME_UNKNOWN,
    TRANSPORT_FAILED,
    CONFIRMATION_REQUIRED,
    GATE_DISABLED,
    UNEXPECTED_RESPONSE;

    /** The category with this wire name, or null. */
    public static Category fromWire(String name) {
        for (Category c : values()) {
            if (c.name().equals(name)) {
                return c;
            }
        }
        return null;
    }

    /** The default retryable of the category. */
    public Retryable defaultRetryable() {
        switch (this) {
            case RATE_LIMITED:
            case UPSTREAM_UNAVAILABLE:
            case TRANSPORT_FAILED:
                return Retryable.AFTER_DELAY;
            case OUTCOME_UNKNOWN:
                return Retryable.SAME_KEY_ONLY;
            default:
                return Retryable.NEVER;
        }
    }
}
