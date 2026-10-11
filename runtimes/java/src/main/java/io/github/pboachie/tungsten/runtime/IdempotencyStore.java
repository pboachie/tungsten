// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * Remembers the key used for one logical intent so that a retry reuses it. Implement it to
 * persist keys across processes; the default is {@link Memory}.
 */
public interface IdempotencyStore {
    /** The key stored for (scope, logicalId), or null. */
    String get(String scope, String logicalId);

    /** Stores the key of (scope, logicalId). */
    void put(String scope, String logicalId, String key);

    /** Keeps keys for the life of the process. */
    final class Memory implements IdempotencyStore {
        private final Map<List<String>, String> keys = new HashMap<>();

        @Override
        public synchronized String get(String scope, String logicalId) {
            return keys.get(List.of(scope, logicalId));
        }

        @Override
        public synchronized void put(String scope, String logicalId, String key) {
            keys.put(List.of(scope, logicalId), key);
        }
    }
}
