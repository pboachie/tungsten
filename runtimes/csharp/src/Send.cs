// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

/// <summary>A decoded response, or an event stream whose body is unread.</summary>
internal sealed record Sent
{
    public Result<JsonNode?>.Ok? Response { get; init; }
    public ResponseMeta? StreamMeta { get; init; }
    public EventBody? StreamBody { get; init; }
}

public sealed partial class ClientCore
{
    /// <summary>The number of same-origin redirects one read follows.</summary>
    private const int MaxRedirects = 5;

    private static bool IsRedirect(int status) => status is 301 or 302 or 303 or 307 or 308;

    private static bool RetryableCategory(Category c) =>
        c is Category.RateLimited or Category.UpstreamUnavailable or Category.TransportFailed or Category.OutcomeUnknown;

    internal async Task<Result<JsonNode?>.Ok> SendAsync(Prepared p, CallOptions opts, CancellationToken ct)
    {
        var sent = await SendWithAsync(p, opts, false, ct).ConfigureAwait(false);
        if (sent.StreamBody is not null)
        {
            sent.StreamBody.Close();
            throw new Diag(p.Op.Id, Category.UnexpectedResponse).Retry(Retryable.Never)
                .Remedy("The SDK received an event stream it did not ask for. This is a bug in the runtime; report it. If the call may have been sent, check its effect before repeating it.").Fail();
        }
        return sent.Response!;
    }

    /// <summary>Sends with retries. With stream, a 2xx text/event-stream answer is returned unread.</summary>
    internal async Task<Sent> SendWithAsync(Prepared p, CallOptions opts, bool stream, CancellationToken ct)
    {
        var op = p.Op;
        var plan = RetryOptionsFor(op);
        bool mutation = Classify.IsMutation(op);
        bool protectedCall = Classify.HasReplayProtection(op, p.HasKey);
        OutcomeCheck? check = mutation && !protectedCall ? OutcomeCheckOf(op, p.Args) : null;
        var attemptTimeout = AttemptTimeout(opts);
        int attempts = 0;
        while (true)
        {
            attempts++;
            var rctx = new RequestContext
            {
                Operation = op.Id,
                Attempt = attempts,
                Method = p.Method,
                Url = p.DisplayUrl,
                Headers = p.Headers.ToMap(true),
                Body = p.Payload.Empty ? null : Json.Copy(p.DisplayBody),
                HasBody = !p.Payload.Empty,
            };
            Hooks(m => m.OnRequest(rctx));
            // Redirects are never followed by the HTTP client: it would forward credential headers
            // to another origin. A read follows same-origin redirects here; a mutation follows none.
            var target = p.Url;
            var method = p.Method;
            var body = p.Payload;
            var entries = p.Headers.Entries.ToList();
            var idle = stream ? idleTimeout : null;
            var outcome = await Transport.AttemptAsync(http, new AttemptRequest { Url = target, Method = method, Headers = entries, Body = body, Timeout = attemptTimeout, Stream = stream, Idle = idle }, ct).ConfigureAwait(false);
            for (int hops = 0; !mutation && hops < MaxRedirects; hops++)
            {
                if (outcome.Kind != OutcomeKind.Response || !IsRedirect(outcome.Status))
                {
                    break;
                }
                outcome.Headers.TryGetValue("location", out var location);
                var next = RedirectTarget(p, target, location);
                if (next is null)
                {
                    break;
                }
                bool rewrite = outcome.Status != 307 && outcome.Status != 308 && method != "GET" && method != "HEAD";
                target = next.Value.Sent;
                if (rewrite)
                {
                    method = "GET";
                    body = Payload.EmptyPayload;
                    entries = entries.Where(h => !string.Equals(h.Name, "content-type", StringComparison.OrdinalIgnoreCase)).ToList();
                }
                rctx.Url = next.Value.Shown;
                rctx.Method = method;
                outcome = await Transport.AttemptAsync(http, new AttemptRequest { Url = target, Method = method, Headers = entries, Body = body, Timeout = attemptTimeout, Stream = stream, Idle = idle }, ct).ConfigureAwait(false);
            }
            var cctx = new CallContext { Api = api, Op = op, Key = p.Key, HasKey = p.HasKey, KeyHeader = p.KeyHeader, Attempts = attempts, Check = check };
            TungstenError error;
            try
            {
                return ClassifyResponse(cctx, p, outcome, rctx, attemptTimeout);
            }
            catch (Failure f)
            {
                error = f.Error;
            }
            var d = p.Secrets.Scrub(error.Diagnostic);
            var done = error with { Diagnostic = d };
            if (attempts > plan.Max || !RetryableCategory(d.Category))
            {
                throw new Failure(done);
            }
            if (d.Retryable is Retryable.Never or Retryable.AfterRemediation || (mutation && !protectedCall))
            {
                throw new Failure(done);
            }
            if (ct.IsCancellationRequested)
            {
                throw new Failure(done);
            }
            double delayMs;
            if (d.RetryAfterMs is long after && plan.HonorRetryAfter)
            {
                if (after > plan.MaxMs)
                {
                    throw new Failure(done);
                }
                delayMs = after;
            }
            else
            {
                double ceiling = Math.Min(plan.MaxMs, plan.BaseMs * Math.Pow(2, attempts - 1));
                double r = Random();
                delayMs = plan.Jitter switch
                {
                    Jitter.None => ceiling,
                    Jitter.Equal => ceiling / 2 + r * ceiling / 2,
                    _ => r * ceiling,
                };
            }
            Hooks(m => m.OnRetry(rctx, d));
            try
            {
                await Task.Delay(Util.DurationMs(delayMs), ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                throw new Failure(done);
            }
        }
    }

    /// <summary>The URL a read's redirect points to when it stays on the request's origin, with the auth query re-applied.</summary>
    private static (string Sent, string Shown)? RedirectTarget(Prepared p, string from, string? location)
    {
        if (string.IsNullOrEmpty(location) || !Uri.TryCreate(from, UriKind.Absolute, out var baseUri) ||
            !Uri.TryCreate(baseUri, location, out var resolved) || !Urls.SameOrigin(baseUri, resolved) || resolved.UserInfo.Length > 0)
        {
            return null;
        }
        var sent = resolved.AbsoluteUri;
        foreach (var (k, v) in p.AuthQuery)
        {
            if (!Urls.Has(sent, k))
            {
                sent = Urls.Set(sent, k, v);
            }
        }
        var shown = sent;
        foreach (var name in p.HiddenQuery)
        {
            if (Urls.Has(shown, name))
            {
                shown = Urls.Set(shown, name, Util.Redacted);
            }
        }
        return (sent, shown);
    }

    private Sent ClassifyResponse(CallContext cctx, Prepared p, AttemptOutcome outcome, RequestContext rctx, TimeSpan attemptTimeout)
    {
        var op = p.Op;
        bool mutation = Classify.IsMutation(op);
        int attempts = cctx.Attempts;
        switch (outcome.Kind)
        {
            case OutcomeKind.NotSent:
                throw new Diag(op.Id, Category.TransportFailed)
                    .Remedy($"The request could not be delivered ({outcome.Detail}), so the server did not receive it. Check BaseUrl and network access, then call again.")
                    .Attempts(attempts).Fail();
            case OutcomeKind.Lost:
                {
                    var cause = $"The connection failed before a response arrived ({outcome.Detail}).";
                    if (mutation)
                    {
                        throw new Failure(Classify.OutcomeUnknown(cctx, cause, new UnknownFields()));
                    }
                    throw new Diag(op.Id, Category.TransportFailed).Remedy(cause + " This read has no side effects; call again.").Attempts(attempts).Fail();
                }
            case OutcomeKind.Aborted:
                if (mutation && outcome.Sent)
                {
                    throw new Failure(Classify.OutcomeUnknown(cctx, "The caller's cancellation token cancelled the call after it was sent.", new UnknownFields()));
                }
                throw new Diag(op.Id, Category.TransportFailed).Retry(Retryable.Never)
                    .Remedy($"The caller's cancellation token cancelled the call{(outcome.Sent ? "" : " before it was sent")}.")
                    .Attempts(attempts).Fail();
            case OutcomeKind.Timeout:
                {
                    var ms = Util.Ms(attemptTimeout);
                    if (mutation)
                    {
                        throw new Failure(Classify.OutcomeUnknown(cctx, $"No response arrived within {ms} ms.", new UnknownFields()));
                    }
                    throw new Diag(op.Id, Category.UpstreamUnavailable)
                        .Remedy($"No response arrived within {ms} ms. This read has no side effects; call again later or with a larger `Timeout`.")
                        .Attempts(attempts).Fail();
                }
            case OutcomeKind.Streaming:
                Hooks(m => m.OnResponse(rctx, new ResponseContext { Status = outcome.Status, Headers = outcome.Headers }));
                return new Sent
                {
                    StreamMeta = new ResponseMeta { Status = outcome.Status, Headers = outcome.Headers, RequestId = Envelope.RequestIdOf(outcome.Headers), Attempts = attempts },
                    StreamBody = outcome.Stream,
                };
        }
        int status = outcome.Status;
        var headersIn = outcome.Headers;
        Hooks(m => m.OnResponse(rctx, new ResponseContext { Status = status, Headers = headersIn }));
        var requestId = Envelope.RequestIdOf(headersIn);
        headersIn.TryGetValue("retry-after", out var retryAfterValue);
        var retryAfter = Transport.ParseRetryAfter(retryAfterValue, Now());
        var declared = Envelope.MatchResponse(op.Responses, status);
        bool success = (status >= 200 && status <= 299) || (declared is not null && declared.Kind == "success" && status >= 100 && status <= 399);
        string? hint = op.Agent.Verify is { } v ? $"Call {v.Operation} to read the current state." : null;
        if (success && !outcome.BodyOk)
        {
            var failure = outcome.BodyFailure == "timeout" ? "timeout" : "broken";
            if (!mutation)
            {
                throw new Diag(op.Id, Category.UpstreamUnavailable).Status(status).RequestId(requestId).Retry(Retryable.AfterDelay)
                    .Remedy($"The response body was cut off ({failure}). This read has no side effects; call again.")
                    .Attempts(attempts).Fail();
            }
            throw new Diag(op.Id, Category.UnexpectedResponse).Status(status).RequestId(requestId).Retry(Retryable.Never)
                .Remedy($"The server accepted {op.Id} (HTTP {status}) but the response body was cut off ({failure}). The call took effect; do not repeat it.")
                .Next(hint).Attempts(attempts).Fail();
        }
        var data = outcome.Body ?? System.Array.Empty<byte>();
        var declaredMedia = declared?.MediaType;
        if (!success)
        {
            if (status >= 300 && status <= 399)
            {
                var to = headersIn.TryGetValue("location", out var l) ? " to " + Util.TakeUnits(l, 200) : "";
                string remediation;
                string? next = null;
                if (mutation)
                {
                    remediation = $"The server answered with a redirect{to}. Redirects are never followed on mutations; check BaseUrl (scheme, host, trailing slash). Whether the call took effect is unknown only if the server applied it before redirecting.";
                    next = hint;
                }
                else
                {
                    remediation = $"The server answered with a redirect{to} that leaves the API's origin (or too many redirects). It is not followed, so credentials never leave the API's host; check BaseUrl.";
                }
                throw new Diag(op.Id, Category.UnexpectedResponse).Status(status).RequestId(requestId).Remedy(remediation)
                    .Next(next).Attempts(attempts).Fail();
            }
            var decodedError = DecodedBody.Decode(data, headersIn, declaredMedia);
            throw new Failure(Classify.Error(cctx, status, headersIn, decodedError, retryAfter));
        }
        var decoded = DecodedBody.Decode(data, headersIn, declaredMedia);
        var mode = validateResponses;
        var afterEffect = mutation ? " The call took effect; do not repeat it." : "";
        Diagnostic? problem = null;
        if (decoded.InvalidJson)
        {
            var expected = "a JSON body";
            var remediation = "The success response announced JSON but did not parse." + afterEffect;
            if (decoded.Jsonl)
            {
                expected = "a JSON Lines body";
                remediation = $"The success response announced JSON Lines but line {decoded.BadLine} did not parse.{afterEffect}";
            }
            problem = new Diag(op.Id, Category.UnexpectedResponse).Status(status).RequestId(requestId).Param("response")
                .Expected(expected).Remedy(remediation).Attempts(attempts).Build();
        }
        else if (mode != ValidateResponses.Off && op.Response is { } schema && decoded.Has)
        {
            var issues = Judge(schema, decoded.Value);
            if (issues.Count > 0)
            {
                var issue = issues[0];
                var path = Schema.Segments(issue.Path);
                var pathText = Schema.FormatPath(issue.Path);
                var kept = mutation && mode == ValidateResponses.Strict
                    ? " The decoded body is in the result's partial; store any value shown only once from it before anything else."
                    : "";
                var redacted = Util.RedactPaths(decoded.Value, op.Agent.SensitiveResponseFields);
                Util.GetPath(redacted, true, path, out var received);
                bool sensitive = path.Any(Util.LooksSensitive);
                problem = new Diag(op.Id, Category.UnexpectedResponse).Status(status).RequestId(requestId)
                    .Param("response" + pathText)
                    .Received(Util.EnvelopeValue(received, sensitive))
                    .Expected(issue.Message)
                    .Remedy($"The success response does not match the API description at response{pathText} ({issue.Message}).{afterEffect}{kept}")
                    .Attempts(attempts).Build();
            }
        }
        if (problem is not null)
        {
            var scrubbed = p.Secrets.Scrub(problem);
            if (mode == ValidateResponses.Strict)
            {
                // The effect of a mutation happened: its body is never dropped, since it can hold
                // the only copy of a value.
                if (mutation && !decoded.InvalidJson)
                {
                    throw new Failure(new TungstenError(scrubbed) { Partial = decoded.Value, HasPartial = decoded.Has });
                }
                throw new Failure(scrubbed);
            }
            Emit(scrubbed);
        }
        return new Sent
        {
            Response = new Result<JsonNode?>.Ok(decoded.Value)
            {
                HasValue = decoded.Has,
                Raw = decoded.Value,
                Meta = new ResponseMeta { Status = status, Headers = headersIn, RequestId = requestId, Attempts = attempts },
            },
        };
    }

    /// <summary>A validator that only judges (responses, page items, events): a failure inside it is an issue at the root.</summary>
    internal static IReadOnlyList<Issue> Judge(Schema schema, JsonNode? value)
    {
        try
        {
            return schema.Validate(value);
        }
        catch (Exception e) when (e is not Failure)
        {
            return new[] { new Issue(System.Array.Empty<object>(), "the response schema failed (the validator panicked)") };
        }
    }

    /// <summary>How to find out whether a mutation without replay protection took effect after its answer was lost.</summary>
    internal OutcomeCheck? OutcomeCheckOf(OperationDescriptor op, JsonObject args)
    {
        var sentArgs = SentArguments(op, args);
        if (op.Agent.Verify is { } hook && hook.Operation.Length > 0 && Classify.VerifyCallable(hook))
        {
            var sc = new JsonObject();
            Json.Put(sc, "args", WithWireNames(op, args));
            var (evaluated, _) = Expr.Evaluate(hook.Args ?? new JsonObject(), sc, 0);
            var call = hook.Operation + WithArguments(evaluated);
            var lost = new List<string>();
            JsonNode? expect = new JsonObject();
            if (hook.Expect is JsonObject)
            {
                expect = ResolveLost(hook.Expect, sc, lost, 0);
            }
            var fields = expect is JsonObject eo ? Json.Keys(eo) : new List<string>();
            string shows;
            if (fields.Count == 0)
            {
                shows = "whether it shows " + Classify.ChangeOf(op);
            }
            else if (lost.Count > 0)
            {
                shows = $"whether {string.Join(" and ", fields)} holds what {op.Id} creates, matching the arguments you sent{sentArgs} (its {string.Join(", ", lost.Distinct())} was in the lost response)";
            }
            else
            {
                shows = "whether " + Expr.DescribePredicate(expect);
            }
            return new OutcomeCheck(call, shows);
        }
        var read = ResourceRead(op, args);
        if (read is null)
        {
            return null;
        }
        return new OutcomeCheck(read.Value.Id + WithArguments(read.Value.Args), "whether it shows " + Classify.ChangeOf(op));
    }

    private static bool IsRootSegment(string s)
    {
        if (s.Length == 0 || s == "api")
        {
            return true;
        }
        if (s.Length > 1 && (s[0] == 'v' || s[0] == 'V'))
        {
            return s[1..].Split('.').All(Util.AllDigits);
        }
        return false;
    }

    /// <summary>
    /// A registered read of the resource op changes: a GET whose path is the longest prefix of op's
    /// path, never the API root, whose required parameters are path parameters op was called with.
    /// </summary>
    private (string Id, JsonObject Args)? ResourceRead(OperationDescriptor op, JsonObject args)
    {
        if (op.Rpc is not null)
        {
            return null;
        }
        var known = new Dictionary<string, JsonNode?>(StringComparer.Ordinal);
        foreach (var p in op.Params.Where(p => p.In == "path"))
        {
            if (args.TryGetPropertyValue(p.Key, out var v) && !Json.IsNull(v))
            {
                known[p.Wire] = v;
            }
        }
        var segments = op.Path.Split('/');
        var reads = registry.Values
            .Where(r => r.Id != op.Id && r.Method == "GET" && r.Agent.Safety == Safety.ReadOnly && r.Rpc is null)
            .OrderBy(r => r.Id, StringComparer.Ordinal)
            .ToList();
        for (int end = segments.Length; end >= 1; end--)
        {
            var prefix = segments.Take(end).ToList();
            if (prefix.All(IsRootSegment))
            {
                break;
            }
            var path = string.Join("/", prefix);
            foreach (var read in reads)
            {
                if (read.Path != path)
                {
                    continue;
                }
                var parameters = read.Arguments.ToList();
                if (parameters.Any(p => p.Required && (p.In != "path" || !known.ContainsKey(p.Wire))))
                {
                    continue;
                }
                var readArgs = new JsonObject();
                foreach (var p in parameters)
                {
                    if (p.In == "path" && known.TryGetValue(p.Wire, out var v))
                    {
                        Json.Put(readArgs, p.Wire, v);
                    }
                }
                return (read.Id, readArgs);
            }
        }
        return null;
    }

    /// <summary>Resolves a node against the call's arguments; references to the lost response show as &lt;response...&gt; and are recorded.</summary>
    private static JsonNode? ResolveLost(JsonNode? node, JsonObject sc, List<string> lost, int depth)
    {
        if (depth > 32)
        {
            return null;
        }
        switch (node)
        {
            case JsonArray a:
                return Json.Array(a.Select(item => ResolveLost(item, sc, lost, depth + 1)));
            case JsonObject o:
                {
                    var outObj = new JsonObject();
                    foreach (var kv in o)
                    {
                        Json.Put(outObj, kv.Key, ResolveLost(kv.Value, sc, lost, depth + 1));
                    }
                    return outObj;
                }
        }
        if (Json.TryString(node, out var t))
        {
            if (t.StartsWith("$response", StringComparison.Ordinal))
            {
                var rest = t["$response".Length..];
                if (rest.StartsWith('.'))
                {
                    rest = rest[1..];
                }
                lost.Add(rest.Length == 0 ? "body" : rest);
                return JsonValue.Create("<" + t[1..] + ">");
            }
            if (t.StartsWith('$'))
            {
                return Expr.ResolveRef(t, sc, out var v) ? Json.Copy(v) : JsonValue.Create("<" + t[1..] + ">");
            }
        }
        return Json.Copy(node);
    }
}
