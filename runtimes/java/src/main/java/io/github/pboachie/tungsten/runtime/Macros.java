// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/** Running and previewing compiled macros. */
final class Macros {
    static final double DEFAULT_BUDGET_MS = 30000;
    static final double PAGE_LIMIT = 100;

    private Macros() {}

    private record PlanStep(MacroDescriptor.Step step, OperationDescriptor op) {}

    private record Plan(List<PlanStep> steps, Safety safety) {}

    private static boolean usesKey(OperationDescriptor op) {
        String p = op.agent().idempotency().policy();
        return p.equals("caller_owned") || p.equals("auto");
    }

    private static TungstenException invalid(ClientCore c, String name, String remediation) {
        return c.diag(name, Category.VALIDATION_FAILED).param("macro").expected("a valid macro descriptor")
                .remedy(remediation).fail();
    }

    private static JsonObject input(ClientCore c, MacroDescriptor m, Object raw) {
        Object converted;
        try {
            converted = Json.normalize(raw);
        } catch (IllegalArgumentException e) {
            converted = raw;
        }
        if (Json.nestsDeeperThan(converted, ClientCore.MAX_ARG_DEPTH)) {
            throw c.diag(m.name(), Category.VALIDATION_FAILED).param("input")
                    .expected("a JSON value nested at most " + ClientCore.MAX_ARG_DEPTH + " levels")
                    .remedy("Pass the input of " + m.name() + " without such deep nesting.").fail();
        }
        JsonObject effective;
        if (converted == null) {
            effective = new JsonObject();
        } else if (converted instanceof JsonObject) {
            effective = ((JsonObject) converted).copy();
        } else {
            throw c.diag(m.name(), Category.VALIDATION_FAILED).param("input").expected("an object")
                    .remedy("Pass the input of " + m.name() + " as one object.").fail();
        }
        JsonObject add = m.input().add();
        if (add != null) {
            for (String key : add.keys()) {
                if (effective.has(key)) {
                    continue;
                }
                if (add.get(key) instanceof JsonObject && ((JsonObject) add.get(key)).has("default")) {
                    effective.set(key, ((JsonObject) add.get(key)).get("default"));
                }
            }
        }
        return effective;
    }

    private static Plan plan(ClientCore c, MacroDescriptor m) {
        Safety safety = m.safety();
        List<PlanStep> steps = new ArrayList<>();
        for (int i = 0; i < m.steps().size(); i++) {
            MacroDescriptor.Step step = m.steps().get(i);
            OperationDescriptor op = c.operation(step.operation());
            if (op == null) {
                throw invalid(c, m.name(), "Step " + (i + 1) + " of " + m.name() + " names " + step.operation()
                        + ", which is not registered with the client.");
            }
            if (op.agent().safety().rank() > safety.rank()) {
                safety = op.agent().safety();
            }
            steps.add(new PlanStep(step, op));
        }
        return new Plan(steps, safety);
    }

    private static Object stepArgs(MacroDescriptor m, MacroDescriptor.Step step, OperationDescriptor op, JsonObject sc,
            Set<String> pending) {
        Object source = step.args() != null ? step.args() : new JsonObject();
        Object evaluated = pending != null ? Expr.dry(source, sc, pending, 0) : Expr.evaluate(source, sc, 0);
        if (evaluated == Json.ABSENT || evaluated == null) {
            evaluated = new JsonObject();
        }
        // The evaluated arguments may share members with the macro input: copy before changing.
        evaluated = Json.deepCopy(evaluated);
        if (evaluated instanceof JsonObject && op.id().equals(m.input().extendsOperation()) && m.input().add() != null) {
            for (String key : m.input().add().keys()) {
                ((JsonObject) evaluated).remove(key);
            }
        }
        return evaluated;
    }

    private static long pageLimit(MacroDescriptor.Step step) {
        Object v = step.maxPages() != null ? step.maxPages() : Json.ABSENT;
        return (long) Math.floor(Args.bounded(v, PAGE_LIMIT, 1, 10000));
    }

    private static CallOptions stepOptions(CallOptions opts, boolean keepKey) {
        CallOptions.Builder b = opts.toBuilder().confirm(Confirm.NONE).verify(false);
        if (!keepKey) {
            b.idempotencyKey(null);
        }
        return b.build();
    }

    static Result.Ok<PreviewResult> preview(ClientCore c, MacroDescriptor m, Object raw, CallOptions opts) {
        String name = m.name();
        Plan plan = plan(c, m);
        JsonObject effective = input(c, m, raw);
        if (plan.steps().isEmpty()) {
            throw invalid(c, name, name + " has no steps; regenerate the SDK.");
        }
        JsonObject sc = new JsonObject();
        sc.set("input", effective);
        Set<String> pending = new HashSet<>();
        List<PreviewResult.StepPreview> previews = new ArrayList<>();
        List<String> effects = new ArrayList<>();
        if (!m.summary().isEmpty()) {
            effects.add(m.summary());
        }
        boolean keyUsed = false;
        for (int index = 0; index < plan.steps().size(); index++) {
            MacroDescriptor.Step step = plan.steps().get(index).step();
            OperationDescriptor op = plan.steps().get(index).op();
            String where = "step " + (index + 1) + " of " + name + ": " + op.id();
            Object args = stepArgs(m, step, op, sc, pending);
            boolean keep = usesKey(op) && !keyUsed;
            if (keep && opts.hasKey()) {
                keyUsed = true;
            }
            CallOptions stepOpts = stepOptions(opts, keep);
            PreviewResult.RenderedRequest request = null;
            if (args instanceof JsonObject) {
                JsonObject obj = (JsonObject) args;
                boolean unrenderable = op.body() != null
                        && (op.body().encoding().equals("bytes") || op.body().encoding().equals("multipart"))
                        && Expr.containsPlaceholder(obj);
                if (!unrenderable) {
                    ClientCore.Prepared p;
                    try {
                        p = c.prepare(op, obj, stepOpts, ClientCore.Purpose.MACRO_PREVIEW, null, null);
                    } catch (TungstenException e) {
                        Diagnostic d = e.diagnostic();
                        throw new TungstenException(d.withOperation(name).withAdvice(d.remediation() + " (" + where + ")",
                                d.retryable(), d.nextAction()), e.partial(), e.hasPartial());
                    }
                    request = new PreviewResult.RenderedRequest(p.method, Args.unescapePlaceholders(p.displayUrl, obj),
                            p.headers.toMap(true), p.displayBody);
                }
            } else if (!Expr.containsPlaceholder(args)) {
                throw c.diag(name, Category.VALIDATION_FAILED).param("input").expected("an object")
                        .remedy("Step " + (index + 1) + " of " + name + " does not evaluate to an argument object.").fail();
            }
            List<String> own = new ArrayList<>();
            OperationDescriptor.Confirmation conf = op.agent().confirmation();
            if (conf != null && conf.message() != null && !conf.message().isEmpty()) {
                own.add(Args.interpolate(conf.message(), args instanceof JsonObject ? (JsonObject) args : new JsonObject(), op));
            }
            if (step.kind().equals("poll")) {
                String until = step.until() != null ? Expr.describe(step.until()) : "";
                double budget = Args.bounded(Expr.evaluate(step.budgetMs(), sc, 0), DEFAULT_BUDGET_MS, 0, Json.MAX_SAFE_INTEGER);
                double interval = Args.bounded(step.intervalMs() != null ? step.intervalMs() : Json.ABSENT,
                        Previews.DEFAULT_POLL_INTERVAL_MS, 0, Json.MAX_SAFE_INTEGER);
                own.add("Repeats " + op.id() + " every " + Json.jsNumber(interval) + " ms"
                        + (until.isEmpty() ? "" : " until " + until) + ", for at most " + Json.jsNumber(budget) + " ms.");
            } else if (step.kind().equals("paginate")) {
                own.add("Reads up to " + pageLimit(step) + " pages of " + op.id() + ".");
            }
            if (op.agent().remediationNote() != null && !op.agent().remediationNote().isEmpty()) {
                own.add(op.agent().remediationNote());
            }
            effects.add("Step " + (index + 1) + ": " + step.kind() + " " + op.id() + " (" + op.agent().safety().wire() + ").");
            effects.addAll(own);
            previews.add(new PreviewResult.StepPreview(index + 1, step.kind(), op.id(), step.as(), op.agent().safety(), request,
                    List.copyOf(own)));
            if (step.as() != null && !step.as().isEmpty()) {
                pending.add(step.as());
            }
        }
        if (m.shownOnce()) {
            effects.add("The result contains values the API shows only once; store them immediately.");
        }
        if (previews.get(0).request() == null) {
            throw c.diag(name, Category.VALIDATION_FAILED).param("input").expected("an object")
                    .remedy("Step 1 of " + name + " cannot be rendered.").fail();
        }
        String token = null;
        Long expires = null;
        if (plan.safety() != Safety.READ_ONLY) {
            token = Tokens.issue(c.confirmationKey, "macro:" + name, effective, c.now());
            expires = Tokens.TTL_MS;
        }
        PreviewResult r = new PreviewResult(name, plan.safety(), previews.get(0).request(), List.copyOf(effects), token, expires,
                null, false, List.copyOf(previews));
        return new Result.Ok<>(r, true, null, ResponseMeta.local(), null);
    }

    /**
     * Running the step again (by rerunning its macro with the same input and options) answers
     * the first result instead of applying the effect twice.
     */
    private static boolean rerunProtected(OperationDescriptor op, CallOptions opts) {
        String policy = op.agent().idempotency().policy();
        if (policy.equals("content_identity") || policy.equals("content_hash") || policy.equals("auto")) {
            return true;
        }
        if (!opts.hasKey()) {
            return false;
        }
        if (policy.equals("caller_owned")) {
            return true;
        }
        for (OperationDescriptor.Param p : op.params()) {
            if (p.role().equals("idempotency_key")) {
                return true;
            }
        }
        return false;
    }

    /**
     * The replay protection a macro's token is bound to: the run's key when a step sends it and
     * every mutating step is protected against a rerun; otherwise none (null).
     */
    private static String bind(Plan plan, CallOptions opts) {
        if (!opts.hasKey()) {
            return null;
        }
        boolean sentKey = false;
        for (PlanStep ps : plan.steps()) {
            boolean keep = usesKey(ps.op()) && !sentKey;
            if (keep) {
                sentKey = true;
            }
            if (ps.op().isMutation() && !rerunProtected(ps.op(), stepOptions(opts, keep))) {
                return null;
            }
        }
        return sentKey ? opts.idempotencyKey() : null;
    }

    static Result.Ok<Object> run(ClientCore c, MacroDescriptor m, Object raw, CallOptions opts) {
        String name = m.name();
        Plan plan = plan(c, m);
        JsonObject effective = input(c, m, raw);
        c.confirmed(new ClientCore.ConfirmTarget(name, "macro:" + name, "the macro's preview(...)"), plan.safety(), effective, opts);
        String bind = bind(plan, opts);
        ClientCore.MacroClaim claim = new ClientCore.MacroClaim(name, opts.confirm().tokenValue(), bind, bind != null);
        JsonObject sc = new JsonObject();
        sc.set("input", effective);
        List<String> completed = new ArrayList<>();
        JsonObject produced = new JsonObject();
        List<String> unprotected = new ArrayList<>();
        boolean keyUsed = false;
        ResponseMeta meta = ResponseMeta.local();
        int total = plan.steps().size();
        for (int index = 0; index < total; index++) {
            MacroDescriptor.Step step = plan.steps().get(index).step();
            OperationDescriptor op = plan.steps().get(index).op();
            Object args = stepArgs(m, step, op, sc, null);
            if (!(args instanceof JsonObject)) {
                throw c.diag(name, Category.VALIDATION_FAILED).param("input").expected("an object")
                        .remedy("Step " + (index + 1) + " of " + name + " does not evaluate to an argument object.").fail();
            }
            boolean keep = usesKey(op) && !keyUsed;
            if (keep && opts.hasKey()) {
                keyUsed = true;
            }
            CallOptions stepOpts = stepOptions(opts, keep);
            Object value = null;
            boolean hasValue = false;
            TungstenException failure = null;
            try {
                if (step.kind().equals("poll")) {
                    double budget = Args.bounded(Expr.evaluate(step.budgetMs(), sc, 0), DEFAULT_BUDGET_MS, 0, Json.MAX_SAFE_INTEGER);
                    double interval = Args.bounded(step.intervalMs() != null ? step.intervalMs() : Json.ABSENT,
                            Previews.DEFAULT_POLL_INTERVAL_MS, 0, Json.MAX_SAFE_INTEGER);
                    Object until = step.until() instanceof JsonObject ? step.until() : new JsonObject();
                    Result.Ok<Polled> polled = Previews.poll(c, op, args, until, Previews.millis(interval),
                            Previews.millis(budget), stepOpts, claim);
                    if (polled.value().timedOut()) {
                        hasValue = true;
                    } else {
                        value = polled.value().body();
                        hasValue = polled.value().hasBody();
                    }
                    meta = polled.meta();
                } else if (step.kind().equals("paginate")) {
                    List<Object> items = new ArrayList<>();
                    long limit = pageLimit(step);
                    long pages = 0;
                    Pages.State st = new Pages.State(args);
                    while (true) {
                        Result<Page<Object>> page = Pages.step(c, op, st, stepOpts, claim);
                        if (page == null) {
                            break;
                        }
                        if (page instanceof Result.Err<Page<Object>> err) {
                            failure = new TungstenException(err.error(), err.partial(), err.hasPartial());
                            break;
                        }
                        Result.Ok<Page<Object>> ok = (Result.Ok<Page<Object>>) page;
                        items.addAll(ok.value().items());
                        meta = ok.meta();
                        pages++;
                        if (pages >= limit) {
                            break;
                        }
                    }
                    value = items;
                    hasValue = true;
                } else {
                    Result.Ok<Object> response = c.callWith(op, args, stepOpts, null, claim);
                    value = response.value();
                    hasValue = response.hasValue();
                    meta = response.meta();
                }
            } catch (TungstenException e) {
                failure = e;
            }
            if (failure != null) {
                throw stopped(failure, m, step, op, index, total, completed, produced, unprotected);
            }
            completed.add(op.id());
            if (step.as() != null && !step.as().isEmpty()) {
                if (hasValue) {
                    sc.set(step.as(), value);
                    produced.set(step.as(), value);
                } else {
                    sc.remove(step.as());
                    produced.set(step.as(), null);
                }
            }
            if (op.isMutation() && !rerunProtected(op, stepOpts)) {
                unprotected.add(op.id());
            }
        }
        Object output = Expr.evaluate(m.output(), sc, 0);
        boolean has = output != Json.ABSENT;
        Object value = has ? output : null;
        return new Result.Ok<>(value, has, value, meta, null);
    }

    private static TungstenException stopped(TungstenException failure, MacroDescriptor m, MacroDescriptor.Step step,
            OperationDescriptor op, int index, int total, List<String> completed, JsonObject produced, List<String> unprotected) {
        String name = m.name();
        Diagnostic d = failure.diagnostic();
        if (failure.hasPartial() && step.as() != null && !step.as().isEmpty()) {
            produced.set(step.as(), failure.partial());
        }
        String done = completed.isEmpty() ? "no step completed before it" : "completed before it: " + String.join(", ", completed);
        String remediation = d.remediation() + " Macro " + name + " stopped at step " + (index + 1) + " of " + total + " ("
                + op.id() + "); " + done + ".";
        Retryable retryable = d.retryable();
        String next = d.nextAction();
        if (!produced.isEmpty()) {
            String once = m.shownOnce() ? ", including values the API shows only once: store them now" : "";
            remediation += " The result's partial holds the completed steps' results (" + String.join(", ", produced.keys()) + ")"
                    + once + ".";
        }
        if (!unprotected.isEmpty()) {
            remediation += " Do not run " + name + " again: " + String.join(", ", unprotected)
                    + " already took effect and has no replay protection.";
            if (retryable == Retryable.AFTER_DELAY || retryable == Retryable.SAME_KEY_ONLY) {
                retryable = Retryable.AFTER_REMEDIATION;
            }
            String finish = "Finish " + name + " without rerunning it: call " + op.id() + " yourself with the values from the result's partial.";
            next = next == null ? finish : next + " " + finish;
        }
        Diagnostic out = d.withOperation(name).withAdvice(remediation, retryable, next);
        return produced.isEmpty() ? new TungstenException(out) : new TungstenException(out, produced, true);
    }
}
