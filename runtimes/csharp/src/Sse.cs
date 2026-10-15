// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;

namespace Tungsten.Runtime;

/// <summary>One dispatched server-sent event.</summary>
internal sealed record SseEvent(string Event, string Data, string? Id, long? Retry);

/// <summary>
/// A server-sent events parser (WHATWG HTML 9.2): lines end with CRLF, LF or CR; a line is a
/// comment (":"), a field (event, data, id, retry; the value loses one leading space) or the
/// blank line that dispatches the event. data lines are joined with LF; an event without data
/// lines is not dispatched; an event still open when the stream ends is discarded. An event may
/// hold at most max bytes (its field lines, each with a one-byte terminator, plus the line being
/// read); past it the parser stops.
/// </summary>
internal sealed class SseParser
{
    private readonly int max;
    private readonly StringBuilder pending = new();
    private int lineBytes;
    private bool pendingCr;
    private bool started;
    private string eventName = "";
    private readonly StringBuilder data = new();
    private bool hasData;
    private string? id;
    private long? retry;
    private int eventBytes;

    public SseParser(int max)
    {
        this.max = max;
    }

    public bool Exceeded { get; private set; }

    private static int Bytes(ReadOnlySpan<char> s) => Encoding.UTF8.GetByteCount(s);

    /// <summary>Adds chunk[from..to] to the line being read; each fragment is scanned and counted once.</summary>
    private void Append(string chunk, int from, int to)
    {
        if (from >= to)
        {
            return;
        }
        int n = pending.Length;
        if (n > 0 && char.IsHighSurrogate(pending[n - 1]) && char.IsLowSurrogate(chunk[from]))
        {
            // The pair was counted as two unpaired surrogates (three bytes each); it is four bytes.
            lineBytes -= 2;
        }
        lineBytes += Bytes(chunk.AsSpan(from, to - from));
        pending.Append(chunk, from, to - from);
    }

    /// <summary>The line read so far ends: it is parsed and the buffer is emptied.</summary>
    private void Complete(List<SseEvent> outList)
    {
        var text = pending.ToString();
        pending.Clear();
        lineBytes = 0;
        Line(text, outList);
    }

    /// <summary>Feeds decoded text and returns the events it completes. A CR at the end is held back until the next text shows whether an LF follows.</summary>
    public List<SseEvent> Push(string chunk)
    {
        var outList = new List<SseEvent>();
        if (Exceeded)
        {
            return outList;
        }
        if (!started && chunk.Length > 0)
        {
            started = true;
            if (chunk[0] == '\uFEFF')
            {
                chunk = chunk[1..];
            }
        }
        int start = 0;
        if (pendingCr)
        {
            if (chunk.Length == 0)
            {
                return outList;
            }
            pendingCr = false;
            Complete(outList);
            if (Exceeded)
            {
                return outList;
            }
            if (chunk[0] == '\n')
            {
                start = 1;
            }
        }
        for (int i = start; i < chunk.Length; i++)
        {
            char c = chunk[i];
            if (c != '\n' && c != '\r')
            {
                continue;
            }
            Append(chunk, start, i);
            if (c == '\r' && i + 1 == chunk.Length)
            {
                // The held CR counts toward the limit: it is the terminator of the line being read.
                pendingCr = true;
                lineBytes += 1;
                CheckLimit();
                return outList;
            }
            Complete(outList);
            if (Exceeded)
            {
                return outList;
            }
            if (c == '\r' && chunk[i + 1] == '\n')
            {
                i++;
            }
            start = i + 1;
        }
        Append(chunk, start, chunk.Length);
        CheckLimit();
        return outList;
    }

    /// <summary>The event and the line being read hold more than max bytes: the parser stops.</summary>
    private void CheckLimit()
    {
        if (eventBytes + lineBytes > max)
        {
            Exceeded = true;
            pending.Clear();
            lineBytes = 0;
        }
    }

    /// <summary>The stream ended; a held-back CR ends its line, the rest is discarded.</summary>
    public List<SseEvent> End()
    {
        var outList = new List<SseEvent>();
        if (!Exceeded && pendingCr)
        {
            Complete(outList);
        }
        pendingCr = false;
        pending.Clear();
        lineBytes = 0;
        eventName = "";
        data.Clear();
        hasData = false;
        return outList;
    }

    private void Line(string line, List<SseEvent> outList)
    {
        if (line.Length == 0)
        {
            if (hasData)
            {
                var text = data.ToString();
                text = text[..^1];
                outList.Add(new SseEvent(eventName.Length == 0 ? "message" : eventName, text, id, retry));
            }
            eventName = "";
            data.Clear();
            hasData = false;
            eventBytes = 0;
            return;
        }
        if (line.StartsWith(':'))
        {
            return;
        }
        eventBytes += Bytes(line) + 1;
        if (eventBytes > max)
        {
            Exceeded = true;
            return;
        }
        int colon = line.IndexOf(':');
        string name = colon < 0 ? line : line[..colon];
        string value = colon < 0 ? "" : line[(colon + 1)..];
        if (value.StartsWith(' '))
        {
            value = value[1..];
        }
        switch (name)
        {
            case "event":
                eventName = value;
                break;
            case "data":
                data.Append(value).Append('\n');
                hasData = true;
                break;
            case "id":
                if (!value.Contains('\0'))
                {
                    id = value;
                }
                break;
            case "retry":
                if (Util.AllDigits(value) && long.TryParse(value, NumberStyles.None, CultureInfo.InvariantCulture, out var ms) && ms <= 9007199254740991)
                {
                    retry = ms;
                }
                break;
        }
    }
}

/// <summary>Joins UTF-8 sequences split across chunks; invalid bytes become U+FFFD.</summary>
internal sealed class Utf8Chunks
{
    private readonly System.Text.Decoder decoder = new UTF8Encoding(false, false).GetDecoder();

    public string Decode(byte[] data)
    {
        var chars = new char[decoder.GetCharCount(data, 0, data.Length, false)];
        int n = decoder.GetChars(data, 0, data.Length, chars, 0, false);
        return new string(chars, 0, n);
    }

    public string Finish()
    {
        var chars = new char[decoder.GetCharCount(System.Array.Empty<byte>(), 0, 0, true)];
        int n = decoder.GetChars(System.Array.Empty<byte>(), 0, 0, chars, 0, true);
        return new string(chars, 0, n);
    }
}
