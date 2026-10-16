// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime.kotlin

import io.github.pboachie.tungsten.runtime.Patch as JavaPatch

/**
 * A member that may be absent, null or a value (four-state presence): [Unset] leaves the member
 * out of the JSON, [Null] sends `null`, [Value] (made with [of]) sends the value. It bridges to
 * the Java runtime's `Patch` with [toJava] and [fromJava].
 *
 * @param T the value type
 */
public sealed class Patch<out T : Any> {
    /** The member is absent. */
    public data object Unset : Patch<Nothing>()

    /** The member is an explicit null. */
    public data object Null : Patch<Nothing>()

    /** The member has a value. */
    public data class Value<out T : Any>(
        /** The value. */
        public val value: T,
    ) : Patch<T>()

    /** Whether the member is present (null or a value). */
    public val isSet: Boolean get() = this !is Unset

    /** Whether the member is an explicit null. */
    public val isNull: Boolean get() = this is Null

    /** Whether the member has a value. */
    public val hasValue: Boolean get() = this is Value

    /** The value, or null when the member is absent or null. */
    public fun valueOrNull(): T? = (this as? Value<T>)?.value

    /** The value mapped by [transform]; [Unset] and [Null] stay as they are. */
    public inline fun <R : Any> map(transform: (T) -> R): Patch<R> =
        when (this) {
            is Value -> Value(transform(value))
            Null -> Null
            Unset -> Unset
        }

    /** The Java runtime's `Patch` of the same state. */
    public fun toJava(): JavaPatch<out T> =
        when (this) {
            is Value -> JavaPatch.of(value)
            Null -> JavaPatch.ofNull()
            Unset -> JavaPatch.unset()
        }

    public companion object {
        /** A member with a value. */
        @JvmStatic
        public fun <T : Any> of(value: T): Patch<T> = Value(value)

        /** [Null] for null, else a member with the value. */
        @JvmStatic
        public fun <T : Any> ofNullable(value: T?): Patch<T> = if (value == null) Null else Value(value)

        /** The patch of the same state as a Java runtime `Patch`. */
        @JvmStatic
        public fun <T : Any> fromJava(patch: JavaPatch<T>): Patch<T> =
            when {
                patch.hasValue() -> Value(patch.value())
                patch.isNull() -> Null
                else -> Unset
            }
    }
}
