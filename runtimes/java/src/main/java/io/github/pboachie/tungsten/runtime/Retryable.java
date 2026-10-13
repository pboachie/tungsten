// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/** Whether and how a failed call may be repeated. */
public enum Retryable {
    NEVER("never"),
    AFTER_DELAY("after_delay"),
    SAME_KEY_ONLY("same_key_only"),
    AFTER_REMEDIATION("after_remediation");

    private final String wire;

    Retryable(String wire) {
        this.wire = wire;
    }

    /** The wire name (after_delay, ...). */
    public String wire() {
        return wire;
    }

    /** The value with this wire name, or null. */
    public static Retryable fromWire(String name) {
        for (Retryable r : values()) {
            if (r.wire.equals(name)) {
                return r;
            }
        }
        return null;
    }
}
