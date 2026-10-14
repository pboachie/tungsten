// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * Confirms a destructive or irreversible operation: a token from preview ({@link #token}) or,
 * for destructive operations only, {@link #YES}. {@link #NONE} confirms nothing.
 */
public final class Confirm {
    /** Accepts a destructive (never an irreversible) operation without a token. */
    public static final Confirm YES = new Confirm(null, true);

    /** No confirmation. */
    public static final Confirm NONE = new Confirm(null, false);

    private final String token;
    private final boolean yes;

    private Confirm(String token, boolean yes) {
        this.token = token;
        this.yes = yes;
    }

    /** A token from preview, bound to the operation and the exact arguments. */
    public static Confirm token(String token) {
        return token == null || token.isEmpty() ? NONE : new Confirm(token, false);
    }

    /** Whether this is {@link #YES}. */
    public boolean isYes() {
        return yes;
    }

    /** The token, or null. */
    public String tokenValue() {
        return token;
    }

    /** Whether the confirmation carries anything. */
    public boolean isSet() {
        return yes || token != null;
    }

    /** Never shows the token. */
    @Override
    public String toString() {
        if (yes) {
            return "Confirm.YES";
        }
        return token != null ? "Confirm.token(" + Text.REDACTED + ")" : "Confirm.NONE";
    }
}
