// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * A failed call as an exception: the envelope, plus what the failed call or macro already
 * produced when its effect happened (see {@link Result.Err}). {@link Result#unwrap} throws it.
 */
public final class TungstenException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    private final transient Diagnostic diagnostic;
    private final transient Object partial;
    private final boolean hasPartial;

    /** A failure with the envelope only. */
    public TungstenException(Diagnostic diagnostic) {
        this(diagnostic, null, false);
    }

    /** A failure with what the call produced before it failed. */
    public TungstenException(Diagnostic diagnostic, Object partial, boolean hasPartial) {
        super(diagnostic.toString(), null, false, false);
        this.diagnostic = diagnostic;
        this.partial = partial;
        this.hasPartial = hasPartial;
    }

    /** The envelope. */
    public Diagnostic diagnostic() {
        return diagnostic;
    }

    /** What the call produced before it failed; meaningful when {@link #hasPartial}. */
    public Object partial() {
        return partial;
    }

    /** Whether {@link #partial} holds a value. */
    public boolean hasPartial() {
        return hasPartial;
    }

    /** The failure as a result. */
    public <T> Result<T> toResult() {
        return new Result.Err<>(diagnostic, partial, hasPartial);
    }
}
