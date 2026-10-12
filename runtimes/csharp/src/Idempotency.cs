// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Concurrent;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Text.Json.Nodes;

namespace Tungsten.Runtime;

/// <summary>Keeps idempotency keys for the life of the process.</summary>
public sealed class MemoryIdempotencyStore : IIdempotencyStore
{
    private readonly ConcurrentDictionary<(string, string), string> keys = new();

    public string? Get(string scope, string logicalId) => keys.TryGetValue((scope, logicalId), out var k) ? k : null;

    public void Put(string scope, string logicalId, string key) => keys[(scope, logicalId)] = key;
}

internal static class Keys
{
    private static string FormatKind(string? format)
    {
        var b = new StringBuilder();
        foreach (var c in (format ?? "").ToLowerInvariant())
        {
            if ((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9'))
            {
                b.Append(c);
            }
        }
        return b.ToString() switch
        {
            "uuidv4" or "uuid4" => "uuid_v4",
            "uuid" => "uuid",
            _ => "token",
        };
    }

    /// <summary>The key format a policy expects, in words.</summary>
    internal static string FormatDescription(string? format) => FormatKind(format) switch
    {
        "uuid_v4" => "UUIDv4 string",
        "uuid" => "UUID string",
        _ => "a non-empty printable ASCII string of at most 255 characters",
    };

    private static bool IsHex(char b) => (b >= '0' && b <= '9') || (b >= 'a' && b <= 'f') || (b >= 'A' && b <= 'F');

    private static bool IsUuid(string key, bool v4Only)
    {
        if (key.Length != 36)
        {
            return false;
        }
        for (int i = 0; i < key.Length; i++)
        {
            char b = key[i];
            bool ok;
            switch (i)
            {
                case 8:
                case 13:
                case 18:
                case 23:
                    ok = b == '-';
                    break;
                case 14:
                    ok = v4Only ? b == '4' : b >= '1' && b <= '8';
                    break;
                case 19:
                    char l = char.ToLowerInvariant(b);
                    ok = l is '8' or '9' or 'a' or 'b';
                    break;
                default:
                    ok = IsHex(b);
                    break;
            }
            if (!ok)
            {
                return false;
            }
        }
        return true;
    }

    /// <summary>The expected format when the key does not match; null when it does.</summary>
    internal static string? CheckFormat(string key, string? format)
    {
        bool valid = FormatKind(format) switch
        {
            "uuid_v4" => IsUuid(key, true),
            "uuid" => IsUuid(key, false),
            _ => key.Length > 0 && key.Length <= 255 && key.All(c => c >= 0x21 && c <= 0x7e),
        };
        return valid ? null : FormatDescription(format);
    }

    /// <summary>The wire header of the operation's key.</summary>
    internal static string Header(OperationDescriptor op)
    {
        if (!string.IsNullOrEmpty(op.Agent.Idempotency.Header))
        {
            return op.Agent.Idempotency.Header;
        }
        return op.Params.FirstOrDefault(p => p.Role == "idempotency_key")?.Wire ?? "Idempotency-Key";
    }
}

/// <summary>Confirmation tokens: <c>tgc1.&lt;expiry ms&gt;.&lt;base64url(HMAC-SHA-256(key, operation, args digest, expiry))&gt;</c>.</summary>
public static class ConfirmationTokens
{
    /// <summary>The lifetime of a confirmation token.</summary>
    public const long TtlMs = 5 * 60 * 1000;

    private const string Prefix = "tgc1";

    /// <summary>The SHA-256 of the canonical JSON of the arguments.</summary>
    public static string ArgsDigest(JsonNode? args) => Util.Sha256Hex(Json.Canonical(args));

    private static string Signature(byte[] key, string operation, string digest, long expiry) =>
        Util.Base64Url(Util.HmacSha256(key, Util.Utf8.GetBytes(operation + "\n" + digest + "\n" + expiry.ToString(CultureInfo.InvariantCulture))));

    /// <summary>A token valid only for the operation and the exact arguments, until its expiry.</summary>
    public static string Issue(byte[] key, string operation, JsonNode? args, long now)
    {
        long expiry = now + TtlMs;
        return Prefix + "." + expiry.ToString(CultureInfo.InvariantCulture) + "." + Signature(key, operation, ArgsDigest(args), expiry);
    }

    /// <summary>The expiry a token claims, when it has the token shape.</summary>
    internal static bool TryExpiry(string token, out long expiry, out string signature)
    {
        expiry = 0;
        signature = "";
        if (!token.StartsWith(Prefix + ".", StringComparison.Ordinal))
        {
            return false;
        }
        var rest = token[(Prefix.Length + 1)..];
        int dot = rest.IndexOf('.', StringComparison.Ordinal);
        if (dot < 0)
        {
            return false;
        }
        var e = rest[..dot];
        var sig = rest[(dot + 1)..];
        if (e.Length == 0 || e.Length > 16 || !e.All(char.IsAsciiDigit) || sig.Length != 43 ||
            !sig.All(c => char.IsAsciiLetterOrDigit(c) || c == '_' || c == '-'))
        {
            return false;
        }
        if (!long.TryParse(e, NumberStyles.None, CultureInfo.InvariantCulture, out expiry))
        {
            return false;
        }
        signature = sig;
        return true;
    }

    internal enum Check
    {
        Valid,
        Expired,
        Mismatch,
        Malformed,
    }

    internal static Check Verify(byte[] key, string token, string operation, JsonNode? args, long now)
    {
        if (!TryExpiry(token, out var expiry, out var given))
        {
            return Check.Malformed;
        }
        if (!Util.TimingSafeEqual(Signature(key, operation, ArgsDigest(args), expiry), given))
        {
            return Check.Mismatch;
        }
        return expiry > now ? Check.Valid : Check.Expired;
    }
}
