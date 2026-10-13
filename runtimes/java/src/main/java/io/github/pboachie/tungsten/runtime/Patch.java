// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.Objects;
import java.util.function.Function;

/**
 * A field that may be absent, null or a value (presence optional-nullable): {@link #unset()}
 * leaves the member out, {@link #ofNull()} sends null, {@link #of} the value.
 *
 * @param <T> the value type
 */
public final class Patch<T> {
    private static final Patch<?> UNSET = new Patch<>(0, null);
    private static final Patch<?> NULL = new Patch<>(1, null);

    private final int state;
    private final T value;

    private Patch(int state, T value) {
        this.state = state;
        this.value = value;
    }

    /** The member is absent. */
    @SuppressWarnings("unchecked")
    public static <T> Patch<T> unset() {
        return (Patch<T>) UNSET;
    }

    /** The member is an explicit null. */
    @SuppressWarnings("unchecked")
    public static <T> Patch<T> ofNull() {
        return (Patch<T>) NULL;
    }

    /** The member has a value. */
    public static <T> Patch<T> of(T value) {
        return new Patch<>(2, Objects.requireNonNull(value, "value; use Patch.ofNull() for null"));
    }

    /** Whether the member is present (null or a value). */
    public boolean isSet() {
        return state != 0;
    }

    /** Whether the member is an explicit null. */
    public boolean isNull() {
        return state == 1;
    }

    /** Whether the member has a value. */
    public boolean hasValue() {
        return state == 2;
    }

    /** The value, or null when absent or null. */
    public T value() {
        return value;
    }

    /** The JSON of a set member: null or the encoded value. */
    Object toJson(Function<T, Object> encode) {
        return state == 2 ? encode.apply(value) : null;
    }

    @Override
    public boolean equals(Object o) {
        return o instanceof Patch<?> && ((Patch<?>) o).state == state && Objects.equals(((Patch<?>) o).value, value);
    }

    @Override
    public int hashCode() {
        return Objects.hash(state, value);
    }

    @Override
    public String toString() {
        return state == 0 ? "Patch.unset()" : state == 1 ? "Patch.ofNull()" : "Patch.of(" + value + ")";
    }
}
