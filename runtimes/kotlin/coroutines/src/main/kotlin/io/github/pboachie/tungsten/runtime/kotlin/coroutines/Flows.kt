// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime.kotlin.coroutines

import io.github.pboachie.tungsten.runtime.EventStream
import io.github.pboachie.tungsten.runtime.Page
import io.github.pboachie.tungsten.runtime.Result
import io.github.pboachie.tungsten.runtime.StreamEvent
import io.github.pboachie.tungsten.runtime.Typed
import io.github.pboachie.tungsten.runtime.kotlin.getOrThrow
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.runInterruptible

/**
 * The pages of a paginated operation as a cold [Flow]. [open] is called once for each
 * collection (it creates the runtime's pages, e.g. `Typed.pages(...)`), so a flow can be
 * collected again; nothing is sent before collection starts.
 *
 * The flow is pull based with no buffer of its own: the next page is requested only after the
 * collector has taken the previous one, so at most one page is held at a time and the
 * runtime's own limits stay in force for each of them (`maxResponseBytes` per response, cycle
 * detection and the page limit of the pagination). Add `.buffer(n)` with an explicit capacity
 * to prefetch; the amount held is then bounded by n pages. An error (an oversized response, a
 * pagination cycle, a transport failure) is the last element, as in the `Sequence`. Cancelling
 * the collector interrupts the page request in flight and closes its connection.
 */
public fun <T> pagesFlow(
    dispatcher: CoroutineDispatcher = Dispatchers.IO,
    open: () -> Typed.TypedPages<T>,
): Flow<Result<Page<T>>> = flow {
    val pages = open()
    while (true) {
        val next = runInterruptible(dispatcher) { if (pages.hasNext()) pages.next() else null } ?: break
        emit(next)
        if (next is Result.Err) break
    }
}

/**
 * The server-sent events of an operation as a cold [Flow]. [open] is called once for each
 * collection (it creates the runtime's stream, e.g. `Typed.stream(...)`); the request is sent
 * when collection starts. Like [pagesFlow] it is pull based: events are read from the
 * connection only as the collector takes them (TCP flow control applies to a slow collector),
 * and `.buffer(n)` bounds any prefetch. The stream is cancelled and closed when the flow
 * completes, fails or is cancelled; cancellation interrupts a read in flight. An error is the
 * last element.
 */
public fun <T> eventsFlow(
    dispatcher: CoroutineDispatcher = Dispatchers.IO,
    open: () -> EventStream<T>,
): Flow<Result<StreamEvent<T>>> = flow {
    val stream = open()
    try {
        while (true) {
            val next = runInterruptible(dispatcher) { if (stream.hasNext()) stream.next() else null } ?: break
            emit(next)
            if (next is Result.Err) break
        }
    } finally {
        stream.cancel()
        stream.close()
    }
}

/** The items of every page; the first error is thrown as the runtime's `TungstenException`. */
public fun <T> Flow<Result<Page<T>>>.itemsOrThrow(): Flow<T> = flow {
    collect { page ->
        for (item in page.getOrThrow().items()) {
            emit(item)
        }
    }
}

/** The decoded values of the events; the first error is thrown as the runtime's `TungstenException`. */
public fun <T> Flow<Result<StreamEvent<T>>>.valuesOrThrow(): Flow<T> = map { it.getOrThrow().value() }
