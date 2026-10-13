// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Tungsten.Runtime;

/// <summary>The JSON type of a value, as every tungsten runtime tells them apart.</summary>
public enum JsonKind
{
    Null,
    Boolean,
    String,
    /// <summary>A number without a fractional part, below 2^53 in magnitude (or an <c>Int64</c>).</summary>
    Integer,
    /// <summary>Any other finite number.</summary>
    Number,
    Array,
    Object,
}

/// <summary>A JSON text that does not parse.</summary>
public sealed class JsonParseException : Exception
{
    public JsonParseException(string message) : base(message) { }
}

/// <summary>
/// The JSON values of the runtime are <see cref="JsonNode"/> trees (<c>null</c> is JSON null).
/// Objects keep their member order. Integral numbers below 2^53 in magnitude are integers
/// (<see cref="long"/>), as the other runtimes read them; other numbers are doubles. Texts are
/// written as <c>JSON.stringify</c> writes them, so request bodies, digests and texts are the same
/// in every runtime.
/// </summary>
public static class Json
{
    /// <summary>2^53, the bound of the integers a JavaScript number holds.</summary>
    public const double MaxSafe = 9007199254740992.0;

    /// <summary>The largest integer a JavaScript number holds exactly.</summary>
    public const double MaxSafeInteger = 9007199254740991.0;

    private const int MaxParseDepth = 10000;

    // ------------------------------------------------------------ parsing

    /// <summary>Parses one JSON document; trailing data is an error.</summary>
    public static JsonNode? Parse(string text)
    {
        return Parse(Encoding.UTF8.GetBytes(text));
    }

    /// <summary>Parses one JSON document from UTF-8 bytes.</summary>
    public static JsonNode? Parse(ReadOnlySpan<byte> utf8)
    {
        var reader = new Utf8JsonReader(utf8, new JsonReaderOptions
        {
            CommentHandling = JsonCommentHandling.Disallow,
            AllowTrailingCommas = false,
            MaxDepth = MaxParseDepth,
        });
        try
        {
            var stack = new Stack<(JsonNode Container, string? Key)>();
            string? pendingKey = null;
            JsonNode? root = null;
            bool done = false;
            while (reader.Read())
            {
                if (done)
                {
                    throw new JsonParseException("trailing data after the JSON value");
                }
                JsonNode? value;
                switch (reader.TokenType)
                {
                    case JsonTokenType.StartObject:
                        stack.Push((new JsonObject(), pendingKey));
                        pendingKey = null;
                        continue;
                    case JsonTokenType.StartArray:
                        stack.Push((new JsonArray(), pendingKey));
                        pendingKey = null;
                        continue;
                    case JsonTokenType.PropertyName:
                        pendingKey = reader.GetString();
                        continue;
                    case JsonTokenType.EndObject:
                    case JsonTokenType.EndArray:
                        {
                            var (container, key) = stack.Pop();
                            pendingKey = key;
                            value = container;
                            break;
                        }
                    case JsonTokenType.String:
                        value = JsonValue.Create(reader.GetString() ?? "");
                        break;
                    case JsonTokenType.Number:
                        value = ParseNumber(ref reader);
                        break;
                    case JsonTokenType.True:
                        value = JsonValue.Create(true);
                        break;
                    case JsonTokenType.False:
                        value = JsonValue.Create(false);
                        break;
                    case JsonTokenType.Null:
                        value = null;
                        break;
                    default:
                        throw new JsonParseException("unexpected token " + reader.TokenType);
                }
                if (stack.Count == 0)
                {
                    root = value;
                    done = true;
                    continue;
                }
                var top = stack.Peek().Container;
                if (top is JsonObject obj)
                {
                    obj[pendingKey ?? ""] = value;
                    pendingKey = null;
                }
                else if (top is JsonArray arr)
                {
                    arr.Add(value);
                }
            }
            if (!done)
            {
                throw new JsonParseException("unexpected end of the JSON text");
            }
            return root;
        }
        catch (JsonException e)
        {
            throw new JsonParseException(e.Message);
        }
        catch (InvalidOperationException e)
        {
            throw new JsonParseException(e.Message);
        }
    }

    /// <summary>Parses JSON, or returns false.</summary>
    public static bool TryParse(string text, out JsonNode? value)
    {
        try
        {
            value = Parse(text);
            return true;
        }
        catch (JsonParseException)
        {
            value = null;
            return false;
        }
    }

    private static JsonNode ParseNumber(ref Utf8JsonReader reader)
    {
        var raw = reader.HasValueSequence ? System.Buffers.BuffersExtensions.ToArray(reader.ValueSequence) : reader.ValueSpan.ToArray();
        bool integral = System.Array.IndexOf(raw, (byte)'.') < 0 && System.Array.IndexOf(raw, (byte)'e') < 0 && System.Array.IndexOf(raw, (byte)'E') < 0;
        if (integral && reader.TryGetInt64(out long l))
        {
            return JsonValue.Create(l);
        }
        var text = Encoding.ASCII.GetString(raw);
        if (!double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out double d) || double.IsInfinity(d) || double.IsNaN(d))
        {
            throw new JsonParseException("number " + text + " out of range");
        }
        return Number(d);
    }

    // ------------------------------------------------------------ values

    /// <summary>A number: an integer when it is integral and below 2^53 in magnitude.</summary>
    public static JsonNode Number(double d)
    {
        if (double.IsNaN(d) || double.IsInfinity(d))
        {
            throw new ArgumentException("a non-finite number is not JSON");
        }
        if (d == Math.Truncate(d) && Math.Abs(d) < MaxSafe)
        {
            return JsonValue.Create((long)d);
        }
        return JsonValue.Create(d);
    }

    /// <summary>An integer.</summary>
    public static JsonNode Integer(long n) => JsonValue.Create(n);

    /// <summary>A string.</summary>
    public static JsonNode String(string s) => JsonValue.Create(s);

    /// <summary>A boolean.</summary>
    public static JsonNode Boolean(bool b) => JsonValue.Create(b);

    /// <summary>An object of the members, in order (null values are JSON null).</summary>
    public static JsonObject Object(params (string Key, JsonNode? Value)[] members)
    {
        var o = new JsonObject();
        foreach (var (k, v) in members)
        {
            Put(o, k, v);
        }
        return o;
    }

    /// <summary>An array of the items.</summary>
    public static JsonArray Array(IEnumerable<JsonNode?> items)
    {
        var a = new JsonArray();
        foreach (var item in items)
        {
            Add(a, item);
        }
        return a;
    }

    /// <summary>An array of strings.</summary>
    public static JsonArray Strings(IEnumerable<string> items) => Array(items.Select(s => (JsonNode?)String(s)));

    /// <summary>
    /// Sets a member (at the end, or in place when present). A value that already belongs to
    /// another tree is copied first.
    /// </summary>
    public static void Put(JsonObject o, string key, JsonNode? value)
    {
        o[key] = Own(value);
    }

    /// <summary>Appends an item; a value that already belongs to another tree is copied first.</summary>
    public static void Add(JsonArray a, JsonNode? value)
    {
        a.Add(Own(value));
    }

    private static JsonNode? Own(JsonNode? value) => value is null || value.Parent is null ? value : value.DeepClone();

    /// <summary>A deep copy.</summary>
    public static JsonNode? Copy(JsonNode? value) => value?.DeepClone();

    /// <summary>A deep copy of an object.</summary>
    public static JsonObject CopyObject(JsonObject o) => (JsonObject)o.DeepClone();

    /// <summary>The member names in order.</summary>
    public static List<string> Keys(JsonObject o) => o.Select(kv => kv.Key).ToList();

    /// <summary>A member and whether it is present.</summary>
    public static bool TryGet(JsonObject o, string key, out JsonNode? value) => o.TryGetPropertyValue(key, out value);

    /// <summary>The JSON type of a value.</summary>
    public static JsonKind KindOf(JsonNode? node)
    {
        switch (node)
        {
            case null:
                return JsonKind.Null;
            case JsonObject:
                return JsonKind.Object;
            case JsonArray:
                return JsonKind.Array;
            case JsonValue v:
                switch (v.GetValueKind())
                {
                    case JsonValueKind.String:
                        return JsonKind.String;
                    case JsonValueKind.True:
                    case JsonValueKind.False:
                        return JsonKind.Boolean;
                    case JsonValueKind.Number:
                        return TryInteger(node, out _) ? JsonKind.Integer : JsonKind.Number;
                    case JsonValueKind.Null:
                        return JsonKind.Null;
                    default:
                        return JsonKind.Null;
                }
            default:
                return JsonKind.Null;
        }
    }

    /// <summary>Whether the value is JSON null.</summary>
    public static bool IsNull(JsonNode? node) => KindOf(node) == JsonKind.Null;

    /// <summary>A string value.</summary>
    public static bool TryString(JsonNode? node, out string value)
    {
        if (node is JsonValue v && v.GetValueKind() == JsonValueKind.String && v.TryGetValue(out string? s) && s is not null)
        {
            value = s;
            return true;
        }
        if (node is JsonValue e && e.TryGetValue(out JsonElement el) && el.ValueKind == JsonValueKind.String)
        {
            value = el.GetString() ?? "";
            return true;
        }
        value = "";
        return false;
    }

    /// <summary>The string value, or null.</summary>
    public static string? AsString(JsonNode? node) => TryString(node, out var s) ? s : null;

    /// <summary>A boolean value.</summary>
    public static bool TryBoolean(JsonNode? node, out bool value)
    {
        if (node is JsonValue v)
        {
            var kind = v.GetValueKind();
            if (kind == JsonValueKind.True || kind == JsonValueKind.False)
            {
                value = kind == JsonValueKind.True;
                return true;
            }
        }
        value = false;
        return false;
    }

    /// <summary>A numeric value as a double.</summary>
    public static bool TryNumber(JsonNode? node, out double value)
    {
        value = 0;
        if (node is not JsonValue v || v.GetValueKind() != JsonValueKind.Number)
        {
            return false;
        }
        if (v.TryGetValue(out long l)) { value = l; return true; }
        if (v.TryGetValue(out double d)) { value = d; return true; }
        if (v.TryGetValue(out int i)) { value = i; return true; }
        if (v.TryGetValue(out decimal m)) { value = (double)m; return true; }
        if (v.TryGetValue(out float f)) { value = f; return true; }
        if (v.TryGetValue(out short s)) { value = s; return true; }
        if (v.TryGetValue(out uint ui)) { value = ui; return true; }
        if (v.TryGetValue(out ulong ul)) { value = ul; return true; }
        if (v.TryGetValue(out byte b)) { value = b; return true; }
        if (v.TryGetValue(out JsonElement e) && e.TryGetDouble(out double ed)) { value = ed; return true; }
        return false;
    }

    /// <summary>An integer value: an <c>Int64</c>, or an integral number below 2^53 in magnitude.</summary>
    public static bool TryInteger(JsonNode? node, out long value)
    {
        value = 0;
        if (node is not JsonValue v || v.GetValueKind() != JsonValueKind.Number)
        {
            return false;
        }
        if (v.TryGetValue(out long l)) { value = l; return true; }
        if (v.TryGetValue(out int i)) { value = i; return true; }
        if (v.TryGetValue(out short s)) { value = s; return true; }
        if (v.TryGetValue(out uint ui)) { value = ui; return true; }
        if (v.TryGetValue(out byte b)) { value = b; return true; }
        if (v.TryGetValue(out JsonElement e) && e.TryGetInt64(out long el))
        {
            var raw = e.GetRawText();
            if (raw.IndexOfAny(new[] { '.', 'e', 'E' }) < 0)
            {
                value = el;
                return true;
            }
        }
        if (TryNumber(node, out double d) && d == Math.Truncate(d) && Math.Abs(d) < MaxSafe)
        {
            value = (long)d;
            return true;
        }
        return false;
    }

    /// <summary>Normalizes a value: numbers to integers where integral, objects and arrays copied.</summary>
    public static JsonNode? Normalize(JsonNode? node)
    {
        switch (node)
        {
            case null:
                return null;
            case JsonObject o:
                {
                    var outObj = new JsonObject();
                    foreach (var kv in o)
                    {
                        outObj[kv.Key] = Normalize(kv.Value);
                    }
                    return outObj;
                }
            case JsonArray a:
                {
                    var outArr = new JsonArray();
                    foreach (var item in a)
                    {
                        outArr.Add(Normalize(item));
                    }
                    return outArr;
                }
            default:
                switch (KindOf(node))
                {
                    case JsonKind.Integer:
                        TryInteger(node, out long l);
                        return JsonValue.Create(l);
                    case JsonKind.Number:
                        TryNumber(node, out double d);
                        return JsonValue.Create(d);
                    case JsonKind.String:
                        return JsonValue.Create(AsString(node) ?? "");
                    case JsonKind.Boolean:
                        TryBoolean(node, out bool b);
                        return JsonValue.Create(b);
                    default:
                        return null;
                }
        }
    }

    // ------------------------------------------------------------ equality

    /// <summary>Structural equality: 1 equals 1.0, member order does not matter.</summary>
    public static bool Equal(JsonNode? a, JsonNode? b)
    {
        var ka = KindOf(a);
        var kb = KindOf(b);
        switch (ka)
        {
            case JsonKind.Null:
                return kb == JsonKind.Null;
            case JsonKind.Boolean:
                return kb == JsonKind.Boolean && TryBoolean(a, out var x) && TryBoolean(b, out var y) && x == y;
            case JsonKind.String:
                return kb == JsonKind.String && AsString(a) == AsString(b);
            case JsonKind.Integer:
            case JsonKind.Number:
                if (kb != JsonKind.Integer && kb != JsonKind.Number)
                {
                    return false;
                }
                if (ka == JsonKind.Integer && kb == JsonKind.Integer)
                {
                    TryInteger(a, out var ia);
                    TryInteger(b, out var ib);
                    return ia == ib;
                }
                TryNumber(a, out var da);
                TryNumber(b, out var db);
                return da == db;
            case JsonKind.Array:
                {
                    if (b is not JsonArray bb || a is not JsonArray aa || aa.Count != bb.Count)
                    {
                        return false;
                    }
                    for (int i = 0; i < aa.Count; i++)
                    {
                        if (!Equal(aa[i], bb[i]))
                        {
                            return false;
                        }
                    }
                    return true;
                }
            case JsonKind.Object:
                {
                    if (b is not JsonObject bo || a is not JsonObject ao || ao.Count != bo.Count)
                    {
                        return false;
                    }
                    foreach (var kv in ao)
                    {
                        if (!bo.TryGetPropertyValue(kv.Key, out var other) || !Equal(kv.Value, other))
                        {
                            return false;
                        }
                    }
                    return true;
                }
        }
        return false;
    }

    /// <summary><see cref="Equal"/> where either side may be absent.</summary>
    public static bool EqualOpt(JsonNode? a, bool aPresent, JsonNode? b, bool bPresent)
    {
        if (!aPresent || !bPresent)
        {
            return aPresent == bPresent;
        }
        return Equal(a, b);
    }

    // ------------------------------------------------------------ texts

    /// <summary><c>JSON.stringify(value)</c>: members in order, no whitespace, numbers as JavaScript writes them.</summary>
    public static string Text(JsonNode? value)
    {
        var b = new StringBuilder();
        Write(b, value, false);
        return b.ToString();
    }

    /// <summary>
    /// The canonical JSON every tungsten runtime digests (confirmation tokens, automatic
    /// idempotency keys, content hashes): members sorted by the UTF-16 code units of their names,
    /// no whitespace, numbers as JavaScript writes them, binary values as <c>{"$bytes": base64}</c>
    /// (or <c>{"$file": {"bytes", "name", "type"}}</c> with a name or type).
    /// </summary>
    public static string Canonical(JsonNode? value)
    {
        var b = new StringBuilder();
        Write(b, value, true);
        return b.ToString();
    }

    /// <summary>A string literal as <c>JSON.stringify</c> writes it.</summary>
    public static string Quote(string text)
    {
        var b = new StringBuilder();
        WriteString(b, text);
        return b.ToString();
    }

    private const string Hex = "0123456789abcdef";

    private static void WriteString(StringBuilder b, string text)
    {
        b.Append('"');
        for (int i = 0; i < text.Length; i++)
        {
            char c = text[i];
            switch (c)
            {
                case '"': b.Append("\\\""); break;
                case '\\': b.Append("\\\\"); break;
                case '\b': b.Append("\\b"); break;
                case '\f': b.Append("\\f"); break;
                case '\n': b.Append("\\n"); break;
                case '\r': b.Append("\\r"); break;
                case '\t': b.Append("\\t"); break;
                default:
                    if (c < 0x20)
                    {
                        b.Append("\\u00").Append(Hex[c >> 4]).Append(Hex[c & 15]);
                    }
                    else if (char.IsHighSurrogate(c) && i + 1 < text.Length && char.IsLowSurrogate(text[i + 1]))
                    {
                        b.Append(c).Append(text[i + 1]);
                        i++;
                    }
                    else if (char.IsSurrogate(c))
                    {
                        // A lone surrogate, as well-formed JSON.stringify writes it.
                        b.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                    }
                    else
                    {
                        b.Append(c);
                    }
                    break;
            }
        }
        b.Append('"');
    }

    /// <summary>
    /// <c>String(value)</c> for a JavaScript number (ECMAScript Number::toString): 1.0 is "1",
    /// 1e-7 is "1e-7", 1e21 is "1e+21".
    /// </summary>
    public static string JsNumber(double value)
    {
        if (double.IsNaN(value)) return "NaN";
        if (double.IsPositiveInfinity(value)) return "Infinity";
        if (double.IsNegativeInfinity(value)) return "-Infinity";
        if (value == 0) return "0";
        string sign = "";
        if (value < 0)
        {
            sign = "-";
            value = -value;
        }
        // The shortest round-trip digits.
        string r = value.ToString("R", CultureInfo.InvariantCulture);
        string mantissa;
        int exponent;
        int e = r.IndexOfAny(new[] { 'E', 'e' });
        if (e >= 0)
        {
            mantissa = r.Substring(0, e);
            exponent = int.Parse(r.Substring(e + 1), NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture);
        }
        else
        {
            mantissa = r;
            exponent = 0;
        }
        // Normalize the mantissa to digits and a decimal exponent n (value = 0.d1d2... * 10^n).
        int dot = mantissa.IndexOf('.');
        string intPart = dot >= 0 ? mantissa.Substring(0, dot) : mantissa;
        string frac = dot >= 0 ? mantissa.Substring(dot + 1) : "";
        string all = (intPart + frac).TrimStart('0');
        int leadingZeros = (intPart + frac).Length - all.Length;
        int n = intPart.Length + exponent - leadingZeros;
        string digits = all.TrimEnd('0');
        if (digits.Length == 0)
        {
            return "0";
        }
        int k = digits.Length;
        string body;
        if (k <= n && n <= 21)
        {
            body = digits + new string('0', n - k);
        }
        else if (0 < n && n <= 21)
        {
            body = digits.Substring(0, n) + "." + digits.Substring(n);
        }
        else if (-6 < n && n <= 0)
        {
            body = "0." + new string('0', -n) + digits;
        }
        else
        {
            int ex = n - 1;
            string mark = ex < 0 ? "-" : "+";
            if (ex < 0) ex = -ex;
            string head = k > 1 ? digits.Substring(0, 1) + "." + digits.Substring(1) : digits;
            body = head + "e" + mark + ex.ToString(CultureInfo.InvariantCulture);
        }
        return sign + body;
    }

    /// <summary>A JSON number as JavaScript writes it.</summary>
    public static string NumberText(JsonNode? node)
    {
        if (node is JsonValue v && v.GetValueKind() == JsonValueKind.Number && v.TryGetValue(out long l))
        {
            return l.ToString(CultureInfo.InvariantCulture);
        }
        if (TryNumber(node, out double d))
        {
            return double.IsNaN(d) || double.IsInfinity(d) ? "null" : JsNumber(d);
        }
        return "null";
    }

    private static int CompareUtf16(string a, string b) => string.CompareOrdinal(a, b);

    private static void Write(StringBuilder b, JsonNode? value, bool canonical)
    {
        switch (KindOf(value))
        {
            case JsonKind.Null:
                b.Append("null");
                return;
            case JsonKind.Boolean:
                TryBoolean(value, out bool flag);
                b.Append(flag ? "true" : "false");
                return;
            case JsonKind.Integer:
            case JsonKind.Number:
                b.Append(NumberText(value));
                return;
            case JsonKind.String:
                WriteString(b, AsString(value) ?? "");
                return;
            case JsonKind.Array:
                {
                    var a = (JsonArray)value!;
                    b.Append('[');
                    for (int i = 0; i < a.Count; i++)
                    {
                        if (i > 0) b.Append(',');
                        Write(b, a[i], canonical);
                    }
                    b.Append(']');
                    return;
                }
            case JsonKind.Object:
                {
                    var o = (JsonObject)value!;
                    if (canonical && Binary.TryFrom(o, out var bin))
                    {
                        WriteBinary(b, bin);
                        return;
                    }
                    var keys = Keys(o);
                    if (canonical)
                    {
                        keys.Sort(CompareUtf16);
                    }
                    b.Append('{');
                    bool first = true;
                    foreach (var k in keys)
                    {
                        if (!first) b.Append(',');
                        first = false;
                        WriteString(b, k);
                        b.Append(':');
                        Write(b, o[k], canonical);
                    }
                    b.Append('}');
                    return;
                }
        }
    }

    private static void WriteBinary(StringBuilder b, Binary bin)
    {
        var encoded = Convert.ToBase64String(bin.Data);
        if (bin.Filename is null && bin.ContentType is null)
        {
            b.Append("{\"$bytes\":");
            WriteString(b, encoded);
            b.Append('}');
            return;
        }
        b.Append("{\"$file\":{\"bytes\":");
        WriteString(b, encoded);
        b.Append(",\"name\":");
        if (bin.Filename is not null) WriteString(b, bin.Filename); else b.Append("null");
        b.Append(",\"type\":");
        if (bin.ContentType is not null) WriteString(b, bin.ContentType); else b.Append("null");
        b.Append("}}");
    }
}
