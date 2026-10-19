// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Linq;
using System.Security.Cryptography;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

/// <summary>What a token store keeps for one scheme. ToString shows no secret.</summary>
public sealed record StoredToken
{
    public string AccessToken { get; init; } = "";
    public string? RefreshToken { get; init; }
    /// <summary>Epoch milliseconds; null when the server gave no lifetime.</summary>
    public long? ExpiresAt { get; init; }
    public string? TokenType { get; init; }
    public string? Scope { get; init; }

    public override string ToString() => "StoredToken { " + Util.Redacted + " }";
}

/// <summary>Holds the tokens of the authorization-code schemes, by scheme name. Implement it to persist tokens.</summary>
public interface ITokenStore
{
    StoredToken? Get(string scheme);

    void Set(string scheme, StoredToken token);

    void Delete(string scheme);
}

/// <summary>The default token store: in memory, per client.</summary>
public sealed class MemoryTokenStore : ITokenStore
{
    private readonly ConcurrentDictionary<string, StoredToken> tokens = new(StringComparer.Ordinal);

    public StoredToken? Get(string scheme) => tokens.TryGetValue(scheme, out var t) ? t : null;

    public void Set(string scheme, StoredToken token) => tokens[scheme] = token;

    public void Delete(string scheme) => tokens.TryRemove(scheme, out _);
}

/// <summary>A PKCE pair (RFC 7636, S256). The verifier is a secret until the exchange.</summary>
public sealed record Pkce(string Verifier, string Challenge)
{
    /// <summary>Always S256.</summary>
    public const string Method = "S256";

    /// <summary>base64url(SHA-256(ASCII(verifier))).</summary>
    public static string ChallengeOf(string verifier) => Util.Base64Url(SHA256.HashData(Util.Utf8.GetBytes(verifier)));

    /// <summary>A 43 character verifier from 32 random bytes and its challenge.</summary>
    public static Pkce Generate()
    {
        var verifier = Util.Base64Url(Util.RandomBytes(32));
        return new Pkce(verifier, ChallengeOf(verifier));
    }

    /// <summary>Shows the challenge only.</summary>
    public override string ToString() => "Pkce { Challenge = " + Challenge + " }";
}

/// <summary>What ExchangeCode and Refresh report: no secrets.</summary>
public sealed record TokenInfo(string Scheme, long? ExpiresAt, string? Scope, bool Refreshable);

/// <summary>The parameters of <see cref="OAuthFlow.AuthorizationUrl"/>.</summary>
public sealed record AuthorizationUrlParams
{
    /// <summary>Defaults to the redirect_uri of the scheme's credential.</summary>
    public string? RedirectUri { get; init; }
    /// <summary>Default to the flow's; an empty list sends none.</summary>
    public IReadOnlyList<string>? Scopes { get; init; }
    public string? State { get; init; }
    public string? CodeChallenge { get; init; }
    /// <summary>Extra query parameters, added in key order; they never override the others.</summary>
    public IReadOnlyDictionary<string, string>? Extra { get; init; }
}

/// <summary>The parameters of <see cref="OAuthFlow.ExchangeCodeAsync"/>.</summary>
public sealed record ExchangeCodeParams
{
    public string Code { get; init; } = "";
    public string? RedirectUri { get; init; }
    public string? CodeVerifier { get; init; }
}

internal sealed record CodeClient(string ClientId, string ClientSecret, string RedirectUri);

internal sealed record TokenFailure(string Message, bool Rejected);

/// <summary>The OAuth2 helpers of one scheme (authorization code with PKCE), as generated clients expose them. Failures are AUTH_FAILED envelopes.</summary>
public sealed class OAuthFlow
{
    private readonly ClientCore core;

    internal OAuthFlow(ClientCore core, string scheme)
    {
        this.core = core;
        Scheme = scheme;
    }

    /// <summary>The scheme the helpers belong to.</summary>
    public string Scheme { get; }

    /// <summary>The tokens' store.</summary>
    public ITokenStore TokenStore => core.TokenStore;

    private string Operation(string helper) => "oauth." + Scheme + "." + helper;

    private static Result<T> Fail<T>(string operation, string remediation) =>
        new Result<T>.Err(new TungstenError(new Diag(operation, Category.AuthFailed).Remedy(remediation).Build()));

    /// <summary>A fresh pair: pass Challenge to the authorization URL and keep Verifier for the exchange.</summary>
    public Pkce Pkce() => Runtime.Pkce.Generate();

    /// <summary>The URL to send the user to.</summary>
    public Result<string> AuthorizationUrl(AuthorizationUrlParams p)
    {
        var operation = Operation("authorizationUrl");
        var (flow, reason) = core.CodeFlow(Scheme);
        if (flow is null)
        {
            return Fail<string>(operation, "Cannot build an authorization URL: " + reason + ".");
        }
        var (client, why) = core.CodeClientOf(Scheme);
        if (client is null)
        {
            return Fail<string>(operation, "Cannot build an authorization URL: " + why + ".");
        }
        var redirect = string.IsNullOrEmpty(p.RedirectUri) ? client.RedirectUri : p.RedirectUri;
        if (string.IsNullOrEmpty(redirect))
        {
            return Fail<string>(operation, $"Pass redirect_uri, or set redirect_uri in auth.{Scheme}; the authorization URL needs one.");
        }
        return new Result<string>.Ok(BuildAuthorizationUrl(flow, client.ClientId, redirect, p)) { Meta = ResponseMeta.Local() };
    }

    internal static string BuildAuthorizationUrl(AuthorizationCodeFlow flow, string clientId, string redirect, AuthorizationUrlParams p)
    {
        var scope = string.Join(" ", p.Scopes ?? flow.Scopes);
        var fields = new List<(string, string)> { ("response_type", "code"), ("client_id", clientId), ("redirect_uri", redirect) };
        if (scope.Length > 0)
        {
            fields.Add(("scope", scope));
        }
        if (!string.IsNullOrEmpty(p.State))
        {
            fields.Add(("state", p.State));
        }
        if (!string.IsNullOrEmpty(p.CodeChallenge))
        {
            fields.Add(("code_challenge", p.CodeChallenge));
            fields.Add(("code_challenge_method", Runtime.Pkce.Method));
        }
        var reserved = fields.Select(f => f.Item1).ToHashSet(StringComparer.Ordinal);
        foreach (var kv in (p.Extra ?? new Dictionary<string, string>()).OrderBy(kv => kv.Key, StringComparer.Ordinal))
        {
            if (!reserved.Contains(kv.Key))
            {
                fields.Add((kv.Key, kv.Value));
            }
        }
        var baseUrl = flow.AuthorizationUrl ?? "";
        var sep = "?";
        if (baseUrl.Contains('?', StringComparison.Ordinal))
        {
            sep = baseUrl.EndsWith('?') || baseUrl.EndsWith('&') ? "" : "&";
        }
        return baseUrl + sep + ClientCore.FormBody(fields);
    }

    /// <summary>Trades the code from the redirect for tokens, stored for later calls.</summary>
    public async Task<Result<TokenInfo>> ExchangeCodeAsync(ExchangeCodeParams p, CancellationToken cancellationToken = default)
    {
        var operation = Operation("exchangeCode");
        var (flow, reason) = core.CodeFlow(Scheme);
        if (flow is null)
        {
            return Fail<TokenInfo>(operation, "Cannot exchange the code: " + reason + ".");
        }
        var (client, why) = core.CodeClientOf(Scheme);
        if (client is null)
        {
            return Fail<TokenInfo>(operation, "Cannot exchange the code: " + why + ".");
        }
        if (string.IsNullOrEmpty(p.Code))
        {
            return Fail<TokenInfo>(operation, "Pass the authorization code the server redirected back with as code.");
        }
        var redirect = string.IsNullOrEmpty(p.RedirectUri) ? client.RedirectUri : p.RedirectUri;
        if (string.IsNullOrEmpty(redirect))
        {
            return Fail<TokenInfo>(operation, $"Pass redirect_uri, or set redirect_uri in auth.{Scheme}; it must equal the one used for the authorization URL.");
        }
        var fields = new List<(string, string)> { ("grant_type", "authorization_code"), ("code", p.Code), ("redirect_uri", redirect) };
        if (!string.IsNullOrEmpty(p.CodeVerifier))
        {
            fields.Add(("code_verifier", p.CodeVerifier));
        }
        var (stored, failure) = await core.TokenRequestAsync(Scheme, flow.TokenUrl ?? "", client, fields, null, cancellationToken).ConfigureAwait(false);
        if (failure is not null || stored is null)
        {
            return Fail<TokenInfo>(operation, failure?.Message ?? "The OAuth2 token endpoint returned no access_token.");
        }
        core.TokenStore.Set(Scheme, stored);
        return new Result<TokenInfo>.Ok(Info(stored)) { Meta = ResponseMeta.Local() };
    }

    /// <summary>Refreshes the stored token now. Calls refresh on their own when it expires.</summary>
    public async Task<Result<TokenInfo>> RefreshAsync(CancellationToken cancellationToken = default)
    {
        var operation = Operation("refresh");
        var (_, failure) = await core.StoredTokenInnerAsync(Scheme, true, cancellationToken).ConfigureAwait(false);
        if (failure is not null)
        {
            return Fail<TokenInfo>(operation, failure.Message);
        }
        var stored = core.ReadToken(Scheme);
        if (stored is null)
        {
            return Fail<TokenInfo>(operation, "No token is stored for " + Scheme + ".");
        }
        return new Result<TokenInfo>.Ok(Info(stored)) { Meta = ResponseMeta.Local() };
    }

    private TokenInfo Info(StoredToken t) => new(Scheme, t.ExpiresAt, t.Scope, !string.IsNullOrEmpty(t.RefreshToken));
}

public sealed partial class ClientCore
{
    /// <summary>Tokens expiring within this many milliseconds count as expired.</summary>
    public const long ExpirySkewMs = 30000;

    /// <summary>Serializes the refreshes of one scheme; Generation counts completed ones.</summary>
    private sealed class RefreshGate
    {
        public readonly SemaphoreSlim Lock = new(1, 1);
        public long Generation;
    }

    private readonly ConcurrentDictionary<string, RefreshGate> oauthGates = new(StringComparer.Ordinal);
    private readonly SemaphoreSlim clientCredentialsLock = new(1, 1);
    private readonly Dictionary<string, (string Token, long ExpiresAt)> clientCredentialsTokens = new(StringComparer.Ordinal);

    /// <summary>The authorization-code helpers of a scheme.</summary>
    public OAuthFlow OAuth(string scheme) => new(this, scheme);

    internal static string FormBody(IEnumerable<(string K, string V)> fields) =>
        string.Join("&", fields.Select(f => Serialize.PercentEncode(f.K) + "=" + Serialize.PercentEncode(f.V)));

    private static bool IsErrorCode(string code) =>
        code.Length >= 1 && code.Length <= 40 && code.All(c => c == '_' || (c >= 'a' && c <= 'z'));

    internal (AuthorizationCodeFlow? Flow, string Reason) CodeFlow(string scheme)
    {
        foreach (var s in api.Auth)
        {
            if (s.Name != scheme)
            {
                continue;
            }
            if (s.Kind == "oauth2" && s.AuthorizationCode is not null)
            {
                return (s.AuthorizationCode, "");
            }
            return (null, "the scheme " + scheme + " has no authorizationCode flow");
        }
        return (null, "the API descriptor defines no scheme named " + scheme);
    }

    internal (CodeClient? Client, string Reason) CodeClientOf(string scheme)
    {
        var invalid = $"auth.{scheme} must be Credential.Parts with flow \"authorizationCode\", client_id, and optionally client_secret and redirect_uri";
        if (!auth.TryGetValue(scheme, out var cred) || !cred.IsParts)
        {
            return (null, invalid);
        }
        string Value(string snake, string camel) => CredPart(cred.PartValues!, snake, camel, out _) ?? "";
        if (Value("flow", "flow") != "authorizationCode")
        {
            return (null, invalid);
        }
        var id = Value("client_id", "clientId");
        if (id.Length == 0)
        {
            return (null, $"auth.{scheme} needs a client_id");
        }
        return (new CodeClient(id, Value("client_secret", "clientSecret"), Value("redirect_uri", "redirectUri")), "");
    }

    internal StoredToken? ReadToken(string scheme)
    {
        var t = tokenStore.Get(scheme);
        return t is null || t.AccessToken.Length == 0 ? null : t;
    }

    /// <summary>The access token to send: the stored one, refreshed first when it expires within ExpirySkewMs (or force).</summary>
    internal async Task<(string? Token, string? Problem)> StoredTokenAsync(string scheme, bool force, CancellationToken ct)
    {
        var (token, failure) = await StoredTokenInnerAsync(scheme, force, ct).ConfigureAwait(false);
        return failure is not null ? (null, failure.Message) : (token, null);
    }

    internal async Task<(string? Token, TokenFailure? Failure)> StoredTokenInnerAsync(string scheme, bool force, CancellationToken ct)
    {
        var missing = new TokenFailure($"No OAuth2 token is stored for {scheme}; send the user to OAuthFlow.AuthorizationUrl, then call ExchangeCodeAsync with the code, or put a token in ClientOptions.TokenStore.", false);
        var stored = ReadToken(scheme);
        if (stored is null)
        {
            return (null, missing);
        }
        bool fresh = stored.ExpiresAt is null || stored.ExpiresAt > Now() + ExpirySkewMs;
        if (fresh && !force)
        {
            return (stored.AccessToken, null);
        }
        var gate = oauthGates.GetOrAdd(scheme, _ => new RefreshGate());
        long seen = Interlocked.Read(ref gate.Generation);
        await gate.Lock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (Interlocked.Read(ref gate.Generation) != seen)
            {
                var t = ReadToken(scheme);
                return t is null ? (null, missing) : (t.AccessToken, null);
            }
            var current = ReadToken(scheme);
            if (current is null)
            {
                return (null, missing);
            }
            var (token, failure) = await RefreshStoredAsync(scheme, current, force, ct).ConfigureAwait(false);
            if (failure is not null)
            {
                return (null, failure);
            }
            Interlocked.Increment(ref gate.Generation);
            return (token, null);
        }
        finally
        {
            gate.Lock.Release();
        }
    }

    /// <summary>Forces a refresh after a 401 when the stored token has a refresh token.</summary>
    internal async Task<string?> RefreshAfterRejectionAsync(string scheme, string rejected, CancellationToken ct)
    {
        var stored = ReadToken(scheme);
        if (stored is null)
        {
            return null;
        }
        if (stored.AccessToken != rejected)
        {
            return stored.AccessToken;
        }
        if (string.IsNullOrEmpty(stored.RefreshToken))
        {
            return null;
        }
        var (token, problem) = await StoredTokenAsync(scheme, true, ct).ConfigureAwait(false);
        return problem is null ? token : null;
    }

    private async Task<(string? Token, TokenFailure? Failure)> RefreshStoredAsync(string scheme, StoredToken stored, bool force, CancellationToken ct)
    {
        var (flow, reason) = CodeFlow(scheme);
        if (flow is null)
        {
            return (null, new TokenFailure($"Cannot refresh the OAuth2 token for {scheme}: {reason}.", false));
        }
        var (client, why) = CodeClientOf(scheme);
        if (client is null)
        {
            return (null, new TokenFailure($"Cannot refresh the OAuth2 token for {scheme}: {why}.", false));
        }
        if (string.IsNullOrEmpty(stored.RefreshToken))
        {
            var what = "has no refresh token";
            if (!force)
            {
                what = "expired and " + what;
            }
            return (null, new TokenFailure($"The OAuth2 token for {scheme} {what}; authorize again with OAuthFlow.AuthorizationUrl and ExchangeCodeAsync.", false));
        }
        var url = string.IsNullOrEmpty(flow.RefreshUrl) ? flow.TokenUrl ?? "" : flow.RefreshUrl;
        var fields = new List<(string, string)> { ("grant_type", "refresh_token"), ("refresh_token", stored.RefreshToken) };
        var (renewed, failure) = await TokenRequestAsync(scheme, url, client, fields, stored.RefreshToken, ct).ConfigureAwait(false);
        if (failure is not null || renewed is null)
        {
            if (failure?.Rejected == true)
            {
                tokenStore.Delete(scheme);
            }
            return (null, failure);
        }
        tokenStore.Set(scheme, renewed);
        return (renewed.AccessToken, null);
    }

    private TimeSpan TokenAttemptTimeout() => timeout < TimeSpan.FromMilliseconds(1) ? TimeSpan.FromMilliseconds(1) : timeout;

    internal async Task<(StoredToken? Token, TokenFailure? Failure)> TokenRequestAsync(string scheme, string url, CodeClient client, List<(string, string)> fields, string? previous, CancellationToken ct)
    {
        if (InsecureHttp(url)) return (null, new TokenFailure(InsecureHttpAuth, false));
        var headers = new List<HeaderEntry>
        {
            new("Content-Type", "application/x-www-form-urlencoded", false),
            new("Accept", "application/json", false),
        };
        var all = new List<(string, string)>(fields);
        if (client.ClientSecret.Length > 0)
        {
            var basic = Convert.ToBase64String(Util.Utf8.GetBytes(Serialize.PercentEncode(client.ClientId) + ":" + Serialize.PercentEncode(client.ClientSecret)));
            headers.Add(new HeaderEntry("Authorization", "Basic " + basic, true));
        }
        else
        {
            all.Add(("client_id", client.ClientId));
        }
        var outcome = await Transport.AttemptAsync(http, new AttemptRequest
        {
            Url = url,
            Method = "POST",
            Headers = headers,
            Body = new Payload { Bytes = Util.Utf8.GetBytes(FormBody(all)) },
            Timeout = TokenAttemptTimeout(),
        }, ct).ConfigureAwait(false);
        if (outcome.Kind != OutcomeKind.Response || !outcome.BodyOk)
        {
            return (null, new TokenFailure($"The OAuth2 token endpoint for {scheme} could not be reached; check network access and the token URL.", false));
        }
        var decoded = DecodedBody.Decode(outcome.Body ?? System.Array.Empty<byte>(), outcome.Headers, "application/json");
        if (outcome.Status < 200 || outcome.Status > 299)
        {
            var named = "";
            if (Util.GetPathStr(decoded.Value, decoded.Has, "error", out var e) && Json.TryString(e, out var code) && IsErrorCode(code))
            {
                named = " (" + code + ")";
            }
            return (null, new TokenFailure(
                $"The OAuth2 token endpoint for {scheme} answered HTTP {outcome.Status}{named}; check the code or refresh token, client credentials and redirect URI.",
                outcome.Status >= 400 && outcome.Status < 500));
        }
        string StringAt(string path) => Util.GetPathStr(decoded.Value, decoded.Has, path, out var v) && Json.TryString(v, out var s) ? s : "";
        var access = StringAt("access_token");
        if (access.Length == 0)
        {
            return (null, new TokenFailure($"The OAuth2 token endpoint for {scheme} returned no access_token.", false));
        }
        var refresh = StringAt("refresh_token");
        if (refresh.Length == 0)
        {
            refresh = previous ?? "";
        }
        long? expires = null;
        if (Util.GetPathStr(decoded.Value, decoded.Has, "expires_in", out var ein) && Json.TryNumber(ein, out var n) && double.IsFinite(n))
        {
            expires = Now() + (long)(Math.Max(n, 0) * 1000);
        }
        var tokenType = StringAt("token_type");
        var scope = StringAt("scope");
        return (new StoredToken
        {
            AccessToken = access,
            RefreshToken = refresh.Length > 0 ? refresh : null,
            ExpiresAt = expires,
            TokenType = tokenType.Length > 0 ? tokenType : null,
            Scope = scope.Length > 0 ? scope : null,
        }, null);
    }

    /// <summary>Fetches (and caches) an OAuth2 client-credentials token; one request at a time per client.</summary>
    private async Task<(string? Token, string? Problem)> ClientCredentialsTokenAsync(Satisfier s, CancellationToken ct)
    {
        if (InsecureHttp(s.TokenUrl)) return (null, InsecureHttpAuth);
        await clientCredentialsLock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            long realNow = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
            if (clientCredentialsTokens.TryGetValue(s.Name, out var cached) && cached.ExpiresAt > realNow + 30000)
            {
                return (cached.Token, null);
            }
            var form = new List<string> { "grant_type=client_credentials" };
            if (s.Scopes.Count > 0)
            {
                form.Add("scope=" + Serialize.FormComponent(string.Join(" ", s.Scopes)));
            }
            var basic = Convert.ToBase64String(Util.Utf8.GetBytes(Serialize.Component(s.ClientId) + ":" + Serialize.Component(s.ClientSecret)));
            var outcome = await Transport.AttemptAsync(http, new AttemptRequest
            {
                Url = s.TokenUrl,
                Method = "POST",
                Headers = new List<HeaderEntry>
                {
                    new("Content-Type", "application/x-www-form-urlencoded", false),
                    new("Accept", "application/json", false),
                    new("Authorization", "Basic " + basic, true),
                },
                Body = new Payload { Bytes = Util.Utf8.GetBytes(string.Join("&", form)) },
                Timeout = TokenAttemptTimeout(),
            }, ct).ConfigureAwait(false);
            if (outcome.Kind != OutcomeKind.Response || !outcome.BodyOk)
            {
                return (null, $"The OAuth2 token endpoint for {s.Name} could not be reached; check network access and the token URL.");
            }
            if (outcome.Status < 200 || outcome.Status > 299)
            {
                return (null, $"The OAuth2 token endpoint for {s.Name} answered HTTP {outcome.Status}; check client_id, client_secret and scopes.");
            }
            var decoded = DecodedBody.Decode(outcome.Body ?? System.Array.Empty<byte>(), outcome.Headers, "application/json");
            if (!Util.GetPathStr(decoded.Value, decoded.Has, "access_token", out var v) || !Json.TryString(v, out var token) || token.Length == 0)
            {
                return (null, $"The OAuth2 token endpoint for {s.Name} returned no access_token.");
            }
            long lifetime = 3600000;
            if (Util.GetPathStr(decoded.Value, decoded.Has, "expires_in", out var e) && Json.TryNumber(e, out var n) && double.IsFinite(n))
            {
                lifetime = (long)Math.Max(n * 1000, 0);
            }
            clientCredentialsTokens[s.Name] = (token, realNow + lifetime);
            return (token, null);
        }
        finally
        {
            clientCredentialsLock.Release();
        }
    }
}
