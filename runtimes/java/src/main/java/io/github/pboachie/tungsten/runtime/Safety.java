// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/** The tier of an operation. */
public enum Safety {
    READ_ONLY("read_only"),
    MUTATING("mutating"),
    DESTRUCTIVE("destructive"),
    IRREVERSIBLE("irreversible");

    private final String wire;

    Safety(String wire) {
        this.wire = wire;
    }

    /** The wire name (read_only, ...). */
    public String wire() {
        return wire;
    }

    /** The tier with this wire name; mutating for an unknown one. */
    public static Safety fromWire(String name) {
        for (Safety s : values()) {
            if (s.wire.equals(name)) {
                return s;
            }
        }
        return MUTATING;
    }

    int rank() {
        return ordinal();
    }
}
