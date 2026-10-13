// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * The result of a call: {@link Ok} with the value and the answer's metadata, or {@link Err} with
 * the error envelope. API and transport failures never throw; they are {@code Err} values.
 *
 * @param <T> the value type
 */
public sealed interface Result<T> permits Result.Ok, Result.Err {
    /** Whether the call succeeded. */
    boolean isOk();

    /** The value of an {@link Ok}; throws {@link TungstenException} for an {@link Err}. */
    T unwrap();

    /** The envelope of an {@link Err}, or null. */
    Diagnostic error();

    /**
     * A success.
     *
     * @param value the typed value (null when the response had no body)
     * @param hasValue whether the response had a body
     * @param raw the decoded wire body
     * @param meta the HTTP answer
     * @param verification the verification hook's result, or null
     * @param <T> the value type
     */
    record Ok<T>(T value, boolean hasValue, Object raw, ResponseMeta meta, Verification verification)
            implements Result<T> {
        @Override
        public boolean isOk() {
            return true;
        }

        @Override
        public T unwrap() {
            return value;
        }

        @Override
        public Diagnostic error() {
            return null;
        }
    }

    /**
     * A failure: the envelope, plus what the failed call or macro already produced when its
     * effect happened (the success body of a mutation whose response failed strict validation,
     * or a macro's completed step results by their "as" names). The partial value can hold
     * values the API shows only once: store them before acting on the error; they are never
     * repeated in the envelope.
     *
     * @param error the envelope
     * @param partial what the call produced, meaningful when hasPartial
     * @param hasPartial whether partial holds a value
     * @param <T> the value type
     */
    record Err<T>(Diagnostic error, Object partial, boolean hasPartial) implements Result<T> {
        @Override
        public boolean isOk() {
            return false;
        }

        @Override
        public T unwrap() {
            throw new TungstenException(error, partial, hasPartial);
        }
    }
}
