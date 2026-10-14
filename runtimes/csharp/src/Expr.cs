// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace Tungsten.Runtime;

/// <summary>
/// Predicates (verify expect and terminal, poll until): field path to <c>{"in": [...]}</c> |
/// <c>{"equals": x}</c> | <c>{"contains": {...}}</c>; any other value is compared for equality.
/// Every entry must hold.
///
/// Expressions: JSON in which a string starting with <c>$</c> is a reference resolved against a
/// scope (<c>$input.a.b</c>, <c>$&lt;name&gt;.a</c>, <c>$response.x</c>, <c>$args.y</c>), an object
/// <c>{"expr": "&lt;ref&gt; in [..]" | "&lt;ref&gt; == x" | "&lt;ref&gt; != x"}</c> is a boolean,
/// anything else a literal. A dry evaluation (macro previews) knows the input but not the earlier
/// results: a reference to one is the placeholder <c>&lt;from step NAME: path&gt;</c>.
/// </summary>
public static class Expr
{
    /// <summary>Whether every entry of the predicate holds on the value. A null predicate always holds; a malformed one never does.</summary>
    public static bool EvaluatePredicate(JsonNode? predicate, JsonNode? value, bool present)
    {
        if (predicate is null)
        {
            return true;
        }
        if (predicate is not JsonObject p)
        {
            return false;
        }
        foreach (var kv in p)
        {
            bool ok = Util.GetPathStr(value, present, kv.Key, out var actual);
            if (!Holds(kv.Value, actual, ok))
            {
                return false;
            }
        }
        return true;
    }

    private static bool Holds(JsonNode? test, JsonNode? actual, bool present)
    {
        if (test is JsonObject m && m.Count == 1)
        {
            if (m.TryGetPropertyValue("in", out var candidates))
            {
                if (candidates is not JsonArray list)
                {
                    return false;
                }
                return list.Any(c => Json.EqualOpt(c, true, actual, present));
            }
            if (m.TryGetPropertyValue("equals", out var expected))
            {
                return Json.EqualOpt(expected, true, actual, present);
            }
            if (m.TryGetPropertyValue("contains", out var pattern))
            {
                if (Json.TryString(pattern, out var text))
                {
                    if (Json.TryString(actual, out var have))
                    {
                        return present && have.Contains(text, StringComparison.Ordinal);
                    }
                    if (actual is JsonArray items)
                    {
                        return items.Any(item => Json.TryString(item, out var s) && s == text);
                    }
                    return false;
                }
                if (present && actual is JsonArray values && pattern is not JsonArray)
                {
                    return values.Any(item => Util.ContainsSubset(item, true, pattern));
                }
                return Util.ContainsSubset(actual, present, pattern);
            }
        }
        return Json.EqualOpt(test, true, actual, present);
    }

    /// <summary>Resolves one <c>$name.path</c> reference.</summary>
    internal static bool ResolveRef(string reference, JsonObject scope, out JsonNode? value)
    {
        value = null;
        var segments = Util.SplitPath(reference.StartsWith('$') ? reference[1..] : reference);
        if (segments.Count == 0 || !scope.TryGetPropertyValue(segments[0], out var bound))
        {
            return false;
        }
        return Util.GetPath(bound, true, segments.Skip(1), out value);
    }

    /// <summary>Evaluates an expression; unknown references are absent (dropped from objects); a malformed expr string is null.</summary>
    internal static (JsonNode? Value, bool Present) Evaluate(JsonNode? expr, JsonObject scope, int depth)
    {
        if (depth > 64)
        {
            return (null, true);
        }
        switch (expr)
        {
            case JsonArray a:
                {
                    var outArr = new JsonArray();
                    foreach (var item in a)
                    {
                        var (v, ok) = Evaluate(item, scope, depth + 1);
                        Json.Add(outArr, ok ? v : null);
                    }
                    return (outArr, true);
                }
            case JsonObject o:
                {
                    if (o.Count == 1 && Json.TryString(o["expr"], out var source))
                    {
                        return (EvaluateBoolean(source, scope), true);
                    }
                    var outObj = new JsonObject();
                    foreach (var kv in o)
                    {
                        var (v, ok) = Evaluate(kv.Value, scope, depth + 1);
                        if (ok)
                        {
                            Json.Put(outObj, kv.Key, v);
                        }
                    }
                    return (outObj, true);
                }
        }
        if (Json.TryString(expr, out var text) && text.StartsWith('$'))
        {
            bool found = ResolveRef(text, scope, out var v);
            return (found ? Json.Copy(v) : null, found);
        }
        return (Json.Copy(expr), true);
    }

    private static string Placeholder(string name, string path) =>
        path.Length == 0 ? "<from step " + name + ">" : "<from step " + name + ": " + path + ">";

    private static readonly Regex PlaceholderPattern = new("^<from step [^<>]+>$", RegexOptions.CultureInvariant);

    internal static void PlaceholdersIn(JsonNode? v, List<string> found, int depth)
    {
        if (depth > 64)
        {
            return;
        }
        switch (v)
        {
            case JsonArray a:
                foreach (var item in a)
                {
                    PlaceholdersIn(item, found, depth + 1);
                }
                break;
            case JsonObject o:
                foreach (var kv in o)
                {
                    PlaceholdersIn(kv.Value, found, depth + 1);
                }
                break;
            default:
                if (Json.TryString(v, out var s) && PlaceholderPattern.IsMatch(s))
                {
                    found.Add(s);
                }
                break;
        }
    }

    internal static bool ContainsPlaceholder(JsonNode? v)
    {
        var found = new List<string>();
        PlaceholdersIn(v, found, 0);
        return found.Count > 0;
    }

    /// <summary>The pending binding a reference names and the path written after it.</summary>
    private static bool PendingRef(string reference, ISet<string> pending, out string name, out string path)
    {
        name = "";
        path = "";
        if (!reference.StartsWith('$'))
        {
            return false;
        }
        var segs = Util.SplitPath(reference[1..]);
        if (segs.Count == 0)
        {
            return false;
        }
        name = segs[0];
        if (!pending.Contains(name) || !reference.StartsWith("$" + name, StringComparison.Ordinal))
        {
            return false;
        }
        var rest = reference[(name.Length + 1)..];
        path = rest.StartsWith('.') ? rest[1..] : rest;
        return true;
    }

    private static readonly Regex DryRef = new("^\\$[^\\s=!]+", RegexOptions.CultureInvariant);

    internal static (JsonNode? Value, bool Present) EvaluateDry(JsonNode? expr, JsonObject scope, ISet<string> pending, int depth)
    {
        if (depth > 64)
        {
            return (null, true);
        }
        switch (expr)
        {
            case JsonArray a:
                {
                    var outArr = new JsonArray();
                    foreach (var item in a)
                    {
                        var (v, ok) = EvaluateDry(item, scope, pending, depth + 1);
                        Json.Add(outArr, ok ? v : null);
                    }
                    return (outArr, true);
                }
            case JsonObject o:
                {
                    if (o.Count == 1 && Json.TryString(o["expr"], out var raw))
                    {
                        var source = raw.Trim();
                        var head = DryRef.Match(source).Value;
                        if (PendingRef(head, pending, out var name, out _))
                        {
                            var rest = source[(name.Length + 1)..];
                            return (Placeholder(name, rest.StartsWith('.') ? rest[1..] : rest), true);
                        }
                        return (EvaluateBoolean(source, scope), true);
                    }
                    var outObj = new JsonObject();
                    foreach (var kv in o)
                    {
                        var (v, ok) = EvaluateDry(kv.Value, scope, pending, depth + 1);
                        if (ok)
                        {
                            Json.Put(outObj, kv.Key, v);
                        }
                    }
                    return (outObj, true);
                }
        }
        if (Json.TryString(expr, out var text) && text.StartsWith('$'))
        {
            if (PendingRef(text, pending, out var name, out var path))
            {
                return (Placeholder(name, path), true);
            }
            bool found = ResolveRef(text, scope, out var v);
            return (found ? Json.Copy(v) : null, found);
        }
        return (Json.Copy(expr), true);
    }

    /// <summary>A predicate in words: state in ["a", "b"], endpoints contains {...}, a = 1, joined with "and".</summary>
    internal static string DescribePredicate(JsonNode? predicate)
    {
        if (predicate is not JsonObject p)
        {
            return "";
        }
        var parts = new List<string>();
        foreach (var kv in p)
        {
            if (kv.Value is JsonObject m && m.Count == 1)
            {
                if (m["in"] is JsonArray list)
                {
                    parts.Add(kv.Key + " in [" + string.Join(", ", list.Select(Json.Text)) + "]");
                    continue;
                }
                if (m.TryGetPropertyValue("equals", out var expected))
                {
                    parts.Add(kv.Key + " = " + Json.Text(expected));
                    continue;
                }
                if (m.TryGetPropertyValue("contains", out var pattern))
                {
                    parts.Add(kv.Key + " contains " + Json.Text(pattern));
                    continue;
                }
            }
            parts.Add(kv.Key + " = " + Json.Text(kv.Value));
        }
        return string.Join(" and ", parts);
    }

    private static readonly Regex SingleQuoted = new("'((?:[^'\\\\]|\\\\.)*)'", RegexOptions.CultureInvariant);

    private static bool ParseLiteral(string text, out JsonNode? value)
    {
        var trimmed = text.Trim();
        if (Json.TryParse(trimmed, out value))
        {
            return true;
        }
        var normalized = SingleQuoted.Replace(trimmed, m => Json.Quote(m.Groups[1].Value.Replace("\\'", "'", StringComparison.Ordinal)));
        return Json.TryParse(normalized, out value);
    }

    private static readonly Regex BooleanExpr = new("^\\s*(\\$[^\\s=!]+)\\s+(in|==|!=)\\s+(.+)$", RegexOptions.Singleline | RegexOptions.CultureInvariant);

    private static JsonNode? EvaluateBoolean(string source, JsonObject scope)
    {
        var m = BooleanExpr.Match(source);
        if (!m.Success || !ParseLiteral(m.Groups[3].Value, out var literal))
        {
            return null;
        }
        bool present = ResolveRef(m.Groups[1].Value, scope, out var actual);
        if (m.Groups[2].Value == "in")
        {
            if (literal is not JsonArray list)
            {
                return null;
            }
            return JsonValue.Create(list.Any(v => Json.EqualOpt(v, true, actual, present)));
        }
        if (!present)
        {
            actual = null;
        }
        bool equal = Json.Equal(actual, literal);
        return JsonValue.Create(m.Groups[2].Value == "==" ? equal : !equal);
    }
}
