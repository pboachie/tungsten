// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/**
 * A JSON object whose members keep their insertion order, so request bodies, digests and texts
 * are the same in every runtime. Values are JSON values of this runtime (see {@link Json}).
 */
public final class JsonObject {
    private final LinkedHashMap<String, Object> members;

    /** An empty object. */
    public JsonObject() {
        members = new LinkedHashMap<>();
    }

    private JsonObject(LinkedHashMap<String, Object> members) {
        this.members = members;
    }

    /** An object of alternating names and values. */
    public static JsonObject of(Object... namesAndValues) {
        if (namesAndValues.length % 2 != 0) {
            throw new IllegalArgumentException("names and values come in pairs");
        }
        JsonObject out = new JsonObject();
        for (int i = 0; i < namesAndValues.length; i += 2) {
            out.set(String.valueOf(namesAndValues[i]), namesAndValues[i + 1]);
        }
        return out;
    }

    /** The number of members. */
    public int size() {
        return members.size();
    }

    /** Whether the object has no members. */
    public boolean isEmpty() {
        return members.isEmpty();
    }

    /** Whether the member is present (its value may be null). */
    public boolean has(String key) {
        return members.containsKey(key);
    }

    /** The member's value; null when absent or null (see {@link #has}). */
    public Object get(String key) {
        return members.get(key);
    }

    /**
     * Adds a member at the end, or replaces the value of an existing one in its place.
     *
     * @return this object
     */
    public JsonObject set(String key, Object value) {
        members.put(Objects.requireNonNull(key, "key"), value);
        return this;
    }

    /** Removes a member. */
    public void remove(String key) {
        members.remove(key);
    }

    /** The member names in order. */
    public List<String> keys() {
        return new ArrayList<>(members.keySet());
    }

    /** A read-only view of the members in order. */
    public Map<String, Object> asMap() {
        return Collections.unmodifiableMap(members);
    }

    /** A shallow copy. */
    public JsonObject copy() {
        return new JsonObject(new LinkedHashMap<>(members));
    }

    @Override
    public boolean equals(Object other) {
        return other instanceof JsonObject && Json.deepEqual(this, other);
    }

    @Override
    public int hashCode() {
        return Json.canonical(this).hashCode();
    }

    /** The object as JSON text. */
    @Override
    public String toString() {
        return Json.write(this);
    }
}
