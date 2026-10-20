// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;

namespace Tungsten.Runtime;

/// <summary>
/// Rewrites an ECMAScript (ECMA-262) regular expression, the dialect of a schema <c>pattern</c> in the
/// shared contract, into the .NET dialect with the same meaning. <c>RegexOptions.ECMAScript</c> is not
/// used: it rejects <c>\p{...}</c>, lookbehind and named groups, and still lets <c>$</c> match before a
/// trailing newline and <c>\s</c> miss the Unicode spaces ECMAScript has. The rewrite is tried in Unicode
/// mode first and then in the lax mode of a pattern without the <c>u</c> flag, as the TypeScript runtime
/// does. A construct with no faithful .NET equivalent is refused with a reason, so the caller can report
/// the pattern as unchecked instead of checking it with different semantics.
/// </summary>
internal static class EcmaPattern
{
    private const string Pair = "[\\uD800-\\uDBFF][\\uDC00-\\uDFFF]";

    private static readonly (int, int)[] Digit = { ('0', '9') };
    private static readonly (int, int)[] Word = { ('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z') };
    private static readonly (int, int)[] Space =
    {
        (9, 13), (32, 32), (0xA0, 0xA0), (0x1680, 0x1680), (0x2000, 0x200A), (0x2028, 0x2029),
        (0x202F, 0x202F), (0x205F, 0x205F), (0x3000, 0x3000), (0xFEFF, 0xFEFF),
    };

    private static readonly Dictionary<string, string> Categories = BuildCategories();

    /// <summary>The .NET pattern with the meaning of the ECMAScript <paramref name="source"/>, or the reason there is none.</summary>
    public static bool TryTranslate(string source, out string translated, out string error)
    {
        if (TryMode(source, true, out translated, out var unicodeError))
        {
            error = "";
            return true;
        }
        if (TryMode(source, false, out translated, out _))
        {
            error = "";
            return true;
        }
        error = unicodeError;
        return false;
    }

    private static bool TryMode(string source, bool unicode, out string translated, out string error)
    {
        try
        {
            translated = new Translator(source, unicode).Run();
            error = "";
            return true;
        }
        catch (UnsupportedException e)
        {
            translated = "";
            error = e.Message;
            return false;
        }
    }

    private sealed class UnsupportedException : Exception
    {
        public UnsupportedException(string message) : base(message) { }
    }

    private static Dictionary<string, string> BuildCategories()
    {
        var d = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var short_ in new[]
        {
            "L", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "P", "Pc", "Pd", "Ps",
            "Pe", "Pi", "Pf", "Po", "S", "Sm", "Sc", "Sk", "So", "Z", "Zs", "Zl", "Zp", "C", "Cc", "Cf", "Cs", "Co", "Cn",
        })
        {
            d[short_] = short_;
        }
        var longs = new (string, string)[]
        {
            ("Letter", "L"), ("Uppercase_Letter", "Lu"), ("Lowercase_Letter", "Ll"), ("Titlecase_Letter", "Lt"),
            ("Modifier_Letter", "Lm"), ("Other_Letter", "Lo"), ("Mark", "M"), ("Combining_Mark", "M"),
            ("Nonspacing_Mark", "Mn"), ("Spacing_Mark", "Mc"), ("Enclosing_Mark", "Me"), ("Number", "N"),
            ("Decimal_Number", "Nd"), ("digit", "Nd"), ("Letter_Number", "Nl"), ("Other_Number", "No"),
            ("Punctuation", "P"), ("punct", "P"), ("Connector_Punctuation", "Pc"), ("Dash_Punctuation", "Pd"),
            ("Open_Punctuation", "Ps"), ("Close_Punctuation", "Pe"), ("Initial_Punctuation", "Pi"),
            ("Final_Punctuation", "Pf"), ("Other_Punctuation", "Po"), ("Symbol", "S"), ("Math_Symbol", "Sm"),
            ("Currency_Symbol", "Sc"), ("Modifier_Symbol", "Sk"), ("Other_Symbol", "So"), ("Separator", "Z"),
            ("Space_Separator", "Zs"), ("Line_Separator", "Zl"), ("Paragraph_Separator", "Zp"), ("Other", "C"),
            ("Control", "Cc"), ("cntrl", "Cc"), ("Format", "Cf"), ("Surrogate", "Cs"), ("Private_Use", "Co"),
            ("Unassigned", "Cn"),
        };
        foreach (var (name, code) in longs)
        {
            d[name] = code;
        }
        return d;
    }

    private static string U(int c) => "\\u" + c.ToString("X4", CultureInfo.InvariantCulture);

    private static string Body((int Lo, int Hi)[] ranges)
    {
        var b = new StringBuilder();
        foreach (var (lo, hi) in ranges)
        {
            b.Append(U(lo));
            if (hi != lo)
            {
                b.Append('-').Append(U(hi));
            }
        }
        return b.ToString();
    }

    private static (int, int)[] Complement((int Lo, int Hi)[] ranges)
    {
        var result = new List<(int, int)>();
        int next = 0;
        foreach (var (lo, hi) in ranges.OrderBy(r => r.Lo))
        {
            if (lo > next)
            {
                result.Add((next, lo - 1));
            }
            next = Math.Max(next, hi + 1);
        }
        if (next <= 0xFFFF)
        {
            result.Add((next, 0xFFFF));
        }
        return result.ToArray();
    }

    private sealed class Translator
    {
        private readonly string s;
        private readonly bool unicode;
        private readonly StringBuilder o = new();
        private int i;

        public Translator(string source, bool unicode)
        {
            s = source;
            this.unicode = unicode;
        }

        public string Run()
        {
            while (i < s.Length)
            {
                char c = s[i];
                switch (c)
                {
                    case '\\':
                        i++;
                        Escape();
                        break;
                    case '[':
                        i++;
                        Class();
                        break;
                    case '.':
                        i++;
                        o.Append(Negated("\\n\\r\\u2028\\u2029"));
                        break;
                    case '$':
                        i++;
                        o.Append("\\z");
                        break;
                    case '(':
                        Group();
                        break;
                    default:
                        i++;
                        if (unicode && char.IsHighSurrogate(c) && i < s.Length && char.IsLowSurrogate(s[i]))
                        {
                            o.Append("(?:").Append(c).Append(s[i]).Append(')');
                            i++;
                        }
                        else
                        {
                            o.Append(c);
                        }
                        break;
                }
            }
            return o.ToString();
        }

        /// <summary>One character outside a set, as a code point in Unicode mode (a surrogate pair is one).</summary>
        private string Negated(string body)
        {
            return unicode ? "(?>" + Pair + "|[^" + body + "])" : "[^" + body + "]";
        }

        private string Complemented((int, int)[] ranges)
        {
            var body = Body(Complement(ranges));
            return unicode ? "(?>" + Pair + "|[" + body + "])" : "[" + body + "]";
        }

        private void Group()
        {
            i++;
            if (i < s.Length && s[i] == '?')
            {
                string rest = s.Substring(i);
                if (rest.StartsWith("?:", StringComparison.Ordinal) || rest.StartsWith("?=", StringComparison.Ordinal)
                    || rest.StartsWith("?!", StringComparison.Ordinal))
                {
                    o.Append('(').Append(rest, 0, 2);
                    i += 2;
                    return;
                }
                if (rest.StartsWith("?<=", StringComparison.Ordinal) || rest.StartsWith("?<!", StringComparison.Ordinal))
                {
                    o.Append('(').Append(rest, 0, 3);
                    i += 3;
                    return;
                }
                if (rest.StartsWith("?<", StringComparison.Ordinal))
                {
                    int end = s.IndexOf('>', i);
                    string name = end < 0 ? "" : s.Substring(i + 2, end - i - 2);
                    if (name.Length == 0 || !IsName(name))
                    {
                        throw new UnsupportedException("a group name that .NET cannot use");
                    }
                    o.Append("(?<").Append(name).Append('>');
                    i = end + 1;
                    return;
                }
                throw new UnsupportedException("the group syntax (?" + (rest.Length > 1 ? rest[1].ToString() : "") + "...");
            }
            o.Append('(');
        }

        private static bool IsName(string name) =>
            !char.IsAsciiDigit(name[0]) && name.All(ch => char.IsAsciiLetterOrDigit(ch) || ch == '_');

        private void Escape()
        {
            if (i >= s.Length)
            {
                throw new UnsupportedException("a trailing backslash");
            }
            char e = s[i++];
            switch (e)
            {
                case 'd':
                    o.Append('[').Append(Body(Digit)).Append(']');
                    return;
                case 'D':
                    o.Append(Complemented(Digit));
                    return;
                case 'w':
                    o.Append('[').Append(Body(Word)).Append(']');
                    return;
                case 'W':
                    o.Append(Complemented(Word));
                    return;
                case 's':
                    o.Append('[').Append(Body(Space)).Append(']');
                    return;
                case 'S':
                    o.Append(Complemented(Space));
                    return;
                case 'b':
                    o.Append("(?:(?<=[").Append(Body(Word)).Append("])(?![").Append(Body(Word)).Append("])|(?<![")
                        .Append(Body(Word)).Append("])(?=[").Append(Body(Word)).Append("]))");
                    return;
                case 'B':
                    o.Append("(?:(?<=[").Append(Body(Word)).Append("])(?=[").Append(Body(Word)).Append("])|(?<![")
                        .Append(Body(Word)).Append("])(?![").Append(Body(Word)).Append("]))");
                    return;
                case 'k':
                    if (i < s.Length && s[i] == '<')
                    {
                        int end = s.IndexOf('>', i);
                        string name = end < 0 ? "" : s.Substring(i + 1, end - i - 1);
                        if (name.Length == 0 || !IsName(name))
                        {
                            throw new UnsupportedException("a group name that .NET cannot use");
                        }
                        o.Append("\\k<").Append(name).Append('>');
                        i = end + 1;
                        return;
                    }
                    Identity(e, false);
                    return;
                case 'p':
                case 'P':
                    if (unicode)
                    {
                        o.Append(Property(e == 'P', false));
                        return;
                    }
                    Identity(e, false);
                    return;
            }
            if (e >= '1' && e <= '9')
            {
                int start = i - 1;
                while (i < s.Length && char.IsAsciiDigit(s[i]))
                {
                    i++;
                }
                o.Append("(?:\\").Append(s, start, i - start).Append(')');
                return;
            }
            string? single = Common(e, false);
            if (single is not null)
            {
                o.Append(single);
                return;
            }
            Identity(e, false);
        }

        /// <summary>The escapes that mean the same inside and outside a set; null when <paramref name="e"/> is none of them.</summary>
        private string? Common(char e, bool inClass)
        {
            switch (e)
            {
                case 'f': return "\\f";
                case 'n': return "\\n";
                case 'r': return "\\r";
                case 't': return "\\t";
                case 'v': return "\\u000B";
                case '0':
                    if (i < s.Length && char.IsAsciiDigit(s[i]))
                    {
                        throw new UnsupportedException("a legacy octal escape");
                    }
                    return "\\u0000";
                case 'c':
                    if (i < s.Length && char.IsAsciiLetter(s[i]))
                    {
                        return U(s[i++] % 32);
                    }
                    return null;
                case 'x':
                    if (i + 1 < s.Length && Uri.IsHexDigit(s[i]) && Uri.IsHexDigit(s[i + 1]))
                    {
                        string hex = s.Substring(i, 2);
                        i += 2;
                        return "\\x" + hex;
                    }
                    return null;
                case 'u':
                    return UnicodeEscape(inClass);
            }
            return null;
        }

        private string? UnicodeEscape(bool inClass)
        {
            if (unicode && i < s.Length && s[i] == '{')
            {
                int end = s.IndexOf('}', i);
                if (end < 0 || end == i + 1
                    || !int.TryParse(s.AsSpan(i + 1, end - i - 1), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out int cp)
                    || cp > 0x10FFFF)
                {
                    throw new UnsupportedException("a malformed \\u{...} escape");
                }
                i = end + 1;
                if (cp <= 0xFFFF)
                {
                    return U(cp);
                }
                if (inClass)
                {
                    throw new UnsupportedException("a code point above U+FFFF inside a character class");
                }
                string pair = char.ConvertFromUtf32(cp);
                return "(?:" + U(pair[0]) + U(pair[1]) + ")";
            }
            if (HexAt(i, 4, out int unit))
            {
                i += 4;
                if (unicode && !inClass && unit >= 0xD800 && unit <= 0xDBFF && i + 1 < s.Length && s[i] == '\\' && s[i + 1] == 'u'
                    && HexAt(i + 2, 4, out int low) && low >= 0xDC00 && low <= 0xDFFF)
                {
                    i += 6;
                    return "(?:" + U(unit) + U(low) + ")";
                }
                return U(unit);
            }
            return null;
        }

        private bool HexAt(int at, int count, out int value)
        {
            value = 0;
            if (at + count > s.Length)
            {
                return false;
            }
            for (int k = 0; k < count; k++)
            {
                if (!Uri.IsHexDigit(s[at + k]))
                {
                    return false;
                }
            }
            value = int.Parse(s.AsSpan(at, count), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture);
            return true;
        }

        /// <summary>An escaped character that stands for itself: any syntax character, and in the lax mode any other.</summary>
        private void Identity(char e, bool inClass)
        {
            if ("^$\\.*+?()[]{}|/".Contains(e, StringComparison.Ordinal))
            {
                o.Append(e == '/' ? "/" : "\\" + e);
                return;
            }
            if (e == '-' && (inClass || !unicode))
            {
                o.Append(inClass ? "\\-" : "-");
                return;
            }
            if (unicode)
            {
                throw new UnsupportedException("the escape \\" + e + " (not valid in a Unicode pattern)");
            }
            o.Append(char.IsAsciiLetterOrDigit(e) || e == '_' ? e.ToString() : "\\" + e);
        }

        private string Property(bool negated, bool inClass)
        {
            if (i >= s.Length || s[i] != '{')
            {
                throw new UnsupportedException("a malformed \\p escape");
            }
            int end = s.IndexOf('}', i);
            if (end < 0)
            {
                throw new UnsupportedException("a malformed \\p escape");
            }
            string name = s.Substring(i + 1, end - i - 1);
            i = end + 1;
            int eq = name.IndexOf('=', StringComparison.Ordinal);
            if (eq >= 0)
            {
                string key = name.Substring(0, eq);
                if (key != "General_Category" && key != "gc")
                {
                    throw new UnsupportedException("the Unicode property " + name + " (only general categories are supported)");
                }
                name = name.Substring(eq + 1);
            }
            if (Categories.TryGetValue(name, out var code))
            {
                return (negated ? "\\P{" : "\\p{") + code + "}";
            }
            (int, int)[] ranges;
            if (name == "ASCII")
            {
                ranges = new[] { (0, 0x7F) };
            }
            else if (name == "Any")
            {
                ranges = new[] { (0, 0xFFFF) };
            }
            else
            {
                throw new UnsupportedException("the Unicode property " + name + " (only general categories, ASCII and Any are supported)");
            }
            var chosen = negated ? Complement(ranges) : ranges;
            if (inClass)
            {
                return Body(chosen);
            }
            return chosen.Length == 0 ? "(?!)" : "[" + Body(chosen) + "]";
        }

        private void Class()
        {
            bool negate = i < s.Length && s[i] == '^';
            if (negate)
            {
                i++;
            }
            if (i < s.Length && s[i] == ']')
            {
                i++;
                o.Append(negate ? "[\\u0000-\\uFFFF]" : "(?!)");
                return;
            }
            var b = new StringBuilder();
            bool lastWasSet = false;
            while (true)
            {
                if (i >= s.Length)
                {
                    throw new UnsupportedException("an unterminated character class");
                }
                char c = s[i++];
                if (c == ']')
                {
                    break;
                }
                if (c == '-' && lastWasSet && i < s.Length && s[i] != ']')
                {
                    if (unicode)
                    {
                        throw new UnsupportedException("a range that starts at a class escape");
                    }
                    b.Append("\\-");
                    lastWasSet = false;
                    continue;
                }
                lastWasSet = false;
                if (c == '-' && i < s.Length && s[i] == '\\' && i + 1 < s.Length && "dDwWsS".Contains(s[i + 1], StringComparison.Ordinal))
                {
                    if (unicode)
                    {
                        throw new UnsupportedException("a range that ends at a class escape");
                    }
                    b.Append("\\-");
                    continue;
                }
                if (c == '[')
                {
                    b.Append("\\[");
                    continue;
                }
                if (c != '\\')
                {
                    b.Append(c);
                    continue;
                }
                if (i >= s.Length)
                {
                    throw new UnsupportedException("a trailing backslash");
                }
                char e = s[i++];
                switch (e)
                {
                    case 'd': b.Append(Body(Digit)); lastWasSet = true; continue;
                    case 'w': b.Append(Body(Word)); lastWasSet = true; continue;
                    case 's': b.Append(Body(Space)); lastWasSet = true; continue;
                    case 'D': b.Append(Body(Complement(Digit))); lastWasSet = true; continue;
                    case 'W': b.Append(Body(Complement(Word))); lastWasSet = true; continue;
                    case 'S': b.Append(Body(Complement(Space))); lastWasSet = true; continue;
                    case 'b': b.Append("\\u0008"); continue;
                    case 'p':
                    case 'P':
                        if (unicode)
                        {
                            b.Append(Property(e == 'P', true));
                            lastWasSet = true;
                            continue;
                        }
                        break;
                }
                string? common = Common(e, true);
                if (common is not null)
                {
                    b.Append(common);
                    continue;
                }
                var saved = o.Length;
                Identity(e, true);
                b.Append(o.ToString(saved, o.Length - saved));
                o.Length = saved;
            }
            if (negate)
            {
                o.Append(Negated(b.ToString()));
            }
            else
            {
                o.Append('[').Append(b).Append(']');
            }
        }
    }
}
