// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime.kotlin

import io.github.pboachie.tungsten.runtime.Diagnostic
import io.github.pboachie.tungsten.runtime.Result
import io.github.pboachie.tungsten.runtime.TungstenException

/** The value of a success; a failure throws [TungstenException] with its envelope and partial value. */
public fun <T> Result<T>.getOrThrow(): T = unwrap()

/** The value of a success, or null for a failure (and for a success without a body). */
public fun <T> Result<T>.getOrNull(): T? =
    when (this) {
        is Result.Ok -> value()
        is Result.Err -> null
    }

/** The envelope of a failure, or null for a success. */
public fun <T> Result<T>.errorOrNull(): Diagnostic? = error()

/** [onOk] of the value of a success, or [onErr] of the envelope of a failure. */
public inline fun <T, R> Result<T>.fold(onOk: (T) -> R, onErr: (Diagnostic) -> R): R =
    when (this) {
        is Result.Ok -> onOk(value())
        is Result.Err -> onErr(error())
    }

/** Runs [action] with the envelope of a failure; returns the result unchanged. */
public inline fun <T> Result<T>.onError(action: (Diagnostic) -> Unit): Result<T> {
    if (this is Result.Err) {
        action(error())
    }
    return this
}

/** Runs [action] with the value of a success; returns the result unchanged. */
public inline fun <T> Result<T>.onOk(action: (T) -> Unit): Result<T> {
    if (this is Result.Ok) {
        action(value())
    }
    return this
}

/** The success value mapped by [transform], keeping the metadata; a failure stays as it is. */
public inline fun <T, R> Result<T>.map(transform: (T) -> R): Result<R> =
    when (this) {
        is Result.Ok -> Result.Ok(transform(value()), hasValue(), raw(), meta(), verification())
        is Result.Err -> Result.Err(error(), partial(), hasPartial())
    }
