// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace Tungsten.Runtime;

/// <summary>One validation problem: the path of the offending value (member names and array indexes) and a message.</summary>
public sealed record Issue(IReadOnlyList<object> Path, string Message);

/// <summary>A schema <c>pattern</c> that was not checked at <paramref name="Path"/>, and why; the value was accepted.</summary>
internal sealed record PatternNote(IReadOnlyList<object> Path, string Pattern, string Reason);

/// <summary>
/// One schema of the runtime schema form (the descriptor document's <c>request</c>, <c>response</c>,
/// <c>page_item</c> and stream <c>event</c> schemas, and its <c>defs</c>): the validator of arguments,
/// bodies, page items and events. The rules are those of the descriptor document v1, the same in
/// every runtime.
/// </summary>
public sealed class Schema
{
    public string Kind { get; init; } = "any";
    public string? Format { get; init; }
    public string? Pattern { get; init; }
    public int? Bits { get; init; }
    public long? MinLength { get; init; }
    public long? MaxLength { get; init; }
    public double? Minimum { get; init; }
    public double? Maximum { get; init; }
    public double? ExclusiveMinimum { get; init; }
    public double? ExclusiveMaximum { get; init; }
    public double? MultipleOf { get; init; }
    public IReadOnlyList<JsonNode?>? Values { get; init; }
    public Schema? Items { get; init; }
    public long? MinItems { get; init; }
    public long? MaxItems { get; init; }
    public bool Unique { get; init; }
    public IReadOnlyList<SchemaField>? Fields { get; init; }
    /// <summary><c>open</c>, <c>closed</c> or <c>schema</c> (checked against <see cref="Extra"/>).</summary>
    public string? Additional { get; init; }
    public Schema? Extra { get; init; }
    public IReadOnlyList<Schema>? Variants { get; init; }
    public string? Tag { get; init; }
    public IReadOnlyList<string>? Tags { get; init; }
    public Schema? Inner { get; init; }
    /// <summary>The type id a <c>ref</c> names.</summary>
    public string? Ref { get; init; }

    internal IReadOnlyDictionary<string, Schema>? Defs { get; set; }

    /// <summary>The issues of a value: structure first, then constraint violations, then unknown members.</summary>
    public IReadOnlyList<Issue> Validate(JsonNode? value) => Validate(value, null);

    /// <summary>As <see cref="Validate(JsonNode?)"/>; the patterns that could not be checked are added to <paramref name="notes"/>.</summary>
    internal IReadOnlyList<Issue> Validate(JsonNode? value, List<PatternNote>? notes)
    {
        var v = new Validation();
        v.Check(this, value, new List<object>(), 0);
        notes?.AddRange(v.Notes);
        return v.Structure.Concat(v.Constraints).Concat(v.Unknown).ToList();
    }

    /// <summary>Whether the value is valid.</summary>
    public bool Accepts(JsonNode? value) => Validate(value).Count == 0;

    // ------------------------------------------------------------ parsing

    /// <summary>Reads a schema of the runtime schema form; <paramref name="defs"/> resolves <c>ref</c>s.</summary>
    public static Schema FromJson(JsonNode? node, IReadOnlyDictionary<string, Schema> defs)
    {
        var s = Read(node, defs);
        return s;
    }

    /// <summary>Reads the <c>defs</c> of a document.</summary>
    public static IReadOnlyDictionary<string, Schema> ReadDefs(JsonNode? node)
    {
        var defs = new Dictionary<string, Schema>(StringComparer.Ordinal);
        if (node is JsonObject o)
        {
            foreach (var kv in o)
            {
                defs[kv.Key] = Read(kv.Value, defs);
            }
        }
        return defs;
    }

    private static Schema Read(JsonNode? node, IReadOnlyDictionary<string, Schema> defs)
    {
        if (node is not JsonObject o)
        {
            return new Schema { Kind = "any", Defs = defs };
        }
        string kind = Json.AsString(o["kind"]) ?? "any";
        double? Num(string k) => Json.TryNumber(o[k], out var d) ? d : null;
        long? Int(string k) => Json.TryInteger(o[k], out var l) ? l : null;
        List<SchemaField>? fields = null;
        if (o["fields"] is JsonArray fa)
        {
            fields = new List<SchemaField>();
            foreach (var f in fa)
            {
                if (f is not JsonObject fo)
                {
                    continue;
                }
                string name = Json.AsString(fo["name"]) ?? "";
                fields.Add(new SchemaField
                {
                    Name = Json.AsString(fo["key"]) ?? name,
                    Wire = Json.AsString(fo["wire"]) ?? name,
                    Schema = Read(fo["schema"], defs),
                    Required = Json.TryBoolean(fo["required"], out var r) && r,
                    Nullable = Json.TryBoolean(fo["nullable"], out var n) && n,
                });
            }
        }
        return new Schema
        {
            Kind = kind,
            Format = Json.AsString(o["format"]),
            Pattern = Json.AsString(o["pattern"]),
            Bits = Int("bits") is long b ? (int)b : null,
            MinLength = Int("min_length"),
            MaxLength = Int("max_length"),
            Minimum = Num("minimum"),
            Maximum = Num("maximum"),
            ExclusiveMinimum = Num("exclusive_minimum"),
            ExclusiveMaximum = Num("exclusive_maximum"),
            MultipleOf = Num("multiple_of"),
            Values = o["values"] is JsonArray va ? va.Select(Json.Copy).ToList() : null,
            Items = o.ContainsKey("items") ? Read(o["items"], defs) : null,
            MinItems = Int("min_items"),
            MaxItems = Int("max_items"),
            Unique = Json.TryBoolean(o["unique"], out var u) && u,
            Fields = fields,
            Additional = Json.AsString(o["additional"]),
            Extra = o.ContainsKey("extra") ? Read(o["extra"], defs) : null,
            Variants = o["variants"] is JsonArray vs ? vs.Select(x => Read(x, defs)).ToList() : null,
            Tag = Json.AsString(o["tag"]),
            Tags = o["tags"] is JsonArray ta ? ta.Select(t => Json.AsString(t) ?? "").ToList() : null,
            Inner = o.ContainsKey("inner") ? Read(o["inner"], defs) : null,
            Ref = Json.AsString(o["ref"]),
            Defs = defs,
        };
    }

    // ------------------------------------------------------------ formats

    private const string DatePattern = "(?:(?:[0-9][0-9][2468][048]|[0-9][0-9][13579][26]|[0-9][0-9]0[48]|[02468][048]00" +
        "|[13579][26]00)-02-29|[0-9]{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12][0-9]|3[01])" +
        "|(?:0[469]|11)-(?:0[1-9]|[12][0-9]|30)|(?:02)-(?:0[1-9]|1[0-9]|2[0-8])))";

    private static readonly Lazy<Dictionary<string, Regex>> FormatPatterns = new(() =>
    {
        const string ipv4Part = "(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])";
        string ipv6 = ("(?:(?:H:){7}H|(?:H:){1,7}:|(?:H:){1,6}:H|(?:H:){1,5}(?::H){1,2}" +
            "|(?:H:){1,4}(?::H){1,3}|(?:H:){1,3}(?::H){1,4}|(?:H:){1,2}(?::H){1,5}" +
            "|H:(?:(?::H){1,6})|:(?:(?::H){1,7}|:))").Replace("H", "[0-9a-fA-F]{1,4}", StringComparison.Ordinal);
        var sources = new Dictionary<string, string>
        {
            ["uuid"] = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
            ["email"] = "[^\\s@\"]{1,64}@[^\\s@]{1,255}",
            ["date-time"] = DatePattern + "T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](?:\\.[0-9]+)?" +
                "(?:Z|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9])",
            ["date"] = DatePattern,
            ["ipv4"] = "(?:" + ipv4Part + "\\.){3}" + ipv4Part,
            ["ipv6"] = ipv6,
        };
        return sources.ToDictionary(kv => kv.Key, kv => new Regex("^(?:" + kv.Value + ")$", RegexOptions.CultureInvariant));
    });

    /// <summary>A compiled schema pattern, or the reason it cannot be checked.</summary>
    private sealed record PatternEntry(Regex? Regex, string? Unchecked, bool Astral = false);

    private static readonly ConcurrentDictionary<string, PatternEntry> Patterns = new(StringComparer.Ordinal);

    private PatternEntry? Compiled()
    {
        if (string.IsNullOrEmpty(Pattern))
        {
            return null;
        }
        return Patterns.GetOrAdd(Pattern, p =>
        {
            // The pattern is ECMAScript; it is rewritten into the .NET dialect with the same meaning.
            // One that has no faithful rewrite, or that .NET still refuses, is not checked and is reported.
            if (!EcmaPattern.TryTranslate(p, out var translated, out var reason, out var astral))
            {
                return new PatternEntry(null, "it uses " + reason + ", which the .NET runtime cannot match like ECMAScript");
            }
            try
            {
                return new PatternEntry(new Regex(translated, RegexOptions.CultureInvariant, TimeSpan.FromSeconds(1)), null, astral);
            }
            catch (ArgumentException e)
            {
                return new PatternEntry(null, "it does not compile in .NET (" + e.Message + ")");
            }
        });
    }

    private static string BoundText(double f)
    {
        if (f == Math.Truncate(f) && Math.Abs(f) < Json.MaxSafe)
        {
            return ((long)f).ToString(CultureInfo.InvariantCulture);
        }
        return Json.JsNumber(f);
    }

    internal static string Describe(JsonNode? value) => Json.KindOf(value) switch
    {
        JsonKind.Null => "null",
        JsonKind.Boolean => "a boolean",
        JsonKind.Integer or JsonKind.Number => "a number",
        JsonKind.String => "a string",
        JsonKind.Array => "an array",
        JsonKind.Object => "an object",
        _ => "a value",
    };

    private sealed class Validation
    {
        public readonly List<Issue> Structure = new();
        public readonly List<Issue> Constraints = new();
        public readonly List<Issue> Unknown = new();
        public readonly List<PatternNote> Notes = new();
        // All pattern matching of one validation shares this budget, so many pathological values cost
        // a bounded time; each single match is also bounded by the pattern's own timeout.
        private static readonly TimeSpan MatchBudget = TimeSpan.FromSeconds(2);
        private readonly System.Diagnostics.Stopwatch matchTime = new();

        private static List<object> With(List<object> path, object seg)
        {
            var o = new List<object>(path) { seg };
            return o;
        }

        private void Bad(List<object> path, string message) => Structure.Add(new Issue(path, message));

        private void Limit(List<object> path, string message) => Constraints.Add(new Issue(path, message));

        public void Check(Schema? s, JsonNode? value, List<object> path, int depth)
        {
            if (s is null)
            {
                return;
            }
            if (depth > 512)
            {
                Bad(path, "the value is nested too deeply");
                return;
            }
            var kind = Json.KindOf(value);
            switch (s.Kind)
            {
                case "any":
                    return;
                case "never":
                    Bad(path, "no value is allowed here");
                    return;
                case "ref":
                    if (s.Ref is not null && s.Defs is not null && s.Defs.TryGetValue(s.Ref, out var target))
                    {
                        Check(target, value, path, depth + 1);
                    }
                    return;
                case "nullable":
                    if (kind != JsonKind.Null)
                    {
                        Check(s.Inner, value, path, depth + 1);
                    }
                    return;
                case "string":
                    if (kind != JsonKind.String)
                    {
                        Bad(path, "expected a string, found " + Describe(value));
                        return;
                    }
                    StringLimits(s, Json.AsString(value) ?? "", path);
                    return;
                case "bytes":
                case "binary":
                    if (Binary.TryFrom(value, out _))
                    {
                        return;
                    }
                    if (kind != JsonKind.String)
                    {
                        Bad(path, "expected binary data, found " + Describe(value));
                    }
                    return;
                case "boolean":
                    if (kind != JsonKind.Boolean)
                    {
                        Bad(path, "expected a boolean, found " + Describe(value));
                    }
                    return;
                case "integer":
                    {
                        if (kind != JsonKind.Integer)
                        {
                            if (kind == JsonKind.Number && Json.TryNumber(value, out var f))
                            {
                                Bad(path, "expected an integer, found " + Json.JsNumber(f));
                            }
                            else
                            {
                                Bad(path, "expected an integer, found " + Describe(value));
                            }
                            return;
                        }
                        Json.TryInteger(value, out long n);
                        if (s.Bits == 32 && (n < int.MinValue || n > int.MaxValue))
                        {
                            Bad(path, "expected a 32-bit integer");
                            return;
                        }
                        NumberLimits(s, n, path);
                        return;
                    }
                case "number":
                    {
                        if (!Json.TryNumber(value, out var f))
                        {
                            Bad(path, "expected a number, found " + Describe(value));
                            return;
                        }
                        NumberLimits(s, f, path);
                        return;
                    }
                case "enum":
                    foreach (var allowed in s.Values ?? System.Array.Empty<JsonNode?>())
                    {
                        if (Json.Equal(allowed, value))
                        {
                            return;
                        }
                    }
                    Bad(path, "must be one of " + Json.Text(Json.Array(s.Values ?? System.Array.Empty<JsonNode?>())));
                    return;
                case "const":
                    if (s.Values is { Count: > 0 } && !Json.Equal(s.Values[0], value))
                    {
                        Bad(path, "must be " + Json.Text(s.Values[0]));
                    }
                    return;
                case "array":
                    {
                        if (value is not JsonArray list)
                        {
                            Bad(path, "expected an array, found " + Describe(value));
                            return;
                        }
                        for (int i = 0; i < list.Count; i++)
                        {
                            Check(s.Items, list[i], With(path, i), depth + 1);
                        }
                        long count = list.Count;
                        if (s.MinItems is long min && count < min)
                        {
                            Limit(path, $"must have at least {min} items");
                        }
                        if (s.MaxItems is long max && count > max)
                        {
                            Limit(path, $"must have at most {max} items");
                        }
                        if (s.Unique)
                        {
                            for (int i = 0; i < list.Count; i++)
                            {
                                for (int j = 0; j < i; j++)
                                {
                                    if (Json.Equal(list[i], list[j]))
                                    {
                                        Limit(path, "items must be unique");
                                        return;
                                    }
                                }
                            }
                        }
                        return;
                    }
                case "map":
                    {
                        if (value is not JsonObject obj)
                        {
                            Bad(path, "expected an object, found " + Describe(value));
                            return;
                        }
                        foreach (var kv in obj)
                        {
                            Check(s.Items, kv.Value, With(path, kv.Key), depth + 1);
                        }
                        return;
                    }
                case "object":
                    {
                        if (value is not JsonObject obj)
                        {
                            Bad(path, "expected an object, found " + Describe(value));
                            return;
                        }
                        Object(s, obj, path, depth);
                        return;
                    }
                case "union":
                    Union(s, value, path, depth);
                    return;
                case "all":
                    foreach (var member in s.Variants ?? System.Array.Empty<Schema>())
                    {
                        Check(member, value, path, depth + 1);
                    }
                    return;
                case "limits":
                    if (kind == JsonKind.String)
                    {
                        StringLimits(s, Json.AsString(value) ?? "", path);
                    }
                    else if (Json.TryNumber(value, out var lf))
                    {
                        NumberLimits(s, lf, path);
                    }
                    return;
                default:
                    Bad(path, "unknown schema kind " + s.Kind);
                    return;
            }
        }

        private void StringLimits(Schema s, string text, List<object> path)
        {
            long n = text.Length;
            if (s.MinLength is long min && n < min)
            {
                Limit(path, $"must have at least {min} characters");
            }
            if (s.MaxLength is long max && n > max)
            {
                Limit(path, $"must have at most {max} characters");
            }
            var entry = s.Compiled();
            if (entry?.Regex is { } re)
            {
                bool matched = true;
                if (entry.Astral && text.Any(char.IsSurrogate))
                {
                    Notes.Add(new PatternNote(path, s.Pattern ?? "", "the value has characters above U+FFFF, which this pattern cannot match like ECMAScript in .NET"));
                }
                else if (matchTime.Elapsed >= MatchBudget)
                {
                    Notes.Add(new PatternNote(path, s.Pattern ?? "", "pattern matching in this validation already used its " + MatchBudget.TotalSeconds.ToString(CultureInfo.InvariantCulture) + " s budget"));
                }
                else
                {
                    matchTime.Start();
                    try
                    {
                        matched = re.IsMatch(text);
                    }
                    catch (RegexMatchTimeoutException)
                    {
                        // Fail open, but never silently: the caller reports the pattern as unchecked.
                        Notes.Add(new PatternNote(path, s.Pattern ?? "", "matching took longer than " + re.MatchTimeout.TotalSeconds.ToString(CultureInfo.InvariantCulture) + " s"));
                        matched = true;
                    }
                    finally
                    {
                        matchTime.Stop();
                    }
                }
                if (!matched)
                {
                    Limit(path, "must match the pattern /" + s.Pattern + "/");
                }
            }
            else if (entry?.Unchecked is { } why)
            {
                Notes.Add(new PatternNote(path, s.Pattern ?? "", why));
            }
            if (s.Format is not null && FormatPatterns.Value.TryGetValue(s.Format, out var format) && !format.IsMatch(text))
            {
                Limit(path, "must be a valid " + s.Format);
            }
        }

        private void NumberLimits(Schema s, double f, List<object> path)
        {
            if (s.Minimum is double min && f < min)
            {
                Limit(path, "must be at least " + BoundText(min));
            }
            if (s.Maximum is double max && f > max)
            {
                Limit(path, "must be at most " + BoundText(max));
            }
            if (s.ExclusiveMinimum is double emin && f <= emin)
            {
                Limit(path, "must be greater than " + BoundText(emin));
            }
            if (s.ExclusiveMaximum is double emax && f >= emax)
            {
                Limit(path, "must be less than " + BoundText(emax));
            }
            if (s.MultipleOf is double m && m != 0)
            {
                double q = f / m;
                if (Math.Abs(q - Math.Round(q)) > 1e-9 * Math.Max(Math.Abs(q), 1))
                {
                    Limit(path, "must be a multiple of " + BoundText(m));
                }
            }
        }

        private void Object(Schema s, JsonObject obj, List<object> path, int depth)
        {
            var known = new HashSet<string>(StringComparer.Ordinal);
            foreach (var f in s.Fields ?? System.Array.Empty<SchemaField>())
            {
                known.Add(f.Name);
                bool present = obj.TryGetPropertyValue(f.Name, out var value);
                var at = With(path, f.Name);
                if (!present)
                {
                    if (f.Required)
                    {
                        Bad(at, "missing required member " + f.Name);
                    }
                }
                else if (Json.IsNull(value) && f.Nullable)
                {
                }
                else
                {
                    Check(f.Schema, value, at, depth + 1);
                }
            }
            foreach (var kv in obj)
            {
                if (known.Contains(kv.Key))
                {
                    continue;
                }
                if (s.Extra is not null)
                {
                    Check(s.Extra, kv.Value, With(path, kv.Key), depth + 1);
                }
                else if (s.Additional == "closed")
                {
                    Unknown.Add(new Issue(With(path, kv.Key), "unknown member " + kv.Key));
                }
            }
        }

        private void Union(Schema s, JsonNode? value, List<object> path, int depth)
        {
            var variants = s.Variants ?? System.Array.Empty<Schema>();
            if (!string.IsNullOrEmpty(s.Tag))
            {
                if (value is not JsonObject obj)
                {
                    Bad(path, "expected an object, found " + Describe(value));
                    return;
                }
                if (!obj.TryGetPropertyValue(s.Tag, out var tag) || !Json.TryString(tag, out var text))
                {
                    Bad(With(path, s.Tag), "missing the discriminator " + s.Tag);
                    return;
                }
                var tags = s.Tags ?? System.Array.Empty<string>();
                for (int i = 0; i < tags.Count; i++)
                {
                    if (tags[i] == text && i < variants.Count)
                    {
                        Check(variants[i], value, path, depth + 1);
                        return;
                    }
                }
                Bad(With(path, s.Tag), "must be one of " + Json.Text(Json.Strings(tags)));
                return;
            }
            foreach (var variant in variants)
            {
                if (variant.Accepts(value))
                {
                    return;
                }
            }
            Bad(path, "does not match any variant");
        }
    }

    // ------------------------------------------------------------ paths

    /// <summary>An issue path as <c>.key</c> and <c>[index]</c> segments.</summary>
    public static string FormatPath(IEnumerable<object> path)
    {
        var b = new StringBuilder();
        foreach (var seg in path)
        {
            switch (seg)
            {
                case int i:
                    b.Append('[').Append(i.ToString(CultureInfo.InvariantCulture)).Append(']');
                    break;
                case string s:
                    if (Util.IsIndex(s))
                    {
                        b.Append('[').Append(s).Append(']');
                    }
                    else
                    {
                        b.Append('.').Append(s);
                    }
                    break;
            }
        }
        return b.ToString();
    }

    internal static List<string> Segments(IEnumerable<object> path) =>
        path.Select(seg => seg is int i ? i.ToString(CultureInfo.InvariantCulture) : seg as string ?? "").ToList();
}

/// <summary>One member of an object schema.</summary>
public sealed class SchemaField
{
    /// <summary>The member's name in the value checked: the arguments key in a request, the wire name in a model.</summary>
    public string Name { get; init; } = "";
    public string Wire { get; init; } = "";
    public Schema? Schema { get; init; }
    public bool Required { get; init; }
    public bool Nullable { get; init; }
}
