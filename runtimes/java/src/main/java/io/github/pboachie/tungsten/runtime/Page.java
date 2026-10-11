// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.List;

/**
 * One page of a paginated operation.
 *
 * @param items the items
 * @param body the full page body
 * @param next the cursor, offset, page number or URL of the next page; null at the end
 * @param <T> the item type
 */
public record Page<T>(List<T> items, Object body, Object next) {}
