// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;

namespace Tungsten.Runtime;

/// <summary>Helpers shared by the runtime and generated SDKs.</summary>
public static class Util
{
    /// <summary>Written wherever a secret or sensitive value would appear.</summary>
    public const string Redacted = "<redacted>";

    internal const int MaxReceivedChars = 200;

    internal const int MaxArgDepth = 256;

    internal static readonly UTF8Encoding Utf8 = new(false, false);

    // ------------------------------------------------------------ crypto

    internal static string Sha256Hex(byte[] data) => Convert.ToHexString(SHA256.HashData(data)).ToLowerInvariant();

    internal static string Sha256Hex(string text) => Sha256Hex(Utf8.GetBytes(text));

    internal static byte[] HmacSha256(byte[] key, byte[] message) => HMACSHA256.HashData(key, message);

    internal static string Base64Url(byte[] data) => Convert.ToBase64String(data).TrimEnd('=').Replace('+', '-').Replace('/', '_');

    internal static bool TimingSafeEqual(string a, string b) =>
        a.Length == b.Length && CryptographicOperations.FixedTimeEquals(Encoding.ASCII.GetBytes(a), Encoding.ASCII.GetBytes(b));

    internal static byte[] RandomBytes(int n) => RandomNumberGenerator.GetBytes(n);

    /// <summary>A random UUID version 4 in canonical lowercase form.</summary>
    public static string UuidV4()
    {
        var b = RandomBytes(16);
        b[6] = (byte)((b[6] & 0x0f) | 0x40);
        b[8] = (byte)((b[8] & 0x3f) | 0x80);
        var h = Convert.ToHexString(b).ToLowerInvariant();
        return h[..8] + "-" + h[8..12] + "-" + h[12..16] + "-" + h[16..20] + "-" + h[20..];
    }

    // ------------------------------------------------------------ text

    /// <summary>The first <paramref name="max"/> UTF-16 code units, never splitting a surrogate pair.</summary>
    internal static string TakeUnits(string text, int max)
    {
        if (text.Length <= max)
        {
            return text;
        }
        int end = max;
        if (end > 0 && char.IsHighSurrogate(text[end - 1]) && end < text.Length && char.IsLowSurrogate(text[end]))
        {
            end--;
        }
        return text[..end];
    }

    internal static string Cut(string text)
    {
        if (text.Length > MaxReceivedChars)
        {
            return TakeUnits(text, MaxReceivedChars - 1) + "…";
        }
        return text;
    }

    /// <summary>Bytes as text: a leading BOM dropped, invalid UTF-8 replaced with U+FFFD.</summary>
    internal static string DecodeText(byte[] data)
    {
        int start = data.Length >= 3 && data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF ? 3 : 0;
        return Utf8.GetString(data, start, data.Length - start);
    }

    internal static bool IsIndex(string s)
    {
        if (s.Length == 0)
        {
            return false;
        }
        foreach (var c in s)
        {
            if (c < '0' || c > '9')
            {
                return false;
            }
        }
        return true;
    }

    internal static bool AllDigits(string s) => IsIndex(s);

    /// <summary>String(value) for a scalar, JSON text for anything else.</summary>
    internal static string Scalar(JsonNode? v)
    {
        switch (Json.KindOf(v))
        {
            case JsonKind.String:
                return Json.AsString(v) ?? "";
            case JsonKind.Integer:
            case JsonKind.Number:
                return Json.NumberText(v);
            case JsonKind.Boolean:
                Json.TryBoolean(v, out var b);
                return b ? "true" : "false";
            default:
                return Json.Text(v);
        }
    }

    /// <summary>JSON for remediation text, cut to 80 characters.</summary>
    internal static string ShortJson(JsonNode? v)
    {
        var text = Json.Text(v);
        var runes = text.EnumerateRunes().ToList();
        if (runes.Count > 80)
        {
            return string.Concat(runes.Take(77).Select(r => r.ToString())) + "...";
        }
        return text;
    }

    internal static string Plural(int n, string one, string many) => n == 1 ? one : many;

    internal static string Ms(TimeSpan t) => ((long)t.TotalMilliseconds).ToString(CultureInfo.InvariantCulture);

    // ------------------------------------------------------------ paths

    /// <summary>Splits a field path (a.b, items[0].id, items.0.id).</summary>
    internal static List<string> SplitPath(string? path)
    {
        var outList = new List<string>();
        if (string.IsNullOrEmpty(path) || path == ".")
        {
            return outList;
        }
        var dotted = new StringBuilder();
        for (int i = 0; i < path.Length;)
        {
            if (path[i] == '[')
            {
                int j = i + 1;
                while (j < path.Length && path[j] >= '0' && path[j] <= '9')
                {
                    j++;
                }
                if (j > i + 1 && j < path.Length && path[j] == ']')
                {
                    dotted.Append('.').Append(path, i + 1, j - i - 1);
                    i = j + 1;
                    continue;
                }
            }
            dotted.Append(path[i]);
            i++;
        }
        foreach (var s in dotted.ToString().Split('.'))
        {
            if (s.Length > 0)
            {
                outList.Add(s);
            }
        }
        return outList;
    }

    /// <summary>Reads a field path; false when absent.</summary>
    internal static bool GetPath(JsonNode? v, bool present, IEnumerable<string> segments, out JsonNode? found)
    {
        found = null;
        if (!present)
        {
            return false;
        }
        var cur = v;
        foreach (var seg in segments)
        {
            switch (cur)
            {
                case JsonArray a:
                    if (!IsIndex(seg) || !int.TryParse(seg, NumberStyles.None, CultureInfo.InvariantCulture, out int n) || n >= a.Count)
                    {
                        return false;
                    }
                    cur = a[n];
                    break;
                case JsonObject o:
                    if (!o.TryGetPropertyValue(seg, out var next))
                    {
                        return false;
                    }
                    cur = next;
                    break;
                default:
                    return false;
            }
        }
        found = cur;
        return true;
    }

    internal static bool GetPathStr(JsonNode? v, bool present, string path, out JsonNode? found) =>
        GetPath(v, present, SplitPath(path), out found);

    internal static bool NestsDeeperThan(JsonNode? v, int limit)
    {
        switch (v)
        {
            case JsonArray a:
                if (limit == 0) return true;
                return a.Any(item => NestsDeeperThan(item, limit - 1));
            case JsonObject o:
                if (limit == 0) return true;
                return o.Any(kv => NestsDeeperThan(kv.Value, limit - 1));
        }
        return false;
    }

    /// <summary>
    /// Every field of the pattern appears in the value with an equal (recursively contained)
    /// value; arrays match when each pattern element is contained by some element.
    /// </summary>
    internal static bool ContainsSubset(JsonNode? value, bool present, JsonNode? pattern)
    {
        switch (pattern)
        {
            case JsonObject p:
                if (!present || value is not JsonObject have)
                {
                    return false;
                }
                foreach (var kv in p)
                {
                    if (!have.TryGetPropertyValue(kv.Key, out var v) || !ContainsSubset(v, true, kv.Value))
                    {
                        return false;
                    }
                }
                return true;
            case JsonArray pa:
                if (!present || value is not JsonArray list)
                {
                    return false;
                }
                foreach (var want in pa)
                {
                    if (!list.Any(h => ContainsSubset(h, true, want)))
                    {
                        return false;
                    }
                }
                return true;
            default:
                return present && Json.Equal(value, pattern);
        }
    }

    // ------------------------------------------------------------ redaction

    private static readonly string[] SensitiveWords =
    {
        "secret", "password", "passwd", "passphrase", "token", "apikey", "api-key", "api_key",
        "privatekey", "private-key", "private_key", "credential", "authorization", "cookie", "session",
    };

    /// <summary>Whether a name very likely holds a secret.</summary>
    internal static bool LooksSensitive(string name)
    {
        var lower = name.ToLowerInvariant();
        return SensitiveWords.Any(w => lower.Contains(w, StringComparison.Ordinal));
    }

    internal static JsonNode? RedactSensitiveKeys(JsonNode? v, int depth)
    {
        if (depth > 32)
        {
            return null;
        }
        switch (v)
        {
            case JsonArray a:
                return Json.Array(a.Select(item => RedactSensitiveKeys(item, depth + 1)));
            case JsonObject o:
                {
                    var outObj = new JsonObject();
                    foreach (var kv in o)
                    {
                        outObj[kv.Key] = LooksSensitive(kv.Key) ? JsonValue.Create(Redacted) : RedactSensitiveKeys(kv.Value, depth + 1);
                    }
                    return outObj;
                }
        }
        return Json.Copy(v);
    }

    private static JsonNode? RedactAt(JsonNode? v, List<string> segments, int at)
    {
        if (at >= segments.Count)
        {
            return JsonValue.Create(Redacted);
        }
        switch (v)
        {
            case JsonArray a:
                return Json.Array(a.Select(item => RedactAt(item, segments, at)));
            case JsonObject o:
                {
                    var head = segments[at];
                    if (!o.ContainsKey(head))
                    {
                        return Json.Copy(o);
                    }
                    var outObj = new JsonObject();
                    foreach (var kv in o)
                    {
                        outObj[kv.Key] = kv.Key == head ? RedactAt(kv.Value, segments, at + 1) : Json.Copy(kv.Value);
                    }
                    return outObj;
                }
        }
        return Json.Copy(v);
    }

    /// <summary>A copy of the value with every listed field path redacted; arrays on the way fan out.</summary>
    internal static JsonNode? RedactPaths(JsonNode? v, IEnumerable<string> paths)
    {
        var outValue = Json.Copy(v);
        foreach (var p in paths)
        {
            var segs = SplitPath(p);
            if (segs.Count > 0)
            {
                outValue = RedactAt(outValue, segs, 0);
            }
        }
        return outValue;
    }

    /// <summary>Makes a value safe for received_value.</summary>
    internal static JsonNode? EnvelopeValue(JsonNode? v, bool sensitive)
    {
        if (sensitive)
        {
            return JsonValue.Create(Redacted);
        }
        switch (Json.KindOf(v))
        {
            case JsonKind.String:
                return JsonValue.Create(Cut(Json.AsString(v) ?? ""));
            case JsonKind.Array:
            case JsonKind.Object:
                {
                    if (Binary.TryFrom(v, out var bin))
                    {
                        return JsonValue.Create("<" + bin.Data.Length.ToString(CultureInfo.InvariantCulture) + " bytes>");
                    }
                    var text = Json.Text(RedactSensitiveKeys(v, 0));
                    if (text.Length > MaxReceivedChars)
                    {
                        return JsonValue.Create(Cut(text));
                    }
                    return Json.TryParse(text, out var parsed) ? parsed : null;
                }
        }
        return Json.Copy(v);
    }

    /// <summary>The Accept of a stream call: the caller's media ranges without repeats, text/event-stream first when they do not name it.</summary>
    internal static string MergeAccept(string value)
    {
        var ranges = new List<string>();
        foreach (var part in value.Split(','))
        {
            var r = part.Trim();
            if (r.Length > 0 && !ranges.Contains(r))
            {
                ranges.Add(r);
            }
        }
        bool names = ranges.Any(r => string.Equals(r.Split(';')[0].Trim(), "text/event-stream", StringComparison.OrdinalIgnoreCase));
        if (!names)
        {
            ranges.Insert(0, "text/event-stream");
        }
        return string.Join(", ", ranges);
    }

    /// <summary>A finite number within [min, max], else the fallback.</summary>
    internal static double Bounded(JsonNode? v, bool present, double fallback, double min, double max)
    {
        if (!present || !Json.TryNumber(v, out var f) || double.IsNaN(f) || double.IsInfinity(f))
        {
            return fallback;
        }
        return Math.Max(min, Math.Min(max, f));
    }

    /// <summary>A delay of <paramref name="ms"/> milliseconds; negative and NaN are zero, huge values capped.</summary>
    internal static TimeSpan DurationMs(double ms)
    {
        if (double.IsNaN(ms) || ms <= 0)
        {
            return TimeSpan.Zero;
        }
        return TimeSpan.FromMilliseconds(Math.Min(ms, uint.MaxValue * 1000.0));
    }
}

/// <summary>Secret values to scrub from texts, in insertion order.</summary>
internal sealed class SecretSet
{
    private readonly List<string> items = new();

    public void Insert(string? secret)
    {
        if (!string.IsNullOrEmpty(secret) && !items.Contains(secret))
        {
            items.Add(secret);
        }
    }

    public bool Empty => items.Count == 0;

    public SecretSet Clone()
    {
        var c = new SecretSet();
        c.items.AddRange(items);
        return c;
    }

    /// <summary>Replaces every occurrence of the secrets (four characters or more).</summary>
    public string Scrub(string text)
    {
        foreach (var secret in items)
        {
            if (secret.EnumerateRunes().Count() >= 4 && text.Contains(secret, StringComparison.Ordinal))
            {
                text = text.Replace(secret, Util.Redacted, StringComparison.Ordinal);
            }
        }
        return text;
    }

    public string? ScrubOpt(string? text) => text is null ? null : Scrub(text);

    public JsonNode? ScrubValue(JsonNode? v)
    {
        switch (Json.KindOf(v))
        {
            case JsonKind.String:
                return JsonValue.Create(Scrub(Json.AsString(v) ?? ""));
            case JsonKind.Array:
            case JsonKind.Object:
                {
                    var scrubbed = Scrub(Json.Text(v));
                    return Json.TryParse(scrubbed, out var parsed) ? parsed : JsonValue.Create(Util.Redacted);
                }
        }
        return Json.Copy(v);
    }

    /// <summary>The envelope with every secret replaced in its text members: a last line of defence.</summary>
    public Diagnostic Scrub(Diagnostic d)
    {
        if (Empty)
        {
            return d;
        }
        return d with
        {
            Code = ScrubOpt(d.Code),
            FailedParameter = ScrubOpt(d.FailedParameter),
            ReceivedValue = ScrubValue(d.ReceivedValue),
            Expected = ScrubOpt(d.Expected),
            Remediation = Scrub(d.Remediation),
            NextAction = ScrubOpt(d.NextAction),
            RequestId = ScrubOpt(d.RequestId),
        };
    }
}
