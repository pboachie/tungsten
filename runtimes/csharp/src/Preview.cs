// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

public sealed partial class ClientCore
{
    private const double DefaultPollIntervalMs = 1000.0;
    private const double DefaultVerifyBudgetMs = 30000.0;

    private async Task<Result<PreviewResult>> PreviewInnerAsync(OperationDescriptor op, JsonNode? args, CallOptions opts, CancellationToken ct)
    {
        var p = await PrepareAsync(op, args, opts, Purpose.Preview, null, null, ct).ConfigureAwait(false);
        var effects = new List<string>();
        if (op.Agent.Confirmation is { Message: { Length: > 0 } message })
        {
            effects.Add(Interpolate(message, p.Args, op));
        }
        if (!string.IsNullOrEmpty(op.Agent.RemediationNote))
        {
            effects.Add(op.Agent.RemediationNote);
        }
        if (op.Agent.Confirmation is { SummaryFields.Count: > 0 } conf)
        {
            var fields = conf.SummaryFields.Select(f => $"{f}={{{f}}}");
            effects.Add(Interpolate("Arguments: " + string.Join(", ", fields) + ".", p.Args, op));
        }
        if (!string.IsNullOrEmpty(op.Summary))
        {
            effects.Add(op.Summary);
        }
        var result = new PreviewResult
        {
            Operation = op.Id,
            Safety = op.Agent.Safety,
            Request = new RenderedRequest { Method = p.Method, Url = p.DisplayUrl, Headers = p.Headers.ToMap(true), Body = p.DisplayBody },
            Effects = effects,
        };
        if (op.Agent.Safety != Safety.ReadOnly)
        {
            result = result with
            {
                ConfirmationToken = ConfirmationTokens.Issue(confirmationKey, op.Id, p.Args, Now()),
                ExpiresInMs = ConfirmationTokens.TtlMs,
            };
        }
        switch (op.Agent.Preview.Mode)
        {
            case "header":
                {
                    var dry = await PrepareAsync(op, args, opts, Purpose.ServerPreview, null, null, ct).ConfigureAwait(false);
                    var answer = await SendAsync(dry, opts, ct).ConfigureAwait(false);
                    if (answer.HasValue)
                    {
                        result = result with { ServerPreview = Util.RedactPaths(answer.Value, op.Agent.SensitiveResponseFields), HasServerPreview = true };
                    }
                    return new Result<PreviewResult>.Ok(result) { Meta = answer.Meta };
                }
            case "endpoint":
                {
                    var name = op.Agent.Preview.Operation ?? "";
                    var target = Operation(name);
                    if (target is null)
                    {
                        throw new Diag(op.Id, Category.ValidationFailed).Param("operation")
                            .Expected($"the preview operation {name} registered with the client")
                            .Remedy($"Register {name} (ClientCore.Register or the operations of the ClientCore constructor) so preview() can call it; nothing was sent.").Fail();
                    }
                    var rest = opts with { Confirm = null, Verify = false };
                    var answer = await CallWithAsync(target, args, rest, null, null, ct).ConfigureAwait(false);
                    result = result with { ServerPreview = answer.Value, HasServerPreview = answer.HasValue };
                    return new Result<PreviewResult>.Ok(result) { Meta = answer.Meta };
                }
        }
        return new Result<PreviewResult>.Ok(result) { Meta = ResponseMeta.Local() };
    }

    /// <summary>Stands in for a verification reference that did not resolve; it equals nothing a response can hold.</summary>
    private static JsonObject Unresolved() => new() { ["$tungsten_unresolved"] = true };

    private static JsonNode? ResolveRefs(JsonNode? node, JsonObject sc, int depth)
    {
        if (depth > 64)
        {
            return Unresolved();
        }
        switch (node)
        {
            case JsonArray a:
                return Json.Array(a.Select(item => ResolveRefs(item, sc, depth + 1)));
            case JsonObject o:
                {
                    var outObj = new JsonObject();
                    foreach (var kv in o)
                    {
                        Json.Put(outObj, kv.Key, ResolveRefs(kv.Value, sc, depth + 1));
                    }
                    return outObj;
                }
        }
        if (Json.TryString(node, out var t) && t.StartsWith('$'))
        {
            if (Expr.ResolveRef(t, sc, out var v) && !Json.IsNull(v))
            {
                return Json.Copy(v);
            }
            return Unresolved();
        }
        return Json.Copy(node);
    }

    internal async Task<Result<Polled>> PollWithAsync(OperationDescriptor op, JsonNode? args, JsonNode? until, TimeSpan interval, TimeSpan budget, CallOptions opts, MacroClaim? claim, CancellationToken ct)
    {
        var started = Stopwatch.StartNew();
        var once = opts with { Verify = false };
        while (true)
        {
            var response = await CallWithAsync(op, args, once, null, claim, ct).ConfigureAwait(false);
            bool done = Expr.EvaluatePredicate(until, response.Value, response.HasValue);
            bool outOfTime = !done && started.Elapsed + interval > budget;
            if (done || outOfTime)
            {
                return new Result<Polled>.Ok(new Polled { Body = response.Value, HasBody = response.HasValue, TimedOut = outOfTime }) { Meta = response.Meta };
            }
            // The next poll could not be sent before the call's deadline, so it is not waited for.
            if (Remaining(opts) is TimeSpan left && left <= interval)
            {
                throw DeadlineExpired(op, "before the next poll");
            }
            try
            {
                await Task.Delay(interval, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                throw new Diag(op.Id, Category.TransportFailed).Retry(Retryable.Never)
                    .Remedy("The poll was cancelled by the caller's cancellation token before its predicate held.").Fail();
            }
        }
    }

    private async Task<Verification> VerifyAsync(OperationDescriptor op, JsonObject args, JsonNode? value, bool hasValue, CallOptions opts, CancellationToken ct)
    {
        var unchecked_ = new Verification();
        var hook = op.Agent.Verify;
        if (hook is null)
        {
            return unchecked_;
        }
        var target = Operation(hook.Operation);
        if (target is null)
        {
            return unchecked_;
        }
        // The hook is written against the API: argument keys and $args references use wire names.
        var sc = new JsonObject();
        if (hasValue)
        {
            Json.Put(sc, "response", value);
        }
        Json.Put(sc, "args", WithWireNames(op, args));
        var (evaluated, _) = Expr.Evaluate(hook.Args ?? new JsonObject(), sc, 0);
        if (evaluated is not JsonObject evaluatedObj)
        {
            return unchecked_;
        }
        var mapped = new JsonObject();
        foreach (var kv in evaluatedObj)
        {
            Json.Put(mapped, target.ArgName(kv.Key), kv.Value);
        }
        JsonNode? Resolve(JsonNode? predicate) => predicate is JsonObject o && o.Count > 0 ? ResolveRefs(predicate, sc, 0) : null;
        var terminal = Resolve(hook.Terminal);
        var expect = Resolve(hook.Expect) ?? new JsonObject();
        var interval = hook.PollIntervalMs is long pi ? TimeSpan.FromMilliseconds(pi) : Util.DurationMs(DefaultPollIntervalMs);
        TimeSpan budget = TimeSpan.Zero;
        if (hook.PollBudgetMs is long pb)
        {
            budget = TimeSpan.FromMilliseconds(pb);
        }
        else if (terminal is not null)
        {
            budget = Util.DurationMs(DefaultVerifyBudgetMs);
        }
        var rest = opts with { Confirm = null, IdempotencyKey = null, Verify = false };
        var until = terminal ?? expect;
        Result<Polled>.Ok polled;
        try
        {
            polled = (Result<Polled>.Ok)await PollWithAsync(target, mapped, until, interval, budget, rest, null, ct).ConfigureAwait(false);
        }
        catch (Failure f)
        {
            return unchecked_ with { Error = f.Error.Diagnostic };
        }
        var body = polled.Value;
        return new Verification
        {
            Checked = true,
            Passed = Expr.EvaluatePredicate(expect, body.Body, body.HasBody),
            TimedOut = budget != TimeSpan.Zero && body.TimedOut,
            Observed = body.HasBody ? Util.RedactPaths(body.Body, target.Agent.SensitiveResponseFields) : null,
            HasObserved = body.HasBody,
        };
    }
}
