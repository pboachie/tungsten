// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * The body a poll ended with.
 *
 * @param body the last body
 * @param hasBody whether the last answer had a body
 * @param timedOut whether the budget ran out before the predicate held
 */
public record Polled(Object body, boolean hasBody, boolean timedOut) {}
