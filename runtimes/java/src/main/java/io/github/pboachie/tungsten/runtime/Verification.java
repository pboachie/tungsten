// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * The result of an operation's verification hook.
 *
 * @param checked whether the check ran
 * @param passed whether expect held
 * @param observed the body the check read (sensitive fields redacted)
 * @param hasObserved whether the check read a body
 * @param timedOut whether polling for the terminal state ran out of budget
 * @param error why the check could not run, or null
 */
public record Verification(
        boolean checked,
        boolean passed,
        Object observed,
        boolean hasObserved,
        boolean timedOut,
        Diagnostic error) {

    static Verification unchecked(Diagnostic error) {
        return new Verification(false, false, null, false, false, error);
    }

    /** The verification as the TypeScript runtime shapes it. */
    public JsonObject toJson() {
        JsonObject o = JsonObject.of("checked", checked, "passed", passed, "observed",
                hasObserved ? observed : null);
        if (timedOut) {
            o.set("timed_out", true);
        }
        if (error != null) {
            o.set("error", error.toJson());
        }
        return o;
    }
}
