// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/** Redirect authority: client options can only restrict the generated API policy. */
public enum RedirectPolicy {
    /** Follow at most five same-origin redirects on reads. */
    SAME_ORIGIN_READ,
    /** Reject every 3xx response without following or retrying it. */
    NONE;

    static RedirectPolicy parse(Object value) {
        if (value == null || "same_origin_read".equals(value)) return SAME_ORIGIN_READ;
        if ("none".equals(value)) return NONE;
        throw new IllegalArgumentException("redirects must be same_origin_read or none");
    }
}
