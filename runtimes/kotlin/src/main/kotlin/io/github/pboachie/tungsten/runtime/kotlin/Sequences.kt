// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime.kotlin

import io.github.pboachie.tungsten.runtime.EventStream
import io.github.pboachie.tungsten.runtime.Page
import io.github.pboachie.tungsten.runtime.Pages
import io.github.pboachie.tungsten.runtime.Result
import io.github.pboachie.tungsten.runtime.StreamEvent
import io.github.pboachie.tungsten.runtime.Typed

/**
 * The pages of a paginated operation as a [Sequence], fetched as the sequence advances. The
 * pages can be iterated once (a second iteration throws `IllegalStateException`); an error,
 * including an item that does not decode, is the last element.
 *
 * @param T the item type
 */
public class PageSequence<T>(private val pages: Typed.TypedPages<T>) : Sequence<Result<Page<T>>> {
    private var iterated = false

    override fun iterator(): Iterator<Result<Page<T>>> {
        check(!iterated) { "the pages can be iterated only once" }
        iterated = true
        return pages
    }

    /** Every page; an error ends the list with it. */
    public fun all(): List<Result<Page<T>>> = iterator().asSequence().toList()

    /** Every page with its items as decoded JSON, each checked to decode as T. */
    public fun allRaw(): List<Result<Page<Any?>>> {
        check(!iterated) { "the pages can be iterated only once" }
        iterated = true
        return pages.allRaw().map { it.uncheckedCast() }
    }

    /** The items of every page; the first error is thrown as the runtime's `TungstenException`. */
    public fun itemsOrThrow(): Sequence<T> = flatMap { page -> page.getOrThrow().items().asSequence() }
}

@Suppress("UNCHECKED_CAST")
private fun <A, B> Result<A>.uncheckedCast(): Result<B> = this as Result<B>

/** The untyped pages of an operation as a [Sequence], iterated once. */
public fun Pages.asPageSequence(): Sequence<Result<Page<Any?>>> {
    val source = this
    return Sequence { source.iterator().asResults() }.constrainOnce()
}

private fun Iterator<Result<Page<Any>>>.asResults(): Iterator<Result<Page<Any?>>> {
    val source = this
    return object : Iterator<Result<Page<Any?>>> {
        override fun hasNext(): Boolean = source.hasNext()

        override fun next(): Result<Page<Any?>> = source.next().uncheckedCast()
    }
}

/**
 * The events of a stream as a [Sequence], iterated once. An error (a transport failure, an
 * event that does not decode) is the last element; close the stream (`use`) when leaving early.
 */
public fun <T> EventStream<T>.events(): Sequence<Result<StreamEvent<T>>> {
    val source: Iterator<Result<StreamEvent<T>>> = this
    return Sequence { source }.constrainOnce()
}

/** The decoded values of a stream's events; the first error is thrown as the runtime's `TungstenException`. */
public fun <T> EventStream<T>.valuesOrThrow(): Sequence<T> = events().map { it.getOrThrow().value() }
