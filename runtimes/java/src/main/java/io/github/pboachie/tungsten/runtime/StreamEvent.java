// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

/**
 * One event of a stream.
 *
 * @param value the event's data as the SDK's type
 * @param raw the event's data, decoded from JSON
 * @param event the event field, "message" when the event has none
 * @param id the last event id the stream has set so far, or null
 * @param retry the last retry value (milliseconds) set so far, or null
 * @param meta the answer of the connection the event arrived on
 * @param reconnects how often the stream reconnected up to this event
 * @param <T> the value type
 */
public record StreamEvent<T>(T value, Object raw, String event, String id, Long retry, ResponseMeta meta, int reconnects) {}
