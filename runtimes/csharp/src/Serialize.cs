// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Text.Json.Nodes;

namespace Tungsten.Runtime;

/// <summary>A value that does not serialize; becomes VALIDATION_FAILED.</summary>
internal sealed class SerializationError : Exception
{
    public SerializationError(string parameter, string expected, JsonNode? value) : base(expected)
    {
        Parameter = parameter;
        ExpectedText = expected;
        Value = value;
    }

    public string Parameter { get; }
    public string ExpectedText { get; }
    public JsonNode? Value { get; }
}

internal enum PartKind
{
    Text,
    Json,
    File,
}

internal sealed record MultipartPart(string Name, PartKind Kind, string Text, byte[] Data, string Filename, string ContentType);

/// <summary>What goes on the wire.</summary>
internal sealed record Payload
{
    public static readonly Payload EmptyPayload = new() { Empty = true };

    public bool Empty { get; init; }
    public byte[] Bytes { get; init; } = System.Array.Empty<byte>();
    public IReadOnlyList<MultipartPart>? Multipart { get; init; }
}

internal sealed record EncodedBody
{
    public Payload Payload { get; init; } = Payload.EmptyPayload;
    public string? ContentType { get; init; }
    public JsonNode? Display { get; init; }
    public byte[]? Hashed { get; init; }
}

internal sealed record HeaderEntry(string Name, string Value, bool Secret);

/// <summary>A case-insensitive header collection that remembers which values are secrets.</summary>
internal sealed class HeaderBag
{
    public List<HeaderEntry> Entries { get; } = new();

    public void Set(string name, string value, bool secret)
    {
        for (int i = 0; i < Entries.Count; i++)
        {
            if (string.Equals(Entries[i].Name, name, StringComparison.OrdinalIgnoreCase))
            {
                Entries[i] = new HeaderEntry(name, value, secret);
                return;
            }
        }
        Entries.Add(new HeaderEntry(name, value, secret));
    }

    public string? Get(string name) =>
        Entries.FirstOrDefault(e => string.Equals(e.Name, name, StringComparison.OrdinalIgnoreCase))?.Value;

    public Dictionary<string, string> ToMap(bool redact)
    {
        var outMap = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var e in Entries)
        {
            outMap[e.Name] = redact && e.Secret ? Util.Redacted : e.Value;
        }
        return outMap;
    }

    public IEnumerable<string> Secrets => Entries.Where(e => e.Secret).Select(e => e.Value);
}

internal static class Serialize
{
    private static bool IsUnreserved(byte b, string extra) =>
        (b >= 'a' && b <= 'z') || (b >= 'A' && b <= 'Z') || (b >= '0' && b <= '9') || extra.IndexOf((char)b) >= 0;

    private static string Encode(string text, string unreserved, bool spaceAsPlus)
    {
        var b = new StringBuilder();
        foreach (var c in Util.Utf8.GetBytes(text))
        {
            if (IsUnreserved(c, unreserved))
            {
                b.Append((char)c);
            }
            else if (spaceAsPlus && c == ' ')
            {
                b.Append('+');
            }
            else
            {
                b.Append('%').Append(c.ToString("X2", CultureInfo.InvariantCulture));
            }
        }
        return b.ToString();
    }

    /// <summary>encodeURIComponent.</summary>
    internal static string Component(string text) => Encode(text, "-_.!~*'()", false);

    /// <summary>One component of a form body (URLSearchParams).</summary>
    internal static string FormComponent(string text) => Encode(text, "*-._", true);

    /// <summary>RFC 3986 percent-encoding.</summary>
    internal static string PercentEncode(string text) => Encode(text, "-._~", false);

    private static List<string> EncodedItems(JsonArray items) =>
        items.Where(v => !Json.IsNull(v)).Select(v => Component(Util.Scalar(v))).ToList();

    private static List<(string K, string V)> EncodedPairs(JsonObject o) =>
        o.Where(kv => !Json.IsNull(kv.Value)).Select(kv => (Component(kv.Key), Component(Util.Scalar(kv.Value)))).ToList();

    private static List<string> FlatPairs(List<(string K, string V)> pairs) => pairs.SelectMany(p => new[] { p.K, p.V }).ToList();

    private static List<string> JoinedPairs(List<(string K, string V)> pairs) => pairs.Select(p => p.K + "=" + p.V).ToList();

    /// <summary>One path parameter value, percent-encoded.</summary>
    internal static string PathParam(ParamDescriptor p, JsonNode? v)
    {
        var name = Component(p.Wire);
        var style = p.Style is "label" or "matrix" ? p.Style : "simple";
        switch (v)
        {
            case JsonArray a:
                {
                    var items = EncodedItems(a);
                    if (style == "label") return "." + string.Join(p.Explode ? "." : ",", items);
                    if (style == "matrix" && p.Explode) return string.Concat(items.Select(i => ";" + name + "=" + i));
                    if (style == "matrix") return ";" + name + "=" + string.Join(",", items);
                    return string.Join(",", items);
                }
            case JsonObject o:
                {
                    var pairs = EncodedPairs(o);
                    var exploded = JoinedPairs(pairs);
                    var flat = FlatPairs(pairs);
                    if (style == "label" && p.Explode) return "." + string.Join(".", exploded);
                    if (style == "label") return "." + string.Join(",", flat);
                    if (style == "matrix" && p.Explode) return string.Concat(exploded.Select(e => ";" + e));
                    if (style == "matrix") return ";" + name + "=" + string.Join(",", flat);
                    if (p.Explode) return string.Join(",", exploded);
                    return string.Join(",", flat);
                }
        }
        var val = Component(Util.Scalar(v));
        return style switch
        {
            "label" => "." + val,
            "matrix" => ";" + name + "=" + val,
            _ => val,
        };
    }

    private static void DeepObjectPairs(string prefix, JsonNode? v, List<string> outList, int depth)
    {
        if (depth > 16 || Json.IsNull(v))
        {
            return;
        }
        switch (v)
        {
            case JsonArray a:
                foreach (var item in a)
                {
                    DeepObjectPairs(prefix, item, outList, depth + 1);
                }
                break;
            case JsonObject o:
                foreach (var kv in o)
                {
                    if (!Json.IsNull(kv.Value))
                    {
                        DeepObjectPairs(prefix + "[" + Component(kv.Key) + "]", kv.Value, outList, depth + 1);
                    }
                }
                break;
            default:
                outList.Add(prefix + "=" + Component(Util.Scalar(v)));
                break;
        }
    }

    /// <summary>The query parts (name=value, encoded) of one parameter.</summary>
    internal static List<string> QueryParam(ParamDescriptor p, JsonNode? v)
    {
        var outList = new List<string>();
        if (Json.IsNull(v))
        {
            return outList;
        }
        var name = Component(p.Wire);
        var delimiter = p.Style switch
        {
            "space_delimited" => "%20",
            "pipe_delimited" => "|",
            _ => ",",
        };
        if (v is JsonObject deep && p.Style == "deep_object")
        {
            DeepObjectPairs(name, deep, outList, 0);
            return outList;
        }
        switch (v)
        {
            case JsonArray a:
                {
                    var items = EncodedItems(a);
                    if (items.Count == 0)
                    {
                        return outList;
                    }
                    if (p.Explode || p.Style == "deep_object")
                    {
                        outList.AddRange(items.Select(i => name + "=" + i));
                        return outList;
                    }
                    outList.Add(name + "=" + string.Join(delimiter, items));
                    return outList;
                }
            case JsonObject o:
                {
                    var pairs = EncodedPairs(o);
                    if (p.Explode)
                    {
                        outList.AddRange(JoinedPairs(pairs));
                        return outList;
                    }
                    outList.Add(name + "=" + string.Join(delimiter, FlatPairs(pairs)));
                    return outList;
                }
        }
        outList.Add(name + "=" + Component(Util.Scalar(v)));
        return outList;
    }

    /// <summary>A header parameter (simple style, not encoded).</summary>
    internal static string HeaderParam(ParamDescriptor p, JsonNode? v)
    {
        switch (v)
        {
            case JsonArray a:
                return string.Join(",", a.Where(i => !Json.IsNull(i)).Select(Util.Scalar));
            case JsonObject o:
                {
                    var parts = new List<string>();
                    foreach (var kv in o)
                    {
                        if (Json.IsNull(kv.Value))
                        {
                            continue;
                        }
                        if (p.Explode)
                        {
                            parts.Add(kv.Key + "=" + Util.Scalar(kv.Value));
                        }
                        else
                        {
                            parts.Add(kv.Key);
                            parts.Add(Util.Scalar(kv.Value));
                        }
                    }
                    return string.Join(",", parts);
                }
        }
        return Util.Scalar(v);
    }

    /// <summary>Whether <paramref name="c"/> is an RFC 6265 cookie-octet.</summary>
    private static bool CookieOctet(int c) =>
        c == 0x21 || (c >= 0x23 && c <= 0x2B) || (c >= 0x2D && c <= 0x3A) || (c >= 0x3C && c <= 0x5B) || (c >= 0x5D && c <= 0x7E);

    /// <summary>A cookie style value as written; only characters a cookie cannot carry are percent-encoded.</summary>
    private static string CookieText(string text)
    {
        var sb = new System.Text.StringBuilder();
        for (var i = 0; i < text.Length; i += char.IsSurrogatePair(text, i) ? 2 : 1)
        {
            var c = char.ConvertToUtf32(text, i);
            if (CookieOctet(c)) sb.Append((char)c);
            else sb.Append(Component(char.ConvertFromUtf32(c)));
        }
        return sb.ToString();
    }

    /// <summary>OpenAPI 3.2 cookie style: n=v, n=v1,v2 or exploded n=v1; n=v2 and k=v; k2=v2.</summary>
    private static string CookieStyle(ParamDescriptor p, JsonNode? v)
    {
        switch (v)
        {
            case JsonArray a:
                {
                    var items = a.Where(x => !Json.IsNull(x)).Select(x => CookieText(Util.Scalar(x))).ToList();
                    if (p.Explode && items.Count > 0) return string.Join("; ", items.Select(i => p.Wire + "=" + i));
                    return p.Wire + "=" + string.Join(",", items);
                }
            case JsonObject o:
                {
                    var pairs = o.Where(kv => !Json.IsNull(kv.Value)).Select(kv => (K: CookieText(kv.Key), V: CookieText(Util.Scalar(kv.Value)))).ToList();
                    if (p.Explode && pairs.Count > 0) return string.Join("; ", JoinedPairs(pairs));
                    return p.Wire + "=" + string.Join(",", FlatPairs(pairs));
                }
        }
        return p.Wire + "=" + CookieText(Util.Scalar(v));
    }

    /// <summary>name=value for a cookie parameter (form style, values encoded; or cookie style).</summary>
    internal static string CookieParam(ParamDescriptor p, JsonNode? v) => p.Style == "cookie" ? CookieStyle(p, v) : v switch
    {
        JsonArray a => p.Wire + "=" + string.Join(",", EncodedItems(a)),
        JsonObject o => p.Wire + "=" + string.Join(",", FlatPairs(EncodedPairs(o))),
        _ => p.Wire + "=" + Component(Util.Scalar(v)),
    };

    // ------------------------------------------------------------ bodies

    /// <summary>
    /// The body before encoding: merged fields picked from the arguments (under their wire names),
    /// or the whole-body argument; wrapped in the rpc envelope when the operation has one.
    /// </summary>
    internal static (JsonNode? Value, bool Has) BodyValue(OperationDescriptor op, JsonObject args, ISet<string> paramKeys)
    {
        JsonNode? value = null;
        bool has = false;
        if (op.Body is { } b)
        {
            if (b.Arg is not null)
            {
                has = args.TryGetPropertyValue(b.Arg, out value);
            }
            else
            {
                var picked = new JsonObject();
                foreach (var f in b.Fields)
                {
                    if (args.TryGetPropertyValue(f.Arg, out var v))
                    {
                        Json.Put(picked, f.Wire, v);
                    }
                }
                if (picked.Count > 0 || b.Required || op.Rpc is not null)
                {
                    value = picked;
                    has = true;
                }
            }
        }
        var rpc = op.Rpc;
        if (rpc is null)
        {
            return (value, has);
        }
        JsonNode? parameters;
        if (op.Body is null)
        {
            var obj = new JsonObject();
            foreach (var kv in args)
            {
                if (!paramKeys.Contains(kv.Key))
                {
                    Json.Put(obj, kv.Key, kv.Value);
                }
            }
            parameters = obj;
        }
        else
        {
            parameters = has ? value : new JsonObject();
        }
        var envelope = new JsonObject();
        if (rpc.Constants is not null)
        {
            foreach (var kv in rpc.Constants)
            {
                Json.Put(envelope, kv.Key, kv.Value);
            }
        }
        envelope[rpc.Field] = rpc.Value;
        Json.Put(envelope, rpc.ParamsField, parameters);
        return (envelope, true);
    }

    private static List<(string K, string V)> FormPairs(JsonNode? v, string parameter)
    {
        if (v is not JsonObject o)
        {
            throw new SerializationError(parameter, "an object of form fields", v);
        }
        var outList = new List<(string, string)>();
        foreach (var kv in o)
        {
            if (Json.IsNull(kv.Value))
            {
                continue;
            }
            if (kv.Value is JsonArray list)
            {
                foreach (var x in list)
                {
                    if (!Json.IsNull(x))
                    {
                        outList.Add((kv.Key, Util.Scalar(x)));
                    }
                }
            }
            else
            {
                outList.Add((kv.Key, Util.Scalar(kv.Value)));
            }
        }
        return outList;
    }

    private static string BinaryDisplay(int n, string? media) =>
        string.IsNullOrEmpty(media) ? $"<{n} bytes>" : $"<{n} bytes of {media}>";

    /// <summary>Encodes the value per the body descriptor.</summary>
    internal static EncodedBody Body(BodyDescriptor? d, JsonNode? v, bool has, Func<JsonNode?, JsonNode?> redact)
    {
        if (!has)
        {
            return new EncodedBody();
        }
        var encoding = d?.Encoding ?? "json";
        var media = d?.MediaType ?? "application/json";
        const string parameter = "body";
        switch (encoding)
        {
            case "form":
                {
                    var pairs = FormPairs(v, parameter);
                    var raw = new JsonObject();
                    foreach (var (k, val) in pairs)
                    {
                        raw[k] = val;
                    }
                    var text = string.Join("&", pairs.Select(p => FormComponent(p.K) + "=" + FormComponent(p.V)));
                    var bytes = Util.Utf8.GetBytes(text);
                    return new EncodedBody { Payload = new Payload { Bytes = bytes }, ContentType = media, Display = redact(raw), Hashed = bytes };
                }
            case "multipart":
                {
                    if (v is not JsonObject o)
                    {
                        throw new SerializationError(parameter, "an object of multipart fields", v);
                    }
                    var parts = new List<MultipartPart>();
                    var display = new JsonObject();
                    foreach (var kv in o)
                    {
                        if (Json.IsNull(kv.Value))
                        {
                            continue;
                        }
                        bool isArray = kv.Value is JsonArray;
                        var list = kv.Value is JsonArray arr ? arr.Where(x => !Json.IsNull(x)).ToList() : new List<JsonNode?> { kv.Value };
                        var shown = new JsonArray();
                        string? declared = d is not null && d.PartContentTypes.TryGetValue(kv.Key, out var dt) ? dt : null;
                        bool declaredJson = declared is not null
                            && Envelope.IsJsonMedia(declared.Split(';', 2)[0].Trim().ToLowerInvariant());
                        foreach (var x in list)
                        {
                            if (Binary.TryFrom(x, out var bin))
                            {
                                if (bin.ContentType is not null && !ValidMediaType(bin.ContentType))
                                {
                                    throw new SerializationError(parameter, "multipart file content types that are valid media types", v);
                                }

                                var own = bin.ContentType ?? declared;
                                shown.Add((JsonNode?)JsonValue.Create(BinaryDisplay(bin.Data.Length, own)));
                                parts.Add(new MultipartPart(kv.Key, PartKind.File, "", bin.Data, bin.Filename ?? kv.Key, own ?? "application/octet-stream"));
                            }
                            else if (x is JsonObject)
                            {
                                parts.Add(new MultipartPart(kv.Key, PartKind.Json, Json.Text(x), System.Array.Empty<byte>(), "", declaredJson ? declared! : "application/json"));
                                Json.Add(shown, x);
                            }
                            else if (declared is not null)
                            {
                                parts.Add(new MultipartPart(kv.Key, PartKind.Json, declaredJson ? Json.Text(x) : Util.Scalar(x), System.Array.Empty<byte>(), "", declared));
                                shown.Add((JsonNode?)JsonValue.Create(Util.Scalar(x)));
                            }
                            else
                            {
                                parts.Add(new MultipartPart(kv.Key, PartKind.Text, Util.Scalar(x), System.Array.Empty<byte>(), "", ""));
                                shown.Add((JsonNode?)JsonValue.Create(Util.Scalar(x)));
                            }
                        }
                        if (isArray)
                        {
                            display[kv.Key] = shown;
                        }
                        else if (shown.Count > 0)
                        {
                            display[kv.Key] = Json.Copy(shown[0]);
                        }
                        else
                        {
                            display[kv.Key] = null;
                        }
                    }
                    return new EncodedBody
                    {
                        Payload = new Payload { Multipart = parts },
                        Display = redact(display),
                        Hashed = Util.Utf8.GetBytes(Json.Canonical(v)),
                    };
                }
            case "bytes":
                {
                    byte[] data;
                    if (Json.TryString(v, out var text))
                    {
                        data = Util.Utf8.GetBytes(text);
                    }
                    else if (Binary.TryFrom(v, out var bin))
                    {
                        data = bin.Data;
                    }
                    else
                    {
                        throw new SerializationError(parameter, "binary data (a Binary value)", v);
                    }
                    return new EncodedBody { Payload = new Payload { Bytes = data }, ContentType = media, Display = BinaryDisplay(data.Length, media), Hashed = data };
                }
            case "text":
                {
                    var kind = Json.KindOf(v);
                    if (kind is not (JsonKind.String or JsonKind.Integer or JsonKind.Number or JsonKind.Boolean))
                    {
                        throw new SerializationError(parameter, "a string", v);
                    }
                    var text = Util.Scalar(v);
                    var bytes = Util.Utf8.GetBytes(text);
                    return new EncodedBody { Payload = new Payload { Bytes = bytes }, ContentType = media, Display = text, Hashed = bytes };
                }
        }
        var jsonText = Json.Text(v);
        if (!Json.TryParse(jsonText, out var parsed))
        {
            throw new SerializationError(parameter, "a JSON-serializable value", v);
        }
        return new EncodedBody
        {
            Payload = new Payload { Bytes = Util.Utf8.GetBytes(jsonText) },
            ContentType = media,
            Display = redact(parsed),
            Hashed = Util.Utf8.GetBytes(Json.Canonical(parsed)),
        };
    }

    /// <summary>An RFC 9110 token.</summary>
    internal static bool ValidHeaderName(string name) =>
        name.Length > 0 && name.All(c => c < 128 && IsUnreserved((byte)c, "!#$%&'*+-.^_`|~"));

    /// <summary>`type/subtype` tokens, optionally followed by `;` parameters, in visible ASCII, space and tab.</summary>
    internal static bool ValidMediaType(string value)
    {
        if (!value.All(c => c == '\t' || (c >= 0x20 && c <= 0x7e)))
        {
            return false;
        }

        var semi = value.IndexOf(';');
        var head = (semi < 0 ? value : value[..semi]).Trim();
        var slash = head.IndexOf('/');
        return slash > 0 && ValidHeaderName(head[..slash]) && ValidHeaderName(head[(slash + 1)..]);
    }

    /// <summary>Visible ASCII, tab, space and Latin-1 characters.</summary>
    internal static bool ValidHeaderValue(string value) =>
        value.All(c => c == '\t' || (c >= 0x20 && c <= 0x7e) || (c >= 0x80 && c <= 0xff));
}
