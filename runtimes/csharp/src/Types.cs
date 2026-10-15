// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json.Nodes;

namespace Tungsten.Runtime;

/// <summary>The tier of an operation.</summary>
public enum Safety
{
    ReadOnly,
    Mutating,
    Destructive,
    Irreversible,
}

/// <summary>Whether and how a failed call may be repeated.</summary>
public enum Retryable
{
    Never,
    AfterDelay,
    SameKeyOnly,
    AfterRemediation,
}

/// <summary>The closed set of error categories shared by every tungsten runtime.</summary>
public enum Category
{
    ValidationFailed,
    MalformedRequest,
    RequestTooLarge,
    AuthFailed,
    NotFound,
    Conflict,
    PreconditionFailed,
    RateLimited,
    UpstreamUnavailable,
    OutcomeUnknown,
    TransportFailed,
    ConfirmationRequired,
    GateDisabled,
    UnexpectedResponse,
}

/// <summary>An operation's idempotency policy.</summary>
public enum IdempotencyKind
{
    None,
    Auto,
    CallerOwned,
    ContentHash,
    ContentIdentity,
}

/// <summary>How success bodies are checked against the API description.</summary>
public enum ValidateResponses
{
    /// <summary>Reports through <see cref="ClientOptions.OnDiagnostic"/> and never fails the call (the default).</summary>
    Warn,
    Off,
    Strict,
}

/// <summary>The retry backoff jitter.</summary>
public enum Jitter
{
    None,
    Full,
    Equal,
}

/// <summary>The contract spellings of the enums.</summary>
public static class Wire
{
    public static string Of(Safety s) => s switch
    {
        Safety.ReadOnly => "read_only",
        Safety.Mutating => "mutating",
        Safety.Destructive => "destructive",
        _ => "irreversible",
    };

    public static string Of(Retryable r) => r switch
    {
        Retryable.Never => "never",
        Retryable.AfterDelay => "after_delay",
        Retryable.SameKeyOnly => "same_key_only",
        _ => "after_remediation",
    };

    public static string Of(Category c) => c switch
    {
        Category.ValidationFailed => "VALIDATION_FAILED",
        Category.MalformedRequest => "MALFORMED_REQUEST",
        Category.RequestTooLarge => "REQUEST_TOO_LARGE",
        Category.AuthFailed => "AUTH_FAILED",
        Category.NotFound => "NOT_FOUND",
        Category.Conflict => "CONFLICT",
        Category.PreconditionFailed => "PRECONDITION_FAILED",
        Category.RateLimited => "RATE_LIMITED",
        Category.UpstreamUnavailable => "UPSTREAM_UNAVAILABLE",
        Category.OutcomeUnknown => "OUTCOME_UNKNOWN",
        Category.TransportFailed => "TRANSPORT_FAILED",
        Category.ConfirmationRequired => "CONFIRMATION_REQUIRED",
        Category.GateDisabled => "GATE_DISABLED",
        _ => "UNEXPECTED_RESPONSE",
    };

    public static string Of(IdempotencyKind k) => k switch
    {
        IdempotencyKind.Auto => "auto",
        IdempotencyKind.CallerOwned => "caller_owned",
        IdempotencyKind.ContentHash => "content_hash",
        IdempotencyKind.ContentIdentity => "content_identity",
        _ => "none",
    };

    public static Safety? ParseSafety(string? s) => s switch
    {
        "read_only" => Safety.ReadOnly,
        "mutating" => Safety.Mutating,
        "destructive" => Safety.Destructive,
        "irreversible" => Safety.Irreversible,
        _ => null,
    };

    public static Retryable? ParseRetryable(string? s) => s switch
    {
        "never" => Retryable.Never,
        "after_delay" => Retryable.AfterDelay,
        "same_key_only" => Retryable.SameKeyOnly,
        "after_remediation" => Retryable.AfterRemediation,
        _ => null,
    };

    public static Category? ParseCategory(string? s)
    {
        foreach (Category c in Enum.GetValues<Category>())
        {
            if (Of(c) == s)
            {
                return c;
            }
        }
        return null;
    }

    public static IdempotencyKind ParseIdempotency(string? s) => s switch
    {
        "auto" => IdempotencyKind.Auto,
        "caller_owned" => IdempotencyKind.CallerOwned,
        "content_hash" => IdempotencyKind.ContentHash,
        "content_identity" => IdempotencyKind.ContentIdentity,
        _ => IdempotencyKind.None,
    };
}

/// <summary>
/// The error envelope. It serializes to exactly the fourteen members of
/// <c>specs/envelope.schema.json</c>, in order, every member present (null when it has no value),
/// identical across the runtimes.
/// </summary>
public sealed record Diagnostic
{
    public Category Category { get; init; }
    public string Operation { get; init; } = "";
    public int? HttpStatus { get; init; }
    public string? Code { get; init; }
    public string? FailedParameter { get; init; }
    /// <summary>Redacted when sensitive; strings cut to 200 characters.</summary>
    public JsonNode? ReceivedValue { get; init; }
    public string? Expected { get; init; }
    public string Remediation { get; init; } = "";
    public Retryable Retryable { get; init; }
    public long? RetryAfterMs { get; init; }
    public string? NextAction { get; init; }
    public string? RequestId { get; init; }
    public int Attempts { get; init; }

    /// <summary>The envelope as a JSON object in the documented member order.</summary>
    public JsonObject ToJson()
    {
        var o = new JsonObject
        {
            ["status"] = "error",
            ["category"] = Wire.Of(Category),
            ["operation"] = Operation,
            ["http_status"] = HttpStatus is int s ? JsonValue.Create((long)s) : null,
            ["code"] = Code,
            ["failed_parameter"] = FailedParameter,
        };
        Json.Put(o, "received_value", ReceivedValue);
        o["expected"] = Expected;
        o["remediation"] = Remediation;
        o["retryable"] = Wire.Of(Retryable);
        o["retry_after_ms"] = RetryAfterMs is long r ? JsonValue.Create(r) : null;
        o["next_action"] = NextAction;
        o["request_id"] = RequestId;
        o["trace"] = new JsonObject { ["attempts"] = (long)Attempts };
        return o;
    }

    /// <summary>"&lt;category&gt; in &lt;operation&gt;: &lt;remediation&gt;".</summary>
    public override string ToString() => $"{Wire.Of(Category)} in {Operation}: {Remediation}";
}

/// <summary>
/// A failed call: the envelope, plus what the failed call or macro already produced when its
/// effect happened (the success body of a mutation whose response failed strict validation, or
/// a macro's completed step results by their <c>as</c> names). <see cref="Partial"/> can hold
/// values the API shows only once, so store them before acting on the error; they are never
/// repeated in the envelope.
/// </summary>
public sealed record TungstenError(Diagnostic Diagnostic)
{
    public JsonNode? Partial { get; init; }

    /// <summary>Whether <see cref="Partial"/> is meaningful.</summary>
    public bool HasPartial { get; init; }

    public override string ToString() => Diagnostic.ToString();
}

/// <summary>A failed call as an exception, for code that prefers <c>try</c>.</summary>
public sealed class TungstenException : Exception
{
    public TungstenException(TungstenError error) : base(error.Diagnostic.ToString())
    {
        Error = error;
    }

    public TungstenError Error { get; }
}

/// <summary>The HTTP answer.</summary>
public sealed record ResponseMeta
{
    public int Status { get; init; }
    /// <summary>Lower-cased names; repeated values joined with ", " (set-cookie keeps its last value).</summary>
    public IReadOnlyDictionary<string, string> Headers { get; init; } = new Dictionary<string, string>();
    public string? RequestId { get; init; }
    public int Attempts { get; init; }

    internal static ResponseMeta Local() => new();
}

/// <summary>The result of an operation's verification hook.</summary>
public sealed record Verification
{
    public bool Checked { get; init; }
    /// <summary>True when expect held.</summary>
    public bool Passed { get; init; }
    public JsonNode? Observed { get; init; }
    public bool HasObserved { get; init; }
    /// <summary>True when polling for terminal ran out of budget.</summary>
    public bool TimedOut { get; init; }
    /// <summary>Why the check could not run; <see cref="Checked"/> is false.</summary>
    public Diagnostic? Error { get; init; }
}

/// <summary>The result of a call: <see cref="Ok"/> or <see cref="Err"/>.</summary>
public abstract record Result<T>
{
    private protected Result() { }

    /// <summary>A success.</summary>
    public sealed record Ok(T Value) : Result<T>
    {
        /// <summary>False when the response had no body.</summary>
        public bool HasValue { get; init; } = true;
        /// <summary>The decoded wire body (typed results keep it too).</summary>
        public JsonNode? Raw { get; init; }
        public ResponseMeta Meta { get; init; } = new();
        public Verification? Verification { get; init; }
    }

    /// <summary>A failure with its envelope.</summary>
    public sealed record Err(TungstenError Error) : Result<T>
    {
        public Diagnostic Diagnostic => Error.Diagnostic;
    }

    public bool IsOk => this is Ok;

    /// <summary>The value; throws <see cref="TungstenException"/> on a failure.</summary>
    public T Unwrap() => this switch
    {
        Ok ok => ok.Value,
        Err err => throw new TungstenException(err.Error),
        _ => throw new InvalidOperationException(),
    };
}

/// <summary>A request rendered without sending it; secrets show as &lt;redacted&gt;.</summary>
public sealed record RenderedRequest
{
    public string Method { get; init; } = "";
    public string Url { get; init; } = "";
    public IReadOnlyDictionary<string, string> Headers { get; init; } = new Dictionary<string, string>();
    public JsonNode? Body { get; init; }
}

/// <summary>One step of a macro preview.</summary>
public sealed record MacroStepPreview
{
    /// <summary>The 1-based position.</summary>
    public int Step { get; init; }
    public string Kind { get; init; } = "";
    public string Operation { get; init; } = "";
    public string? As { get; init; }
    public Safety Safety { get; init; }
    /// <summary>Null when the step's arguments come from an earlier result.</summary>
    public RenderedRequest? Request { get; init; }
    public IReadOnlyList<string> Effects { get; init; } = System.Array.Empty<string>();
}

/// <summary>What a preview returns.</summary>
public sealed record PreviewResult
{
    public string Operation { get; init; } = "";
    public Safety Safety { get; init; }
    /// <summary>The request (for a macro: the first step's).</summary>
    public RenderedRequest Request { get; init; } = new();
    public IReadOnlyList<string> Effects { get; init; } = System.Array.Empty<string>();
    /// <summary>Set for mutating operations; bound to the exact arguments for <see cref="ExpiresInMs"/>.</summary>
    public string? ConfirmationToken { get; init; }
    public long? ExpiresInMs { get; init; }
    /// <summary>The server's answer for header and endpoint previews.</summary>
    public JsonNode? ServerPreview { get; init; }
    public bool HasServerPreview { get; init; }
    /// <summary>Set for macro previews.</summary>
    public IReadOnlyList<MacroStepPreview>? Steps { get; init; }

    /// <summary>The preview as the JSON the other runtimes hand out.</summary>
    public JsonObject ToJson()
    {
        var o = new JsonObject
        {
            ["operation"] = Operation,
            ["safety"] = Wire.Of(Safety),
            ["request"] = Rendered(Request),
            ["effects"] = Json.Strings(Effects),
            ["confirmation_token"] = ConfirmationToken,
            ["expires_in_ms"] = ConfirmationToken is null ? null : JsonValue.Create(ExpiresInMs ?? 0),
        };
        if (HasServerPreview)
        {
            Json.Put(o, "server_preview", ServerPreview);
        }
        if (Steps is not null)
        {
            var steps = new JsonArray();
            foreach (var s in Steps)
            {
                steps.Add((JsonNode)new JsonObject
                {
                    ["step"] = (long)s.Step,
                    ["kind"] = s.Kind,
                    ["operation"] = s.Operation,
                    ["as"] = s.As,
                    ["safety"] = Wire.Of(s.Safety),
                    ["request"] = s.Request is null ? null : Rendered(s.Request),
                    ["effects"] = Json.Strings(s.Effects),
                });
            }
            o["steps"] = steps;
        }
        return o;
    }

    private static JsonObject Rendered(RenderedRequest r)
    {
        var headers = new JsonObject();
        foreach (var kv in r.Headers.OrderBy(kv => kv.Key, StringComparer.Ordinal))
        {
            headers[kv.Key] = kv.Value;
        }
        var o = new JsonObject
        {
            ["method"] = r.Method,
            ["url"] = r.Url,
            ["headers"] = headers,
        };
        Json.Put(o, "body", r.Body);
        return o;
    }
}

/// <summary>One page of a paginated operation.</summary>
public sealed record Page<T>
{
    public IReadOnlyList<T> Items { get; init; } = System.Array.Empty<T>();
    /// <summary>The items as decoded JSON, before typing.</summary>
    public IReadOnlyList<JsonNode?> RawItems { get; init; } = System.Array.Empty<JsonNode?>();
    /// <summary>The full page body.</summary>
    public JsonNode? Body { get; init; }
    /// <summary>The cursor, offset, page number or URL of the next page; null at the end.</summary>
    public JsonNode? Next { get; init; }
}

/// <summary>The body a poll ended with.</summary>
public sealed record Polled
{
    public JsonNode? Body { get; init; }
    public bool HasBody { get; init; }
    public bool TimedOut { get; init; }
}

/// <summary>
/// Who the runtime says it is: the <c>(language)</c> token of <c>User-Agent</c> and the
/// <c>tungsten-&lt;runtime&gt;/&lt;version&gt;</c> of <c>X-Tungsten-Runtime</c>. An SDK in another .NET
/// language built on this runtime (F#) passes its own identity to <see cref="ClientCore"/>; the
/// prose of envelopes keeps this runtime's spellings.
/// </summary>
public sealed record RuntimeIdentity(string Language, string Runtime, string Version)
{
    /// <summary>The C# runtime's identity: <c>(csharp)</c>, <c>tungsten-cs/</c><see cref="ClientCore.Version"/>.</summary>
    public static RuntimeIdentity CSharp { get; } = new("csharp", "cs", ClientCore.Version);
}

// ------------------------------------------------------------------ options

/// <summary>
/// The credential of one auth scheme: a secret string for api_key, http_bearer and http_basic
/// ("user:password") schemes, or parts keyed by cookie name or config key for a composite profile
/// (or the client_id, client_secret of an OAuth2 scheme).
/// </summary>
public sealed class Credential
{
    private Credential(string secret, IReadOnlyDictionary<string, string>? parts)
    {
        SecretValue = secret;
        PartValues = parts;
    }

    internal string SecretValue { get; }

    internal IReadOnlyDictionary<string, string>? PartValues { get; }

    internal bool IsParts => PartValues is not null;

    /// <summary>A secret string credential.</summary>
    public static Credential Secret(string value) => new(value, null);

    /// <summary>A credential made of named parts.</summary>
    public static Credential Parts(IReadOnlyDictionary<string, string> parts) =>
        new("", new Dictionary<string, string>(parts, StringComparer.Ordinal));

    /// <summary>Never shows the credential.</summary>
    public override string ToString() => IsParts
        ? $"Credential.Parts({PartValues!.Count} parts, {Util.Redacted})"
        : $"Credential.Secret({Util.Redacted})";
}

/// <summary>
/// Confirms a destructive or irreversible operation: a token from a preview
/// (<see cref="Token"/>) or, for destructive operations only, <see cref="Yes"/>.
/// </summary>
public sealed class Confirm
{
    private Confirm(string? token, bool yes)
    {
        TokenValue = token;
        IsYes = yes;
    }

    internal string? TokenValue { get; }

    internal bool IsYes { get; }

    /// <summary>Accepts a destructive (never an irreversible) operation without a token.</summary>
    public static Confirm Yes { get; } = new(null, true);

    /// <summary>Confirms with a token from a preview, bound to the operation and the exact arguments.</summary>
    public static Confirm Token(string token) => new(token, false);

    /// <summary>Never shows the token.</summary>
    public override string ToString() => IsYes ? "Confirm.Yes" : $"Confirm.Token({Util.Redacted})";
}

/// <summary>Retry settings; null members are unset (the API's per-tier defaults apply).</summary>
public sealed record RetryOptions
{
    /// <summary>Attempts after the first.</summary>
    public int? Max { get; init; }
    public TimeSpan? Base { get; init; }
    public TimeSpan? MaxDelay { get; init; }
    public Jitter? Jitter { get; init; }
    public bool? HonorRetryAfter { get; init; }
}

/// <summary>What middleware sees of one attempt. Secrets are redacted.</summary>
public sealed record RequestContext
{
    public string Operation { get; init; } = "";
    public int Attempt { get; init; }
    public string Method { get; set; } = "";
    public string Url { get; set; } = "";
    public IReadOnlyDictionary<string, string> Headers { get; init; } = new Dictionary<string, string>();
    /// <summary>The body as JSON when it is JSON, a text rendering otherwise.</summary>
    public JsonNode? Body { get; init; }
    public bool HasBody { get; init; }
}

/// <summary>What middleware sees of an answer.</summary>
public sealed record ResponseContext
{
    public int Status { get; init; }
    public IReadOnlyDictionary<string, string> Headers { get; init; } = new Dictionary<string, string>();
}

/// <summary>Observes the request lifecycle. Hooks are synchronous and must not block.</summary>
public interface IMiddleware
{
    void OnRequest(RequestContext context) { }

    void OnResponse(RequestContext context, ResponseContext response) { }

    void OnRetry(RequestContext context, Diagnostic reason) { }
}

/// <summary>Middleware from delegates; null hooks do nothing.</summary>
public sealed class Middleware : IMiddleware
{
    public Action<RequestContext>? Request { get; init; }
    public Action<RequestContext, ResponseContext>? Response { get; init; }
    public Action<RequestContext, Diagnostic>? Retry { get; init; }

    void IMiddleware.OnRequest(RequestContext context) => Request?.Invoke(context);

    void IMiddleware.OnResponse(RequestContext context, ResponseContext response) => Response?.Invoke(context, response);

    void IMiddleware.OnRetry(RequestContext context, Diagnostic reason) => Retry?.Invoke(context, reason);
}

/// <summary>Remembers the key used for one logical intent so that a retry reuses it.</summary>
public interface IIdempotencyStore
{
    string? Get(string scope, string logicalId);

    void Put(string scope, string logicalId, string key);
}

/// <summary>Configures a client.</summary>
public sealed record ClientOptions
{
    /// <summary>Restricts redirects to same_origin_read or none; none always wins and requires the default HTTP client.</summary>
    public string? Redirects { get; init; }

    /// <summary>Default timeout of one attempt.</summary>
    public static readonly TimeSpan DefaultTimeout = TimeSpan.FromSeconds(30);
    public const int DefaultMaxEventBytes = 1024 * 1024;
    public const int DefaultMaxCollectBytes = 4 * 1024 * 1024;
    public static readonly TimeSpan DefaultMaxCollectTime = TimeSpan.FromSeconds(60);
    public const int DefaultMaxReconnects = 3;
    public static readonly TimeSpan DefaultReconnectMax = TimeSpan.FromSeconds(30);

    /// <summary>Defaults to the first server.</summary>
    public string? BaseUrl { get; init; }
    public IReadOnlyDictionary<string, Credential>? Auth { get; init; }
    /// <summary>Holds the tokens of authorizationCode schemes. Default: in memory, per client.</summary>
    public ITokenStore? TokenStore { get; init; }
    /// <summary>The per-attempt timeout (default 30 s).</summary>
    public TimeSpan? Timeout { get; init; }
    /// <summary>Overrides the API's per-tier retry defaults, for every tier.</summary>
    public RetryOptions? Retries { get; init; }
    public IIdempotencyStore? IdempotencyStore { get; init; }
    public IReadOnlyList<IMiddleware>? Middleware { get; init; }
    /// <summary>Defaults to <see cref="Runtime.ValidateResponses.Warn"/>.</summary>
    public ValidateResponses? ValidateResponses { get; init; }
    public Action<Diagnostic>? OnDiagnostic { get; init; }
    /// <summary>Sent on every request.</summary>
    public IReadOnlyDictionary<string, string>? Headers { get; init; }
    /// <summary>The largest server-sent event a stream accepts, in UTF-8 bytes (default 1 MiB).</summary>
    public int? MaxEventBytes { get; init; }
    /// <summary>Bounds collecting a stream (default 4 MiB).</summary>
    public int? MaxCollectBytes { get; init; }
    /// <summary>Bounds collecting a stream (default 60 s).</summary>
    public TimeSpan? MaxCollectTime { get; init; }
    /// <summary>
    /// How often a stream reconnects after it dropped, with Last-Event-ID set to the last event id
    /// it delivered; only for calls that are safe to repeat. Null means 3; 0 disables it.
    /// </summary>
    public int? MaxReconnects { get; init; }
    /// <summary>Caps the wait before a reconnect (default 30 s).</summary>
    public TimeSpan? ReconnectMax { get; init; }
    /// <summary>
    /// A stream that delivers no bytes for this long is idle: it is reconnected like a dropped one,
    /// else ends with an error whose code is STREAM_IDLE. Null: the per-attempt timeout ends a
    /// silent stream, without a reconnect.
    /// </summary>
    public TimeSpan? IdleTimeout { get; init; }
    /// <summary>Signs confirmation tokens; default random per client.</summary>
    public byte[]? ConfirmationKey { get; init; }
    /// <summary>
    /// Used for every request; it must not follow redirects. Default: a client without redirects,
    /// cookies, compression or a timeout of its own.
    /// </summary>
    public System.Net.Http.HttpClient? HttpClient { get; init; }
    /// <summary>The clock in epoch milliseconds.</summary>
    public Func<long>? Now { get; init; }
    /// <summary>A number in [0, 1) for retry jitter.</summary>
    public Func<double>? Random { get; init; }
}

/// <summary>Configures one call.</summary>
public sealed record CallOptions
{
    /// <summary>Required for caller_owned operations with a required persisted key.</summary>
    public string? IdempotencyKey { get; init; }
    /// <summary>
    /// A token authorizes one intent: the first call that sends spends it, and it is accepted
    /// again only for a retry with the same idempotency key (or the identical body of a
    /// content_identity operation).
    /// </summary>
    public Confirm? Confirm { get; init; }
    /// <summary>
    /// <see cref="Runtime.Confirm.Yes"/> does not confirm a destructive operation either, and
    /// remediation never offers it (callers that only forward preview tokens).
    /// </summary>
    public bool RefuseConfirmYes { get; init; }
    /// <summary>The per-attempt timeout; null uses the client's.</summary>
    public TimeSpan? Timeout { get; init; }
    /// <summary>Runs the operation's verification hook after success.</summary>
    public bool Verify { get; init; }
    public IReadOnlyDictionary<string, string>? Headers { get; init; }

    /// <summary>Never shows keys or tokens.</summary>
    public override string ToString() =>
        $"CallOptions {{ IdempotencyKey = {(IdempotencyKey is null ? "null" : Util.Redacted)}, Confirm = {Confirm?.ToString() ?? "null"}, Timeout = {Timeout}, Verify = {Verify}, Headers = [{string.Join(" ", Headers?.Keys ?? System.Array.Empty<string>())}] }}";
}
