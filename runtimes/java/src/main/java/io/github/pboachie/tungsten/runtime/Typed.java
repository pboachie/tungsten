// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.NoSuchElementException;
import java.util.function.Function;

/**
 * The helpers generated SDKs call: encode a request, call the operation through {@link
 * ClientCore}, and decode the success body into the SDK's type. A body that does not decode is
 * UNEXPECTED_RESPONSE with the body as the partial value.
 */
public final class Typed {
    private Typed() {}

    /** VALIDATION_FAILED for a request that could not be encoded as JSON. */
    static TungstenException argumentsError(ClientCore core, String operation, RuntimeException e) {
        return core.diag(operation, Category.VALIDATION_FAILED).param("args").expected("arguments that encode as JSON")
                .remedy("The arguments of " + operation + " could not be encoded as JSON (" + e.getMessage() + "). Nothing was sent.")
                .fail();
    }

    private static Object encode(ClientCore core, OperationDescriptor op, Json.Encodable request) {
        try {
            return Json.normalize(request);
        } catch (IllegalArgumentException e) {
            throw argumentsError(core, op.id(), e);
        }
    }

    private static TungstenException mismatch(ClientCore core, String operation, String prefix, Decode.Failure f, Object root,
            ResponseMeta meta) {
        List<String> segments = Schema.segments(f.path());
        Object shown;
        if (segments.isEmpty() && prefix.isEmpty()) {
            shown = Text.envelopeValue(root, false);
        } else {
            Object found = Json.at(root, segments);
            boolean sensitive = false;
            for (String s : segments) {
                sensitive |= Text.looksSensitive(s);
            }
            shown = Text.envelopeValue(found == Json.ABSENT ? null : found, sensitive);
        }
        String location = "response" + prefix + Schema.formatPath(f.path());
        Diag d = core.diag(operation, Category.UNEXPECTED_RESPONSE);
        if (meta.status() > 0) {
            d.status(meta.status());
        }
        return new TungstenException(d.requestId(meta.requestId()).param(location).received(shown).expected(f.getMessage())
                .retry(Retryable.NEVER)
                .remedy("The response of " + operation + " could not be decoded into the SDK's type at " + location + " ("
                        + f.getMessage() + "). If the call changed state it already took effect; do not repeat it.")
                .attempts(meta.attempts()).build(), root, true);
    }

    static TungstenException eventMismatch(ClientCore core, String operation, int index, Decode.Failure f, Object value,
            ResponseMeta meta) {
        List<String> segments = Schema.segments(f.path());
        String location = "events[" + index + "]" + Schema.formatPath(f.path());
        Object found = Json.at(value, segments);
        boolean sensitive = false;
        for (String s : segments) {
            sensitive |= Text.looksSensitive(s);
        }
        Diag d = core.diag(operation, Category.UNEXPECTED_RESPONSE);
        if (meta.status() > 0) {
            d.status(meta.status());
        }
        return d.requestId(meta.requestId()).param(location)
                .received(Text.envelopeValue(found == Json.ABSENT ? null : found, sensitive)).expected(f.getMessage())
                .retry(Retryable.NEVER)
                .remedy("Event " + index + " of the stream of " + operation + " could not be decoded into the SDK's type at "
                        + location + " (" + f.getMessage() + "). If the call changed state it already took effect; do not repeat it.")
                .attempts(meta.attempts()).fail();
    }

    /**
     * An arguments object decoded into a request record, or null when it does not decode (the
     * caller then calls the core with the raw arguments, so pre-flight validation reports it).
     */
    public static <T> T decodeArgs(Object args, Function<Object, T> fromJson) {
        Object converted;
        try {
            converted = Json.normalize(args);
        } catch (IllegalArgumentException e) {
            return null;
        }
        try {
            return fromJson.apply(converted == null ? new JsonObject() : converted);
        } catch (Decode.Failure e) {
            return null;
        }
    }

    /** A typed result as a dynamic one whose value is the decoded wire body. */
    public static <T> Result<Object> untyped(Result<T> result) {
        if (result instanceof Result.Err<T> err) {
            return new Result.Err<>(err.error(), err.partial(), err.hasPartial());
        }
        Result.Ok<T> ok = (Result.Ok<T>) result;
        return new Result.Ok<>(ok.raw(), ok.hasValue(), ok.raw(), ok.meta(), ok.verification());
    }

    /** A dynamic result decoded into the SDK's type; a missing body decodes as JSON null. */
    public static <T> Result<T> decode(ClientCore core, String operation, Result<Object> outcome, Function<Object, T> decoder) {
        if (outcome instanceof Result.Err<Object> err) {
            return new Result.Err<>(err.error(), err.partial(), err.hasPartial());
        }
        Result.Ok<Object> ok = (Result.Ok<Object>) outcome;
        T value;
        try {
            value = ok.value() == null ? null : decoder.apply(ok.value());
        } catch (Decode.Failure f) {
            return mismatch(core, operation, "", f, ok.value(), ok.meta()).toResult();
        }
        return new Result.Ok<>(value, ok.hasValue(), ok.value(), ok.meta(), ok.verification());
    }

    /** Encodes a request, calls the operation and decodes the body. */
    public static <T> Result<T> call(ClientCore core, OperationDescriptor op, Json.Encodable request, CallOptions opts,
            Function<Object, T> decoder) {
        Object args;
        try {
            args = encode(core, op, request);
        } catch (TungstenException e) {
            return e.toResult();
        }
        return decode(core, op.id(), core.call(op, args, opts), decoder);
    }

    /** Encodes a request and previews the operation. */
    public static Result<PreviewResult> preview(ClientCore core, OperationDescriptor op, Json.Encodable request, CallOptions opts) {
        Object args;
        try {
            args = encode(core, op, request);
        } catch (TungstenException e) {
            return e.toResult();
        }
        return core.preview(op, args, opts);
    }

    /** Encodes a request and iterates the operation's pages with typed items. */
    public static <T> TypedPages<T> pages(ClientCore core, OperationDescriptor op, Json.Encodable request, CallOptions opts,
            Function<Object, T> item) {
        Object args;
        try {
            args = encode(core, op, request);
        } catch (TungstenException e) {
            return new TypedPages<>(core, op, null, item, e);
        }
        return new TypedPages<>(core, op, core.pages(op, args, opts), item, null);
    }

    /** Encodes a request and opens the operation's event stream with typed values. */
    public static <T> EventStream<T> stream(ClientCore core, OperationDescriptor op, Json.Encodable request, CallOptions opts,
            Function<Object, T> decoder) {
        Object args = null;
        TungstenException failed = null;
        try {
            args = encode(core, op, request);
        } catch (TungstenException e) {
            failed = e;
        }
        return new EventStream<>(core, op, args, opts, decoder, failed);
    }

    /**
     * The pages of an operation with items typed as T; an item that does not decode ends the
     * iteration with an UNEXPECTED_RESPONSE error.
     *
     * @param <T> the item type
     */
    public static final class TypedPages<T> implements Iterable<Result<Page<T>>>, Iterator<Result<Page<T>>> {
        private final ClientCore core;
        private final OperationDescriptor op;
        private final Pages pages;
        private final Function<Object, T> item;
        private TungstenException pending;
        private boolean done;
        private Result<Page<T>> ahead;
        private Result<Page<Object>> raw;

        TypedPages(ClientCore core, OperationDescriptor op, Pages pages, Function<Object, T> item, TungstenException pending) {
            this.core = core;
            this.op = op;
            this.pages = pages;
            this.item = item;
            this.pending = pending;
        }

        @Override
        public Iterator<Result<Page<T>>> iterator() {
            return this;
        }

        @Override
        public boolean hasNext() {
            if (ahead == null && !done) {
                ahead = advance();
            }
            return ahead != null;
        }

        @Override
        public Result<Page<T>> next() {
            if (!hasNext()) {
                throw new NoSuchElementException();
            }
            Result<Page<T>> out = ahead;
            ahead = null;
            return out;
        }

        private Result<Page<T>> advance() {
            if (pending != null) {
                done = true;
                raw = pending.toResult();
                Result<Page<T>> out = pending.toResult();
                pending = null;
                return out;
            }
            if (!pages.hasNext()) {
                done = true;
                return null;
            }
            Result<Page<Object>> page = pages.next();
            raw = page;
            if (page instanceof Result.Err<Page<Object>> err) {
                done = true;
                return new Result.Err<>(err.error(), err.partial(), err.hasPartial());
            }
            Result.Ok<Page<Object>> ok = (Result.Ok<Page<Object>>) page;
            List<T> items = new ArrayList<>();
            List<Object> rawItems = ok.value().items();
            for (int i = 0; i < rawItems.size(); i++) {
                try {
                    items.add(item.apply(rawItems.get(i)));
                } catch (Decode.Failure f) {
                    done = true;
                    TungstenException e = mismatch(core, op.id(), ".items[" + i + "]", f, rawItems.get(i), ok.meta());
                    raw = e.toResult();
                    return e.toResult();
                }
            }
            return new Result.Ok<>(new Page<>(items, ok.value().body(), ok.value().next()), true, ok.raw(), ok.meta(), null);
        }

        /** Every page with its items as decoded JSON, each checked to decode as T. */
        public List<Result<Page<Object>>> allRaw() {
            List<Result<Page<Object>>> out = new ArrayList<>();
            while (hasNext()) {
                next();
                out.add(raw);
            }
            return out;
        }

        /** Every typed page; an error ends the list with it. */
        public List<Result<Page<T>>> all() {
            List<Result<Page<T>>> out = new ArrayList<>();
            while (hasNext()) {
                out.add(next());
            }
            return out;
        }
    }
}
