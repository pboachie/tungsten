// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.time.Duration;
import java.util.ArrayList;
import java.util.List;

/** Previews, polling and verification hooks. */
final class Previews {
    static final double DEFAULT_POLL_INTERVAL_MS = 1000;
    static final double DEFAULT_VERIFY_BUDGET_MS = 30000;

    private Previews() {}

    static Result.Ok<PreviewResult> preview(ClientCore c, OperationDescriptor op, Object args, CallOptions opts) {
        ClientCore.Prepared p = c.prepare(op, args, opts, ClientCore.Purpose.PREVIEW, null, null);
        List<String> effects = new ArrayList<>();
        OperationDescriptor.Confirmation conf = op.agent().confirmation();
        if (conf != null && conf.message() != null && !conf.message().isEmpty()) {
            effects.add(Args.interpolate(conf.message(), p.args, op));
        }
        if (op.agent().remediationNote() != null && !op.agent().remediationNote().isEmpty()) {
            effects.add(op.agent().remediationNote());
        }
        if (conf != null && !conf.summaryFields().isEmpty()) {
            List<String> fields = new ArrayList<>();
            for (String f : conf.summaryFields()) {
                fields.add(f + "={" + f + "}");
            }
            effects.add(Args.interpolate("Arguments: " + String.join(", ", fields) + ".", p.args, op));
        }
        if (op.summary() != null && !op.summary().isEmpty()) {
            effects.add(op.summary());
        }
        String token = null;
        Long expires = null;
        if (op.agent().safety() != Safety.READ_ONLY) {
            token = Tokens.issue(c.confirmationKey, op.id(), p.args, c.now());
            expires = Tokens.TTL_MS;
        }
        PreviewResult.RenderedRequest request =
                new PreviewResult.RenderedRequest(p.method, p.displayUrl, p.headers.toMap(true), p.displayBody);
        switch (op.agent().preview().mode()) {
            case "header": {
                ClientCore.Prepared dry = c.prepare(op, args, opts, ClientCore.Purpose.SERVER_PREVIEW, null, null);
                Result.Ok<Object> answer = c.send(dry, opts);
                Object server = answer.hasValue() ? Text.redactPaths(answer.value(), op.agent().sensitiveResponseFields()) : null;
                PreviewResult r = new PreviewResult(op.id(), op.agent().safety(), request, List.copyOf(effects), token, expires,
                        server, answer.hasValue(), null);
                return new Result.Ok<>(r, true, null, answer.meta(), null);
            }
            case "endpoint": {
                String name = op.agent().preview().operation();
                OperationDescriptor target = name == null ? null : c.operation(name);
                if (target == null) {
                    throw c.diag(op.id(), Category.VALIDATION_FAILED).param("operation")
                            .expected("the preview operation " + name + " registered with the client")
                            .remedy("Register " + name + " (ClientCore.register) so preview() can call it; nothing was sent.")
                            .fail();
                }
                CallOptions rest = opts.toBuilder().confirm(Confirm.NONE).verify(false).build();
                Result.Ok<Object> answer = c.callWith(target, args, rest, null, null);
                PreviewResult r = new PreviewResult(op.id(), op.agent().safety(), request, List.copyOf(effects), token, expires,
                        answer.value(), answer.hasValue(), null);
                return new Result.Ok<>(r, true, null, answer.meta(), null);
            }
            default: {
                PreviewResult r = new PreviewResult(op.id(), op.agent().safety(), request, List.copyOf(effects), token, expires,
                        null, false, null);
                return new Result.Ok<>(r, true, null, ResponseMeta.local(), null);
            }
        }
    }

    /** Stands in for a verification reference that did not resolve; it equals nothing a response holds. */
    private static JsonObject unresolved() {
        return JsonObject.of("$tungsten_unresolved", true);
    }

    private static Object resolveRefs(Object node, JsonObject sc, int depth) {
        if (depth > 64) {
            return unresolved();
        }
        if (node instanceof String) {
            String t = (String) node;
            if (t.startsWith("$")) {
                Object v = Expr.ref(t, sc);
                return v != Json.ABSENT && v != null ? v : unresolved();
            }
            return t;
        }
        if (node instanceof List<?>) {
            List<Object> out = new ArrayList<>();
            for (Object item : (List<?>) node) {
                out.add(resolveRefs(item, sc, depth + 1));
            }
            return out;
        }
        if (node instanceof JsonObject) {
            JsonObject t = (JsonObject) node;
            JsonObject out = new JsonObject();
            for (String k : t.keys()) {
                out.set(k, resolveRefs(t.get(k), sc, depth + 1));
            }
            return out;
        }
        return node;
    }

    static Result.Ok<Polled> poll(ClientCore c, OperationDescriptor op, Object args, Object until, Duration interval,
            Duration budget, CallOptions opts, ClientCore.MacroClaim claim) {
        long started = System.nanoTime();
        CallOptions once = opts.toBuilder().verify(false).build();
        while (true) {
            Result.Ok<Object> response = c.callWith(op, args, once, null, claim);
            Object value = response.hasValue() ? response.value() : Json.ABSENT;
            boolean done = Expr.predicate(until, value);
            boolean outOfTime = !done && Duration.ofNanos(System.nanoTime() - started).plus(interval).compareTo(budget) > 0;
            if (done || outOfTime) {
                return new Result.Ok<>(new Polled(response.value(), response.hasValue(), outOfTime), true, response.raw(),
                        response.meta(), null);
            }
            if (!ClientCore.sleep(interval.toNanos() / 1e6)) {
                throw c.diag(op.id(), Category.TRANSPORT_FAILED).retry(Retryable.NEVER)
                        .remedy("The poll was interrupted before its predicate held.").fail();
            }
        }
    }

    static Duration millis(double ms) {
        if (Double.isNaN(ms) || ms <= 0) {
            return Duration.ZERO;
        }
        return Duration.ofNanos((long) Math.min(ms * 1e6, 4.0e18));
    }

    /** Runs the operation's verification hook after a success; value is ABSENT without a body. */
    static Verification verify(ClientCore c, OperationDescriptor op, JsonObject args, Object value, CallOptions opts) {
        OperationDescriptor.Verify hook = op.agent().verify();
        if (hook == null) {
            return Verification.unchecked(null);
        }
        OperationDescriptor target = c.operation(hook.operation());
        if (target == null) {
            return Verification.unchecked(null);
        }
        // The hook is written against the API: argument keys and $args references use wire names.
        JsonObject sc = new JsonObject();
        if (value != Json.ABSENT) {
            sc.set("response", value);
        }
        sc.set("args", Args.withWireNames(op, args));
        Object hookArgs = hook.args() != null ? hook.args() : new JsonObject();
        Object evaluated = Expr.evaluate(hookArgs, sc, 0);
        if (!(evaluated instanceof JsonObject)) {
            return Verification.unchecked(null);
        }
        JsonObject mapped = new JsonObject();
        for (String k : ((JsonObject) evaluated).keys()) {
            mapped.set(Args.argName(target, k), ((JsonObject) evaluated).get(k));
        }
        Object terminal = hook.terminal() instanceof JsonObject && !((JsonObject) hook.terminal()).isEmpty()
                ? resolveRefs(hook.terminal(), sc, 0)
                : null;
        Object expect = hook.expect() instanceof JsonObject && !((JsonObject) hook.expect()).isEmpty()
                ? resolveRefs(hook.expect(), sc, 0)
                : new JsonObject();
        Duration interval = millis(hook.pollIntervalMs() != null ? hook.pollIntervalMs() : DEFAULT_POLL_INTERVAL_MS);
        Duration budget;
        if (hook.pollBudgetMs() != null) {
            budget = millis(hook.pollBudgetMs());
        } else {
            budget = terminal != null ? millis(DEFAULT_VERIFY_BUDGET_MS) : Duration.ZERO;
        }
        CallOptions rest = opts.toBuilder().confirm(Confirm.NONE).idempotencyKey(null).verify(false).build();
        Result.Ok<Polled> polled;
        try {
            polled = poll(c, target, mapped, terminal != null ? terminal : expect, interval, budget, rest, null);
        } catch (TungstenException e) {
            return Verification.unchecked(e.diagnostic());
        }
        Polled body = polled.value();
        boolean passed = Expr.predicate(expect, body.hasBody() ? body.body() : Json.ABSENT);
        Object observed = body.hasBody() ? Text.redactPaths(body.body(), target.agent().sensitiveResponseFields()) : null;
        return new Verification(true, passed, observed, body.hasBody(), !budget.isZero() && body.timedOut(), null);
    }
}
