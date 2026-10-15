// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

internal enum Purpose
{
    Call,
    Preview,
    ServerPreview,
    MacroPreview,
}

/// <summary>A macro run's one-time spend of its confirmation token: the first step about to send claims it.</summary>
internal sealed class MacroClaim
{
    private int claimed;
    private Failure? error;

    public MacroClaim(string name, string? token, string? bind)
    {
        Name = name;
        Token = token;
        Bind = bind;
    }

    public string Name { get; }
    public string? Token { get; }
    public string? Bind { get; }

    public void Claim(ClientCore core)
    {
        if (Token is null)
        {
            return;
        }
        if (Interlocked.Exchange(ref claimed, 1) == 0)
        {
            error = core.ClaimToken(Name, Token, Bind, "the macro's preview(...)");
            if (error is not null)
            {
                throw error;
            }
        }
    }
}

internal sealed record ConfirmTarget(string Id, string Subject, string PreviewCall);

/// <summary>A request ready to send, with its redacted rendering.</summary>
internal sealed class Prepared
{
    public required OperationDescriptor Op { get; init; }
    public required JsonObject Args { get; init; }
    public required string Method { get; init; }
    public required string Url { get; init; }
    public required string DisplayUrl { get; init; }
    public required HeaderBag Headers { get; init; }
    public required Payload Payload { get; init; }
    public JsonNode? DisplayBody { get; init; }
    public string? Key { get; init; }
    public bool HasKey => Key is not null;
    public required string KeyHeader { get; init; }
    public required List<(string K, string V)> AuthQuery { get; init; }
    public required List<string> HiddenQuery { get; init; }
    public required SecretSet Secrets { get; init; }
    public string? OAuth { get; set; }

    /// <summary>The URL that answered, after any followed redirect.</summary>
    public string? FinalUrl { get; set; }
}

/// <summary>Query manipulation on URL texts (URLSearchParams semantics).</summary>
internal static class Urls
{
    internal static (string Head, string Query, string Fragment) Split(string url)
    {
        string fragment = "";
        int hash = url.IndexOf('#', StringComparison.Ordinal);
        if (hash >= 0)
        {
            fragment = url[hash..];
            url = url[..hash];
        }
        int q = url.IndexOf('?', StringComparison.Ordinal);
        return q < 0 ? (url, "", fragment) : (url[..q], url[(q + 1)..], fragment);
    }

    private static string Unescape(string s)
    {
        try
        {
            return Uri.UnescapeDataString(s.Replace('+', ' '));
        }
        catch (UriFormatException)
        {
            return s;
        }
    }

    internal static List<(string K, string V)> Pairs(string query)
    {
        var outList = new List<(string, string)>();
        foreach (var part in query.Split('&'))
        {
            if (part.Length == 0)
            {
                continue;
            }
            int eq = part.IndexOf('=', StringComparison.Ordinal);
            var k = eq < 0 ? part : part[..eq];
            var v = eq < 0 ? "" : part[(eq + 1)..];
            outList.Add((Unescape(k), Unescape(v)));
        }
        return outList;
    }

    internal static bool Has(string url, string name) => Pairs(Split(url).Query).Any(p => p.K == name);

    /// <summary>Sets the query parameter, replacing existing ones (URLSearchParams.set).</summary>
    internal static string Set(string url, string name, string value)
    {
        var (head, query, fragment) = Split(url);
        var pairs = Pairs(query);
        int index = pairs.FindIndex(p => p.K == name);
        if (index < 0)
        {
            pairs.Add((name, value));
        }
        else
        {
            pairs[index] = (name, value);
            var kept = new List<(string, string)>();
            bool seen = false;
            foreach (var p in pairs)
            {
                if (p.K == name)
                {
                    if (seen)
                    {
                        continue;
                    }
                    seen = true;
                }
                kept.Add(p);
            }
            pairs = kept;
        }
        var text = string.Join("&", pairs.Select(p => Serialize.FormComponent(p.K) + "=" + Serialize.FormComponent(p.V)));
        return head + (text.Length > 0 ? "?" + text : "") + fragment;
    }

    private static int DefaultPort(string scheme) => scheme.ToLowerInvariant() switch
    {
        "http" => 80,
        "https" => 443,
        _ => -1,
    };

    internal static bool SameOrigin(Uri a, Uri b) =>
        string.Equals(a.Scheme, b.Scheme, StringComparison.OrdinalIgnoreCase) &&
        string.Equals(a.Host, b.Host, StringComparison.OrdinalIgnoreCase) &&
        (a.IsDefaultPort ? DefaultPort(a.Scheme) : a.Port) == (b.IsDefaultPort ? DefaultPort(b.Scheme) : b.Port);
}

public sealed partial class ClientCore
{
    private static List<object> KeyPath(string name) => new() { name };

    private static bool IsDotSegment(string segment)
    {
        if (segment.Length == 0)
        {
            return true;
        }
        var rest = segment.ToLowerInvariant();
        int count = 0;
        while (count < 3)
        {
            if (rest.StartsWith("%2e", StringComparison.Ordinal))
            {
                rest = rest[3..];
            }
            else if (rest.StartsWith('.'))
            {
                rest = rest[1..];
            }
            else
            {
                break;
            }
            count++;
        }
        return rest.Length == 0 && count >= 1 && count <= 2;
    }

    // ------------------------------------------------------------ argument paths

    /// <summary>
    /// The JSON path of an argument issue: <c>body.&lt;wire&gt;</c> for a merged body field,
    /// <c>body</c> for the whole-body argument, else <c>args.&lt;key&gt;</c>.
    /// </summary>
    internal static string ArgumentPath(OperationDescriptor op, IReadOnlyList<object> path)
    {
        if (op.Body is { } body && path.Count > 0)
        {
            var head = path[0] as string ?? path[0].ToString() ?? "";
            var rest = path.Skip(1);
            if (body.Arg is not null)
            {
                if (body.Arg == head)
                {
                    return "body" + Schema.FormatPath(rest);
                }
            }
            else
            {
                var f = body.Fields.FirstOrDefault(x => x.Arg == head);
                if (f is not null)
                {
                    return "body." + f.Wire + Schema.FormatPath(rest);
                }
            }
        }
        if (path.Count == 0)
        {
            return "args";
        }
        return "args" + Schema.FormatPath(path);
    }

    private static IEnumerable<string> SensitiveRequestPaths(OperationDescriptor op) => op.Agent.SensitiveRequestFields.Where(p => p.Length > 0);

    /// <summary>An argument path without array indices.</summary>
    private static string DottedArgPath(IEnumerable<object> path) =>
        string.Join(".", path.OfType<string>().Where(s => !s.All(char.IsAsciiDigit)));

    internal static bool SensitiveArg(OperationDescriptor op, IReadOnlyList<object> path)
    {
        if (path.Count > 0 && path[0] is string head && op.Params.Any(p => p.Sensitive && p.Key == head))
        {
            return true;
        }
        var dotted = DottedArgPath(path);
        if (dotted.Length > 0 && SensitiveRequestPaths(op).Any(f => dotted == f || dotted.StartsWith(f + ".", StringComparison.Ordinal)))
        {
            return true;
        }
        return path.OfType<string>().Any(Util.LooksSensitive);
    }

    /// <summary>Redacts the sensitive fields below the argument at path.</summary>
    private static JsonNode? RedactBelow(OperationDescriptor op, IReadOnlyList<object> path, JsonNode? value)
    {
        var dotted = DottedArgPath(path);
        var prefix = dotted.Length > 0 ? dotted + "." : "";
        var below = SensitiveRequestPaths(op).Where(f => f.StartsWith(prefix, StringComparison.Ordinal)).Select(f => f[prefix.Length..]).ToList();
        return below.Count == 0 ? value : Util.RedactPaths(value, below);
    }

    /// <summary>The sensitive argument paths relative to the body as sent; "" is the whole body.</summary>
    private static List<string> SensitiveBodyPaths(OperationDescriptor op)
    {
        var paths = SensitiveRequestPaths(op).ToList();
        var relative = new List<string>();
        if (op.Body is { Arg: null } merged)
        {
            foreach (var p in paths)
            {
                var segs = Util.SplitPath(p);
                if (segs.Count == 0)
                {
                    continue;
                }
                var f = merged.Fields.FirstOrDefault(x => x.Arg == segs[0]);
                if (f is not null)
                {
                    var rest = string.Join(".", segs.Skip(1));
                    relative.Add(rest.Length == 0 ? f.Wire : f.Wire + "." + rest);
                }
            }
        }
        else if (op.Body is { Arg: string arg })
        {
            foreach (var p in paths)
            {
                if (p == arg)
                {
                    relative.Add("");
                }
                else if (p.StartsWith(arg + ".", StringComparison.Ordinal))
                {
                    relative.Add(p[(arg.Length + 1)..]);
                }
            }
        }
        else if (op.Rpc is not null)
        {
            relative.AddRange(paths);
        }
        if (op.Rpc is null)
        {
            return relative;
        }
        return relative.Select(p => p.Length == 0 ? op.Rpc.ParamsField : op.Rpc.ParamsField + "." + p).ToList();
    }

    /// <summary>Every string or number found at a path, through arrays.</summary>
    private static void ValuesAt(JsonNode? v, List<string> segments, int at, List<string> outList, int depth)
    {
        if (depth > 64)
        {
            return;
        }
        if (v is JsonArray list)
        {
            foreach (var item in list)
            {
                ValuesAt(item, segments, at, outList, depth + 1);
            }
            return;
        }
        if (at >= segments.Count)
        {
            switch (Json.KindOf(v))
            {
                case JsonKind.String:
                    outList.Add(Json.AsString(v) ?? "");
                    break;
                case JsonKind.Integer:
                case JsonKind.Number:
                    outList.Add(Json.NumberText(v));
                    break;
                case JsonKind.Object:
                    foreach (var kv in (JsonObject)v!)
                    {
                        ValuesAt(kv.Value, segments, segments.Count, outList, depth + 1);
                    }
                    break;
            }
            return;
        }
        if (v is JsonObject o && o.TryGetPropertyValue(segments[at], out var next))
        {
            ValuesAt(next, segments, at + 1, outList, depth + 1);
        }
    }

    /// <summary>Adds each parameter's value under its wire name, so references written against the API find arguments whose key differs.</summary>
    internal static JsonObject WithWireNames(OperationDescriptor op, JsonObject args)
    {
        var outObj = Json.CopyObject(args);
        foreach (var p in op.Params)
        {
            if (args.TryGetPropertyValue(p.Key, out var v) && !outObj.ContainsKey(p.Wire))
            {
                Json.Put(outObj, p.Wire, v);
            }
        }
        return outObj;
    }

    /// <summary>Rewrites a leading parameter wire name to its arguments key.</summary>
    private static List<string> ArgPathOf(OperationDescriptor op, List<string> path)
    {
        if (path.Count == 0)
        {
            return path;
        }
        var p = op.Params.FirstOrDefault(x => x.Wire == path[0] && x.Key != path[0]);
        if (p is null)
        {
            return path;
        }
        var outList = new List<string> { p.Key };
        outList.AddRange(path.Skip(1));
        return outList;
    }

    private static readonly Regex PlaceholderRef = new("\\{([^{}]+)\\}", RegexOptions.CultureInvariant);

    /// <summary>Fills {field} references from the arguments; sensitive ones are redacted, unset ones shown as &lt;unset&gt;.</summary>
    internal static string Interpolate(string template, JsonObject args, OperationDescriptor op) =>
        PlaceholderRef.Replace(template, m =>
        {
            var inner = m.Groups[1].Value.Trim();
            var segments = ArgPathOf(op, inner.Split('.').ToList());
            if (!Util.GetPath(args, true, segments, out var value) || Json.IsNull(value))
            {
                return "<unset>";
            }
            if (SensitiveArg(op, segments.Cast<object>().ToList()))
            {
                return Util.Redacted;
            }
            var shown = Util.EnvelopeValue(value, false);
            return Json.TryString(shown, out var text) ? text : Json.Text(shown);
        });

    /// <summary>Shows the placeholders of args in a preview URL as written instead of percent-encoded.</summary>
    internal static string UnescapePlaceholders(string url, JsonNode? args)
    {
        var found = new List<string>();
        Expr.PlaceholdersIn(args, found, 0);
        foreach (var text in found.Distinct())
        {
            url = url.Replace(Serialize.Component(text), text, StringComparison.Ordinal);
        }
        return url;
    }

    /// <summary>" with name value, ..." for remediation text.</summary>
    internal static string WithArguments(JsonNode? args)
    {
        if (args is not JsonObject o || o.Count == 0)
        {
            return "";
        }
        return " with " + string.Join(", ", o.Select(kv => kv.Key + " " + Util.ShortJson(kv.Value)));
    }

    /// <summary>" (name value, ...)": the scalar, non-sensitive arguments a call was made with (at most four, by wire name).</summary>
    internal static string SentArguments(OperationDescriptor op, JsonObject args)
    {
        var shown = new List<string>();
        foreach (var kv in args)
        {
            if (shown.Count == 4)
            {
                break;
            }
            if (Json.KindOf(kv.Value) is not (JsonKind.String or JsonKind.Integer or JsonKind.Number or JsonKind.Boolean))
            {
                continue;
            }
            if (SensitiveArg(op, new List<object> { kv.Key }))
            {
                continue;
            }
            var wire = op.Params.FirstOrDefault(p => p.Key == kv.Key)?.Wire ?? kv.Key;
            shown.Add(wire + " " + Util.ShortJson(kv.Value));
        }
        return shown.Count == 0 ? "" : " (" + string.Join(", ", shown) + ")";
    }

    /// <summary>A structural problem that makes the descriptor unusable, or null.</summary>
    private static string? DescriptorProblem(OperationDescriptor op)
    {
        if (op.Id.Length == 0)
        {
            return "`id` must be a non-empty string";
        }
        if (op.Params.Any(p => p.Key.Length == 0 || p.Wire.Length == 0))
        {
            return "every parameter needs a `name` and a `wire`";
        }
        if (op.Rpc is not null && (op.Rpc.Field.Length == 0 || op.Rpc.ParamsField.Length == 0))
        {
            return "`rpc` needs `field` and `params_field`";
        }
        return null;
    }

    // ------------------------------------------------------------ validation

    private Failure Validation(OperationDescriptor op, IReadOnlyList<object> path, JsonNode? value, bool present, string expected, string remediation)
    {
        var received = present ? Util.EnvelopeValue(RedactBelow(op, path, value), SensitiveArg(op, path)) : Util.EnvelopeValue(null, SensitiveArg(op, path));
        return new Diag(op.Id, Category.ValidationFailed)
            .Param(ArgumentPath(op, path))
            .Received(received)
            .Expected(expected)
            .Remedy(remediation)
            .Fail();
    }

    private JsonObject ValidateArgs(OperationDescriptor op, JsonObject args, bool placeholders)
    {
        var parameters = op.Arguments.ToList();
        foreach (var p in parameters)
        {
            bool present = args.TryGetPropertyValue(p.Key, out var v);
            if (p.Required && (!present || Json.IsNull(v)))
            {
                throw Validation(op, KeyPath(p.Key), v, present,
                    $"a value for the required {p.In} parameter {p.Wire}",
                    $"Pass {p.Key}; {op.Id} cannot be called without it.");
            }
        }
        var body = op.Body;
        if (body is { Required: true, Arg: string bodyArg } && !args.ContainsKey(bodyArg))
        {
            throw Validation(op, KeyPath(bodyArg), null, false, "a request body", $"Pass the request body as {bodyArg}.");
        }
        if (op.Request is { } request)
        {
            IReadOnlyList<Issue> issues;
            try
            {
                issues = request.Validate(args);
            }
            catch (Exception e) when (e is not Failure)
            {
                throw new Diag(op.Id, Category.UnexpectedResponse).Retry(Retryable.Never)
                    .Remedy("The SDK failed while building or sending the request (the request validator panicked). This is a bug in the generated SDK or the runtime; report it. Nothing was sent.").Fail();
            }
            if (issues.Count == 0)
            {
                return args;
            }
            Issue? issue;
            if (placeholders)
            {
                bool Pending(IReadOnlyList<object> path)
                {
                    var segs = Schema.Segments(path);
                    for (int i = 0; i < segs.Count; i++)
                    {
                        if (Util.GetPath(args, true, segs.Take(i + 1), out var v) && Expr.ContainsPlaceholder(v))
                        {
                            return true;
                        }
                    }
                    return false;
                }
                issue = issues.FirstOrDefault(i => !Pending(i.Path));
                if (issue is null)
                {
                    return args;
                }
            }
            else
            {
                issue = issues[0];
            }
            var shown = ArgumentPath(op, issue.Path);
            bool has = Util.GetPath(args, true, Schema.Segments(issue.Path), out var value);
            throw Validation(op, issue.Path, value, has, issue.Message, $"Fix {shown} ({issue.Message}) and call again.");
        }
        // Without a validator, unknown keys are rejected so a misspelt argument is never dropped
        // (rpc methods without a body take free-form args).
        if (op.Rpc is not null && body is null)
        {
            return args;
        }
        var allowed = new HashSet<string>(parameters.Select(p => p.Key), StringComparer.Ordinal);
        if (body is not null)
        {
            if (body.Arg is not null)
            {
                allowed.Add(body.Arg);
            }
            else
            {
                foreach (var f in body.Fields)
                {
                    allowed.Add(f.Arg);
                }
            }
        }
        foreach (var kv in args)
        {
            if (allowed.Contains(kv.Key))
            {
                continue;
            }
            var names = allowed.OrderBy(n => n, StringComparer.Ordinal).Take(20).ToList();
            var list = names.Count > 0 ? string.Join(", ", names) : "no arguments";
            throw Validation(op, KeyPath(kv.Key), kv.Value, true, "one of: " + list, $"Remove {kv.Key}; {op.Id} does not accept it.");
        }
        return args;
    }

    private void CheckKey(OperationDescriptor op, string? key, Purpose purpose)
    {
        var policy = op.Agent.Idempotency;
        var note = string.IsNullOrEmpty(policy.Note) ? "" : " " + policy.Note;
        if (key is null)
        {
            if (purpose == Purpose.Call && policy.Policy == IdempotencyKind.CallerOwned && policy.PersistRequired)
            {
                var fresh = (policy.Format ?? "").ToLowerInvariant().Contains("uuid", StringComparison.Ordinal) ? "UUIDv4" : "unique key";
                throw new Diag(op.Id, Category.ValidationFailed)
                    .Param("IdempotencyKey")
                    .Expected(Keys.FormatDescription(policy.Format))
                    .Remedy($"Generate a random {fresh}, persist it with the intent of this {op.Id} call, and pass it as IdempotencyKey. Reuse the exact same key on every retry; a new key can apply the effect twice.{note}")
                    .Fail();
            }
            return;
        }
        if (policy.Policy == IdempotencyKind.None && !op.Params.Any(p => p.Role == "idempotency_key"))
        {
            return;
        }
        if (policy.Policy is IdempotencyKind.ContentIdentity or IdempotencyKind.ContentHash)
        {
            return;
        }
        var format = policy.Policy == IdempotencyKind.CallerOwned ? policy.Format : null;
        if (Keys.CheckFormat(key, format) is string expected)
        {
            throw new Diag(op.Id, Category.ValidationFailed)
                .Param("IdempotencyKey")
                .Received(Util.EnvelopeValue(JsonValue.Create(key), false))
                .Expected(expected)
                .Remedy($"Pass IdempotencyKey as {expected}. Generate it once, persist it with the intent of this call, and reuse it on every retry.{note}")
                .Fail();
        }
    }

    /// <summary>
    /// The confirmation rule of a tier: destructive accepts Confirm.Yes (unless refused) or a
    /// token, irreversible only a token issued for the subject and these exact args.
    /// </summary>
    internal void Confirmed(ConfirmTarget t, Safety safety, JsonNode? args, CallOptions opts)
    {
        if (safety != Safety.Destructive && safety != Safety.Irreversible)
        {
            return;
        }
        var tier = safety == Safety.Destructive ? "destructive" : "irreversible";
        bool allowYes = !opts.RefuseConfirmYes;
        var how = $"call {t.PreviewCall} with the same arguments and pass its confirmation_token as confirm";
        Failure Required(string remediation) => new Diag(t.Id, Category.ConfirmationRequired)
            .Param("confirm")
            .Expected("a confirmation_token from preview()")
            .Remedy(remediation)
            .Fail();
        var confirm = opts.Confirm;
        if (confirm is { IsYes: true })
        {
            if (safety == Safety.Destructive && allowYes)
            {
                return;
            }
            throw Required($"{t.Id} is {tier}, so Confirm.Yes is not accepted: {how}.");
        }
        if (confirm?.TokenValue is string token)
        {
            switch (ConfirmationTokens.Verify(confirmationKey, token, t.Subject, args, Now()))
            {
                case ConfirmationTokens.Check.Valid:
                    return;
                case ConfirmationTokens.Check.Expired:
                    throw Required($"The confirmation token expired (tokens last {ConfirmationTokens.TtlMs / 60000} minutes): {how}.");
                default:
                    throw Required($"The confirmation token was not issued by this client for {t.Id} with exactly these arguments: {how}.");
            }
        }
        var orYes = safety == Safety.Destructive && allowYes ? " (or pass Confirm.Yes)" : "";
        throw Required($"{t.Id} is {tier}: {how}{orYes}.");
    }

    /// <summary>
    /// Spends a valid token on the call about to be sent: its first use binds it to that call's
    /// replay protection, and a later use is accepted only as a retry under the same protection.
    /// </summary>
    internal Failure? ClaimToken(string id, string token, string? bind, string previewCall)
    {
        long now = Now();
        lock (usedTokensLock)
        {
            foreach (var expired in usedTokens.Where(kv => kv.Value.Expiry <= now).Select(kv => kv.Key).ToList())
            {
                usedTokens.Remove(expired);
            }
            if (usedTokens.TryGetValue(token, out var previous))
            {
                if (previous.Bind is not null && bind is not null && previous.Bind == bind)
                {
                    return null;
                }
                string why = previous.Bind is null
                    ? "This call has no idempotency key, so a repeat can apply the effect twice"
                    : bind is null
                        ? "It was used with an idempotency key; only a retry with that same key may reuse it"
                        : "It was used with another idempotency key; only a retry with that same key may reuse it";
                return new Diag(id, Category.ConfirmationRequired)
                    .Param("confirm")
                    .Expected("a confirmation_token from preview()")
                    .Remedy($"This confirmation token was already used for one {id} call. {why}. Check whether that call took effect; to send again, call {previewCall} and confirm with its new token.")
                    .Fail();
            }
            long expiry = ConfirmationTokens.TryExpiry(token, out var e, out _) ? e : now + ConfirmationTokens.TtlMs;
            usedTokens[token] = (bind, expiry);
            return null;
        }
    }

    // ------------------------------------------------------------ prepare

    internal async Task<Prepared> PrepareAsync(OperationDescriptor? op, JsonNode? rawArgs, CallOptions opts, Purpose purpose, string? urlOverride, MacroClaim? claim, CancellationToken ct)
    {
        if (op is null)
        {
            throw new Diag("<unknown operation>", Category.ValidationFailed).Param("operation")
                .Expected("a valid operation descriptor")
                .Remedy("The operation descriptor is invalid: the descriptor is missing. Regenerate the SDK or fix the descriptor; nothing was sent.").Fail();
        }
        if (DescriptorProblem(op) is string problem)
        {
            throw new Diag(op.Id.Length > 0 ? op.Id : "<unknown operation>", Category.ValidationFailed).Param("operation")
                .Expected("a valid operation descriptor")
                .Remedy($"The operation descriptor is invalid: {problem}. Regenerate the SDK or fix the descriptor; nothing was sent.").Fail();
        }
        Register(new[] { op });
        JsonObject supplied;
        if (rawArgs is null)
        {
            supplied = new JsonObject();
        }
        else if (rawArgs is JsonObject obj)
        {
            supplied = (JsonObject)Json.Normalize(obj)!;
        }
        else
        {
            var names = op.Arguments.Take(3).Select(p => p.Key + ": ...");
            throw new Diag(op.Id, Category.ValidationFailed).Param("args")
                .Received(Util.EnvelopeValue(rawArgs, false))
                .Expected("an object of named arguments")
                .Remedy($"Pass the arguments of {op.Id} as one object, e.g. {{{string.Join(", ", names)}}}.").Fail();
        }
        if (Util.NestsDeeperThan(supplied, Util.MaxArgDepth))
        {
            throw Validation(op, new List<object>(), JsonValue.Create("<deeply nested value>"), true,
                $"a JSON value nested at most {Util.MaxArgDepth} levels", "Pass arguments without such deep nesting.");
        }
        var args = ValidateArgs(op, supplied, purpose == Purpose.MacroPreview);
        var key = string.IsNullOrEmpty(opts.IdempotencyKey) ? null : opts.IdempotencyKey;
        CheckKey(op, key, purpose);
        if (purpose == Purpose.Call && claim is null)
        {
            Confirmed(new ConfirmTarget(op.Id, op.Id, "preview(...)"), op.Agent.Safety, args, opts);
        }
        var method = op.Method;
        var authResult = await ResolveAuthAsync(op, method, ct).ConfigureAwait(false);
        if (authResult.Failed is not null)
        {
            throw new Diag(op.Id, Category.AuthFailed).Remedy(authResult.Failed).Fail();
        }
        var secrets = new SecretSet();
        foreach (var s in authResult.Secrets)
        {
            secrets.Insert(s);
        }
        foreach (var p in op.Params.Where(p => p.Sensitive))
        {
            if (args.TryGetPropertyValue(p.Key, out var v))
            {
                if (Json.TryString(v, out var sv))
                {
                    secrets.Insert(sv);
                }
                else if (Json.KindOf(v) is JsonKind.Integer or JsonKind.Number)
                {
                    secrets.Insert(Json.NumberText(v));
                }
            }
        }
        foreach (var path in SensitiveRequestPaths(op))
        {
            var found = new List<string>();
            ValuesAt(args, Util.SplitPath(path), 0, found, 0);
            foreach (var v in found)
            {
                secrets.Insert(v);
            }
        }
        var (fullUrl, displayUrl) = BuildUrl(op, args, authResult.Plan, urlOverride);
        var headerBag = new HeaderBag();
        BuildHeaders(op, args, opts, authResult.Plan, headerBag);

        var paramKeys = new HashSet<string>(op.Arguments.Select(p => p.Key), StringComparer.Ordinal);
        var bodyPaths = SensitiveBodyPaths(op);
        JsonNode? value = null;
        bool hasValue = false;
        if (!IsSafeMethod(method) || method == "OPTIONS")
        {
            (value, hasValue) = Serialize.BodyValue(op, args, paramKeys);
        }
        JsonNode? Redact(JsonNode? display)
        {
            if (bodyPaths.Contains(""))
            {
                return JsonValue.Create(Util.Redacted);
            }
            return Util.RedactSensitiveKeys(Util.RedactPaths(display, bodyPaths), 0);
        }
        EncodedBody encoded;
        try
        {
            encoded = Serialize.Body(op.Body, value, hasValue, Redact);
        }
        catch (SerializationError e)
        {
            var path = op.Body?.Arg is string arg && e.Parameter == "body" ? KeyPath(arg) : new List<object>();
            throw Validation(op, path, e.Value, true, e.ExpectedText, $"Pass {e.Parameter} as {e.ExpectedText}.");
        }
        if (encoded.ContentType is not null)
        {
            headerBag.Set("Content-Type", encoded.ContentType, false);
        }
        var header = Keys.Header(op);
        var idem = await IdempotencyKeyAsync(op, args, key, purpose, encoded, ct).ConfigureAwait(false);
        if (idem is not null)
        {
            headerBag.Set(header, idem, true);
            secrets.Insert(idem);
        }
        else if (purpose != Purpose.Call && op.Agent.Idempotency.Policy is IdempotencyKind.Auto or IdempotencyKind.CallerOwned)
        {
            headerBag.Set(header, "<set at call time>", false);
        }
        foreach (var h in authResult.Plan.Headers)
        {
            headerBag.Set(h.Name, h.Value, h.Secret);
        }
        if (purpose == Purpose.ServerPreview && op.Agent.Preview.Mode == "header")
        {
            headerBag.Set(op.Agent.Preview.Header ?? "", op.Agent.Preview.Value ?? "", false);
        }
        foreach (var e in headerBag.Entries)
        {
            if (Serialize.ValidHeaderName(e.Name) && Serialize.ValidHeaderValue(e.Value))
            {
                continue;
            }
            if (authResult.Plan.Headers.Any(h => string.Equals(h.Name, e.Name, StringComparison.OrdinalIgnoreCase)))
            {
                throw new Diag(op.Id, Category.AuthFailed)
                    .Remedy($"The credential for header {e.Name} contains characters not allowed in an HTTP header; check ClientOptions.auth.").Fail();
            }
            var param = op.Params.FirstOrDefault(p => string.Equals(p.Wire, e.Name, StringComparison.OrdinalIgnoreCase));
            var path = param is null ? new List<object>() : KeyPath(param.Key);
            JsonNode shown = JsonValue.Create(e.Secret ? Util.Redacted : e.Value);
            throw Validation(op, path, shown, true,
                "a header value of visible ASCII characters (no line breaks)",
                $"Header {e.Name} cannot carry this value; remove line breaks and non-Latin-1 characters.");
        }
        if (purpose == Purpose.Call)
        {
            if (claim is not null)
            {
                claim.Claim(this);
            }
            else if (opts.Confirm?.TokenValue is string token && op.Agent.Safety is Safety.Destructive or Safety.Irreversible)
            {
                var bind = idem;
                if (bind is null && op.Agent.Idempotency.Policy == IdempotencyKind.ContentIdentity)
                {
                    bind = "content-identity";
                }
                if (ClaimToken(op.Id, token, bind, "preview(...)") is Failure f)
                {
                    throw f;
                }
            }
        }
        foreach (var s in headerBag.Secrets)
        {
            secrets.Insert(s);
        }
        var hidden = authResult.Plan.Query.Select(q => q.K).ToList();
        hidden.AddRange(op.Params.Where(p => p.In == "query" && p.Sensitive).Select(p => p.Wire));
        return new Prepared
        {
            Op = op,
            Args = args,
            Method = method,
            Url = fullUrl,
            DisplayUrl = displayUrl,
            Headers = headerBag,
            Payload = encoded.Payload,
            DisplayBody = encoded.Display,
            Key = idem,
            KeyHeader = header,
            AuthQuery = authResult.Plan.Query,
            HiddenQuery = hidden,
            Secrets = secrets,
            OAuth = authResult.OAuth,
        };
    }

    private Failure BadBase(OperationDescriptor op) =>
        new Diag(op.Id, Category.TransportFailed).Retry(Retryable.Never)
            .Remedy("The base URL is not a valid absolute http(s) URL without credentials: fix ClientOptions.BaseUrl. Nothing was sent.").Fail();

    private (string Full, string Display) BuildUrl(OperationDescriptor op, JsonObject args, AuthPlan plan, string? urlOverride)
    {
        if (baseUrl.Length == 0 && urlOverride is null)
        {
            throw new Diag(op.Id, Category.TransportFailed).Retry(Retryable.Never)
                .Remedy("No base URL is configured: set ClientOptions.BaseUrl. Nothing was sent.").Fail();
        }
        var parameters = op.Arguments.ToList();
        string full, display;
        if (urlOverride is not null)
        {
            full = urlOverride;
            display = urlOverride;
        }
        else
        {
            string? unknown = null;
            ParamDescriptor? missing = null;
            var path = new StringBuilder();
            var rest = op.Path;
            while (true)
            {
                int open = rest.IndexOf('{', StringComparison.Ordinal);
                if (open < 0)
                {
                    break;
                }
                int closeRel = rest[open..].IndexOf('}', StringComparison.Ordinal);
                if (closeRel < 0)
                {
                    break;
                }
                int end = open + closeRel;
                var inner = rest[(open + 1)..end];
                if (inner.Length == 0 || inner.Contains('{', StringComparison.Ordinal))
                {
                    path.Append(rest, 0, open + 1);
                    rest = rest[(open + 1)..];
                    continue;
                }
                path.Append(rest, 0, open);
                var p = parameters.FirstOrDefault(x => x.In == "path" && x.Wire == inner)
                    ?? parameters.FirstOrDefault(x => x.In == "path" && x.Key == inner);
                if (p is null)
                {
                    unknown = inner;
                    path.Append(rest, open, end + 1 - open);
                }
                else if (!args.TryGetPropertyValue(p.Key, out var v) || Json.IsNull(v))
                {
                    missing = p;
                    path.Append(rest, open, end + 1 - open);
                }
                else
                {
                    path.Append(Serialize.PathParam(p, v));
                }
                rest = rest[(end + 1)..];
            }
            path.Append(rest);
            var built = path.ToString();
            if (unknown is not null)
            {
                throw new Diag(op.Id, Category.ValidationFailed).Param("operation")
                    .Expected("a valid operation descriptor")
                    .Remedy($"The path template has a placeholder {{{unknown}}} with no path parameter; regenerate the SDK.").Fail();
            }
            if (missing is not null)
            {
                throw Validation(op, KeyPath(missing.Key), null, false, "a value for path parameter " + missing.Wire, $"Pass {missing.Key}.");
            }
            var template = op.Path.Split('/');
            var segments = built.Split('/');
            if (template.Length == segments.Length)
            {
                for (int i = 0; i < segments.Length; i++)
                {
                    var tmpl = template[i];
                    var segment = segments[i];
                    if (!tmpl.Contains('{', StringComparison.Ordinal) || !IsDotSegment(segment))
                    {
                        continue;
                    }
                    int b = tmpl.IndexOf('{', StringComparison.Ordinal);
                    var after = tmpl[(b + 1)..];
                    int c = after.IndexOf('}', StringComparison.Ordinal);
                    var wire = c >= 0 ? after[..c] : after;
                    var p = parameters.FirstOrDefault(x => x.In == "path" && (x.Wire == wire || x.Key == wire));
                    JsonNode? shownValue = JsonValue.Create(segment);
                    var at = new List<object>();
                    var name = "the path parameter";
                    if (p is not null)
                    {
                        args.TryGetPropertyValue(p.Key, out shownValue);
                        at = KeyPath(p.Key);
                        name = p.Key;
                    }
                    throw Validation(op, at, shownValue, true,
                        "a non-empty path segment other than . and ..",
                        $"Pass {name} as the identifier of one resource; \"{segment}\" would change the request path.");
                }
            }
            var parts = new List<string>();
            var shown = new List<string>();
            foreach (var p in parameters.Where(x => x.In == "query"))
            {
                if (!args.TryGetPropertyValue(p.Key, out var v))
                {
                    continue;
                }
                var serialized = Serialize.QueryParam(p, v);
                foreach (var s in serialized)
                {
                    shown.Add(p.Sensitive ? s.Split('=')[0] + "=" + Util.Redacted : s);
                }
                parts.AddRange(serialized);
            }
            foreach (var (k, v) in plan.Query)
            {
                parts.Add(Serialize.Component(k) + "=" + Serialize.Component(v));
                shown.Add(Serialize.Component(k) + "=" + Util.Redacted);
            }
            var root = baseUrl.TrimEnd('/');
            string Join(List<string> list) => list.Count == 0 ? "" : (built.Contains('?', StringComparison.Ordinal) ? "&" : "?") + string.Join("&", list);
            full = root + built + Join(parts);
            display = root + built + Join(shown);
        }
        if (urlOverride is not null && plan.Query.Count > 0)
        {
            if (!Uri.TryCreate(full, UriKind.Absolute, out _))
            {
                throw BadBase(op);
            }
            foreach (var (k, v) in plan.Query)
            {
                if (!Urls.Has(full, k))
                {
                    full = Urls.Set(full, k, v);
                }
            }
            foreach (var (k, _) in plan.Query)
            {
                display = Urls.Set(display, k, Util.Redacted);
            }
        }
        if (!ValidBaseUrl(full))
        {
            throw BadBase(op);
        }
        return (full, display);
    }

    private void BuildHeaders(OperationDescriptor op, JsonObject args, CallOptions opts, AuthPlan plan, HeaderBag bag)
    {
        var accept = new List<string>();
        foreach (var r in op.Responses)
        {
            if (r.Kind == "success" && !string.IsNullOrEmpty(r.MediaType) && !accept.Contains(r.MediaType))
            {
                accept.Add(r.MediaType);
            }
        }
        if (accept.Count > 0)
        {
            bag.Set("Accept", string.Join(", ", accept), false);
        }
        var apiName = string.IsNullOrEmpty(api.Name) ? "api" : api.Name;
        var apiVersion = string.IsNullOrEmpty(api.Version) ? "0" : api.Version;
        bag.Set("X-Tungsten-Runtime", $"tungsten-{identity.Runtime}/{identity.Version} {apiName}-sdk/{apiVersion}", false);
        bag.Set("X-Tungsten-Operation", op.Id, false);
        var tungsten = string.IsNullOrEmpty(api.TungstenVersion) ? Version : api.TungstenVersion;
        bag.Set("User-Agent", $"{apiName}-sdk/{apiVersion} tungsten/{tungsten} ({identity.Language})", false);
        foreach (var p in op.Params)
        {
            if (p.Role == "constant" && p.In == "header" && p.Constant is not null)
            {
                bag.Set(p.Wire, p.Constant, p.Sensitive);
            }
        }
        foreach (var extra in new[] { headers, opts.Headers ?? new Dictionary<string, string>() })
        {
            foreach (var kv in extra.OrderBy(kv => kv.Key, StringComparer.Ordinal))
            {
                bag.Set(kv.Key, kv.Value, Util.LooksSensitive(kv.Key));
            }
        }
        var cookies = new List<string>();
        if (bag.Get("Cookie") is string existing)
        {
            cookies.Add(existing);
        }
        foreach (var p in op.Arguments)
        {
            if (!args.TryGetPropertyValue(p.Key, out var v) || Json.IsNull(v))
            {
                continue;
            }
            if (p.In == "header")
            {
                bag.Set(p.Wire, Serialize.HeaderParam(p, v), p.Sensitive);
            }
            else if (p.In == "cookie")
            {
                cookies.Add(Serialize.CookieParam(p, v));
            }
        }
        foreach (var (k, v) in plan.Cookies)
        {
            cookies.Add(k + "=" + v);
        }
        // Cookies are session material: the whole header is always redacted.
        if (cookies.Count > 0)
        {
            bag.Set("Cookie", string.Join("; ", cookies), true);
        }
    }

    private async Task<string?> IdempotencyKeyAsync(OperationDescriptor op, JsonObject args, string? supplied, Purpose purpose, EncodedBody encoded, CancellationToken ct)
    {
        switch (op.Agent.Idempotency.Policy)
        {
            case IdempotencyKind.ContentIdentity:
                return null;
            case IdempotencyKind.ContentHash:
                return encoded.Hashed is null ? null : Util.Sha256Hex(encoded.Hashed);
            case IdempotencyKind.CallerOwned:
                return supplied;
            case IdempotencyKind.Auto:
                {
                    if (supplied is not null)
                    {
                        return supplied;
                    }
                    if (purpose != Purpose.Call)
                    {
                        return null;
                    }
                    Failure Refuse(string why) => new Diag(op.Id, Category.TransportFailed).Retry(Retryable.Never)
                        .Remedy($"The idempotency store failed ({why}), so the call was not sent rather than risk a second key for the same intent. Fix or replace ClientOptions.IdempotencyStore.").Fail();
                    var logical = Util.Sha256Hex(Json.Canonical(args));
                    await keyLock.WaitAsync(ct).ConfigureAwait(false);
                    try
                    {
                        try
                        {
                            string key = IdempotencyReservation.Reserve(store, op.Id, logical, Util.UuidV4());
                            if (string.IsNullOrEmpty(key))
                            {
                                throw Refuse("the store returned no key");
                            }
                            return key;
                        }
                        catch (Failure)
                        {
                            throw;
                        }
                        catch (Exception)
                        {
                            throw Refuse("the key could not be stored");
                        }
                    }
                    finally
                    {
                        keyLock.Release();
                    }
                }
        }
        return op.Params.Any(p => p.Role == "idempotency_key") ? supplied : null;
    }
}
