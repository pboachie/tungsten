// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime.kotlin

import io.github.pboachie.tungsten.runtime.CallOptions
import io.github.pboachie.tungsten.runtime.ClientOptions
import kotlin.time.Duration
import kotlin.time.toJavaDuration

/**
 * The total deadline of this call as a [Duration] (`deadline(10.seconds)`); see the Java
 * `CallOptions.Builder.deadline`. Zero, negative or infinite means none, so the client's applies.
 * Unlike a coroutine `withTimeout`, its expiry is a `Result.Err` with code `DEADLINE_EXCEEDED`
 * (or `OUTCOME_UNKNOWN` for a mutation already sent), not a `CancellationException`.
 */
public fun CallOptions.Builder.deadline(value: Duration): CallOptions.Builder =
    deadline(if (value.isPositive() && value.isFinite()) value.toJavaDuration() else null)

/**
 * The total deadline of each call of the client as a [Duration]; see
 * [CallOptions.Builder.deadline]. Zero, negative or infinite means none.
 */
public fun ClientOptions.Builder.deadline(value: Duration): ClientOptions.Builder =
    deadline(if (value.isPositive() && value.isFinite()) value.toJavaDuration() else null)
