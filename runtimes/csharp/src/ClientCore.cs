// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Runtime.CompilerServices;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

/// <summary>A client that could not be built.</summary>
public sealed class ClientConfigException : Exception
{
    public ClientConfigException(string message) : base(message) { }
}

/// <summary>
/// The engine behind every generated client: pre-flight validation, auth, idempotency keys,
/// confirmation rules, sending with tier-aware retries and classification into a result or the
/// error envelope. It is safe for concurrent use. Every call takes a
/// <see cref="CancellationToken"/>; cancelling it ends the call (a mutation in flight is
/// reported as OUTCOME_UNKNOWN).
/// </summary>
public sealed partial class ClientCore
{
    /// <summary>The version of this runtime, sent as <c>X-Tungsten-Runtime: tungsten-cs/&lt;version&gt;</c>.</summary>
    public const string Version = "0.1.3";

    private readonly ApiDescriptor api;
    private readonly IReadOnlyDictionary<string, Credential> auth;
    private readonly IReadOnlyDictionary<string, string> headers;
    private readonly string baseUrl;
    private readonly TimeSpan timeout;
    private readonly int maxEventBytes;
    private readonly int maxCollectBytes;
    private readonly TimeSpan maxCollectTime;
    private readonly int maxReconnects;
    private readonly TimeSpan reconnectMax;
    private readonly TimeSpan? idleTimeout;
    private readonly RetryOptions retries;
    private readonly IIdempotencyStore store;
    private readonly IReadOnlyList<IMiddleware> middleware;
    private readonly ValidateResponses validateResponses;
    private readonly Action<Diagnostic>? onDiagnostic;
    private readonly byte[] confirmationKey;
    private readonly HttpClient http;
    private readonly bool redirectsDisabled;

    /// <summary>Generated clients require this capability when redirects are forbidden.</summary>
    public static void RequireRedirectPolicyV1() { }

    private static bool RedirectsNone(string? value)
    {
        if (value is null or "same_origin_read") return false;
        if (value == "none") return true;
        throw new ClientConfigException("redirects must be same_origin_read or none");
    }
    private readonly Func<long>? nowFn;
    private readonly Func<double>? randomFn;
    private readonly ITokenStore tokenStore;
    private readonly RuntimeIdentity identity = RuntimeIdentity.CSharp;

    private readonly ConcurrentDictionary<string, OperationDescriptor> registry = new(StringComparer.Ordinal);
    private readonly object usedTokensLock = new();
    private readonly Dictionary<string, (string? Bind, long Expiry)> usedTokens = new(StringComparer.Ordinal);
    /// <summary>Makes "find or create the key of a logical call" atomic.</summary>
    private readonly SemaphoreSlim keyLock = new(1, 1);

    private static readonly Lazy<HttpClient> SharedHttp = new(Transport.NewHttpClient);

    private static bool ValidBaseUrl(string url) =>
        Uri.TryCreate(url, UriKind.Absolute, out var u) && (u.Scheme == "http" || u.Scheme == "https") &&
        u.Host.Length > 0 && string.IsNullOrEmpty(u.UserInfo);

    /// <summary>Builds a client; throws <see cref="ClientConfigException"/> when the options are invalid.</summary>
    public ClientCore(ApiDescriptor api, ClientOptions? options = null, IEnumerable<OperationDescriptor>? operations = null)
    {
        options ??= new ClientOptions();
        this.api = api ?? throw new ClientConfigException("the API descriptor is missing");
        var apiNone = RedirectsNone(api.Redirects);
        var clientNone = RedirectsNone(options.Redirects);
        redirectsDisabled = apiNone || clientNone;
        if (redirectsDisabled && options.HttpClient is not null)
        {
            throw new ClientConfigException("redirects none requires the default HTTP client; a supplied HttpClient may follow redirects internally");
        }
        var b = options.BaseUrl;
        if (string.IsNullOrEmpty(b) && api.Servers.Count > 0)
        {
            b = api.Servers[0];
        }
        if (!string.IsNullOrEmpty(b) && !ValidBaseUrl(b))
        {
            throw new ClientConfigException("the base URL is not a valid absolute http(s) URL without credentials");
        }
        baseUrl = b ?? "";
        auth = options.Auth ?? new Dictionary<string, Credential>();
        headers = options.Headers ?? new Dictionary<string, string>();
        timeout = options.Timeout is TimeSpan t && t > TimeSpan.Zero ? t : ClientOptions.DefaultTimeout;
        maxEventBytes = options.MaxEventBytes is int m && m > 0 ? m : ClientOptions.DefaultMaxEventBytes;
        maxCollectBytes = options.MaxCollectBytes is int c && c > 0 ? c : ClientOptions.DefaultMaxCollectBytes;
        maxCollectTime = options.MaxCollectTime is TimeSpan ct && ct > TimeSpan.Zero ? ct : ClientOptions.DefaultMaxCollectTime;
        maxReconnects = options.MaxReconnects is int r ? Math.Min(Math.Max(r, 0), 1000) : ClientOptions.DefaultMaxReconnects;
        reconnectMax = options.ReconnectMax is TimeSpan rm && rm > TimeSpan.Zero ? rm : ClientOptions.DefaultReconnectMax;
        if (options.IdleTimeout is TimeSpan idle && idle > TimeSpan.Zero)
        {
            idleTimeout = idle < TimeSpan.FromMilliseconds(1) ? TimeSpan.FromMilliseconds(1) : idle;
        }
        retries = options.Retries ?? new RetryOptions();
        store = options.IdempotencyStore ?? new MemoryIdempotencyStore();
        middleware = options.Middleware ?? System.Array.Empty<IMiddleware>();
        validateResponses = options.ValidateResponses ?? ValidateResponses.Warn;
        onDiagnostic = options.OnDiagnostic;
        confirmationKey = options.ConfirmationKey is { Length: > 0 } key ? key : Util.RandomBytes(32);
        http = options.HttpClient ?? SharedHttp.Value;
        nowFn = options.Now;
        randomFn = options.Random;
        tokenStore = options.TokenStore ?? new MemoryTokenStore();
        if (operations is not null)
        {
            Register(operations);
        }
    }

    /// <summary>A client of the operations and the API of a descriptor document.</summary>
    public ClientCore(SdkDescriptors descriptors, ClientOptions? options = null)
        : this(descriptors.Api, options, descriptors.Operations.Values)
    {
    }

    /// <summary>
    /// A client of a descriptor document for an SDK in another .NET language built on this runtime
    /// (F#), which identifies itself with <paramref name="identity"/> on the wire.
    /// </summary>
    public ClientCore(SdkDescriptors descriptors, ClientOptions? options, RuntimeIdentity identity)
        : this(descriptors.Api, options, descriptors.Operations.Values)
    {
        this.identity = identity ?? throw new ClientConfigException("the runtime identity is missing");
    }

    /// <summary>The API descriptor.</summary>
    public ApiDescriptor Api => api;

    /// <summary>Who this client says it is in <c>User-Agent</c> and <c>X-Tungsten-Runtime</c>.</summary>
    public RuntimeIdentity Identity => identity;

    internal ITokenStore TokenStore => tokenStore;

    /// <summary>Makes operations resolvable by id (verification, endpoint previews, macros).</summary>
    public void Register(IEnumerable<OperationDescriptor> operations)
    {
        foreach (var op in operations)
        {
            if (op is not null && op.Id.Length > 0)
            {
                registry.TryAdd(op.Id, op);
            }
        }
    }

    /// <summary>A registered operation.</summary>
    public OperationDescriptor? Operation(string id) => registry.TryGetValue(id, out var op) ? op : null;

    // ------------------------------------------------------------ public API

    private static Result<T> Catch<T>(Failure f) => new Result<T>.Err(f.Error);

    /// <summary>
    /// Validates, authenticates, applies idempotency and confirmation rules, sends with retries and
    /// classifies the response. <paramref name="args"/> is the arguments object, keyed by arguments
    /// key (null is no arguments); the value is the decoded wire body.
    /// </summary>
    public async Task<Result<JsonNode?>> CallAsync(OperationDescriptor op, JsonNode? args, CallOptions? options = null, CancellationToken cancellationToken = default)
    {
        try
        {
            var outcome = await CallWithAsync(op, args, options ?? new CallOptions(), null, null, cancellationToken).ConfigureAwait(false);
            return outcome;
        }
        catch (Failure f)
        {
            return Catch<JsonNode?>(f);
        }
    }

    /// <summary>
    /// Renders the request without sending it (or runs the operation's header or endpoint
    /// preview). Mutating operations get a confirmation token bound to these exact arguments for
    /// five minutes.
    /// </summary>
    public async Task<Result<PreviewResult>> PreviewAsync(OperationDescriptor op, JsonNode? args, CallOptions? options = null, CancellationToken cancellationToken = default)
    {
        try
        {
            return await PreviewInnerAsync(op, args, options ?? new CallOptions(), cancellationToken).ConfigureAwait(false);
        }
        catch (Failure f)
        {
            return Catch<PreviewResult>(f);
        }
    }

    /// <summary>The pages of a paginated operation; the first failure ends the sequence.</summary>
    public async IAsyncEnumerable<Result<Page<JsonNode?>>> PagesAsync(OperationDescriptor op, JsonNode? args, CallOptions? options = null, [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        var state = new PageState(args);
        var opts = options ?? new CallOptions();
        while (true)
        {
            Result<Page<JsonNode?>>? item;
            try
            {
                item = await PageStepAsync(op, state, opts, null, cancellationToken).ConfigureAwait(false);
            }
            catch (Failure f)
            {
                state.Done = true;
                item = Catch<Page<JsonNode?>>(f);
            }
            if (item is null)
            {
                yield break;
            }
            yield return item;
            if (item is Result<Page<JsonNode?>>.Err)
            {
                yield break;
            }
        }
    }

    /// <summary>Calls a read operation until <paramref name="until"/> holds or the budget runs out; on a timeout the last body is returned with TimedOut.</summary>
    public async Task<Result<Polled>> PollAsync(OperationDescriptor op, JsonNode? args, JsonNode? until, TimeSpan interval, TimeSpan budget, CallOptions? options = null, CancellationToken cancellationToken = default)
    {
        try
        {
            return await PollWithAsync(op, args, until, interval, budget, options ?? new CallOptions(), null, cancellationToken).ConfigureAwait(false);
        }
        catch (Failure f)
        {
            return Catch<Polled>(f);
        }
    }

    /// <summary>
    /// Runs a compiled macro: its output, or the failing step's envelope with the completed steps'
    /// results as the partial. A destructive or irreversible macro needs <see cref="CallOptions.Confirm"/>.
    /// </summary>
    public async Task<Result<JsonNode?>> RunMacroAsync(MacroDescriptor macro, JsonNode? input, CallOptions? options = null, CancellationToken cancellationToken = default)
    {
        try
        {
            return await RunMacroInnerAsync(macro, input, options ?? new CallOptions(), cancellationToken).ConfigureAwait(false);
        }
        catch (Failure f)
        {
            return Catch<JsonNode?>(f);
        }
    }

    /// <summary>Previews a macro without sending anything.</summary>
    public async Task<Result<PreviewResult>> PreviewMacroAsync(MacroDescriptor macro, JsonNode? input, CallOptions? options = null, CancellationToken cancellationToken = default)
    {
        try
        {
            return await PreviewMacroInnerAsync(macro, input, options ?? new CallOptions(), cancellationToken).ConfigureAwait(false);
        }
        catch (Failure f)
        {
            return Catch<PreviewResult>(f);
        }
    }

    /// <summary>The server-sent events of an operation; nothing is sent until the stream is read.</summary>
    public EventStream<JsonNode?> Stream(OperationDescriptor op, JsonNode? args, CallOptions? options = null) =>
        StreamTyped(op, args, v => v, options);

    // ------------------------------------------------------------ pipeline

    /// <summary>The pipeline shared by calls, pages, polls and macro steps.</summary>
    internal Task<Result<JsonNode?>.Ok> CallWithAsync(OperationDescriptor op, JsonNode? args, CallOptions opts, string? urlOverride, MacroClaim? claim, CancellationToken ct) =>
        CallWithAsync(op, args, opts, urlOverride, claim, null, ct);

    /// <summary>As the shared pipeline; with a trace, a request URL used before is refused unsent and the final URL is recorded.</summary>
    internal async Task<Result<JsonNode?>.Ok> CallWithAsync(OperationDescriptor op, JsonNode? args, CallOptions opts, string? urlOverride, MacroClaim? claim, PageTrace? trace, CancellationToken ct)
    {
        var p = await PrepareAsync(op, args, opts, Purpose.Call, urlOverride, claim, ct).ConfigureAwait(false);
        if (trace is not null && !trace.Urls.Add(p.Url))
        {
            throw new Failure(new Diag(op.Id, Category.UnexpectedResponse)
                .Remedy("Pagination detected a repeated effective page request URL after authentication. Fix the server's next-page Link before continuing; no repeated request was sent.")
                .Build());
        }
        Result<JsonNode?>.Ok response;
        try
        {
            response = await SendAsync(p, opts, ct).ConfigureAwait(false);
        }
        catch (Failure f) when (f.Error.Diagnostic.HttpStatus == 401 && p.OAuth is not null)
        {
            response = await ResendRejectedAsync(p, f, opts, ct).ConfigureAwait(false);
        }
        if (trace is not null)
        {
            trace.LastUrl = p.FinalUrl;
            if (p.FinalUrl is not null)
            {
                trace.Urls.Add(p.FinalUrl);
            }
        }
        if (opts.Verify && op.Agent.Verify is not null)
        {
            var v = await VerifyAsync(op, p.Args, response.Value, response.HasValue, opts, ct).ConfigureAwait(false);
            response = response with { Verification = v };
        }
        return response;
    }

    /// <summary>Sends a request whose stored OAuth2 token was rejected (401) once more with the refreshed token.</summary>
    private async Task<Result<JsonNode?>.Ok> ResendRejectedAsync(Prepared p, Failure rejected, CallOptions opts, CancellationToken ct)
    {
        var scheme = p.OAuth!;
        p.OAuth = null;
        var current = p.Headers.Get("Authorization");
        if (current is null || !current.StartsWith("Bearer ", StringComparison.Ordinal))
        {
            throw rejected;
        }
        var token = await RefreshAfterRejectionAsync(scheme, current["Bearer ".Length..], ct).ConfigureAwait(false);
        if (token is null)
        {
            throw rejected;
        }
        p.Headers.Set("Authorization", "Bearer " + token, true);
        p.Secrets.Insert(token);
        return await SendAsync(p, opts, ct).ConfigureAwait(false);
    }

    internal long Now() => nowFn?.Invoke() ?? DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

    internal double Random()
    {
        double v;
        if (randomFn is not null)
        {
            v = randomFn();
        }
        else
        {
            var bytes = Util.RandomBytes(8);
            v = (BitConverter.ToUInt64(bytes, 0) >> 11) / (double)(1UL << 53);
        }
        if (double.IsNaN(v) || double.IsInfinity(v))
        {
            return 0.5;
        }
        return Math.Max(0, Math.Min(1, v));
    }

    internal sealed record RetryPlan(int Max, double BaseMs, double MaxMs, Jitter Jitter, bool HonorRetryAfter);

    /// <summary>The runtime defaults, then the API's defaults for the operation's tier, then ClientOptions.Retries, field by field.</summary>
    internal RetryPlan RetryOptionsFor(OperationDescriptor op)
    {
        var plan = new RetryPlan(3, 200, 5000, Jitter.Full, true);
        var layers = new List<RetryOptions>();
        if (api.Retries is { } tiers)
        {
            layers.Add(Classify.IsMutation(op) ? tiers.Mutating : tiers.ReadOnly);
        }
        layers.Add(retries);
        foreach (var l in layers)
        {
            if (l.Max is int max)
            {
                plan = plan with { Max = Math.Max(0, Math.Min(100, max)) };
            }
            if (l.Base is TimeSpan b)
            {
                plan = plan with { BaseMs = b.TotalMilliseconds };
            }
            if (l.MaxDelay is TimeSpan md)
            {
                plan = plan with { MaxMs = md.TotalMilliseconds };
            }
            if (l.Jitter is Jitter j)
            {
                plan = plan with { Jitter = j };
            }
            if (l.HonorRetryAfter is bool h)
            {
                plan = plan with { HonorRetryAfter = h };
            }
        }
        return plan;
    }

    /// <summary>The per-attempt timeout (at least one millisecond).</summary>
    internal TimeSpan AttemptTimeout(CallOptions opts)
    {
        var t = opts.Timeout is TimeSpan x && x > TimeSpan.Zero ? x : timeout;
        return t < TimeSpan.FromMilliseconds(1) ? TimeSpan.FromMilliseconds(1) : t;
    }

    /// <summary>Reports a warning; a failing observer never changes the call.</summary>
    internal void Emit(Diagnostic d)
    {
        if (onDiagnostic is null)
        {
            return;
        }
        try
        {
            onDiagnostic(d);
        }
        catch (Exception)
        {
            // Observers never change the call.
        }
    }

    private void Hooks(Action<IMiddleware> hook)
    {
        foreach (var m in middleware)
        {
            try
            {
                hook(m);
            }
            catch (Exception)
            {
                // Observers never change the call.
            }
        }
    }
}
