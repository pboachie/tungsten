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

    /**
     * Returns an existing nonempty key, or persists candidate. The compatible default serializes
     * get/put users of this store instance; shared external storage must override this method
     * with an atomic transaction or owner lease.
     */
    default String reserve(String scope, String logicalId, String candidate) {
        synchronized (this) {
            String existing = get(scope, logicalId);
            if (existing != null && !existing.isEmpty()) {
                return existing;
            }
            put(scope, logicalId, candidate);
            if (!candidate.equals(get(scope, logicalId))) {
                throw new IllegalStateException("the idempotency key could not be stored");
            }
            return candidate;
        }
    }

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

        @Override
        public synchronized String reserve(String scope, String logicalId, String candidate) {
            List<String> slot = List.of(scope, logicalId);
            String existing = keys.get(slot);
            if (existing != null && !existing.isEmpty()) {
                return existing;
            }
            keys.put(slot, candidate);
            return candidate;
        }
    }
}
