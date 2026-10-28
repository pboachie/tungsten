// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

internal sealed record PlanHeader(string Name, string Value, bool Secret);

internal sealed class AuthPlan
{
    public List<PlanHeader> Headers { get; } = new();
    public List<(string K, string V)> Cookies { get; } = new();
    public List<(string K, string V)> Query { get; } = new();
}

internal enum SatisfierKind
{
    Composite,
    Secret,
    OAuthCode,
    OAuth,
}

internal sealed record Satisfier
{
    public SatisfierKind Kind { get; init; }
    public string Name { get; init; } = "";
    public IReadOnlyList<CompositePart> Parts { get; init; } = System.Array.Empty<CompositePart>();
    public IReadOnlyDictionary<string, string> Values { get; init; } = new Dictionary<string, string>();
    public bool Mutation { get; init; }
    public AuthScheme? Scheme { get; init; }
    public string Secret { get; init; } = "";
    public string ClientSecret { get; init; } = "";
    public string ClientId { get; init; } = "";
    public string TokenUrl { get; init; } = "";
    public IReadOnlyList<string> Scopes { get; init; } = System.Array.Empty<string>();
}

internal sealed class AuthResult
{
    public AuthPlan Plan { get; } = new();
    public List<string> Secrets { get; } = new();
    /// <summary>The authorization-code scheme whose stored token the plan carries.</summary>
    public string? OAuth { get; set; }
    /// <summary>The remediation of an AUTH_FAILED envelope.</summary>
    public string? Failed { get; set; }
}

/// <summary>
/// Auth resolution: an operation's security is an OR of AND-sets of scheme names; the first
/// alternative the configured credentials fully satisfy is applied. A composite profile
/// satisfies the names in its Satisfies list when every part it needs for this request is
/// configured.
/// </summary>
public sealed partial class ClientCore
{
    private const string InsecureHttpAuth = "Refusing to send credentials over non-loopback HTTP. Use HTTPS, or set ClientOptions.AllowInsecureHttp = true for intentional insecure transport. Nothing was sent.";

    private bool CredentialHeader(string name, bool preservedIdempotency = false) => (!preservedIdempotency && Util.LooksSensitive(name)) || name.Equals("Authorization", StringComparison.OrdinalIgnoreCase)
        || name.Equals("Proxy-Authorization", StringComparison.OrdinalIgnoreCase) || name.Equals("Cookie", StringComparison.OrdinalIgnoreCase)
        || api.Auth.Any(s => s.Kind == "api_key" && s.In == "header" && s.Wire.Equals(name, StringComparison.OrdinalIgnoreCase)
            || s.Kind == "composite" && s.Parts.Any(p => p.Kind == "header" && string.IsNullOrEmpty(p.FromConfig) && string.Equals(p.Name, name, StringComparison.OrdinalIgnoreCase)));

    private bool InsecureHttp(string target)
    {
        if (allowInsecureHttp || !Uri.TryCreate(target, UriKind.Absolute, out var url) || url.Scheme != "http") return false;
        var host = url.Host.Trim('[', ']').ToLowerInvariant();
        if (host is "localhost" or "localhost.") return false;
        if (!System.Net.IPAddress.TryParse(host, out var ip)) return true;
        return ip.AddressFamily == System.Net.Sockets.AddressFamily.InterNetwork
            ? ip.GetAddressBytes()[0] != 127 : !ip.Equals(System.Net.IPAddress.IPv6Loopback);
    }
    internal static bool IsSafeMethod(string method) => method is "GET" or "HEAD" or "OPTIONS" or "TRACE" or "QUERY";

    private static string CompositeConfigKey(CompositePart p)
    {
        switch (p.Kind)
        {
            case "cookie":
                return p.Name ?? "";
            case "bearer":
                return "bearer";
        }
        if (!string.IsNullOrEmpty(p.EqualsCookie))
        {
            return p.EqualsCookie;
        }
        if (!string.IsNullOrEmpty(p.FromConfig))
        {
            return p.FromConfig;
        }
        return p.Name ?? "";
    }

    private static string? CheckPrefix(string token, string? prefix, string what)
    {
        if (!string.IsNullOrEmpty(prefix) && !token.StartsWith(prefix, StringComparison.Ordinal))
        {
            return $"The {what} credential must start with \"{prefix}\"; check that the right kind of token is configured.";
        }
        return null;
    }

    private static bool Skips(CompositePart p, bool mutation) => p.Kind == "header" && p.MutationOnly && !mutation;

    private static (string? Key, Satisfier? Sat, List<string> Missing) TryComposite(string name, IReadOnlyList<CompositePart> parts, Credential cred, string method)
    {
        if (!cred.IsParts)
        {
            return (null, null, new List<string> { $"auth.{name} (an object for the composite profile {name})" });
        }
        bool mutation = !IsSafeMethod(method);
        var missing = new List<string>();
        var values = cred.PartValues!;
        foreach (var p in parts)
        {
            if (Skips(p, mutation))
            {
                continue;
            }
            var key = CompositeConfigKey(p);
            if (!values.TryGetValue(key, out var v) || v.Length == 0)
            {
                string role = p.Kind switch
                {
                    "cookie" => "cookie " + p.Name,
                    "bearer" => "bearer token",
                    _ => "header " + p.Name + (!string.IsNullOrEmpty(p.EqualsCookie) ? " (equal to cookie " + p.EqualsCookie + ")" : ""),
                };
                missing.Add($"auth.{name}[{Json.Quote(key)}] ({role} of the composite profile {name})");
            }
        }
        if (missing.Count > 0)
        {
            return (null, null, missing);
        }
        return ("composite:" + name, new Satisfier { Kind = SatisfierKind.Composite, Name = name, Parts = parts, Values = values, Mutation = mutation }, new List<string>());
    }

    private static string? CredPart(IReadOnlyDictionary<string, string> parts, string snake, string camel, out bool present)
    {
        if (parts.TryGetValue(snake, out var v))
        {
            present = true;
            return v;
        }
        present = parts.TryGetValue(camel, out v);
        return v;
    }

    private static (string? Key, Satisfier? Sat, List<string> Missing) TryDirect(AuthScheme scheme, Credential? cred)
    {
        var name = scheme.Name;
        if (scheme.Kind == "oauth2" && cred is not null && cred.IsParts)
        {
            var parts = cred.PartValues!;
            if (CredPart(parts, "flow", "flow", out _) == "authorizationCode")
            {
                if (scheme.AuthorizationCode is null)
                {
                    return (null, null, new List<string> { $"auth.{name} (sets flow \"authorizationCode\", but the API descriptor has no authorizationCode flow for {name})" });
                }
                if (string.IsNullOrEmpty(CredPart(parts, "client_id", "clientId", out _)))
                {
                    return (null, null, new List<string> { $"auth.{name} (a client_id for the authorizationCode flow)" });
                }
                var secret = CredPart(parts, "client_secret", "clientSecret", out _) ?? "";
                return ("scheme:" + name, new Satisfier { Kind = SatisfierKind.OAuthCode, Name = name, ClientSecret = secret }, new List<string>());
            }
            var id = CredPart(parts, "client_id", "clientId", out var hasId);
            var clientSecret = CredPart(parts, "client_secret", "clientSecret", out var hasSecret);
            if (hasId && hasSecret && !string.IsNullOrEmpty(scheme.TokenUrl))
            {
                return ("scheme:" + name, new Satisfier
                {
                    Kind = SatisfierKind.OAuth,
                    Name = name,
                    TokenUrl = scheme.TokenUrl,
                    Scopes = scheme.Scopes,
                    ClientId = id ?? "",
                    ClientSecret = clientSecret ?? "",
                }, new List<string>());
            }
            var code = scheme.AuthorizationCode is not null ? ", or {flow: \"authorizationCode\", client_id, client_secret?, redirect_uri?}" : "";
            return (null, null, new List<string> { $"auth.{name} (an access token, or {{client_id, client_secret}} for the token URL{code})" });
        }
        if (cred is not null && !cred.IsParts && cred.SecretValue.Length > 0)
        {
            return ("scheme:" + name, new Satisfier { Kind = SatisfierKind.Secret, Scheme = scheme, Secret = cred.SecretValue }, new List<string>());
        }
        string what = scheme.Kind switch
        {
            "http_basic" => $"\"user:password\" for HTTP Basic scheme {name}",
            "api_key" => $"API key for scheme {name} ({scheme.In} {scheme.Wire})",
            _ => "bearer token for scheme " + name,
        };
        return (null, null, new List<string> { $"auth.{name} ({what})" });
    }

    private (string? Key, Satisfier? Sat, List<string> Missing) TryScheme(string name, string method)
    {
        var missing = new List<string>();
        foreach (var s in api.Auth)
        {
            if (s.Kind != "composite" || !s.Satisfies.Contains(name))
            {
                continue;
            }
            if (!auth.TryGetValue(s.Name, out var cred))
            {
                missing.Add($"auth.{s.Name} (composite profile {s.Name})");
                continue;
            }
            var (key, sat, more) = TryComposite(s.Name, s.Parts, cred, method);
            if (sat is not null)
            {
                return (key, sat, new List<string>());
            }
            missing.AddRange(more);
        }
        var direct = api.Auth.FirstOrDefault(a => a.Name == name && a.Kind != "composite");
        if (direct is not null)
        {
            auth.TryGetValue(name, out var cred);
            var (key, sat, more) = TryDirect(direct, cred);
            if (sat is not null)
            {
                return (key, sat, new List<string>());
            }
            // A composite profile covering the name explains the gap better.
            if (missing.Count == 0)
            {
                missing.AddRange(more);
            }
        }
        else if (missing.Count == 0)
        {
            missing.Add($"a scheme named {name} (the API descriptor does not define it)");
        }
        return (null, null, missing);
    }

    private bool Configured(string name)
    {
        if (auth.ContainsKey(name))
        {
            return true;
        }
        return api.Auth.Any(s => s.Kind == "composite" && s.Satisfies.Contains(name) && auth.ContainsKey(s.Name));
    }

    private static void Bearer(AuthPlan plan, string token) => plan.Headers.Add(new PlanHeader("Authorization", "Bearer " + token, true));

    private async Task<string?> ApplyAsync(Satisfier s, AuthResult result, CancellationToken ct)
    {
        var plan = result.Plan;
        switch (s.Kind)
        {
            case SatisfierKind.Composite:
                foreach (var p in s.Parts)
                {
                    if (Skips(p, s.Mutation))
                    {
                        continue;
                    }
                    var value = s.Values.TryGetValue(CompositeConfigKey(p), out var v) ? v : "";
                    switch (p.Kind)
                    {
                        case "cookie":
                            plan.Cookies.Add((p.Name ?? "", value));
                            result.Secrets.Add(value);
                            break;
                        case "bearer":
                            if (CheckPrefix(value, p.Prefix, s.Name + " bearer") is string problem)
                            {
                                return problem;
                            }
                            Bearer(plan, value);
                            result.Secrets.Add(value);
                            break;
                        default:
                            bool secret = string.IsNullOrEmpty(p.FromConfig);
                            plan.Headers.Add(new PlanHeader(p.Name ?? "", value, secret));
                            if (secret)
                            {
                                result.Secrets.Add(value);
                            }
                            break;
                    }
                }
                break;
            case SatisfierKind.Secret:
                {
                    var scheme = s.Scheme!;
                    result.Secrets.Add(s.Secret);
                    switch (scheme.Kind)
                    {
                        case "api_key":
                            switch (scheme.In)
                            {
                                case "query":
                                    plan.Query.Add((scheme.Wire, s.Secret));
                                    break;
                                case "cookie":
                                    plan.Cookies.Add((scheme.Wire, s.Secret));
                                    break;
                                default:
                                    plan.Headers.Add(new PlanHeader(scheme.Wire, s.Secret, true));
                                    break;
                            }
                            break;
                        case "http_bearer":
                            if (CheckPrefix(s.Secret, scheme.Prefix, scheme.Name) is string problem)
                            {
                                return problem;
                            }
                            Bearer(plan, s.Secret);
                            break;
                        case "http_basic":
                            var encoded = Convert.ToBase64String(Util.Utf8.GetBytes(s.Secret));
                            result.Secrets.Add(encoded);
                            plan.Headers.Add(new PlanHeader("Authorization", "Basic " + encoded, true));
                            break;
                        default:
                            Bearer(plan, s.Secret);
                            break;
                    }
                    break;
                }
            case SatisfierKind.OAuthCode:
                {
                    if (s.ClientSecret.Length > 0)
                    {
                        result.Secrets.Add(s.ClientSecret);
                    }
                    var (token, problem) = await StoredTokenAsync(s.Name, false, ct).ConfigureAwait(false);
                    if (problem is not null)
                    {
                        return problem;
                    }
                    result.Secrets.Add(token!);
                    Bearer(plan, token!);
                    result.OAuth ??= s.Name;
                    break;
                }
            case SatisfierKind.OAuth:
                {
                    result.Secrets.Add(s.ClientSecret);
                    var (token, problem) = await ClientCredentialsTokenAsync(s, ct).ConfigureAwait(false);
                    if (problem is not null)
                    {
                        return problem;
                    }
                    result.Secrets.Add(token!);
                    Bearer(plan, token!);
                    break;
                }
        }
        return null;
    }

    /// <summary>Resolves the credentials for an operation; a problem is the result's Failed remediation.</summary>
    private async Task<AuthResult> ResolveAuthAsync(OperationDescriptor op, string method, CancellationToken ct, string? credentialError = null)
    {
        if (op.Security.Count == 0)
        {
            return new AuthResult();
        }
        List<string>? best = null;
        bool bestTouched = false;
        // An empty alternative (anonymous access) is the fallback, never preferred over configured credentials.
        var ordered = op.Security.Where(a => a.Count > 0).Concat(op.Security.Where(a => a.Count == 0)).ToList();
        foreach (var alternative in ordered)
        {
            var sats = new List<(string Key, Satisfier Sat)>();
            var missing = new List<string>();
            foreach (var name in alternative)
            {
                var (key, sat, more) = TryScheme(name, method);
                if (sat is null || key is null)
                {
                    missing.AddRange(more);
                    continue;
                }
                int at = sats.FindIndex(x => x.Key == key);
                if (at >= 0)
                {
                    sats[at] = (key, sat);
                }
                else
                {
                    sats.Add((key, sat));
                }
            }
            if (missing.Count == 0)
            {
                if (sats.Count > 0 && credentialError is not null) return new AuthResult { Failed = credentialError };
                var result = new AuthResult();
                foreach (var (_, sat) in sats)
                {
                    if (await ApplyAsync(sat, result, ct).ConfigureAwait(false) is string problem)
                    {
                        return new AuthResult { Failed = $"{problem} Operation {op.Id} was not sent." };
                    }
                }
                return result;
            }
            var unique = missing.Distinct().ToList();
            bool touched = alternative.Any(Configured);
            bool better = best is null || (touched && !bestTouched) || (touched == bestTouched && unique.Count < best.Count);
            if (better)
            {
                best = unique;
                bestTouched = touched;
            }
        }
        var alternatives = op.Security.Select(a => a.Count == 0 ? "no credentials" : string.Join(" AND ", a));
        var missingText = best is null ? "credentials" : string.Join("; ", best);
        return new AuthResult
        {
            Failed = $"Operation {op.Id} needs {string.Join(" OR ", alternatives)}. Missing: {missingText}. Configure it in ClientOptions.auth and call again; the request was not sent.",
        };
    }
}
