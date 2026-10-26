// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.net.URI;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.Iterator;
import java.util.List;
import java.util.NoSuchElementException;
import java.util.Set;

/**
 * The pages of a paginated operation, fetched one by one as the iteration advances. Each element
 * is a {@link Result}: the first {@link Result.Err} ends the iteration.
 */
public final class Pages implements Iterable<Result<Page<Object>>>, Iterator<Result<Page<Object>>> {
    private final ClientCore core;
    private final OperationDescriptor op;
    private final CallOptions opts;
    private final State state;
    private Result<Page<Object>> ahead;
    /** The traversal's total deadline, fixed when it was created; null without one. */
    private final Long deadline;

    Pages(ClientCore core, OperationDescriptor op, Object args, CallOptions opts) {
        this.core = core;
        this.op = op;
        this.opts = opts;
        this.state = new State(args);
        this.deadline = core.deadlineAt(opts);
    }

    /** The paginated operation. */
    public OperationDescriptor operation() {
        return op;
    }

    @Override
    public Iterator<Result<Page<Object>>> iterator() {
        return this;
    }

    @Override
    public boolean hasNext() {
        if (ahead == null) {
            ahead = ClientCore.withDeadline(deadline, () -> step(core, op, state, opts, null));
        }
        return ahead != null;
    }

    @Override
    public Result<Page<Object>> next() {
        if (!hasNext()) {
            throw new NoSuchElementException();
        }
        Result<Page<Object>> out = ahead;
        ahead = null;
        return out;
    }

    /** Collects every page; an error ends the list with it. */
    public List<Result<Page<Object>>> all() {
        List<Result<Page<Object>>> out = new ArrayList<>();
        while (hasNext()) {
            out.add(next());
        }
        return out;
    }

    /** The iteration state of one pagination. */
    static final class State {
        Object args;
        String overrideUrl;
        final java.util.Set<String> seen = new java.util.HashSet<>();
        final ClientCore.PageTrace trace = new ClientCore.PageTrace();
        boolean started;
        boolean done;
        TungstenException pendingError;

        State(Object raw) {
            Object converted;
            try {
                converted = Json.normalize(raw);
            } catch (IllegalArgumentException e) {
                converted = raw;
            }
            if (converted == null) {
                converted = new JsonObject();
            }
            args = converted instanceof JsonObject ? ((JsonObject) converted).copy() : converted;
        }

        void setArg(String key, Object value) {
            if (args instanceof JsonObject) {
                ((JsonObject) args).set(key, value);
            }
        }

        Object arg(String key) {
            return args instanceof JsonObject && ((JsonObject) args).has(key) ? ((JsonObject) args).get(key) : Json.ABSENT;
        }
    }

    private static Object asNumber(double n) {
        return Json.normalizeDouble(n);
    }

    /** The next page, an error that ends the iteration, or null at the end. */
    static Result<Page<Object>> step(ClientCore c, OperationDescriptor op, State st, CallOptions opts, ClientCore.MacroClaim claim) {
        if (st.pendingError != null) {
            TungstenException e = st.pendingError;
            st.pendingError = null;
            st.done = true;
            return e.toResult();
        }
        if (st.done) {
            return null;
        }
        OperationDescriptor.Pagination first = op.pagination();
        if (!st.started) {
            st.started = true;
            if (first != null && "cursor".equals(first.style())) {
                Object initial = st.arg(Args.argName(op, first.requestParam()));
                if (usable(initial)) {
                    st.seen.add(Json.canonical(initial));
                }
            }
        }
        boolean linked = first != null && "link_header".equals(first.style());
        Result.Ok<Object> response;
        try {
            response = c.callWith(op, st.args, opts, st.overrideUrl, claim, linked ? st.trace : null);
        } catch (TungstenException e) {
            st.done = true;
            return e.toResult();
        }
        Object body = response.hasValue() ? response.value() : Json.ABSENT;
        OperationDescriptor.Pagination pg = op.pagination();
        String itemsField = pg != null ? pg.itemsField() : "";
        Object found = itemsField.isEmpty() ? body : Json.at(body, itemsField);
        List<Object> items = new ArrayList<>();
        if (found instanceof List<?>) {
            items.addAll((List<?>) found);
        }
        Object next = null;
        if (pg != null) {
            switch (pg.style()) {
                case "cursor": {
                    Object cursor = Json.ABSENT;
                    if (pg.responseField() != null && !pg.responseField().isEmpty()) {
                        Object v = Json.at(body, pg.responseField());
                        if (usable(v)) {
                            cursor = v;
                        }
                    }
                    if (cursor == Json.ABSENT && pg.cursorItemField() != null && !pg.cursorItemField().isEmpty() && !items.isEmpty()) {
                        Object v = Json.at(items.get(items.size() - 1), pg.cursorItemField());
                        if (usable(v)) {
                            cursor = v;
                        }
                    }
                    boolean finished = false;
                    if (pg.hasMoreField() != null && !pg.hasMoreField().isEmpty()) {
                        finished = Boolean.FALSE.equals(Json.at(body, pg.hasMoreField()));
                    }
                    if (cursor != Json.ABSENT && !finished) {
                        next = cursor;
                        st.setArg(Args.argName(op, pg.requestParam()), cursor);
                    }
                    break;
                }
                case "offset": {
                    String offsetKey = Args.argName(op, pg.offsetParam());
                    Object limit = st.arg(Args.argName(op, pg.limitParam()));
                    double offset = Args.bounded(st.arg(offsetKey), 0, 0, Json.MAX_SAFE_INTEGER);
                    boolean shortPage = Json.isNumber(limit) && items.size() < Json.asDouble(limit);
                    if (!items.isEmpty() && !shortPage) {
                        Object v = asNumber(offset + items.size());
                        st.setArg(offsetKey, v);
                        next = v;
                    }
                    break;
                }
                case "page": {
                    String pageKey = Args.argName(op, pg.pageParam());
                    Object size = st.arg(Args.argName(op, pg.sizeParam()));
                    double page = Args.bounded(st.arg(pageKey), 1, 0, Json.MAX_SAFE_INTEGER);
                    boolean shortPage = Json.isNumber(size) && items.size() < Json.asDouble(size);
                    if (!items.isEmpty() && !shortPage) {
                        Object v = asNumber(page + 1);
                        st.setArg(pageKey, v);
                        next = v;
                    }
                    break;
                }
                case "link_header": {
                    String link = Text.nextLink(response.meta().headers().get("link"));
                    if (link == null) {
                        break;
                    }
                    String base = st.trace.lastUrl != null ? st.trace.lastUrl : st.overrideUrl != null ? st.overrideUrl : c.baseUrl;
                    st.seen.add(base);
                    URI resolved = Urls.resolve(base, link);
                    URI origin = null;
                    try {
                        origin = URI.create(base);
                    } catch (IllegalArgumentException e) {
                        // not followed below
                    }
                    if (resolved != null && origin != null && origin.getScheme() != null && Urls.sameOrigin(origin, resolved)) {
                        String target = resolved.toString();
                        int hash = target.indexOf('#');
                        st.overrideUrl = hash < 0 ? target : target.substring(0, hash);
                        next = st.overrideUrl;
                        break;
                    }
                    st.pendingError = c.diag(op.id(), Category.UNEXPECTED_RESPONSE).status(response.meta().status())
                            .requestId(response.meta().requestId())
                            .remedy("The next-page Link header points to another origin; it is not followed so credentials never leave the API's host. Stop paginating here.")
                            .attempts(response.meta().attempts()).fail();
                    break;
                }
                default:
                    break;
            }
        }
        if (next != null && st.pendingError == null) {
            String key = pg != null && "link_header".equals(pg.style()) ? (String) next : Json.canonical(next);
            if (!st.seen.add(key)) {
                next = null;
                st.pendingError = c.diag(op.id(), Category.UNEXPECTED_RESPONSE).status(response.meta().status())
                        .requestId(response.meta().requestId())
                        .remedy("Pagination detected a repeated cursor or page URL. Fix the server's next-page value before continuing.")
                        .attempts(response.meta().attempts()).fail();
            }
        }
        TungstenException bad = checkItems(c, op, itemsField, items, response);
        if (bad != null) {
            st.done = true;
            return bad.toResult();
        }
        if (next == null) {
            st.done = true;
        }
        return new Result.Ok<>(new Page<>(items, response.value(), next), true, response.value(), response.meta(), null);
    }

    private static boolean usable(Object v) {
        return v != Json.ABSENT && v != null && !"".equals(v);
    }

    /** Validates page items with the operation's page_item schema. */
    private static TungstenException checkItems(ClientCore c, OperationDescriptor op, String itemsField, List<Object> items,
            Result.Ok<Object> response) {
        if (op.pageItem() == null || c.validateResponses == ClientOptions.ValidateResponses.OFF) {
            return null;
        }
        Set<String> seen = new HashSet<>();
        for (int index = 0; index < items.size(); index++) {
            List<Schema.Issue> issues = c.judge(op.id(), "response" + (itemsField.isEmpty() ? "" : "." + itemsField) + "[" + index + "]",
                    op.pageItem(), items.get(index), seen);
            if (issues.isEmpty()) {
                continue;
            }
            Schema.Issue issue = issues.get(0);
            String path = "response" + (itemsField.isEmpty() ? "" : "." + itemsField) + "[" + index + "]"
                    + Schema.formatPath(issue.path());
            Diagnostic d = c.diag(op.id(), Category.UNEXPECTED_RESPONSE).status(response.meta().status())
                    .requestId(response.meta().requestId()).param(path).expected(issue.message())
                    .remedy("A page item does not match the API description at " + path + " (" + issue.message() + ").")
                    .attempts(response.meta().attempts()).build();
            if (c.validateResponses == ClientOptions.ValidateResponses.STRICT) {
                return new TungstenException(d);
            }
            c.emit(d);
            return null;
        }
        return null;
    }
}
