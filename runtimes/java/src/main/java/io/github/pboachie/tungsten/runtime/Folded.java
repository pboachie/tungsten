// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * What the consuming helpers of a stream return: the result, or, when the stream failed, what
 * was gathered before the failure with the failure as {@code error}.
 *
 * @param value the result (what was gathered before a failure)
 * @param error the failure, or null
 * @param reconnects the reconnects of the last event delivered
 * @param <R> the result type
 */
public record Folded<R>(R value, Diagnostic error, int reconnects) {}
