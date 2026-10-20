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
/// trailing newline and <c>\s</c> miss the Unicode spaces ECMAScript has.
/// <para>
/// The rewrite follows the TypeScript runtime: the pattern is read in Unicode mode (the <c>u</c> flag), and
/// only when ECMAScript itself rejects it there, as the lax mode of a pattern without the flag. A construct
/// whose meaning cannot be reproduced faithfully is refused (the caller then reports the pattern as
/// unchecked) rather than approximated, so a pattern is either checked with ECMAScript semantics or not
/// checked at all. The one exception is input: a Unicode-mode pattern that is
/// <paramref name="astralSensitive"/> (it can tell a surrogate pair from two code units) is not checked
/// against a value that contains surrogates, because .NET reads UTF-16 code units, not code points.
/// </para>
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
    public static bool TryTranslate(string source, out string translated, out string error) =>
        TryTranslate(source, out translated, out error, out _);

    /// <summary>As above; <paramref name="astralSensitive"/> says whether a value with surrogates must not be checked.</summary>
    public static bool TryTranslate(string source, out string translated, out string error, out bool astralSensitive)
    {
        translated = "";
        astralSensitive = false;
        string reason;
        try
        {
            var t = new Translator(source, true);
            translated = t.Run();
            astralSensitive = t.Sensitive;
            error = "";
            return true;
        }
        catch (InvalidPatternException e)
        {
            // ECMAScript rejects the pattern in Unicode mode, so the TypeScript runtime reads it without the flag.
            reason = e.Message;
        }
        catch (UnsupportedException e)
        {
            error = e.Message;
            return false;
        }
        try
        {
            translated = new Translator(source, false).Run();
            error = "";
            return true;
        }
        catch (Exception e) when (e is UnsupportedException or InvalidPatternException)
        {
            error = e is InvalidPatternException ? reason : e.Message;
            return false;
        }
    }

    /// <summary>A syntax error of ECMAScript itself in the mode being read.</summary>
    private sealed class InvalidPatternException : Exception
    {
        public InvalidPatternException(string message) : base(message) { }
    }

    /// <summary>A construct with no faithful .NET equivalent.</summary>
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

    private static bool IsName(string name) =>
        name.Length > 0 && !char.IsAsciiDigit(name[0]) && name.All(ch => char.IsAsciiLetterOrDigit(ch) || ch == '_');

    private enum Kind { Capture, Plain, Ahead, NotAhead, Behind }

    private sealed class Frame
    {
        public Kind Kind;
        public int Index;
        public bool Repeated;
        public List<Frame> Ancestors = new();
    }

    private sealed class Translator
    {
        private readonly string s;
        private readonly bool unicode;
        private readonly StringBuilder o = new();
        private int i;

        // The capture groups of the whole pattern, in ECMAScript order, found before the rewrite starts.
        private int groupCount;
        private readonly Dictionary<string, int> names = new(StringComparer.Ordinal);

        private readonly List<Frame> stack = new();
        private readonly Dictionary<int, Frame> captures = new();
        private readonly List<(int Index, bool InBehind)> references = new();
        private int behind;
        private int nextGroup;
        private bool quantifiable;
        private bool lookaheadEnded;
        private int lastUnit = -1;

        /// <summary>The pattern can tell a surrogate pair from two code units (Unicode mode only).</summary>
        public bool Sensitive { get; private set; }

        public Translator(string source, bool unicode)
        {
            s = source;
            this.unicode = unicode;
        }

        public string Run()
        {
            Prescan();
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
                        Atom();
                        break;
                    case '.':
                        i++;
                        o.Append(Negated("\\n\\r\\u2028\\u2029"));
                        Atom();
                        break;
                    case '^':
                    case '$':
                        i++;
                        o.Append(c == '^' ? "^" : "\\z");
                        Assertion();
                        break;
                    case '|':
                        i++;
                        o.Append('|');
                        Assertion();
                        break;
                    case '(':
                        Group();
                        break;
                    case ')':
                        CloseGroup();
                        break;
                    case '*':
                    case '+':
                    case '?':
                        i++;
                        Quantifier(c.ToString());
                        break;
                    case '{':
                        Brace();
                        break;
                    case '}':
                    case ']':
                        i++;
                        if (unicode)
                        {
                            throw new InvalidPatternException("a lone " + c);
                        }
                        o.Append('\\').Append(c);
                        Atom();
                        break;
                    default:
                        i++;
                        if (char.IsHighSurrogate(c) && i < s.Length && char.IsLowSurrogate(s[i]) && unicode)
                        {
                            o.Append("(?:").Append(c).Append(s[i]).Append(')');
                            i++;
                        }
                        else
                        {
                            if (unicode && char.IsSurrogate(c))
                            {
                                Sensitive = true;
                            }
                            o.Append(c);
                        }
                        Atom();
                        break;
                }
            }
            if (stack.Count > 0)
            {
                throw new InvalidPatternException("an unterminated group");
            }
            foreach (var (index, inBehind) in references)
            {
                var g = captures[index];
                if (inBehind || g.Ancestors.Any(a => a.Repeated || a.Kind == Kind.NotAhead))
                {
                    throw new UnsupportedException("a back-reference whose group is reset by a loop or a negative lookahead, or read inside a lookbehind");
                }
            }
            return o.ToString();
        }

        private void Atom()
        {
            quantifiable = true;
            lookaheadEnded = false;
        }

        private void Assertion()
        {
            quantifiable = false;
            lookaheadEnded = false;
        }

        /// <summary>Counts the capture groups and names them before the rewrite, so a reference can point forward.</summary>
        private void Prescan()
        {
            bool inClass = false;
            for (int k = 0; k < s.Length; k++)
            {
                char c = s[k];
                if (c == '\\')
                {
                    k++;
                }
                else if (inClass)
                {
                    inClass = c != ']';
                }
                else if (c == '[')
                {
                    inClass = true;
                }
                else if (c == '(')
                {
                    if (k + 1 < s.Length && s[k + 1] == '?')
                    {
                        if (k + 2 < s.Length && s[k + 2] == '<' && k + 3 < s.Length && s[k + 3] != '=' && s[k + 3] != '!')
                        {
                            int end = s.IndexOf('>', k);
                            string name = end < 0 ? "" : s.Substring(k + 3, end - k - 3);
                            if (!IsName(name))
                            {
                                throw new UnsupportedException("a group name that .NET cannot use");
                            }
                            groupCount++;
                            if (!names.TryAdd(name, groupCount))
                            {
                                throw new InvalidPatternException("a duplicate group name");
                            }
                        }
                    }
                    else
                    {
                        groupCount++;
                    }
                }
            }
        }

        /// <summary>One character outside a set, as a code point in Unicode mode (a surrogate pair is one).</summary>
        private string Negated(string body) =>
            unicode ? "(?>" + Pair + "|[^" + body + "])" : "[^" + body + "]";

        private string Complemented((int, int)[] ranges)
        {
            var body = Body(Complement(ranges));
            return unicode ? "(?>" + Pair + "|[" + body + "])" : "[" + body + "]";
        }

        private void Group()
        {
            i++;
            var frame = new Frame { Ancestors = new List<Frame>(stack) };
            if (i < s.Length && s[i] == '?')
            {
                string rest = s.Substring(i);
                if (rest.StartsWith("?:", StringComparison.Ordinal))
                {
                    frame.Kind = Kind.Plain;
                    o.Append("(?:");
                    i += 2;
                }
                else if (rest.StartsWith("?=", StringComparison.Ordinal))
                {
                    frame.Kind = Kind.Ahead;
                    o.Append("(?=");
                    i += 2;
                }
                else if (rest.StartsWith("?!", StringComparison.Ordinal))
                {
                    frame.Kind = Kind.NotAhead;
                    o.Append("(?!");
                    i += 2;
                }
                else if (rest.StartsWith("?<=", StringComparison.Ordinal) || rest.StartsWith("?<!", StringComparison.Ordinal))
                {
                    frame.Kind = Kind.Behind;
                    o.Append('(').Append(rest, 0, 3);
                    i += 3;
                    behind++;
                }
                else if (rest.StartsWith("?<", StringComparison.Ordinal))
                {
                    int end = s.IndexOf('>', i);
                    string name = end < 0 ? "" : s.Substring(i + 2, end - i - 2);
                    if (!IsName(name))
                    {
                        throw new UnsupportedException("a group name that .NET cannot use");
                    }
                    OpenCapture(frame);
                    i = end + 1;
                }
                else
                {
                    throw new UnsupportedException("the group syntax (?" + (rest.Length > 1 ? rest[1].ToString() : "") + "...");
                }
            }
            else
            {
                OpenCapture(frame);
            }
            stack.Add(frame);
            Assertion();
        }

        /// <summary>Every capture group is numbered explicitly, in ECMAScript order, so .NET cannot renumber named ones.</summary>
        private void OpenCapture(Frame frame)
        {
            if (behind > 0)
            {
                throw new UnsupportedException("a capture group inside a lookbehind (.NET matches it right to left)");
            }
            frame.Kind = Kind.Capture;
            frame.Index = ++nextGroup;
            captures[frame.Index] = frame;
            o.Append("(?<").Append(frame.Index.ToString(CultureInfo.InvariantCulture)).Append('>');
        }

        private void CloseGroup()
        {
            i++;
            if (stack.Count == 0)
            {
                throw new InvalidPatternException("an unmatched )");
            }
            var frame = stack[^1];
            stack.RemoveAt(stack.Count - 1);
            o.Append(')');
            if (frame.Kind == Kind.Behind)
            {
                behind--;
            }
            if (i < s.Length && (s[i] == '*' || s[i] == '+' || (s[i] == '{' && IsQuantifierAt(i))))
            {
                frame.Repeated = true;
            }
            if (frame.Kind is Kind.Ahead or Kind.NotAhead)
            {
                quantifiable = false;
                lookaheadEnded = !unicode;
            }
            else if (frame.Kind == Kind.Behind)
            {
                Assertion();
            }
            else
            {
                Atom();
            }
        }

        private bool IsQuantifierAt(int at) => TryBrace(at, out _, out _, out _);

        private bool TryBrace(int at, out int end, out long min, out long max)
        {
            end = at;
            min = 0;
            max = -1;
            int k = at + 1;
            int d = k;
            while (k < s.Length && char.IsAsciiDigit(s[k]))
            {
                k++;
            }
            if (k == d || k - d > 9)
            {
                return false;
            }
            min = long.Parse(s.AsSpan(d, k - d), NumberStyles.None, CultureInfo.InvariantCulture);
            max = min;
            if (k < s.Length && s[k] == ',')
            {
                k++;
                int d2 = k;
                while (k < s.Length && char.IsAsciiDigit(s[k]))
                {
                    k++;
                }
                if (k - d2 > 9)
                {
                    return false;
                }
                max = k == d2 ? -1 : long.Parse(s.AsSpan(d2, k - d2), NumberStyles.None, CultureInfo.InvariantCulture);
            }
            if (k >= s.Length || s[k] != '}')
            {
                return false;
            }
            end = k + 1;
            return true;
        }

        private void Brace()
        {
            if (!TryBrace(i, out int end, out long min, out long max))
            {
                i++;
                if (unicode)
                {
                    throw new InvalidPatternException("a lone {");
                }
                o.Append("\\{");
                Atom();
                return;
            }
            if (max >= 0 && max < min)
            {
                throw new InvalidPatternException("a quantifier out of order");
            }
            string text = s.Substring(i, end - i);
            i = end;
            Quantifier(text);
        }

        private void Quantifier(string text)
        {
            if (!quantifiable)
            {
                if (lookaheadEnded)
                {
                    throw new UnsupportedException("a quantified lookahead");
                }
                throw new InvalidPatternException("a quantifier with nothing to repeat");
            }
            o.Append(text);
            if (i < s.Length && s[i] == '?')
            {
                i++;
                o.Append('?');
            }
            quantifiable = false;
            lookaheadEnded = false;
        }

        private void Escape()
        {
            if (i >= s.Length)
            {
                throw new InvalidPatternException("a trailing backslash");
            }
            char e = s[i++];
            switch (e)
            {
                case 'd':
                    o.Append('[').Append(Body(Digit)).Append(']');
                    Atom();
                    return;
                case 'D':
                    o.Append(Complemented(Digit));
                    Atom();
                    return;
                case 'w':
                    o.Append('[').Append(Body(Word)).Append(']');
                    Atom();
                    return;
                case 'W':
                    o.Append(Complemented(Word));
                    Atom();
                    return;
                case 's':
                    o.Append('[').Append(Body(Space)).Append(']');
                    Atom();
                    return;
                case 'S':
                    o.Append(Complemented(Space));
                    Atom();
                    return;
                case 'b':
                    o.Append("(?:(?<=[").Append(Body(Word)).Append("])(?![").Append(Body(Word)).Append("])|(?<![")
                        .Append(Body(Word)).Append("])(?=[").Append(Body(Word)).Append("]))");
                    Assertion();
                    return;
                case 'B':
                    o.Append("(?:(?<=[").Append(Body(Word)).Append("])(?=[").Append(Body(Word)).Append("])|(?<![")
                        .Append(Body(Word)).Append("])(?![").Append(Body(Word)).Append("]))");
                    Assertion();
                    return;
                case 'k':
                    if (unicode || names.Count > 0)
                    {
                        int end = i < s.Length && s[i] == '<' ? s.IndexOf('>', i) : -1;
                        string name = end < 0 ? "" : s.Substring(i + 1, end - i - 1);
                        if (!names.TryGetValue(name, out int target))
                        {
                            throw new InvalidPatternException("a \\k reference to no group");
                        }
                        i = end + 1;
                        Reference(target);
                        return;
                    }
                    Identity(e, false);
                    Atom();
                    return;
                case 'p':
                case 'P':
                    if (unicode)
                    {
                        o.Append(Property(e == 'P', false));
                        Atom();
                        return;
                    }
                    Identity(e, false);
                    Atom();
                    return;
            }
            if (e >= '1' && e <= '9')
            {
                int start = i - 1;
                while (i < s.Length && char.IsAsciiDigit(s[i]))
                {
                    i++;
                }
                if (i - start > 6 || !int.TryParse(s.AsSpan(start, i - start), NumberStyles.None, CultureInfo.InvariantCulture, out int n)
                    || n > groupCount)
                {
                    if (unicode)
                    {
                        throw new InvalidPatternException("a back-reference to no group");
                    }
                    throw new UnsupportedException("a legacy octal or identity escape");
                }
                Reference(n);
                return;
            }
            string? single = Common(e, false);
            if (single is not null)
            {
                o.Append(single);
                Atom();
                return;
            }
            Identity(e, false);
            Atom();
        }

        /// <summary>
        /// A back-reference matches the empty string in ECMAScript when its group has not captured (it is later, in
        /// another alternative or in a skipped branch); .NET fails there, so the reference is guarded.
        /// </summary>
        private void Reference(int target)
        {
            string n = target.ToString(CultureInfo.InvariantCulture);
            o.Append("(?(").Append(n).Append(")\\k<").Append(n).Append(">|)");
            references.Add((target, behind > 0));
            Atom();
        }

        /// <summary>The escapes that mean the same inside and outside a set; null when <paramref name="e"/> is none of them.</summary>
        private string? Common(char e, bool inClass)
        {
            lastUnit = -1;
            switch (e)
            {
                case 'f': lastUnit = 12; return "\\f";
                case 'n': lastUnit = 10; return "\\n";
                case 'r': lastUnit = 13; return "\\r";
                case 't': lastUnit = 9; return "\\t";
                case 'v': lastUnit = 11; return "\\u000B";
                case '0':
                    if (i < s.Length && char.IsAsciiDigit(s[i]))
                    {
                        if (unicode)
                        {
                            throw new InvalidPatternException("a legacy octal escape");
                        }
                        throw new UnsupportedException("a legacy octal escape");
                    }
                    lastUnit = 0;
                    return "\\u0000";
                case 'c':
                    if (i < s.Length && char.IsAsciiLetter(s[i]))
                    {
                        lastUnit = s[i] % 32;
                        return U(s[i++] % 32);
                    }
                    if (unicode)
                    {
                        throw new InvalidPatternException("a \\c without a letter");
                    }
                    throw new UnsupportedException("a \\c without a letter (a literal backslash in a pattern without the u flag)");
                case 'x':
                    if (i + 1 < s.Length && Uri.IsHexDigit(s[i]) && Uri.IsHexDigit(s[i + 1]))
                    {
                        string hex = s.Substring(i, 2);
                        i += 2;
                        lastUnit = Convert.ToInt32(hex, 16);
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
                    throw new InvalidPatternException("a malformed \\u{...} escape");
                }
                i = end + 1;
                if (cp <= 0xFFFF)
                {
                    lastUnit = cp;
                    if (cp >= 0xD800 && cp <= 0xDFFF)
                    {
                        Sensitive = true;
                    }
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
                if (unicode && inClass && unit >= 0xD800 && unit <= 0xDBFF && i + 1 < s.Length && s[i] == '\\' && s[i + 1] == 'u'
                    && HexAt(i + 2, 4, out int low2) && low2 >= 0xDC00 && low2 <= 0xDFFF)
                {
                    throw new UnsupportedException("a code point above U+FFFF inside a character class");
                }
                if (unicode && unit >= 0xD800 && unit <= 0xDFFF)
                {
                    Sensitive = true;
                }
                lastUnit = unit;
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
            lastUnit = e;
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
                throw new InvalidPatternException("the escape \\" + e + " (not valid in a Unicode pattern)");
            }
            if (char.IsAsciiDigit(e))
            {
                throw new UnsupportedException("a legacy octal or identity escape");
            }
            if (char.IsSurrogate(e))
            {
                o.Append(e);
                return;
            }
            o.Append(char.IsAsciiLetterOrDigit(e) || e == '_' ? e.ToString() : "\\" + e);
        }

        private string Property(bool negated, bool inClass)
        {
            Sensitive = true;
            if (i >= s.Length || s[i] != '{')
            {
                throw new InvalidPatternException("a malformed \\p escape");
            }
            int end = s.IndexOf('}', i);
            if (end < 0)
            {
                throw new InvalidPatternException("a malformed \\p escape");
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

        /// <summary>An astral code point written at <paramref name="at"/> (a raw pair, a \u{...} or a \uD83D\uDE00 escape), or -1.</summary>
        private int PeekAstral(int at, out int length)
        {
            length = 0;
            if (at >= s.Length)
            {
                return -1;
            }
            if (char.IsHighSurrogate(s[at]) && at + 1 < s.Length && char.IsLowSurrogate(s[at + 1]))
            {
                length = 2;
                return char.ConvertToUtf32(s[at], s[at + 1]);
            }
            if (s[at] != '\\' || at + 2 >= s.Length || s[at + 1] != 'u')
            {
                return -1;
            }
            if (s[at + 2] == '{')
            {
                int end = s.IndexOf('}', at);
                if (end > at + 3 && int.TryParse(s.AsSpan(at + 3, end - at - 3), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out int cp)
                    && cp > 0xFFFF && cp <= 0x10FFFF)
                {
                    length = end + 1 - at;
                    return cp;
                }
                return -1;
            }
            if (HexAt(at + 2, 4, out int hi) && hi >= 0xD800 && hi <= 0xDBFF && at + 7 < s.Length && s[at + 6] == '\\' && s[at + 7] == 'u'
                && HexAt(at + 8, 4, out int lo) && lo >= 0xDC00 && lo <= 0xDFFF)
            {
                length = 12;
                return char.ConvertToUtf32((char)hi, (char)lo);
            }
            return -1;
        }

        /// <summary>The surrogate-pair alternatives that match the code points <paramref name="lo"/> to <paramref name="hi"/>.</summary>
        private static string AstralRange(int lo, int hi)
        {
            string a = char.ConvertFromUtf32(lo), b = char.ConvertFromUtf32(hi);
            int hl = a[0], ll = a[1], hh = b[0], lh = b[1];
            if (hl == hh)
            {
                return U(hl) + "[" + U(ll) + "-" + U(lh) + "]";
            }
            var parts = new List<string> { U(hl) + "[" + U(ll) + "-\\uDFFF]" };
            if (hl + 1 <= hh - 1)
            {
                parts.Add("[" + U(hl + 1) + "-" + U(hh - 1) + "][\\uDC00-\\uDFFF]");
            }
            parts.Add(U(hh) + "[\\uDC00-" + U(lh) + "]");
            return string.Join("|", parts);
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
                if (!negate)
                {
                    o.Append("(?!)");
                }
                else
                {
                    o.Append(unicode ? "(?>" + Pair + "|[\\u0000-\\uFFFF])" : "[\\u0000-\\uFFFF]");
                }
                return;
            }
            var b = new StringBuilder();
            var astral = new List<string>();
            bool lastWasSet = false;
            int prev = -1;
            bool range = false;

            void Unit(int cu)
            {
                if (range && prev >= 0 && unicode && prev <= 0xDFFF && cu >= 0xD800)
                {
                    Sensitive = true;
                }
                if (unicode && cu >= 0xD800 && cu <= 0xDFFF)
                {
                    Sensitive = true;
                }
                prev = cu;
                range = false;
            }

            while (true)
            {
                if (i >= s.Length)
                {
                    throw new InvalidPatternException("an unterminated character class");
                }
                if (unicode)
                {
                    int cp = PeekAstral(i, out int len);
                    if (cp > 0)
                    {
                        if (negate || range)
                        {
                            throw new UnsupportedException("a code point above U+FFFF inside a negated character class or a range from a BMP character");
                        }
                        i += len;
                        int top = cp;
                        if (i < s.Length && s[i] == '-' && i + 1 < s.Length && s[i + 1] != ']')
                        {
                            int end = PeekAstral(i + 1, out int len2);
                            if (end < 0)
                            {
                                throw new UnsupportedException("a range from a code point above U+FFFF to a BMP character");
                            }
                            if (end < cp)
                            {
                                throw new InvalidPatternException("a class range out of order");
                            }
                            top = end;
                            i += 1 + len2;
                        }
                        astral.Add(AstralRange(cp, top));
                        lastWasSet = false;
                        prev = -1;
                        continue;
                    }
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
                        throw new InvalidPatternException("a range that starts at a class escape");
                    }
                    b.Append("\\-");
                    lastWasSet = false;
                    prev = -1;
                    continue;
                }
                lastWasSet = false;
                if (c == '-' && i < s.Length && s[i] == '\\' && i + 1 < s.Length && "dDwWsS".Contains(s[i + 1], StringComparison.Ordinal))
                {
                    if (unicode)
                    {
                        throw new InvalidPatternException("a range that ends at a class escape");
                    }
                    b.Append("\\-");
                    prev = -1;
                    continue;
                }
                if (c == '-' && prev >= 0 && i < s.Length && s[i] != ']')
                {
                    range = true;
                    b.Append('-');
                    continue;
                }
                if (c == '[')
                {
                    b.Append("\\[");
                    Unit('[');
                    continue;
                }
                if (c != '\\')
                {
                    b.Append(c);
                    Unit(c);
                    continue;
                }
                if (i >= s.Length)
                {
                    throw new InvalidPatternException("a trailing backslash");
                }
                char e = s[i++];
                switch (e)
                {
                    case 'd': b.Append(Body(Digit)); lastWasSet = true; prev = -1; continue;
                    case 'w': b.Append(Body(Word)); lastWasSet = true; prev = -1; continue;
                    case 's': b.Append(Body(Space)); lastWasSet = true; prev = -1; continue;
                    case 'D': b.Append(Body(Complement(Digit))); lastWasSet = true; prev = -1; MarkAstral(); continue;
                    case 'W': b.Append(Body(Complement(Word))); lastWasSet = true; prev = -1; MarkAstral(); continue;
                    case 'S': b.Append(Body(Complement(Space))); lastWasSet = true; prev = -1; MarkAstral(); continue;
                    case 'b': b.Append("\\u0008"); Unit(8); continue;
                    case 'p':
                    case 'P':
                        if (unicode)
                        {
                            b.Append(Property(e == 'P', true));
                            lastWasSet = true;
                            prev = -1;
                            continue;
                        }
                        break;
                }
                string? common = Common(e, true);
                if (common is not null)
                {
                    b.Append(common);
                    Unit(lastUnit);
                    continue;
                }
                var saved = o.Length;
                Identity(e, true);
                b.Append(o.ToString(saved, o.Length - saved));
                o.Length = saved;
                Unit(lastUnit);
            }
            if (negate)
            {
                o.Append(Negated(b.ToString()));
            }
            else if (astral.Count > 0)
            {
                o.Append("(?:");
                if (b.Length > 0)
                {
                    o.Append('[').Append(b).Append("]|");
                }
                o.Append(string.Join("|", astral)).Append(')');
            }
            else
            {
                o.Append('[').Append(b).Append(']');
            }
        }

        private void MarkAstral()
        {
            if (unicode)
            {
                Sensitive = true;
            }
        }
    }
}
