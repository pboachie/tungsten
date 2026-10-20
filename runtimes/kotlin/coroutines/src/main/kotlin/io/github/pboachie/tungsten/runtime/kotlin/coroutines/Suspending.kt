// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime.kotlin.coroutines

import io.github.pboachie.tungsten.runtime.Result
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runInterruptible

/**
 * Runs one blocking call of the runtime ([call], typically `Typed.call(...)`) as a cancellable
 * suspend call and returns its [Result] unchanged.
 *
 * The call runs on [dispatcher] (`Dispatchers.IO` by default, so it never blocks the caller's
 * thread). Cancelling the coroutine interrupts the thread running the call; the Java runtime
 * answers an interrupt by closing the connection of the in-flight request and returning, so
 * cancellation propagates to the underlying request instead of leaving it to run to its
 * timeout. The suspend call then ends with a `CancellationException` (structured cancellation),
 * not with a [Result]. A mutation cancelled after it was sent has an unknown outcome; do not
 * repeat it blindly (reuse its idempotency key). A coroutine that is already cancelled sends
 * nothing.
 */
public suspend fun <T> suspending(
    dispatcher: CoroutineDispatcher = Dispatchers.IO,
    call: () -> Result<T>,
): Result<T> = runInterruptible(dispatcher, call)
