// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json.Nodes;

namespace Tungsten.Runtime;

/// <summary>A failed call inside the runtime; public entry points turn it into a <see cref="Result{T}.Err"/>.</summary>
internal sealed class Failure : Exception
{
    public Failure(TungstenError error) : base(error.Diagnostic.ToString())
    {
        Error = error;
    }

    public Failure(Diagnostic d) : this(new TungstenError(d)) { }

    public TungstenError Error { get; }
}

/// <summary>Builds an envelope with every member present.</summary>
internal sealed class Diag
{
    private Diagnostic d;
    private bool hasRemedy;
    private bool hasRetryable;

    public Diag(string operation, Category c)
    {
        d = new Diagnostic { Operation = operation, Category = c };
    }

    public Diag Status(int? code) { d = d with { HttpStatus = code }; return this; }
    public Diag Code(string? code) { d = d with { Code = code }; return this; }
    public Diag Param(string path) { d = d with { FailedParameter = path }; return this; }
    public Diag Received(JsonNode? v) { d = d with { ReceivedValue = v }; return this; }
    public Diag Expected(string text) { d = d with { Expected = text }; return this; }
    public Diag Remedy(string text) { d = d with { Remediation = text }; hasRemedy = true; return this; }
    public Diag Retry(Retryable r) { d = d with { Retryable = r }; hasRetryable = true; return this; }
    public Diag RetryOpt(Retryable? r) => r is Retryable x ? Retry(x) : this;
    public Diag RetryAfter(long? ms) { d = d with { RetryAfterMs = ms }; return this; }
    public Diag Next(string? text) { d = d with { NextAction = text }; return this; }
    public Diag RequestId(string? id) { d = d with { RequestId = id }; return this; }
    public Diag Attempts(int n) { d = d with { Attempts = n }; return this; }

    public Diagnostic Build()
    {
        var outD = d;
        if (!hasRemedy)
        {
            outD = outD with { Remediation = Envelope.GenericRemediation(outD.Category) };
        }
        if (!hasRetryable)
        {
            outD = outD with { Retryable = Envelope.DefaultRetryable(outD.Category) };
        }
        return outD;
    }

    public Failure Fail() => new(Build());
}

/// <summary>Remediation texts and the classification of answers into envelopes.</summary>
public static class Envelope
{
    /// <summary>The default retryable of a category.</summary>
    public static Retryable DefaultRetryable(Category c) => c switch
    {
        Category.RateLimited or Category.UpstreamUnavailable or Category.TransportFailed => Retryable.AfterDelay,
        Category.OutcomeUnknown => Retryable.SameKeyOnly,
        _ => Retryable.Never,
    };

    /// <summary>The remediation used when no manifest entry applies.</summary>
    public static string GenericRemediation(Category c) => c switch
    {
        Category.ValidationFailed => "The server rejected the arguments. Fix the parameter the API error names and call again; the same request fails the same way.",
        Category.MalformedRequest => "The server could not parse the request. Check the format of path and query parameters (for example canonical UUIDs); do not retry unchanged.",
        Category.RequestTooLarge => "The request body is larger than the server accepts. Send a smaller body; do not retry unchanged.",
        Category.AuthFailed => "The credential is missing, expired, revoked or lacks permission. Do not retry; fix the credential configured in ClientOptions.auth.",
        Category.NotFound => "The resource does not exist or is not visible to this credential. Check the identifiers; do not retry unchanged.",
        Category.Conflict => "The request conflicts with the resource's current state. Read the current state before deciding what to do; do not retry unchanged.",
        Category.PreconditionFailed => "A precondition of this operation is not met (account, billing or resource state). Resolve it first; do not retry unchanged.",
        Category.RateLimited => "Rate limited. Wait retry_after_ms (or a few seconds when it is null) before calling again.",
        Category.UpstreamUnavailable => "The service is temporarily unavailable. Wait, then call again.",
        Category.OutcomeUnknown => "The server may or may not have applied this call; do not retry it blindly. Check whether it took effect before doing anything else.",
        Category.TransportFailed => "The request could not be delivered (DNS, TLS or connection failure), so the server did not receive it. Check BaseUrl and network access, then call again.",
        Category.ConfirmationRequired => "This operation needs confirmation: call preview(...) and pass its confirmation_token.",
        Category.GateDisabled => "This operation is disabled on this deployment. It is a deployment setting; do not retry.",
        _ => "The server's response did not match the API description. Do not retry blindly; report it.",
    };

    internal static Category CategoryForStatus(int status, bool jsonBody)
    {
        switch (status)
        {
            case 401: case 403: case 407: return Category.AuthFailed;
            case 404: case 410: return Category.NotFound;
            case 409: return Category.Conflict;
            case 402: case 412: case 428: return Category.PreconditionFailed;
            case 413: return Category.RequestTooLarge;
            case 422: return Category.ValidationFailed;
            case 429: return Category.RateLimited;
            case 408: case 502: case 503: case 504: return Category.UpstreamUnavailable;
        }
        if (status >= 500 && status <= 599)
        {
            return Category.UpstreamUnavailable;
        }
        if (status >= 400 && status <= 499)
        {
            return jsonBody ? Category.ValidationFailed : Category.MalformedRequest;
        }
        return Category.UnexpectedResponse;
    }

    private static readonly string[] RequestIdHeaders = { "x-request-id", "request-id", "x-correlation-id", "x-amzn-requestid", "cf-ray" };

    internal static string? RequestIdOf(IReadOnlyDictionary<string, string> headers)
    {
        foreach (var name in RequestIdHeaders)
        {
            if (headers.TryGetValue(name, out var v) && v.Length > 0)
            {
                return Util.TakeUnits(v, 200);
            }
        }
        return null;
    }

    /// <summary>The media type without parameters, lower-cased.</summary>
    internal static string MediaTypeOf(IReadOnlyDictionary<string, string> headers)
    {
        if (!headers.TryGetValue("content-type", out var v))
        {
            return "";
        }
        return v.Split(';')[0].Trim().ToLowerInvariant();
    }

    internal static bool IsJsonMedia(string media) => media == "application/json" || media.EndsWith("+json", StringComparison.Ordinal) || media == "text/json";

    internal static bool IsJsonlMedia(string media) =>
        media is "application/jsonl" or "application/x-jsonl" or "application/ndjson" or "application/x-ndjson";

    internal static ResponseDescriptor? MatchResponse(IReadOnlyList<ResponseDescriptor> responses, int status) =>
        responses.FirstOrDefault(r => r.StatusKind == "exact" && r.Code == status)
        ?? responses.FirstOrDefault(r => r.StatusKind == "class" && r.Code == status / 100)
        ?? responses.FirstOrDefault(r => r.StatusKind == "default");

    private static readonly string[] MessageFields = { "message", "error.message", "detail", "error_description", "title", "error" };

    internal static string ServerMessage(JsonNode? body, bool has)
    {
        foreach (var field in MessageFields)
        {
            if (Util.GetPathStr(body, has, field, out var v) && Json.TryString(v, out var text))
            {
                text = text.Trim();
                if (text.Length > 0)
                {
                    return Util.TakeUnits(text, 200);
                }
            }
        }
        return "";
    }

    internal static string? ErrorCode(OperationDescriptor op, JsonNode? body, bool has)
    {
        if (string.IsNullOrEmpty(op.ErrorCodeField) || !Util.GetPathStr(body, has, op.ErrorCodeField, out var v))
        {
            return null;
        }
        switch (Json.KindOf(v))
        {
            case JsonKind.String:
                var s = Json.AsString(v) ?? "";
                return s.Length > 0 ? Util.TakeUnits(s, 200) : null;
            case JsonKind.Integer:
            case JsonKind.Number:
                return Json.NumberText(v);
        }
        return null;
    }
}

/// <summary>A decoded response body.</summary>
internal sealed record DecodedBody
{
    /// <summary>Parsed JSON, text as a string, bytes as the binary form; absent for an empty body.</summary>
    public JsonNode? Value { get; init; }
    public bool Has { get; init; }
    public bool Json { get; init; }
    public bool InvalidJson { get; init; }
    public bool Empty { get; init; }
    public bool Jsonl { get; init; }
    public int BadLine { get; init; }

    public static DecodedBody Decode(byte[] data, IReadOnlyDictionary<string, string> headers, string? declared)
    {
        var announced = Envelope.MediaTypeOf(headers);
        if (data.Length == 0 && !Envelope.IsJsonlMedia(announced))
        {
            return new DecodedBody { Empty = true };
        }
        var media = announced.Length > 0 ? announced : (declared ?? "").ToLowerInvariant();
        var jsonlMedia = announced.Length > 0 ? announced : media.Split(';')[0].Trim();
        if (Envelope.IsJsonlMedia(jsonlMedia))
        {
            var raw = Util.DecodeText(data);
            var values = new JsonArray();
            var lines = raw.Split('\n');
            for (int i = 0; i < lines.Length; i++)
            {
                if (lines[i].Trim().Length == 0)
                {
                    continue;
                }
                if (!Runtime.Json.TryParse(lines[i], out var v))
                {
                    return new DecodedBody { Value = JsonValue.Create(raw), Has = true, InvalidJson = true, Jsonl = true, BadLine = i + 1 };
                }
                values.Add(v);
            }
            return new DecodedBody { Value = values, Has = true, Jsonl = true };
        }
        if (Envelope.IsJsonMedia(media))
        {
            var raw = Util.DecodeText(data);
            if (!Runtime.Json.TryParse(raw, out var v))
            {
                return new DecodedBody { Value = JsonValue.Create(raw), Has = true, InvalidJson = true };
            }
            return new DecodedBody { Value = v, Has = true, Json = true };
        }
        if (media.Length == 0 || media.StartsWith("text/", StringComparison.Ordinal) || media == "application/problem+xml" ||
            media.EndsWith("+xml", StringComparison.Ordinal) || media == "application/xml")
        {
            var raw = Util.DecodeText(data);
            var trimmed = raw.TrimStart(' ', '\t', '\r', '\n', '\v', '\f');
            if (media.Length == 0 && (trimmed.StartsWith('[') || trimmed.StartsWith('{')) && Runtime.Json.TryParse(raw, out var v))
            {
                return new DecodedBody { Value = v, Has = true, Json = true };
            }
            return new DecodedBody { Value = JsonValue.Create(raw), Has = true };
        }
        return new DecodedBody { Value = new Binary(data).ToJson(), Has = true };
    }
}

/// <summary>Says, in words, how to find out whether an unprotected mutation took effect.</summary>
internal sealed record OutcomeCheck(string Call, string Shows);

/// <summary>What every classification of one call shares.</summary>
internal sealed record CallContext
{
    public required ApiDescriptor Api { get; init; }
    public required OperationDescriptor Op { get; init; }
    public string? Key { get; init; }
    public bool HasKey { get; init; }
    public string KeyHeader { get; init; } = "Idempotency-Key";
    public int Attempts { get; init; }
    public OutcomeCheck? Check { get; init; }
}

internal sealed record UnknownFields
{
    public string? NextAction { get; init; }
    public int? HttpStatus { get; init; }
    public string? Code { get; init; }
    public string? RequestId { get; init; }
    public long? RetryAfterMs { get; init; }
}

internal static class Classify
{
    internal static bool IsMutation(OperationDescriptor op) => op.Agent.Safety != Safety.ReadOnly;

    internal static bool HasReplayProtection(OperationDescriptor op, bool hasKey) =>
        op.Agent.Idempotency.Policy == IdempotencyKind.ContentIdentity || hasKey;

    private static bool RefersToResponse(JsonNode? node, int depth)
    {
        if (depth > 32)
        {
            return true;
        }
        switch (node)
        {
            case JsonArray a:
                return a.Any(item => RefersToResponse(item, depth + 1));
            case JsonObject o:
                return o.Any(kv => RefersToResponse(kv.Value, depth + 1));
        }
        return Json.TryString(node, out var s) && (s == "$response" || s.StartsWith("$response.", StringComparison.Ordinal));
    }

    /// <summary>After a lost answer there is no success body, so a hook whose arguments reference $response cannot be called.</summary>
    internal static bool VerifyCallable(VerifyDescriptor v) => !RefersToResponse(v.Args, 0);

    internal static string? NextActionHint(CallContext ctx)
    {
        var op = ctx.Op;
        if (op.Agent.Verify is { } v && v.Operation.Length > 0 && VerifyCallable(v))
        {
            var with = v.Args is JsonObject o && o.Count > 0 ? " with " + string.Join(", ", Json.Keys(o)) : "";
            return $"Call {v.Operation}{with} to check whether {op.Id} took effect before doing anything else.";
        }
        if (op.Agent.Idempotency.Policy == IdempotencyKind.ContentIdentity)
        {
            return $"Call {op.Id} again with the identical body; the body is its own identity, so the server answers the original result instead of applying it twice.";
        }
        if (ctx.HasKey)
        {
            return $"Call {op.Id} again with the same {ctx.KeyHeader} value and identical arguments; the server answers the original result instead of applying it twice.";
        }
        return null;
    }

    /// <summary>The change an operation makes, in words.</summary>
    internal static string ChangeOf(OperationDescriptor op)
    {
        var summary = (op.Summary ?? "").Trim();
        if (summary.EndsWith('.'))
        {
            summary = summary[..^1];
        }
        if (summary.Length == 0)
        {
            return "the change " + op.Id + " makes";
        }
        return "the change " + Json.Quote(summary);
    }

    /// <summary>
    /// OUTCOME_UNKNOWN for a mutation whose effect cannot be known. Without replay protection a
    /// repeat can apply the effect twice, so the envelope never offers one.
    /// </summary>
    internal static Diagnostic OutcomeUnknown(CallContext ctx, string cause, UnknownFields fields)
    {
        var op = ctx.Op;
        string rule;
        string? hint = null;
        if (op.Agent.Idempotency.Policy == IdempotencyKind.ContentIdentity)
        {
            rule = "If you retry, resend the identical bytes only; the body is its own identity.";
        }
        else if (ctx.HasKey)
        {
            rule = $"If you retry, reuse the SAME {ctx.KeyHeader} value; a new key can apply the effect twice.";
        }
        else if (ctx.Check is { } check)
        {
            rule = $"This operation has no idempotency key, so repeating it can apply the effect twice. Before calling {op.Id} again, call {check.Call} and check {check.Shows}: if it does, {op.Id} took effect and must not be repeated; call it again only if it did not.";
            hint = $"Call {check.Call} and check {check.Shows}; call {op.Id} again only if it did not take effect.";
        }
        else
        {
            var change = ChangeOf(op);
            rule = $"This operation has no idempotency key and no registered way to verify it, so {op.Id} may have taken effect and a repeat can apply the effect twice: do not retry it blindly. Check the outcome by other means if you can (look for {change}); if you cannot, ask whoever owns the task before calling it again.";
            hint = $"Do not retry {op.Id} blindly: it may have taken effect. Check for {change} if you can; call {op.Id} again only if it is not there, or after the task owner accepts that it may apply twice.";
        }
        var retryable = HasReplayProtection(op, ctx.HasKey) ? Retryable.SameKeyOnly : Retryable.AfterRemediation;
        var next = fields.NextAction ?? hint ?? NextActionHint(ctx);
        return new Diag(op.Id, Category.OutcomeUnknown)
            .Remedy($"{cause} The server may or may not have applied {op.Id}. {rule}")
            .Retry(retryable)
            .Next(next)
            .Status(fields.HttpStatus)
            .Code(fields.Code)
            .RequestId(fields.RequestId)
            .RetryAfter(fields.RetryAfterMs)
            .Attempts(ctx.Attempts)
            .Build();
    }

    /// <summary>A manifest entry with one of these categories (or retryable never / after_remediation) is a definite answer even for an ambiguous status.</summary>
    private static bool Definite(Category? c, Retryable? r)
    {
        if (r is Retryable.Never or Retryable.AfterRemediation)
        {
            return true;
        }
        return c is Category.ValidationFailed or Category.MalformedRequest or Category.RequestTooLarge or Category.AuthFailed
            or Category.NotFound or Category.Conflict or Category.PreconditionFailed or Category.RateLimited or Category.GateDisabled;
    }

    private static RemediationEntry? Lookup(IReadOnlyDictionary<string, RemediationEntry> table, string? code) =>
        code is not null && table.TryGetValue(code, out var e) ? e : null;

    private static string? OptText(string? s) => string.IsNullOrEmpty(s) ? null : s;

    /// <summary>Classifies an HTTP error status.</summary>
    internal static Diagnostic Error(CallContext ctx, int status, IReadOnlyDictionary<string, string> headers, DecodedBody decoded, long? retryAfter)
    {
        var api = ctx.Api;
        var op = ctx.Op;
        var requestId = Envelope.RequestIdOf(headers);
        Diag Base(Diag b) => b.Status(status).RequestId(requestId).RetryAfter(retryAfter).Attempts(ctx.Attempts);
        string? code = decoded.Json ? Envelope.ErrorCode(op, decoded.Value, decoded.Has) : null;
        if (op.Gate is { } g && status == g.DisabledStatus && code is null)
        {
            if (!api.Gates.TryGetValue(g.EnvVar, out var text))
            {
                text = $"This deployment disables {op.Id} because {g.EnvVar} is off (HTTP {status}). It is a deployment setting, not a missing resource; do not retry.";
            }
            return Base(new Diag(op.Id, Category.GateDisabled)).Code(code).Remedy(text).Retry(Retryable.Never).Build();
        }
        var entry = Lookup(op.Agent.Remediation, code) ?? Lookup(api.ErrorCodes, code);
        var media = "none";
        if (!decoded.Empty)
        {
            var m = Envelope.MediaTypeOf(headers);
            if (m.Length > 0)
            {
                media = m;
            }
        }
        NonJsonError? nonJson = null;
        if (!decoded.Json)
        {
            nonJson = api.NonJson.FirstOrDefault(n => n.Status == status && (n.Media == media || (n.Media == "none" && decoded.Empty)));
        }
        var declared = Envelope.MatchResponse(op.Responses, status);
        bool ambiguous = api.AmbiguousStatuses.Contains(status) || (declared is not null && declared.Kind == "ambiguous") || (status >= 500 && status <= 599);
        bool definite = entry is not null ? Definite(entry.Category, entry.Retryable) : nonJson is not null && Definite(nonJson.Category, nonJson.Retryable);
        if (ambiguous && IsMutation(op) && !definite)
        {
            string said = "";
            if (!string.IsNullOrEmpty(entry?.Text))
            {
                said = entry.Text;
            }
            else if (!string.IsNullOrEmpty(nonJson?.Text))
            {
                said = nonJson.Text;
            }
            var cause = $"HTTP {status} leaves the outcome of this call unknown.";
            if (said.Length > 0)
            {
                cause += " " + said;
            }
            return OutcomeUnknown(ctx, cause, new UnknownFields
            {
                NextAction = OptText(entry?.NextAction),
                HttpStatus = status,
                Code = code,
                RequestId = requestId,
                RetryAfterMs = retryAfter,
            });
        }
        Category category;
        Retryable? retryable = null;
        string? remedy = null;
        string? next = null;
        if (entry is not null)
        {
            category = entry.Category ?? Envelope.CategoryForStatus(status, decoded.Json);
            retryable = entry.Retryable;
            remedy = entry.Text;
            next = OptText(entry.NextAction);
        }
        else if (nonJson is not null)
        {
            category = nonJson.Category;
            retryable = nonJson.Retryable;
            remedy = nonJson.Text;
        }
        else
        {
            category = Envelope.CategoryForStatus(status, decoded.Json);
        }
        if (category == Category.OutcomeUnknown && !IsMutation(op))
        {
            category = Category.UpstreamUnavailable;
            if (retryable is null or Retryable.SameKeyOnly)
            {
                retryable = Retryable.AfterDelay;
            }
        }
        if (string.IsNullOrEmpty(remedy))
        {
            remedy = Envelope.GenericRemediation(category);
            if (decoded.Json)
            {
                var said = Envelope.ServerMessage(decoded.Value, decoded.Has);
                if (said.Length > 0)
                {
                    remedy += " Server message: " + Json.Quote(said);
                }
            }
        }
        return Base(new Diag(op.Id, category)).Code(code).Remedy(remedy).RetryOpt(retryable).Next(next).Build();
    }
}
