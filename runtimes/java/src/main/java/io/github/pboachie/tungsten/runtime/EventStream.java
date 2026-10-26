// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.time.Duration;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.NoSuchElementException;
import java.util.Set;
import java.util.function.BiFunction;
import java.util.function.Consumer;
import java.util.function.Function;
import java.util.function.Predicate;

/**
 * The server-sent events of an operation. The request is sent by the first {@link #hasNext};
 * each element is a {@link Result}, and the first {@link Result.Err} ends the stream. A dropped
 * stream of a call that is safe to repeat reconnects with Last-Event-ID (never delivering an
 * event twice), and a silent one ends after the idle timeout. {@link #cancel} ends it for good
 * from any thread; {@link #close} releases the connection.
 *
 * @param <T> the type of each event's value
 */
public final class EventStream<T> implements Iterator<Result<StreamEvent<T>>>, Iterable<Result<StreamEvent<T>>>, AutoCloseable {
    /** The wait before a reconnect when the server sent no retry value. */
    static final long DEFAULT_RECONNECT_MS = 250;

    private final ClientCore core;
    private final OperationDescriptor op;
    private final OperationDescriptor.Stream spec;
    private final Object args;
    private final CallOptions opts;
    private final Function<Object, T> decoder;
    private final TungstenException initial;
    private Open open;
    private int state;
    private Result<StreamEvent<T>> ahead;
    private volatile boolean cancelled;
    private final Object lock = new Object();
    private Transport.Body body;
    private long collectDeadline;
    /** The call deadline: it bounds connecting and each reconnect, never an open stream. */
    private final Long deadline;

    EventStream(ClientCore core, OperationDescriptor op, Object args, CallOptions opts, Function<Object, T> decoder,
            TungstenException initial) {
        this.core = core;
        this.op = op;
        this.spec = op.stream() != null ? op.stream() : new OperationDescriptor.Stream(null, null, null);
        this.args = args;
        this.opts = opts;
        this.decoder = decoder;
        this.initial = initial;
        this.deadline = Deadline.resolve(core.deadline, opts);
    }

    /** The streaming operation. */
    public OperationDescriptor operation() {
        return op;
    }

    /** One connection of the stream. */
    private static final class Open {
        Transport.Body body;
        Sse parser;
        final ArrayDeque<Sse.Event> queue = new ArrayDeque<>();
        boolean ended;
        boolean oversize;
        ResponseMeta meta;
        int count;
        String key;
        boolean hasKey;
        String keyHeader;
        Classify.OutcomeCheck check;
        List<String> secrets;
        Duration timeout;
        boolean repeatable;
        int reconnects;
        String lastId;
        Long lastRetry;
        Set<String> delivered = new HashSet<>();
        boolean replaying;
    }

    private enum Dropped {
        NONE,
        LOST,
        IDLE,
        EARLY_END
    }

    private void setOpen(Open o) {
        boolean wasCancelled;
        synchronized (lock) {
            open = o;
            body = o == null ? null : o.body;
            wasCancelled = cancelled;
        }
        if (wasCancelled && o != null) {
            o.body.close();
        }
    }

    /** Ends the stream for good, without an error and without a reconnect; any thread may call it. */
    public void cancel() {
        Transport.Body b;
        synchronized (lock) {
            if (cancelled) {
                return;
            }
            cancelled = true;
            b = body;
        }
        if (b != null) {
            b.close();
        }
    }

    /** Releases the connection. */
    @Override
    public void close() {
        if (open != null) {
            open.body.close();
            setOpen(null);
        }
        state = 2;
    }

    @Override
    public Iterator<Result<StreamEvent<T>>> iterator() {
        return this;
    }

    @Override
    public boolean hasNext() {
        if (ahead == null) {
            ahead = advance();
        }
        return ahead != null;
    }

    @Override
    public Result<StreamEvent<T>> next() {
        if (!hasNext()) {
            throw new NoSuchElementException();
        }
        Result<StreamEvent<T>> out = ahead;
        ahead = null;
        return out;
    }

    private Result<StreamEvent<T>> fail(TungstenException e) {
        close();
        return e.toResult();
    }

    private Duration readWait(Open o) {
        Duration wait = o.body.idle;
        if (collectDeadline != 0) {
            long left = collectDeadline - System.nanoTime();
            if (left < wait.toNanos()) {
                return Duration.ofNanos(Math.max(left, 1_000_000));
            }
        }
        return wait;
    }

    private boolean overDeadline() {
        return collectDeadline != 0 && System.nanoTime() >= collectDeadline;
    }

    private Result<StreamEvent<T>> advance() {
        while (true) {
            if (state == 2) {
                return null;
            }
            if (state == 0) {
                if (initial != null) {
                    state = 2;
                    return initial.toResult();
                }
                if (cancelled) {
                    state = 2;
                    return null;
                }
                Open o;
                try {
                    o = openStream(null);
                } catch (TungstenException e) {
                    return fail(e);
                }
                setOpen(o);
                state = 1;
                if (cancelled) {
                    close();
                    return null;
                }
                continue;
            }
            if (cancelled) {
                // Events already read but not yet delivered are dropped too.
                close();
                return null;
            }
            Open o = open;
            if (!o.queue.isEmpty()) {
                Sse.Event ev = o.queue.poll();
                Accepted<T> item;
                try {
                    item = accept(o, ev);
                } catch (TungstenException e) {
                    return fail(e);
                }
                if (item.done) {
                    close();
                    return null;
                }
                if (item.event == null) {
                    continue;
                }
                return new Result.Ok<>(item.event, true, item.event.raw(), item.event.meta(), null);
            }
            if (o.oversize) {
                return fail(oversize(o));
            }
            if (overDeadline()) {
                close();
                return null;
            }
            Dropped why = Dropped.NONE;
            if (o.ended) {
                // A stream that names its terminal event has not finished without it.
                if (spec.done() == null) {
                    close();
                    return null;
                }
                why = Dropped.EARLY_END;
            } else {
                Transport.Read r = o.body.read(readWait(o));
                if (cancelled) {
                    close();
                    return null;
                }
                if (r == Transport.Read.CHUNK) {
                    o.queue.addAll(o.parser.push(o.parser.decode(o.body.chunk)));
                    o.oversize = o.parser.exceeded;
                    continue;
                }
                if (r == Transport.Read.END) {
                    o.queue.addAll(o.parser.push(o.parser.finish()));
                    o.queue.addAll(o.parser.end());
                    o.oversize = o.parser.exceeded;
                    o.ended = true;
                    continue;
                }
                if (r == Transport.Read.TIMEOUT) {
                    if (overDeadline()) {
                        close();
                        return null;
                    }
                    if (core.idleTimeout == null) {
                        return fail(interrupted(o, Dropped.NONE));
                    }
                    why = Dropped.IDLE;
                } else {
                    why = Dropped.LOST;
                }
            }
            if (!mayReconnect(o)) {
                // Without reconnects a clean end stays a clean end.
                if (why == Dropped.EARLY_END && o.reconnects == 0) {
                    close();
                    return null;
                }
                return fail(interrupted(o, why));
            }
            Open fresh;
            try {
                fresh = reconnect(o);
            } catch (TungstenException e) {
                if (cancelled) {
                    close();
                    return null;
                }
                return fail(e);
            }
            if (cancelled) {
                fresh.body.close();
                close();
                return null;
            }
            setOpen(fresh);
        }
    }

    /** Sets the stream's request flag to true, in a merged body field or inside an object body argument. */
    private Object withFlag(Object raw) {
        if (spec.flag() == null || op.body() == null || !(raw instanceof JsonObject)) {
            return raw;
        }
        JsonObject o = ((JsonObject) raw).copy();
        if (op.body().arg() == null) {
            for (OperationDescriptor.MergedField f : op.body().fields()) {
                if (f.wire().equals(spec.flag())) {
                    o.set(f.arg(), true);
                    break;
                }
            }
        } else if (o.get(op.body().arg()) instanceof JsonObject) {
            JsonObject inner = ((JsonObject) o.get(op.body().arg())).copy();
            inner.set(spec.flag(), true);
            o.set(op.body().arg(), inner);
        }
        return o;
    }

    private Open openStream(String resumeFrom) {
        return Deadline.within(deadline, () -> openStreamWithin(resumeFrom));
    }

    private Open openStreamWithin(String resumeFrom) {
        String accept = "";
        for (Map.Entry<String, String> e : core.headers.entrySet()) {
            if (e.getKey().equalsIgnoreCase("accept")) {
                accept = e.getValue();
            }
        }
        Map<String, String> own = new java.util.LinkedHashMap<>();
        for (Map.Entry<String, String> e : opts.headers().entrySet()) {
            if (e.getKey().equalsIgnoreCase("accept")) {
                accept = e.getValue();
            } else {
                own.put(e.getKey(), e.getValue());
            }
        }
        own.put("Accept", Text.mergeAccept(accept));
        if (resumeFrom != null) {
            own.keySet().removeIf(k -> k.equalsIgnoreCase("last-event-id"));
            own.put("Last-Event-ID", resumeFrom);
        }
        CallOptions streamOpts = opts.toBuilder().headers(own).build();
        Object normalized;
        try {
            normalized = Json.normalize(args);
        } catch (IllegalArgumentException e) {
            normalized = args;
        }
        ClientCore.Prepared p = core.prepare(op, withFlag(normalized), streamOpts, ClientCore.Purpose.CALL, null, null);
        ClientCore.Sent result = core.sendWith(p, streamOpts, true);
        if (result.stream() == null) {
            Result.Ok<Object> response = result.response();
            String contentType = response.meta().headers().get("content-type");
            String after = op.isMutation() ? " The call took effect; do not repeat it." : "";
            throw core.diag(op.id(), Category.UNEXPECTED_RESPONSE).status(response.meta().status())
                    .requestId(response.meta().requestId()).param("response")
                    .received(contentType == null ? null : Text.envelopeValue(contentType, false))
                    .expected("a text/event-stream body")
                    .remedy("The success response is not an event stream (Content-Type " + (contentType == null ? "missing" : contentType) + ")." + after)
                    .retry(Retryable.NEVER).attempts(response.meta().attempts()).fail();
        }
        Open o = new Open();
        o.body = result.stream();
        o.parser = new Sse(core.maxEventBytes);
        o.meta = result.streamMeta();
        o.key = p.key;
        o.hasKey = p.hasKey;
        o.keyHeader = p.keyHeader;
        o.check = op.isMutation() && !Classify.hasReplayProtection(op, p.hasKey) ? core.outcomeCheck(op, p.args) : null;
        o.secrets = new ArrayList<>(p.secrets);
        o.timeout = core.attemptTimeout(streamOpts);
        o.repeatable = !op.isMutation() || Classify.hasReplayProtection(op, p.hasKey);
        return o;
    }

    /**
     * A dropped stream may be reconnected when the call is safe to repeat, reconnects are left,
     * and an event id to resume from is known (or no event was delivered yet).
     */
    private boolean mayReconnect(Open o) {
        return o.repeatable && o.reconnects < core.maxReconnects
                && (o.count == 0 || (o.lastId != null && Text.validHeaderValue(o.lastId)));
    }

    /**
     * Waits as the server asked (capped, shortened by up to 25 % at random), then sends the
     * request again with Last-Event-ID.
     */
    private Open reconnect(Open old) {
        old.body.close();
        double ceiling = old.lastRetry != null ? old.lastRetry : DEFAULT_RECONNECT_MS;
        ceiling = Math.min(ceiling, core.reconnectMax.toMillis());
        double wait = ceiling - core.random() * 0.25 * ceiling;
        // A reconnect wait never outlasts the deadline; the reconnect then fails unsent.
        if (deadline != null) {
            wait = Math.max(0, Math.min(wait, (deadline - System.nanoTime()) / 1e6));
        }
        ClientCore.sleep(wait);
        Open fresh = openStream(old.lastId);
        fresh.count = old.count;
        fresh.reconnects = old.reconnects + 1;
        fresh.lastId = old.lastId;
        fresh.lastRetry = old.lastRetry;
        fresh.delivered = old.delivered;
        fresh.replaying = true;
        return fresh;
    }

    private static String plural(int n, String one, String many) {
        return n == 1 ? one : many;
    }

    private Classify.Context context(Open o) {
        return new Classify.Context(core.identity, core.api, op, o.key, o.hasKey, o.keyHeader, o.meta.attempts(), o.check);
    }

    /** The final error of a stream that broke after o.count events; NONE is a silence past the timeout. */
    private TungstenException interrupted(Open o, Dropped why) {
        String after = "after " + o.count + " " + plural(o.count, "event", "events");
        String retried = o.reconnects == 0 ? "" : o.reconnects == 1 ? " (1 reconnect made)" : " (" + o.reconnects + " reconnects made)";
        boolean idle = why == Dropped.IDLE;
        String code = idle ? "STREAM_IDLE" : null;
        int status = o.meta.status();
        Diagnostic d;
        if (why == Dropped.NONE || idle) {
            String silence = "No event arrived within " + o.timeout.toMillis() + " ms " + after;
            String option = core.identity.timeout();
            if (idle) {
                silence = "No bytes arrived within " + core.idleTimeout.toMillis() + " ms " + after + retried;
                option = core.identity.idleTimeout();
            }
            String cause = silence + "; the stream was abandoned.";
            if (op.isMutation()) {
                d = Classify.outcomeUnknown(context(o), cause, null, status, code, o.meta.requestId(), null);
            } else {
                d = core.diag(op.id(), Category.UPSTREAM_UNAVAILABLE).status(status).code(code).requestId(o.meta.requestId())
                        .remedy(cause + " This read has no side effects; call again later or with a larger " + option + ".")
                        .attempts(o.meta.attempts()).build();
            }
        } else {
            String cause = "The event stream was cut off " + after + retried + ": the connection failed before it ended.";
            if (op.isMutation()) {
                d = Classify.outcomeUnknown(context(o), cause, null, status, null, o.meta.requestId(), null);
            } else {
                d = core.diag(op.id(), Category.TRANSPORT_FAILED).status(status).requestId(o.meta.requestId())
                        .remedy(cause + " This read has no side effects; call again.").attempts(o.meta.attempts()).build();
            }
        }
        return new TungstenException(d.scrubbed(o.secrets));
    }

    private TungstenException overBudget(boolean bytes, int kept) {
        String what = bytes
                ? "at most " + core.maxCollectBytes + " bytes of events"
                : "at most " + core.maxCollectTime.toMillis() + " ms to collect the events";
        String after = op.isMutation() ? " The call took effect; do not repeat it." : "";
        return core.diag(op.id(), Category.UNEXPECTED_RESPONSE).param("events").expected(what)
                .remedy("The stream was abandoned after " + kept + " " + plural(kept, "event", "events") + " (" + what
                        + "; ClientOptions.maxCollectBytes and maxCollectTime)." + after
                        + " The events collected so far precede this error.")
                .retry(Retryable.NEVER).fail();
    }

    private TungstenException oversize(Open o) {
        int limit = core.maxEventBytes;
        String after = op.isMutation() ? " The call took effect; do not repeat it." : "";
        String option = "ClientOptions." + core.identity.maxEventBytes();
        Diagnostic d = core.diag(op.id(), Category.UNEXPECTED_RESPONSE).status(o.meta.status()).requestId(o.meta.requestId())
                .param("events[" + o.count + "]").expected("an event of at most " + limit + " bytes")
                .remedy("Event " + o.count + " of the stream is larger than the limit of " + limit + " bytes (" + option
                        + "); the stream was abandoned." + after + " Events before it were delivered. Raise "
                        + core.identity.maxEventBytes() + " if the server sends events this large on purpose.")
                .retry(Retryable.NEVER).attempts(o.meta.attempts()).build();
        return new TungstenException(d.scrubbed(o.secrets));
    }

    private record Accepted<T>(StreamEvent<T> event, boolean done) {}

    /** Decodes, checks and numbers one event. */
    private Accepted<T> accept(Open o, Sse.Event ev) {
        if (spec.done() != null && spec.done().equals(ev.data())) {
            return new Accepted<>(null, true);
        }
        if (o.replaying) {
            if (ev.id() != null && o.delivered.contains(ev.id())) {
                return new Accepted<>(null, false);
            }
            o.replaying = false;
        }
        int index = o.count;
        String afterEffect = op.isMutation() ? " The call took effect; do not repeat it." : "";
        Object value = ev.data();
        if (!spec.text()) {
            try {
                value = Json.parse(ev.data());
            } catch (Json.JsonException e) {
                throw failure(o, core.diag(op.id(), Category.UNEXPECTED_RESPONSE).param("events[" + index + "]")
                        .received(Text.envelopeValue(ev.data(), false)).expected("JSON in the data of every event")
                        .remedy("Event " + index + " of the stream (event \"" + ev.event() + "\") does not carry JSON in its data."
                                + afterEffect + " Events before it were delivered."));
            }
        }
        if (core.validateResponses != ClientOptions.ValidateResponses.OFF && spec.event() != null) {
            List<Schema.Issue> issues = core.judge(op.id(), "events[" + index + "]", spec.event(), value, null);
            if (!issues.isEmpty()) {
                Schema.Issue issue = issues.get(0);
                List<String> path = Schema.segments(issue.path());
                String pathText = Schema.formatPath(issue.path());
                Object received = Json.at(Text.redactPaths(value, op.agent().sensitiveResponseFields()), path);
                boolean sensitive = false;
                for (String s : path) {
                    sensitive |= Text.looksSensitive(s);
                }
                TungstenException e = failure(o, core.diag(op.id(), Category.UNEXPECTED_RESPONSE)
                        .param("events[" + index + "]" + pathText)
                        .received(Text.envelopeValue(received == Json.ABSENT ? null : received, sensitive))
                        .expected(issue.message())
                        .remedy("Event " + index + " of the stream (event \"" + ev.event() + "\") does not match the API description at events["
                                + index + "]" + pathText + " (" + issue.message() + ")." + afterEffect));
                if (core.validateResponses == ClientOptions.ValidateResponses.STRICT) {
                    throw e;
                }
                core.emit(e.diagnostic());
            }
        }
        T typed;
        try {
            typed = decoder.apply(value);
        } catch (Decode.Failure f) {
            throw Typed.eventMismatch(core, op.id(), index, f, value, o.meta);
        }
        o.count++;
        if (ev.id() != null) {
            o.delivered.add(ev.id());
            o.lastId = ev.id();
        }
        if (ev.retry() != null) {
            o.lastRetry = ev.retry();
        }
        return new Accepted<>(new StreamEvent<>(typed, value, ev.event(), o.lastId, o.lastRetry, o.meta, o.reconnects), false);
    }

    private TungstenException failure(Open o, Diag d) {
        return new TungstenException(d.status(o.meta.status()).requestId(o.meta.requestId()).retry(Retryable.NEVER)
                .attempts(o.meta.attempts()).build().scrubbed(o.secrets));
    }

    // ------------------------------------------------------------ helpers

    /** Reads the stream to its end and folds its events into one value, then closes it. */
    public <A> Folded<A> reduce(A initialValue, BiFunction<A, StreamEvent<T>, A> fold) {
        try (EventStream<T> s = this) {
            A value = initialValue;
            int reconnects = 0;
            while (s.hasNext()) {
                Result<StreamEvent<T>> r = s.next();
                if (r instanceof Result.Err<StreamEvent<T>> err) {
                    return new Folded<>(value, err.error(), reconnects);
                }
                StreamEvent<T> ev = r.unwrap();
                reconnects = ev.reconnects();
                value = fold.apply(value, ev);
            }
            return new Folded<>(value, null, reconnects);
        }
    }

    /** The first event for which the predicate holds (null when none), then closes the stream. */
    public Folded<StreamEvent<T>> first(Predicate<StreamEvent<T>> predicate) {
        try (EventStream<T> s = this) {
            int reconnects = 0;
            while (s.hasNext()) {
                Result<StreamEvent<T>> r = s.next();
                if (r instanceof Result.Err<StreamEvent<T>> err) {
                    return new Folded<>(null, err.error(), reconnects);
                }
                StreamEvent<T> ev = r.unwrap();
                reconnects = ev.reconnects();
                if (predicate.test(ev)) {
                    return new Folded<>(ev, null, reconnects);
                }
            }
            return new Folded<>(null, null, reconnects);
        }
    }

    /**
     * Reads the stream to its end and calls the handler named like each event ("message" for an
     * event without a name), else the one named "*"; the value is the number of events.
     */
    public Folded<Integer> on(Map<String, Consumer<StreamEvent<T>>> handlers) {
        return reduce(0, (n, ev) -> {
            Consumer<StreamEvent<T>> h = handlers.get(ev.event());
            if (h == null) {
                h = handlers.get("*");
            }
            if (h != null) {
                h.accept(ev);
            }
            return n + 1;
        });
    }

    /**
     * Every event in order, bounded: the collection ends with an UNEXPECTED_RESPONSE error once
     * the kept events hold more than ClientOptions.maxCollectBytes of JSON or it ran longer than
     * maxCollectTime.
     */
    public Folded<List<StreamEvent<T>>> collect() {
        collectDeadline = System.nanoTime() + core.maxCollectTime.toNanos();
        if (collectDeadline == 0) {
            collectDeadline = 1;
        }
        List<StreamEvent<T>> out = new ArrayList<>();
        int bytes = 0;
        int reconnects = 0;
        try (EventStream<T> s = this) {
            while (s.hasNext()) {
                Result<StreamEvent<T>> r = s.next();
                if (r instanceof Result.Err<StreamEvent<T>> err) {
                    return new Folded<>(out, err.error(), reconnects);
                }
                StreamEvent<T> ev = r.unwrap();
                if (overDeadline()) {
                    return new Folded<>(out, overBudget(false, out.size()).diagnostic(), reconnects);
                }
                bytes += Text.utf8(Json.write(ev.raw())).length;
                if (bytes > core.maxCollectBytes) {
                    return new Folded<>(out, overBudget(true, out.size()).diagnostic(), reconnects);
                }
                reconnects = ev.reconnects();
                out.add(ev);
            }
            if (overDeadline() && !cancelled) {
                return new Folded<>(out, overBudget(false, out.size()).diagnostic(), reconnects);
            }
            return new Folded<>(out, null, reconnects);
        }
    }
}
