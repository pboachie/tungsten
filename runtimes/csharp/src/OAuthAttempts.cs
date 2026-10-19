// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

public sealed partial class OAuthFlow
{
    /// <summary>Begin a bounded, session-bound S256/state attempt; retain its handle with the owning listener.</summary>
    public Result<AuthorizationAttempt> AuthorizationUrl(AuthorizationUrlParams parameters, CancellationToken cancellationToken = default) =>
        core.BeginAuthorization(Scheme, parameters, cancellationToken);

    /// <summary>Validate the full callback and claim exactly one exchange. Failure never reopens the code.</summary>
    public Task<Result<TokenInfo>> ExchangeCodeAsync(ExchangeCodeParams parameters, CancellationToken cancellationToken = default) =>
        core.CompleteAuthorizationAsync(Scheme, parameters, cancellationToken);

    /// <summary>Invalidate one owned attempt. Claimed work retains capacity until it actually settles.</summary>
    public Result<NoContent> CancelAuthorization(string attemptHandle, string sessionBinding) =>
        core.CancelAuthorization(Scheme, attemptHandle, sessionBinding);

    /// <summary>Invalidate local attempts before token removal; this does not revoke credentials remotely.</summary>
    public Result<NoContent> Logout() => core.LogoutAuthorization(Scheme);
}

public sealed partial class ClientCore
{
    private readonly object oauthLock = new();
    private readonly Dictionary<string, PendingAuthorization> oauthAttempts = new(StringComparer.Ordinal);
    private readonly Dictionary<string, OAuthSessionState> oauthSessions = new(StringComparer.Ordinal);
    private int oauthBeginning;

    private static Result<T> OAuthFailure<T>(string scheme, string helper, string remedy) =>
        new Result<T>.Err(new TungstenError(new Diag("oauth." + scheme + "." + helper, Category.AuthFailed).Remedy(remedy).Build()));
    private static Result<T> OAuthOk<T>(T value) => new Result<T>.Ok(value) { Meta = ResponseMeta.Local() };
    private static TokenFailure OAuthSessionChanged() => new("OAuth session changed; authorize again.", false);
    private OAuthSessionState OAuthSessionLocked(string scheme)
    {
        if (!oauthSessions.TryGetValue(scheme, out var session)) oauthSessions.Add(scheme, session = new());
        return session;
    }
    private OAuthVersion OAuthVersionOf(string scheme)
    {
        lock (oauthLock)
        {
            var session = OAuthSessionLocked(scheme);
            return new(session.Generation, session.Epoch);
        }
    }
    private bool OAuthActiveLocked(string scheme, ulong generation)
    {
        var session = OAuthSessionLocked(scheme);
        return !session.Closed && session.Generation == generation && session.LoggingOut == 0;
    }
    private bool OAuthVersionCurrent(string scheme, OAuthVersion version)
    {
        lock (oauthLock) return OAuthActiveLocked(scheme, version.Generation) && OAuthSessionLocked(scheme).Epoch == version.Epoch;
    }
    // Called exactly once by a store's final serialized mutation predicate.
    private ulong? OAuthMutateLocked(string scheme)
    {
        var session = OAuthSessionLocked(scheme);
        if (session.Epoch == ulong.MaxValue) { session.Closed = true; return null; }
        return ++session.Epoch;
    }
    private void RemoveAuthorizationLocked(PendingAuthorization pending)
    {
        if (oauthAttempts.TryGetValue(pending.Handle, out var current) && ReferenceEquals(current, pending)) oauthAttempts.Remove(pending.Handle);
        pending.DisposeSecrets();
    }
    private void CancelAuthorizationLocked(PendingAuthorization pending)
    {
        pending.Canceled = true;
        pending.Verifier = null;
        if (!pending.Claimed) RemoveAuthorizationLocked(pending);
    }
    private void SweepAuthorizationsLocked()
    {
        foreach (var pending in oauthAttempts.Values.Where(p => p.Expired).ToArray()) CancelAuthorizationLocked(pending);
    }
    private bool AuthorizationLiveLocked(PendingAuthorization pending) =>
        oauthAttempts.TryGetValue(pending.Handle, out var current) && ReferenceEquals(current, pending)
        && !pending.Canceled && !pending.BeginCancellation.IsCancellationRequested
        && !pending.Expired && OAuthActiveLocked(pending.Scheme, pending.Generation);

    private bool AuthorizationConfigurationCurrent(PendingAuthorization pending)
    {
        try
        {
            var (flow, _) = CodeFlow(pending.Scheme);
            var (client, _) = CodeClientOf(pending.Scheme);
            if (flow is null || client != pending.Client
                || flow.AuthorizationUrl != pending.Flow.AuthorizationUrl || flow.TokenUrl != pending.Flow.TokenUrl
                || flow.RefreshUrl != pending.Flow.RefreshUrl || flow.Scopes.Count > OAuthAttemptRules.MaxItems) return false;
            int index = 0;
            foreach (var scope in flow.Scopes)
            {
                if (index >= pending.Flow.Scopes.Count || scope != pending.Flow.Scopes[index++]) return false;
            }
            return index == pending.Flow.Scopes.Count;
        }
        catch (Exception) { return false; }
    }
    private bool AuthorizationCurrent(PendingAuthorization pending)
    {
        if (!AuthorizationConfigurationCurrent(pending)) return false;
        lock (oauthLock) return AuthorizationLiveLocked(pending);
    }
    private void CancelOwnedAuthorization(PendingAuthorization pending)
    {
        lock (oauthLock)
        {
            if (oauthAttempts.TryGetValue(pending.Handle, out var current) && ReferenceEquals(current, pending)) CancelAuthorizationLocked(pending);
        }
    }
    private sealed record AuthorizationTimerState(WeakReference<ClientCore> Client, WeakReference<PendingAuthorization> Pending);
    private static void AuthorizationCanceled(object? state)
    {
        if (state is AuthorizationTimerState owned && owned.Client.TryGetTarget(out var client) && owned.Pending.TryGetTarget(out var pending))
            client.CancelOwnedAuthorization(pending);
    }

    private PendingAuthorization CaptureAuthorization(string scheme, AuthorizationUrlParams p, ulong generation)
    {
        var (flow, _) = CodeFlow(scheme);
        var (client, _) = CodeClientOf(scheme);
        OAuthAttemptRules.Require(flow is not null && client is not null && p.S256Supported
            && !string.IsNullOrEmpty(p.SessionBinding) && p.IssuerDefense is { Value.Length: > 0 });
        var ownedFlow = flow!;
        var ownedClient = client!;
        var redirect = string.IsNullOrEmpty(p.RedirectUri) ? ownedClient.RedirectUri : p.RedirectUri;
        var budget = new OAuthInputBudget();
        foreach (var value in new[] { scheme, ownedClient.ClientId, ownedClient.ClientSecret, ownedClient.RedirectUri,
            redirect, ownedFlow.AuthorizationUrl, ownedFlow.TokenUrl, ownedFlow.RefreshUrl ?? "", p.SessionBinding,
            p.IssuerDefense!.Value, p.Resource ?? "", p.Audience ?? "", p.AccountContext ?? "" }) budget.Charge(value);
        var authorizationUrl = ownedFlow.AuthorizationUrl;
        var tokenUrl = ownedFlow.TokenUrl;
        var refreshUrl = ownedFlow.RefreshUrl;
        var configuredScopes = budget.Scopes(ownedFlow.Scopes);
        var requestedScopes = p.Scopes is null ? configuredScopes : budget.Scopes(p.Scopes);
        var extra = new Dictionary<string, string>(StringComparer.Ordinal);
        if (p.Extra is not null)
        {
            OAuthAttemptRules.Require(p.Extra.Count <= OAuthAttemptRules.MaxItems);
            foreach (var (key, value) in p.Extra)
            {
                OAuthAttemptRules.Require(extra.Count < OAuthAttemptRules.MaxItems && !OAuthAttemptRules.Reserved(key));
                budget.Charge(key); budget.Charge(value);
                extra.Add(key, value);
            }
        }
        OAuthAttemptRules.Endpoint(authorizationUrl!);
        OAuthAttemptRules.Endpoint(tokenUrl!);
        OAuthAttemptRules.Endpoint(redirect!, true);
        OAuthAttemptRules.Require(!InsecureHttp(authorizationUrl!) && !InsecureHttp(tokenUrl!));
        if (!string.IsNullOrEmpty(refreshUrl))
        {
            OAuthAttemptRules.Endpoint(refreshUrl);
            OAuthAttemptRules.Require(!InsecureHttp(refreshUrl));
        }
        if (p.Resource is not null) extra.Add("resource", p.Resource);
        if (p.Audience is not null) extra.Add("audience", p.Audience);
        var pkce = Pkce.Generate();
        var state = Util.Base64Url(Util.RandomBytes(32));
        var captured = new AuthorizationCodeFlow { AuthorizationUrl = authorizationUrl, TokenUrl = tokenUrl, RefreshUrl = refreshUrl, Scopes = configuredScopes };
        var url = OAuthFlow.BuildAuthorizationUrl(captured, ownedClient.ClientId, redirect!, new LowLevelAuthorizationUrlParams
        { Scopes = requestedScopes, State = state, CodeChallenge = pkce.Challenge, Extra = extra });
        return new PendingAuthorization
        {
            Scheme = scheme, Generation = generation, Client = ownedClient, Flow = captured, Binding = p.SessionBinding,
            Redirect = redirect!, RequestedScopes = requestedScopes, Resource = p.Resource, Audience = p.Audience,
            AccountContext = p.AccountContext, Defense = p.IssuerDefense!, State = state, Verifier = pkce.Verifier,
            Url = url,
        };
    }

    internal Result<AuthorizationAttempt> BeginAuthorization(string scheme, AuthorizationUrlParams p, CancellationToken ct)
    {
        Result<AuthorizationAttempt> Refuse() => OAuthFailure<AuthorizationAttempt>(scheme, "authorizationUrl",
            "Cannot begin authorization: qualify S256 and issuer defense, supply an owning session binding, bounded valid endpoints and an atomic fenced token store; protected fields are reserved.");
        PendingAuthorization? pending = null;
        bool reserved = false;
        try
        {
            if (tokenStore is not IFencedTokenStore || !OAuthAttemptRules.ValidTokenLimit(maxOAuthTokenResponseBytes) || ct.IsCancellationRequested) return Refuse();
            ulong generation;
            lock (oauthLock)
            {
                SweepAuthorizationsLocked();
                generation = OAuthSessionLocked(scheme).Generation;
                if (!OAuthActiveLocked(scheme, generation)) return Refuse();
                if (oauthAttempts.Count + oauthBeginning >= OAuthAttemptRules.MaxAttempts)
                    return OAuthFailure<AuthorizationAttempt>(scheme, "authorizationUrl", "Authorization attempt capacity is exhausted; finish or cancel an existing attempt.");
                oauthBeginning++;
                reserved = true;
            }
            pending = CaptureAuthorization(scheme, p, generation);
            pending.BeginCancellation = ct;
            lock (oauthLock)
            {
                OAuthAttemptRules.Require(OAuthActiveLocked(scheme, generation) && !ct.IsCancellationRequested);
                for (int collision = 0; collision < 4; collision++)
                {
                    pending.Handle = Util.Base64Url(Util.RandomBytes(32));
                    if (!oauthAttempts.ContainsKey(pending.Handle)) break;
                }
                OAuthAttemptRules.Require(!oauthAttempts.ContainsKey(pending.Handle));
                oauthAttempts.Add(pending.Handle, pending);
                oauthBeginning--;
                reserved = false;
            }
            var timerState = new AuthorizationTimerState(new(this), new(pending));
            var timer = new Timer(AuthorizationCanceled, timerState, OAuthAttemptRules.Lifetime, Timeout.InfiniteTimeSpan);
            lock (oauthLock)
            {
                if (AuthorizationLiveLocked(pending)) pending.Timer = timer;
                else { timer.Dispose(); throw new OAuthInputException(); }
            }
            var cancellation = ct.Register(AuthorizationCanceled, timerState);
            lock (oauthLock)
            {
                if (AuthorizationLiveLocked(pending)) pending.Cancellation = cancellation;
                else { cancellation.Unregister(); throw new OAuthInputException(); }
            }
            return OAuthOk(new AuthorizationAttempt(pending.Handle, pending.Url, (long)OAuthAttemptRules.Lifetime.TotalMilliseconds));
        }
        catch (Exception)
        {
            if (pending is not null) lock (oauthLock) RemoveAuthorizationLocked(pending);
            return Refuse();
        }
        finally { if (reserved) lock (oauthLock) oauthBeginning--; }
    }

    internal async Task<Result<TokenInfo>> CompleteAuthorizationAsync(string scheme, ExchangeCodeParams p, CancellationToken ct)
    {
        Result<TokenInfo> Refuse() => OAuthFailure<TokenInfo>(scheme, "exchangeCode", "Authorization callback refused: invalid, expired, consumed or mismatched attempt. Begin a new authorization attempt if needed.");
        if (p is null || string.IsNullOrEmpty(p.AttemptHandle) || p.CallbackUrl is null || p.SessionBinding is null) return Refuse();
        PendingAuthorization? pending;
        lock (oauthLock) oauthAttempts.TryGetValue(p.AttemptHandle, out pending);
        if (pending is null || pending.Scheme != scheme || !AuthorizationCurrent(pending)) return Refuse();
        string code, verifier;
        bool claimed = false;
        try
        {
            lock (oauthLock)
            {
                OAuthAttemptRules.Require(AuthorizationLiveLocked(pending) && !pending.Claimed && !ct.IsCancellationRequested && p.SessionBinding == pending.Binding);
                OAuthAttemptRules.Require(p.CallbackUrl.Length <= OAuthAttemptRules.MaxCallbackBytes
                    && OAuthAttemptRules.Utf8.GetByteCount(p.CallbackUrl) <= OAuthAttemptRules.MaxCallbackBytes && !p.CallbackUrl.Contains('#'));
                int query = p.CallbackUrl.IndexOf('?');
                OAuthAttemptRules.Require(query > 0 && p.CallbackUrl[..query] == pending.Redirect);
                var fields = OAuthAttemptRules.Fields(p.CallbackUrl[(query + 1)..]);
                OAuthAttemptRules.Require(fields.GetValueOrDefault("state") == pending.State
                    && (!pending.Defense.Response || fields.GetValueOrDefault("iss") == pending.Defense.Value));
                bool hasCode = fields.TryGetValue("code", out var callbackCode);
                bool denied = fields.TryGetValue("error", out var error);
                OAuthAttemptRules.Require(hasCode != denied && (!hasCode || callbackCode!.Length > 0)
                    && (!denied || error!.Length > 0) && (!hasCode || (!fields.ContainsKey("error_description") && !fields.ContainsKey("error_uri"))));
                pending.Claimed = true;
                claimed = true;
                if (denied)
                {
                    RemoveAuthorizationLocked(pending);
                    return OAuthFailure<TokenInfo>(scheme, "exchangeCode", "Authorization was denied. Begin a new authorization attempt to authorize again.");
                }
                code = callbackCode!;
                verifier = pending.Verifier ?? throw new OAuthInputException();
                pending.Verifier = null;
            }
        }
        catch (Exception)
        {
            if (claimed) lock (oauthLock) RemoveAuthorizationLocked(pending);
            return Refuse();
        }
        var cancellationState = new AuthorizationTimerState(new(this), new(pending));
        CancellationTokenRegistration cancellation = default;
        try
        {
            cancellation = ct.Register(AuthorizationCanceled, cancellationState);
            if (ct.IsCancellationRequested || !AuthorizationCurrent(pending)) return Refuse();
            var fields = new List<(string, string)> { ("grant_type", "authorization_code"), ("code", code), ("redirect_uri", pending.Redirect), ("code_verifier", verifier) };
            var (stored, failure) = await TokenRequestAsync(scheme, pending.Flow.TokenUrl!, pending.Client, fields, null, ct).ConfigureAwait(false);
            if (stored is null || failure is not null) return OAuthFailure<TokenInfo>(scheme, "exchangeCode", "Authorization exchange did not complete. The code may have been consumed; begin a new authorization attempt.");
            if (tokenStore is not IFencedTokenStore fenced) return Refuse();
            ulong? committed = null;
            bool changed = fenced.SetIfCurrent(scheme, stored, _ =>
            {
                if (ct.IsCancellationRequested || !AuthorizationConfigurationCurrent(pending)) return false;
                lock (oauthLock)
                {
                    if (!AuthorizationLiveLocked(pending)) return false;
                    committed = OAuthMutateLocked(scheme);
                    return committed is not null;
                }
            });
            if (!changed || committed is not ulong epoch || ct.IsCancellationRequested || !AuthorizationCurrent(pending)
                || !OAuthVersionCurrent(scheme, new(pending.Generation, epoch))) return Refuse();
            return OAuthOk(new TokenInfo(scheme, stored.ExpiresAt, stored.Scope, !string.IsNullOrEmpty(stored.RefreshToken)));
        }
        catch (Exception) { return Refuse(); }
        finally
        {
            cancellation.Unregister();
            lock (oauthLock) RemoveAuthorizationLocked(pending);
        }
    }

    internal Result<NoContent> CancelAuthorization(string scheme, string handle, string binding)
    {
        if (string.IsNullOrEmpty(handle) || binding is null)
            return OAuthFailure<NoContent>(scheme, "cancelAuthorization", "Authorization attempt is unavailable or belongs to a different session.");
        lock (oauthLock)
        {
            if (!oauthAttempts.TryGetValue(handle, out var pending) || pending.Scheme != scheme || pending.Binding != binding)
                return OAuthFailure<NoContent>(scheme, "cancelAuthorization", "Authorization attempt is unavailable or belongs to a different session.");
            CancelAuthorizationLocked(pending);
        }
        return OAuthOk(NoContent.Value);
    }
    internal Result<NoContent> LogoutAuthorization(string scheme)
    {
        lock (oauthLock)
        {
            var session = OAuthSessionLocked(scheme);
            if (session.Generation == ulong.MaxValue) session.Closed = true;
            else session.Generation++;
            session.LoggingOut++;
            foreach (var pending in oauthAttempts.Values.Where(p => p.Scheme == scheme).ToArray()) CancelAuthorizationLocked(pending);
        }
        try { tokenStore.Delete(scheme); return OAuthOk(NoContent.Value); }
        catch (Exception) { return OAuthFailure<NoContent>(scheme, "logout", "Local credential removal did not complete; check the token store before authorizing again."); }
        finally { lock (oauthLock) OAuthSessionLocked(scheme).LoggingOut--; }
    }
}
