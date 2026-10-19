// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Text;
using System.Threading;

namespace Tungsten.Runtime;

/// <summary>An explicit provider-qualified issuer defense. Missing iss never selects a fallback.</summary>
public sealed class IssuerDefense
{
    private IssuerDefense(bool response, string value) { Response = response; Value = value; }
    internal bool Response { get; }
    internal string Value { get; }
    /// <summary>Require the provider's exact decoded authorization-response issuer, including on denial.</summary>
    public static IssuerDefense IssuerResponse(string issuer) => new(true, issuer);
    /// <summary>The application qualifies a redirect that isolates this issuer from other issuers.</summary>
    public static IssuerDefense DistinctRedirect(string qualification) => new(false, qualification);
    public override string ToString() => "IssuerDefense { .. }";
}

/// <summary>Managed begin inputs. Qualify S256/issuer defense and bind the owning session/listener.</summary>
public sealed record AuthorizationUrlParams
{
    public string SessionBinding { get; init; } = "";
    public bool S256Supported { get; init; }
    public IssuerDefense? IssuerDefense { get; init; }
    public string? RedirectUri { get; init; }
    public IReadOnlyList<string>? Scopes { get; init; }
    public string? Resource { get; init; }
    public string? Audience { get; init; }
    public string? AccountContext { get; init; }
    public IReadOnlyDictionary<string, string>? Extra { get; init; }
    public override string ToString() => "AuthorizationUrlParams { .. }";
}

/// <summary>A handle and URL for the owning listener; neither is included in ToString.</summary>
public sealed record AuthorizationAttempt(string AttemptHandle, string Url, long ExpiresInMs)
{
    public override string ToString() => $"AuthorizationAttempt {{ ExpiresInMs = {ExpiresInMs} }}";
}

/// <summary>Complete one managed attempt using the entire callback URL and its owning binding.</summary>
public sealed record ExchangeCodeParams
{
    public string AttemptHandle { get; init; } = "";
    public string CallbackUrl { get; init; } = "";
    public string SessionBinding { get; init; } = "";
    public override string ToString() => "ExchangeCodeParams { .. }";
}

internal sealed class OAuthInputException : Exception { }

internal sealed class OAuthInputBudget
{
    private int bytes;
    internal void Charge(string? value)
    {
        OAuthAttemptRules.Require(value is not null && value.Length <= OAuthAttemptRules.MaxCaptureBytes);
        bytes = checked(bytes + OAuthAttemptRules.Utf8.GetByteCount(value!));
        OAuthAttemptRules.Require(bytes <= OAuthAttemptRules.MaxCaptureBytes);
    }
    internal string[] Scopes(IReadOnlyList<string> values)
    {
        OAuthAttemptRules.Require(values.Count <= OAuthAttemptRules.MaxItems);
        var copy = new List<string>();
        foreach (var value in values)
        {
            OAuthAttemptRules.Require(copy.Count < OAuthAttemptRules.MaxItems);
            Charge(value);
            copy.Add(value);
        }
        return copy.ToArray();
    }
}

internal static class OAuthAttemptRules
{
    internal const int MaxAttempts = 32;
    internal const int MaxItems = 64;
    internal const int MaxCaptureBytes = 32768;
    internal const int MaxCallbackBytes = 16384;
    internal const long DefaultTokenResponseBytes = 65536;
    internal static readonly TimeSpan Lifetime = TimeSpan.FromMinutes(10);
    internal static readonly UTF8Encoding Utf8 = new(false, true);
    private static readonly HashSet<string> Protected = new(StringComparer.Ordinal)
    {
        "response_type", "client_id", "client_secret", "redirect_uri", "scope", "resource", "audience",
        "state", "code_challenge", "code_challenge_method", "code_verifier", "grant_type", "code", "iss",
        "error", "error_description", "error_uri", "request", "request_uri", "response_mode",
    };
    internal static void Require(bool allowed) { if (!allowed) throw new OAuthInputException(); }
    internal static bool Reserved(string key) => Protected.Contains(key);
    internal static bool ValidTokenLimit(long limit) => limit > 0 && limit <= 9007199254740991;

    private static int Hex(byte value) => value switch
    {
        >= (byte)'0' and <= (byte)'9' => value - '0',
        >= (byte)'a' and <= (byte)'f' => value - 'a' + 10,
        >= (byte)'A' and <= (byte)'F' => value - 'A' + 10,
        _ => throw new OAuthInputException(),
    };
    private static string Decode(string value)
    {
        var raw = Utf8.GetBytes(value);
        var decoded = new byte[raw.Length];
        int used = 0;
        for (int i = 0; i < raw.Length; i++)
        {
            if (raw[i] == '%')
            {
                Require(i + 2 < raw.Length);
                decoded[used++] = (byte)((Hex(raw[i + 1]) << 4) | Hex(raw[i + 2]));
                i += 2;
            }
            else decoded[used++] = raw[i] == '+' ? (byte)' ' : raw[i];
        }
        return Utf8.GetString(decoded, 0, used);
    }
    internal static Dictionary<string, string> Fields(string query)
    {
        Require(query.Length <= MaxCallbackBytes && Utf8.GetByteCount(query) <= MaxCallbackBytes);
        int count = query.Length == 0 ? 0 : 1;
        foreach (char ch in query) if (ch == '&') Require(++count <= MaxItems);
        var fields = new Dictionary<string, string>(StringComparer.Ordinal);
        int size = 0;
        if (query.Length == 0) return fields;
        foreach (var pair in query.Split('&'))
        {
            int equal = pair.IndexOf('=');
            Require(equal > 0);
            var key = Decode(pair[..equal]);
            var value = Decode(pair[(equal + 1)..]);
            size += Utf8.GetByteCount(key) + Utf8.GetByteCount(value);
            Require(key.Length > 0 && size <= MaxCallbackBytes && fields.TryAdd(key, value));
        }
        return fields;
    }
    internal static void Endpoint(string value, bool redirect = false)
    {
        Require(value.Length <= MaxCaptureBytes && Utf8.GetByteCount(value) <= MaxCaptureBytes);
        Require(Uri.TryCreate(value, UriKind.Absolute, out var uri) && uri.UserInfo.Length == 0 && !value.Contains('#'));
        int query = value.IndexOf('?');
        if (redirect) Require(query < 0);
        else
        {
            var endpoint = uri!;
            Require((endpoint.Scheme is "http" or "https") && endpoint.Host.Length > 0);
            if (query >= 0) foreach (var key in Fields(value[(query + 1)..]).Keys) Require(!Reserved(key));
        }
    }
}

internal readonly record struct OAuthVersion(ulong Generation, ulong Epoch);
internal sealed class OAuthSessionState
{
    internal ulong Generation;
    internal ulong Epoch;
    internal int LoggingOut;
    internal bool Closed;
}

internal sealed class PendingAuthorization
{
    internal required string Scheme { get; init; }
    internal ulong Generation;
    internal string Handle = "";
    internal required CodeClient Client { get; init; }
    internal required AuthorizationCodeFlow Flow { get; init; }
    internal required string Binding { get; init; }
    internal required string Url { get; init; }
    internal required string Redirect { get; init; }
    internal required string[] RequestedScopes { get; init; }
    internal string? Resource { get; init; }
    internal string? Audience { get; init; }
    internal string? AccountContext { get; init; }
    internal required IssuerDefense Defense { get; init; }
    internal long Started { get; } = Stopwatch.GetTimestamp();
    internal required string State { get; set; }
    internal string? Verifier { get; set; }
    internal bool Claimed;
    internal bool Canceled;
    internal CancellationToken BeginCancellation;
    internal Timer? Timer;
    internal CancellationTokenRegistration Cancellation;
    internal bool Expired => Stopwatch.GetElapsedTime(Started) >= OAuthAttemptRules.Lifetime;
    internal void DisposeSecrets()
    {
        Verifier = null;
        State = "";
        Timer?.Dispose();
        Timer = null;
        Cancellation.Unregister(); // Never wait for a callback blocked on the state lock.
        BeginCancellation = default;
    }
}
