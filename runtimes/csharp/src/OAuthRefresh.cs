// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

public sealed partial class ClientCore
{
    private sealed record OAuthRefreshResult(string? Token, TokenFailure? Failure, OAuthVersion? Committed = null)
    {
        public override string ToString() => "OAuthRefreshResult { .. }";
    }
    private sealed class OAuthRefreshFlight
    {
        internal readonly TaskCompletionSource<OAuthRefreshResult> Result = new(TaskCreationOptions.RunContinuationsAsynchronously);
    }
    private readonly Dictionary<(string Scheme, OAuthVersion Version), OAuthRefreshFlight> oauthRefreshes = new();

    internal StoredToken? ReadToken(string scheme)
    {
        var version = OAuthVersionOf(scheme);
        if (!OAuthVersionCurrent(scheme, version)) return null;
        try
        {
            var stored = tokenStore.Get(scheme);
            return stored is not null && stored.AccessToken.Length > 0 && OAuthVersionCurrent(scheme, version) ? stored : null;
        }
        catch (Exception) { return null; }
    }

    internal async Task<(string? Token, string? Problem)> StoredTokenAsync(string scheme, bool force, CancellationToken ct)
    {
        var (token, failure) = await StoredTokenInnerAsync(scheme, force, ct).ConfigureAwait(false);
        return failure is not null ? (null, failure.Message) : (token, null);
    }

    internal async Task<(string? Token, TokenFailure? Failure)> StoredTokenInnerAsync(string scheme, bool force, CancellationToken ct)
    {
        if (ct.IsCancellationRequested) return (null, new("OAuth token acquisition was canceled by the caller.", false));
        var version = OAuthVersionOf(scheme);
        var stored = ReadToken(scheme);
        if (!OAuthVersionCurrent(scheme, version)) return (null, OAuthSessionChanged());
        if (stored is null) return (null, new($"No OAuth2 token is stored for {scheme}; begin a managed AuthorizationUrl attempt and complete it with its handle, full callback URL and owning session binding.", false));
        bool fresh = stored.ExpiresAt is null || stored.ExpiresAt > Now() + ExpirySkewMs;
        if (fresh && !force) return (stored.AccessToken, null);
        var key = (scheme, version);
        OAuthRefreshFlight flight;
        bool owner = false;
        lock (oauthLock)
        {
            if (!OAuthActiveLocked(scheme, version.Generation) || OAuthSessionLocked(scheme).Epoch != version.Epoch) return (null, OAuthSessionChanged());
            if (!oauthRefreshes.TryGetValue(key, out flight!))
            {
                if (oauthRefreshes.Count >= OAuthAttemptRules.MaxAttempts) return (null, new("OAuth refresh capacity is exhausted; wait for an owned refresh to settle.", false));
                flight = new();
                oauthRefreshes.Add(key, flight);
                owner = true;
            }
        }
        if (owner) _ = RunOAuthRefreshAsync(scheme, version, stored, force, flight);
        OAuthRefreshResult result;
        try { result = await flight.Result.Task.WaitAsync(ct).ConfigureAwait(false); }
        catch (OperationCanceledException) { return (null, new("OAuth token acquisition was canceled by the caller.", false)); }
        if (result.Failure is not null) return (null, result.Failure);
        if (result.Committed is not OAuthVersion committed || !OAuthVersionCurrent(scheme, committed)) return (null, OAuthSessionChanged());
        return (result.Token, null);
    }

    private async Task RunOAuthRefreshAsync(string scheme, OAuthVersion version, StoredToken previous, bool force, OAuthRefreshFlight flight)
    {
        OAuthRefreshResult result;
        try { result = await RefreshStoredAsync(scheme, previous, force, version).ConfigureAwait(false); }
        catch (Exception) { result = new(null, new("OAuth token storage or refresh did not complete; authorize again.", false)); }
        lock (oauthLock)
        {
            var key = (scheme, version);
            if (oauthRefreshes.TryGetValue(key, out var current) && ReferenceEquals(current, flight)) oauthRefreshes.Remove(key);
        }
        flight.Result.TrySetResult(result);
    }

    internal async Task<string?> RefreshAfterRejectionAsync(string scheme, string rejected, CancellationToken ct)
    {
        var version = OAuthVersionOf(scheme);
        var stored = ReadToken(scheme);
        if (stored is null || !OAuthVersionCurrent(scheme, version)) return null;
        if (stored.AccessToken != rejected) return stored.AccessToken;
        if (string.IsNullOrEmpty(stored.RefreshToken)) return null;
        var (token, problem) = await StoredTokenAsync(scheme, true, ct).ConfigureAwait(false);
        return problem is null ? token : null;
    }

    private static bool SameOAuthToken(StoredToken? current, StoredToken previous) =>
        current is not null && current.AccessToken == previous.AccessToken && current.RefreshToken == previous.RefreshToken;

    private async Task<OAuthRefreshResult> RefreshStoredAsync(string scheme, StoredToken previous, bool force, OAuthVersion version)
    {
        if (!OAuthVersionCurrent(scheme, version)) return new(null, OAuthSessionChanged());
        if (tokenStore is not IFencedTokenStore fenced) return new(null, new("OAuth refresh requires an atomic fenced token store.", false));
        var (flow, reason) = CodeFlow(scheme);
        if (flow is null) return new(null, new($"Cannot refresh the OAuth2 token for {scheme}: {reason}.", false));
        var (client, why) = CodeClientOf(scheme);
        if (client is null) return new(null, new($"Cannot refresh the OAuth2 token for {scheme}: {why}.", false));
        if (string.IsNullOrEmpty(previous.RefreshToken))
        {
            var what = "has no refresh token";
            if (!force) what = "expired and has no refresh token";
            return new(null, new($"The OAuth2 token for {scheme} {what}; begin a new managed authorization attempt.", false));
        }
        var url = string.IsNullOrEmpty(flow.RefreshUrl) ? flow.TokenUrl ?? "" : flow.RefreshUrl;
        try { OAuthAttemptRules.Endpoint(url); }
        catch (Exception) { return new(null, new("OAuth refresh requires a valid endpoint without protected query fields.", false)); }
        var fields = new List<(string, string)> { ("grant_type", "refresh_token"), ("refresh_token", previous.RefreshToken) };
        // The job owns its transport deadline and admission until actual settlement, even if all waiters leave.
        var (renewed, failure) = await TokenRequestAsync(scheme, url, client, fields, previous.RefreshToken, CancellationToken.None).ConfigureAwait(false);
        bool CurrentLocked(StoredToken? current) => OAuthActiveLocked(scheme, version.Generation)
            && OAuthSessionLocked(scheme).Epoch == version.Epoch && SameOAuthToken(current, previous);
        if (failure is not null || renewed is null)
        {
            if (failure?.Rejected == true)
                fenced.DeleteIfCurrent(scheme, current =>
                {
                    lock (oauthLock) return CurrentLocked(current) && OAuthMutateLocked(scheme) is not null;
                });
            return new(null, failure ?? new("OAuth refresh returned no token; authorize again.", false));
        }
        ulong? committed = null;
        bool changed = fenced.SetIfCurrent(scheme, renewed, current =>
        {
            lock (oauthLock)
            {
                if (!CurrentLocked(current)) return false;
                committed = OAuthMutateLocked(scheme);
                return committed is not null;
            }
        });
        if (!changed || committed is not ulong epoch || !OAuthVersionCurrent(scheme, new(version.Generation, epoch))) return new(null, OAuthSessionChanged());
        return new(renewed.AccessToken, null, new(version.Generation, epoch));
    }
}
