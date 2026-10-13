// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

public sealed partial class ClientCore
{
    private const double DefaultMacroBudgetMs = 30000.0;
    private const double MacroPageLimit = 100.0;

    private static int SafetyRank(Safety s) => s switch
    {
        Safety.Mutating => 1,
        Safety.Destructive => 2,
        Safety.Irreversible => 3,
        _ => 0,
    };

    private static bool UsesKey(OperationDescriptor op) =>
        op.Agent.Idempotency.Policy is IdempotencyKind.CallerOwned or IdempotencyKind.Auto;

    private static Failure MacroInvalid(string name, string remediation) =>
        new Diag(name, Category.ValidationFailed).Param("macro").Expected("a valid macro descriptor").Remedy(remediation).Fail();

    private static JsonObject MacroInputOf(MacroDescriptor m, JsonNode? input)
    {
        if (Util.NestsDeeperThan(input, Util.MaxArgDepth))
        {
            throw new Diag(m.Name, Category.ValidationFailed).Param("input")
                .Expected($"a JSON value nested at most {Util.MaxArgDepth} levels")
                .Remedy($"Pass the input of {m.Name} without such deep nesting.").Fail();
        }
        JsonObject effective;
        if (input is null)
        {
            effective = new JsonObject();
        }
        else if (input is JsonObject o)
        {
            effective = (JsonObject)Json.Normalize(o)!;
        }
        else
        {
            throw new Diag(m.Name, Category.ValidationFailed).Param("input").Expected("an object")
                .Remedy($"Pass the input of {m.Name} as one object.").Fail();
        }
        foreach (var kv in m.Input.Add)
        {
            if (effective.ContainsKey(kv.Key))
            {
                continue;
            }
            if (kv.Value is JsonObject schema && schema.TryGetPropertyValue("default", out var def))
            {
                Json.Put(effective, kv.Key, def);
            }
        }
        return effective;
    }

    private (List<(MacroStep Step, OperationDescriptor Op)> Steps, Safety Safety) MacroPlan(MacroDescriptor m)
    {
        var safety = m.Safety;
        var steps = new List<(MacroStep, OperationDescriptor)>();
        for (int i = 0; i < m.Steps.Count; i++)
        {
            var step = m.Steps[i];
            var op = Operation(step.Operation) ?? throw MacroInvalid(m.Name, $"Step {i + 1} of {m.Name} names {step.Operation}, which is not registered with the client.");
            if (SafetyRank(op.Agent.Safety) > SafetyRank(safety))
            {
                safety = op.Agent.Safety;
            }
            steps.Add((step, op));
        }
        return (steps, safety);
    }

    private static JsonNode? StepArgs(MacroDescriptor m, MacroStep step, OperationDescriptor op, JsonObject sc, ISet<string>? pending)
    {
        var source = step.Args ?? new JsonObject();
        var (evaluated, ok) = pending is not null ? Expr.EvaluateDry(source, sc, pending, 0) : Expr.Evaluate(source, sc, 0);
        if (!ok || evaluated is null)
        {
            evaluated = new JsonObject();
        }
        if (evaluated is JsonObject o && op.Id == m.Input.Extends)
        {
            foreach (var kv in m.Input.Add)
            {
                o.Remove(kv.Key);
            }
        }
        return evaluated;
    }

    private static long PageLimit(MacroStep step) =>
        (long)Math.Floor(Util.Bounded(step.MaxPages is long n ? JsonValue.Create(n) : null, step.MaxPages is not null, MacroPageLimit, 1, 10000));

    private static Failure Renamed(Failure f, string name, string where)
    {
        var d = f.Error.Diagnostic with { Operation = name, Remediation = $"{f.Error.Diagnostic.Remediation} ({where})" };
        return new Failure(f.Error with { Diagnostic = d });
    }

    private async Task<Result<PreviewResult>> PreviewMacroInnerAsync(MacroDescriptor m, JsonNode? input, CallOptions opts, CancellationToken ct)
    {
        var name = m.Name;
        var (steps, safety) = MacroPlan(m);
        var effective = MacroInputOf(m, input);
        if (steps.Count == 0)
        {
            throw MacroInvalid(name, $"{name} has no steps; regenerate the SDK.");
        }
        var rest = opts with { Confirm = null, Verify = false };
        var sc = new JsonObject();
        Json.Put(sc, "input", effective);
        var pending = new HashSet<string>(StringComparer.Ordinal);
        var previews = new List<MacroStepPreview>();
        var effects = new List<string>();
        if (m.Summary.Length > 0)
        {
            effects.Add(m.Summary);
        }
        bool keyUsed = false;
        for (int index = 0; index < steps.Count; index++)
        {
            var (step, op) = steps[index];
            var where = $"step {index + 1} of {name}: {op.Id}";
            var args = StepArgs(m, step, op, sc, pending);
            var stepOpts = rest;
            if (!UsesKey(op) || keyUsed)
            {
                stepOpts = stepOpts with { IdempotencyKey = null };
            }
            else if (!string.IsNullOrEmpty(opts.IdempotencyKey))
            {
                keyUsed = true;
            }
            RenderedRequest? request = null;
            if (args is JsonObject obj)
            {
                bool unrenderable = op.Body is { Encoding: "bytes" or "multipart" } && Expr.ContainsPlaceholder(obj);
                if (!unrenderable)
                {
                    Prepared p;
                    try
                    {
                        p = await PrepareAsync(op, obj, stepOpts, Purpose.MacroPreview, null, null, ct).ConfigureAwait(false);
                    }
                    catch (Failure f)
                    {
                        throw Renamed(f, name, where);
                    }
                    request = new RenderedRequest
                    {
                        Method = p.Method,
                        Url = UnescapePlaceholders(p.DisplayUrl, obj),
                        Headers = p.Headers.ToMap(true),
                        Body = p.DisplayBody,
                    };
                }
            }
            else if (!Expr.ContainsPlaceholder(args))
            {
                throw new Diag(name, Category.ValidationFailed).Param("input").Expected("an object")
                    .Remedy($"Step {index + 1} of {name} does not evaluate to an argument object.").Fail();
            }
            var own = new List<string>();
            if (op.Agent.Confirmation is { Message: { Length: > 0 } message })
            {
                own.Add(Interpolate(message, args as JsonObject ?? new JsonObject(), op));
            }
            switch (step.Kind)
            {
                case "poll":
                    {
                        var until = step.Until is null ? "" : Expr.DescribePredicate(step.Until);
                        var (bv, bok) = Expr.Evaluate(step.BudgetMs, sc, 0);
                        var budget = Util.Bounded(bv, bok, DefaultMacroBudgetMs, 0, Json.MaxSafeInteger);
                        var interval = Util.Bounded(step.IntervalMs is long iv ? JsonValue.Create(iv) : null, step.IntervalMs is not null, DefaultPollIntervalMs, 0, Json.MaxSafeInteger);
                        var untilText = until.Length > 0 ? " until " + until : "";
                        own.Add($"Repeats {op.Id} every {Json.JsNumber(interval)} ms{untilText}, for at most {Json.JsNumber(budget)} ms.");
                        break;
                    }
                case "paginate":
                    own.Add($"Reads up to {PageLimit(step)} pages of {op.Id}.");
                    break;
            }
            if (!string.IsNullOrEmpty(op.Agent.RemediationNote))
            {
                own.Add(op.Agent.RemediationNote);
            }
            effects.Add($"Step {index + 1}: {step.Kind} {op.Id} ({Wire.Of(op.Agent.Safety)}).");
            effects.AddRange(own);
            previews.Add(new MacroStepPreview
            {
                Step = index + 1,
                Kind = step.Kind,
                Operation = op.Id,
                As = step.As,
                Safety = op.Agent.Safety,
                Request = request,
                Effects = own,
            });
            if (!string.IsNullOrEmpty(step.As))
            {
                pending.Add(step.As);
            }
        }
        if (m.ShownOnce)
        {
            effects.Add("The result contains values the API shows only once; store them immediately.");
        }
        if (previews[0].Request is null)
        {
            throw new Diag(name, Category.ValidationFailed).Param("input").Expected("an object")
                .Remedy($"Step 1 of {name} cannot be rendered.").Fail();
        }
        var result = new PreviewResult
        {
            Operation = name,
            Safety = safety,
            Request = previews[0].Request!,
            Effects = effects,
            Steps = previews,
        };
        if (safety != Safety.ReadOnly)
        {
            result = result with
            {
                ConfirmationToken = ConfirmationTokens.Issue(confirmationKey, "macro:" + name, effective, Now()),
                ExpiresInMs = ConfirmationTokens.TtlMs,
            };
        }
        return new Result<PreviewResult>.Ok(result) { Meta = ResponseMeta.Local() };
    }

    /// <summary>Running the step again (by rerunning its macro with the same input and options) answers the first result instead of applying the effect twice.</summary>
    private static bool RerunProtected(OperationDescriptor op, CallOptions opts)
    {
        if (op.Agent.Idempotency.Policy is IdempotencyKind.ContentIdentity or IdempotencyKind.ContentHash or IdempotencyKind.Auto)
        {
            return true;
        }
        if (string.IsNullOrEmpty(opts.IdempotencyKey))
        {
            return false;
        }
        return op.Agent.Idempotency.Policy == IdempotencyKind.CallerOwned || op.Params.Any(p => p.Role == "idempotency_key");
    }

    /// <summary>The replay protection a macro's token is bound to: the run's key when a step sends it and every mutating step is protected against a rerun.</summary>
    private static string? MacroBind(List<(MacroStep Step, OperationDescriptor Op)> plan, CallOptions opts)
    {
        if (string.IsNullOrEmpty(opts.IdempotencyKey))
        {
            return null;
        }
        bool sentKey = false;
        foreach (var (_, op) in plan)
        {
            var stepOpts = opts;
            if (!UsesKey(op) || sentKey)
            {
                stepOpts = stepOpts with { IdempotencyKey = null };
            }
            else
            {
                sentKey = true;
            }
            if (Classify.IsMutation(op) && !RerunProtected(op, stepOpts))
            {
                return null;
            }
        }
        return sentKey ? opts.IdempotencyKey : null;
    }

    private async Task<Result<JsonNode?>> RunMacroInnerAsync(MacroDescriptor m, JsonNode? input, CallOptions opts, CancellationToken ct)
    {
        var name = m.Name;
        var (steps, safety) = MacroPlan(m);
        var effective = MacroInputOf(m, input);
        Confirmed(new ConfirmTarget(name, "macro:" + name, "the macro's preview(...)"), safety, effective, opts);
        var claim = new MacroClaim(name, opts.Confirm?.TokenValue, MacroBind(steps, opts));
        var sc = new JsonObject();
        Json.Put(sc, "input", effective);
        var completed = new List<string>();
        var produced = new JsonObject();
        var unprotected = new List<string>();
        bool keyUsed = false;
        var meta = ResponseMeta.Local();
        for (int index = 0; index < steps.Count; index++)
        {
            var (step, op) = steps[index];
            var args = StepArgs(m, step, op, sc, null);
            if (args is not JsonObject)
            {
                throw new Diag(name, Category.ValidationFailed).Param("input").Expected("an object")
                    .Remedy($"Step {index + 1} of {name} does not evaluate to an argument object.").Fail();
            }
            var stepOpts = opts with { Confirm = null, Verify = false };
            if (!UsesKey(op) || keyUsed)
            {
                stepOpts = stepOpts with { IdempotencyKey = null };
            }
            else if (!string.IsNullOrEmpty(opts.IdempotencyKey))
            {
                keyUsed = true;
            }
            JsonNode? value = null;
            bool hasValue = false;
            TungstenError? failure = null;
            try
            {
                switch (step.Kind)
                {
                    case "poll":
                        {
                            var (bv, bok) = Expr.Evaluate(step.BudgetMs, sc, 0);
                            var budget = Util.Bounded(bv, bok, DefaultMacroBudgetMs, 0, Json.MaxSafeInteger);
                            var interval = Util.Bounded(step.IntervalMs is long iv ? JsonValue.Create(iv) : null, step.IntervalMs is not null, DefaultPollIntervalMs, 0, Json.MaxSafeInteger);
                            JsonNode until = step.Until as JsonObject ?? new JsonObject();
                            var polled = (Result<Polled>.Ok)await PollWithAsync(op, args, until, Util.DurationMs(interval), Util.DurationMs(budget), stepOpts, claim, ct).ConfigureAwait(false);
                            if (polled.Value.TimedOut)
                            {
                                value = null;
                                hasValue = true;
                            }
                            else
                            {
                                value = polled.Value.Body;
                                hasValue = polled.Value.HasBody;
                            }
                            meta = polled.Meta;
                            break;
                        }
                    case "paginate":
                        {
                            var items = new JsonArray();
                            long limit = PageLimit(step);
                            long pages = 0;
                            var st = new PageState(args);
                            while (true)
                            {
                                var page = await PageStepAsync(op, st, stepOpts, claim, ct).ConfigureAwait(false);
                                if (page is null)
                                {
                                    break;
                                }
                                if (page is Result<Page<JsonNode?>>.Err err)
                                {
                                    failure = err.Error;
                                    break;
                                }
                                var ok = (Result<Page<JsonNode?>>.Ok)page;
                                foreach (var item in ok.Value.Items)
                                {
                                    Json.Add(items, item);
                                }
                                meta = ok.Meta;
                                pages++;
                                if (pages >= limit)
                                {
                                    break;
                                }
                            }
                            value = items;
                            hasValue = true;
                            break;
                        }
                    default:
                        {
                            var response = await CallWithAsync(op, args, stepOpts, null, claim, ct).ConfigureAwait(false);
                            value = response.Value;
                            hasValue = response.HasValue;
                            meta = response.Meta;
                            break;
                        }
                }
            }
            catch (Failure f)
            {
                failure = f.Error;
            }
            if (failure is not null)
            {
                var d = failure.Diagnostic;
                if (failure.HasPartial && !string.IsNullOrEmpty(step.As))
                {
                    Json.Put(produced, step.As, failure.Partial);
                }
                var done = completed.Count > 0 ? "completed before it: " + string.Join(", ", completed) : "no step completed before it";
                var remediation = $"{d.Remediation} Macro {name} stopped at step {index + 1} of {steps.Count} ({op.Id}); {done}.";
                var retryable = d.Retryable;
                var next = d.NextAction;
                var kept = Json.Keys(produced);
                if (kept.Count > 0)
                {
                    var once = m.ShownOnce ? ", including values the API shows only once: store them now" : "";
                    remediation += $" The result's partial holds the completed steps' results ({string.Join(", ", kept)}){once}.";
                }
                if (unprotected.Count > 0)
                {
                    remediation += $" Do not run {name} again: {string.Join(", ", unprotected)} already took effect and has no replay protection.";
                    if (retryable is Retryable.AfterDelay or Retryable.SameKeyOnly)
                    {
                        retryable = Retryable.AfterRemediation;
                    }
                    var finish = $"Finish {name} without rerunning it: call {op.Id} yourself with the values from the result's partial.";
                    next = next is null ? finish : next + " " + finish;
                }
                var renamed = d with { Operation = name, Remediation = remediation, Retryable = retryable, NextAction = next };
                if (produced.Count > 0)
                {
                    return new Result<JsonNode?>.Err(new TungstenError(renamed) { Partial = produced, HasPartial = true });
                }
                return new Result<JsonNode?>.Err(new TungstenError(renamed));
            }
            completed.Add(op.Id);
            if (!string.IsNullOrEmpty(step.As))
            {
                if (hasValue)
                {
                    Json.Put(sc, step.As, value);
                    Json.Put(produced, step.As, value);
                }
                else
                {
                    sc.Remove(step.As);
                    produced[step.As] = null;
                }
            }
            if (Classify.IsMutation(op) && !RerunProtected(op, stepOpts))
            {
                unprotected.Add(op.Id);
            }
        }
        var (output, present) = Expr.Evaluate(m.Output, sc, 0);
        return new Result<JsonNode?>.Ok(output) { HasValue = present, Raw = output, Meta = meta };
    }
}
