// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Net.Sockets;
using System.Security.Authentication;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

internal enum OutcomeKind
{
    Response,
    Streaming,
    NotSent,
    Lost,
    Timeout,
    /// <summary>The caller's token ended the attempt; Sent says whether the request had been written.</summary>
    Aborted,
}

/// <summary>What one attempt gave.</summary>
internal sealed record AttemptOutcome
{
    public OutcomeKind Kind { get; init; }
    public int Status { get; init; }
    public IReadOnlyDictionary<string, string> Headers { get; init; } = new Dictionary<string, string>();
    public byte[]? Body { get; init; }
    public bool BodyOk { get; init; }
    public string? BodyFailure { get; init; }
    public EventBody? Stream { get; init; }
    public string Detail { get; init; } = "";
    public bool Sent { get; init; }
}

internal sealed record AttemptRequest
{
    public required string Url { get; init; }
    public required string Method { get; init; }
    public required IReadOnlyList<HeaderEntry> Headers { get; init; }
    public Payload Body { get; init; } = Payload.EmptyPayload;
    public TimeSpan Timeout { get; init; }
    /// <summary>Hands a 2xx text/event-stream answer back unread.</summary>
    public bool Stream { get; init; }
    /// <summary>With Stream: the silence after which a read of the body times out; null means Timeout.</summary>
    public TimeSpan? Idle { get; init; }
}

internal enum BodyRead
{
    Chunk,
    End,
    Timeout,
    Lost,
}

/// <summary>The unread body of an event stream; each read waits at most the idle time.</summary>
internal sealed class EventBody
{
    private readonly HttpResponseMessage response;
    private readonly Stream stream;
    private readonly CancellationTokenSource cancel;
    private readonly TimeSpan idle;
    private readonly byte[] buffer = new byte[32 * 1024];
    private int closed;

    public EventBody(HttpResponseMessage response, Stream stream, CancellationTokenSource cancel, TimeSpan idle)
    {
        this.response = response;
        this.stream = stream;
        this.cancel = cancel;
        this.idle = idle;
    }

    public async Task<(byte[] Chunk, BodyRead Kind)> ReadAsync(CancellationToken caller)
    {
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancel.Token, caller);
        timer.CancelAfter(idle);
        try
        {
            int n = await stream.ReadAsync(buffer.AsMemory(), timer.Token).ConfigureAwait(false);
            if (n == 0)
            {
                return (System.Array.Empty<byte>(), BodyRead.End);
            }
            return (buffer.AsSpan(0, n).ToArray(), BodyRead.Chunk);
        }
        catch (OperationCanceledException)
        {
            if (!cancel.IsCancellationRequested && !caller.IsCancellationRequested)
            {
                Close();
                return (System.Array.Empty<byte>(), BodyRead.Timeout);
            }
            return (System.Array.Empty<byte>(), BodyRead.Lost);
        }
        catch (Exception e) when (e is IOException or HttpRequestException or ObjectDisposedException)
        {
            return (System.Array.Empty<byte>(), BodyRead.Lost);
        }
    }

    public void Close()
    {
        if (Interlocked.Exchange(ref closed, 1) == 1)
        {
            return;
        }
        try
        {
            cancel.Cancel();
        }
        catch (ObjectDisposedException)
        {
        }
        stream.Dispose();
        response.Dispose();
        cancel.Dispose();
    }
}

/// <summary>Request content that notes when it starts going out, so a cancelled call knows whether it was sent.</summary>
internal sealed class TrackedContent : HttpContent
{
    private readonly byte[] data;

    public TrackedContent(byte[] data)
    {
        this.data = data;
    }

    public bool Started { get; private set; }

    protected override async Task SerializeToStreamAsync(Stream stream, TransportContext? context)
    {
        Started = true;
        await stream.WriteAsync(data).ConfigureAwait(false);
    }

    protected override async Task SerializeToStreamAsync(Stream stream, TransportContext? context, CancellationToken cancellationToken)
    {
        Started = true;
        await stream.WriteAsync(data, cancellationToken).ConfigureAwait(false);
    }

    protected override bool TryComputeLength(out long length)
    {
        length = data.Length;
        return true;
    }
}

internal static class Transport
{
    /// <summary>
    /// The default client: HTTP/1.1, no redirects (the runtime follows same-origin redirects of
    /// reads itself, and never on mutations), no cookie jar, no compression, no trace headers,
    /// and no timeout of its own (each attempt has a deadline).
    /// </summary>
    internal static HttpClient NewHttpClient()
    {
        var handler = new SocketsHttpHandler
        {
            AllowAutoRedirect = false,
            UseCookies = false,
            AutomaticDecompression = DecompressionMethods.None,
            ActivityHeadersPropagator = null,
            RequestHeaderEncodingSelector = (_, _) => Encoding.Latin1,
            ResponseHeaderEncodingSelector = (_, _) => Encoding.Latin1,
            PooledConnectionLifetime = TimeSpan.FromMinutes(5),
        };
        return new HttpClient(handler) { Timeout = System.Threading.Timeout.InfiniteTimeSpan };
    }

    internal static bool IsEventStream(IReadOnlyDictionary<string, string> headers) =>
        headers.TryGetValue("content-type", out var v) &&
        string.Equals(v.Split(';')[0].Trim(), "text/event-stream", StringComparison.OrdinalIgnoreCase);

    private static List<string> ErrorCodes(Exception e)
    {
        var codes = new List<string>();
        void Push(string code)
        {
            if (!codes.Contains(code))
            {
                codes.Add(code);
            }
        }
        for (Exception? cur = e; cur is not null; cur = cur.InnerException)
        {
            if (cur is SocketException se)
            {
                switch (se.SocketErrorCode)
                {
                    case SocketError.ConnectionRefused: Push("ECONNREFUSED"); break;
                    case SocketError.ConnectionReset: Push("ECONNRESET"); break;
                    case SocketError.ConnectionAborted: Push("ECONNABORTED"); break;
                    case SocketError.NotConnected: Push("ENOTCONN"); break;
                    case SocketError.Shutdown: Push("EPIPE"); break;
                    case SocketError.TimedOut: Push("ETIMEDOUT"); break;
                    case SocketError.AddressNotAvailable: Push("EADDRNOTAVAIL"); break;
                    case SocketError.HostUnreachable: Push("EHOSTUNREACH"); break;
                    case SocketError.NetworkUnreachable: Push("ENETUNREACH"); break;
                    case SocketError.HostNotFound:
                    case SocketError.TryAgain:
                    case SocketError.NoData:
                        Push("ENOTFOUND");
                        break;
                }
            }
            if (cur is AuthenticationException)
            {
                Push("CERT_VERIFY_FAILED");
            }
            if (cur is HttpRequestException hre)
            {
                switch (hre.HttpRequestError)
                {
                    case HttpRequestError.NameResolutionError: Push("ENOTFOUND"); break;
                    case HttpRequestError.SecureConnectionError: Push("TLS_ERROR"); break;
                    case HttpRequestError.ResponseEnded: Push("CONNECTION_CLOSED"); break;
                }
            }
            if (cur is EndOfStreamException)
            {
                Push("CONNECTION_CLOSED");
            }
        }
        return codes;
    }

    /// <summary>Whether a failure happened before the request could reach the server: DNS, connect, TLS handshake.</summary>
    private static bool NotSent(Exception e) =>
        e is HttpRequestException hre && hre.HttpRequestError is HttpRequestError.ConnectionError or HttpRequestError.NameResolutionError or HttpRequestError.SecureConnectionError;

    private static string FailureDetail(Exception e, bool sent)
    {
        var codes = ErrorCodes(e);
        if (codes.Count == 0)
        {
            return sent ? "network error" : "connect error";
        }
        return string.Join(", ", codes);
    }

    private static byte[] MultipartBody(IReadOnlyList<MultipartPart> parts, out string contentType)
    {
        var boundary = Convert.ToHexString(Util.RandomBytes(30)).ToLowerInvariant();
        var b = new MemoryStream();
        void Write(string s)
        {
            var bytes = Util.Utf8.GetBytes(s);
            b.Write(bytes, 0, bytes.Length);
        }
        static string Escape(string s) => s.Replace("\\", "\\\\", StringComparison.Ordinal).Replace("\"", "\\\"", StringComparison.Ordinal)
            .Replace("\r", "%0D", StringComparison.Ordinal).Replace("\n", "%0A", StringComparison.Ordinal);
        foreach (var p in parts)
        {
            Write("--" + boundary + "\r\n");
            switch (p.Kind)
            {
                case PartKind.File:
                    Write($"Content-Disposition: form-data; name=\"{Escape(p.Name)}\"; filename=\"{Escape(p.Filename)}\"\r\n");
                    Write($"Content-Type: {p.ContentType}\r\n\r\n");
                    b.Write(p.Data, 0, p.Data.Length);
                    break;
                case PartKind.Json:
                    Write($"Content-Disposition: form-data; name=\"{Escape(p.Name)}\"\r\n");
                    Write("Content-Type: application/json\r\n\r\n");
                    Write(p.Text);
                    break;
                default:
                    Write($"Content-Disposition: form-data; name=\"{Escape(p.Name)}\"\r\n\r\n");
                    Write(p.Text);
                    break;
            }
            Write("\r\n");
        }
        Write("--" + boundary + "--\r\n");
        contentType = "multipart/form-data; boundary=" + boundary;
        return b.ToArray();
    }

    private static Dictionary<string, string> ResponseHeaders(HttpResponseMessage resp)
    {
        var headers = new Dictionary<string, string>(StringComparer.Ordinal);
        IEnumerable<KeyValuePair<string, IEnumerable<string>>> all = resp.Headers;
        if (resp.Content is not null)
        {
            all = all.Concat(resp.Content.Headers);
        }
        foreach (var kv in all)
        {
            var lower = kv.Key.ToLowerInvariant();
            foreach (var value in kv.Value)
            {
                if (headers.TryGetValue(lower, out var known) && lower != "set-cookie")
                {
                    headers[lower] = known + ", " + value;
                }
                else
                {
                    headers[lower] = value;
                }
            }
        }
        return headers;
    }

    /// <summary>Sends once. It never throws: every outcome is an <see cref="AttemptOutcome"/>.</summary>
    internal static async Task<AttemptOutcome> AttemptAsync(HttpClient client, AttemptRequest req, CancellationToken caller)
    {
        HttpRequestMessage message;
        TrackedContent? content = null;
        try
        {
            message = new HttpRequestMessage(new HttpMethod(req.Method), req.Url)
            {
                Version = HttpVersion.Version11,
                VersionPolicy = HttpVersionPolicy.RequestVersionExact,
            };
        }
        catch (Exception e) when (e is UriFormatException or ArgumentException or InvalidOperationException)
        {
            return new AttemptOutcome { Kind = OutcomeKind.NotSent, Detail = "INVALID_REQUEST" };
        }
        string? multipartType = null;
        if (!req.Body.Empty)
        {
            byte[] data = req.Body.Bytes;
            if (req.Body.Multipart is { } parts)
            {
                data = MultipartBody(parts, out var type);
                multipartType = type;
            }
            content = new TrackedContent(data);
            message.Content = content;
            if (multipartType is not null)
            {
                message.Content.Headers.TryAddWithoutValidation("Content-Type", multipartType);
            }
        }
        else if (req.Method is "POST" or "PUT" or "PATCH")
        {
            // A POST, PUT or PATCH without a body says so (content-length: 0, RFC 9110 8.6), as
            // fetch, httpx and net/http do.
            content = new TrackedContent(System.Array.Empty<byte>());
            message.Content = content;
        }
        message.Headers.ExpectContinue = false;
        bool hasAccept = false;
        foreach (var h in req.Headers)
        {
            if (string.Equals(h.Name, "Content-Type", StringComparison.OrdinalIgnoreCase))
            {
                if (message.Content is not null && multipartType is null)
                {
                    message.Content.Headers.Remove("Content-Type");
                    message.Content.Headers.TryAddWithoutValidation("Content-Type", h.Value);
                }
                continue;
            }
            if (string.Equals(h.Name, "Content-Length", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }
            if (string.Equals(h.Name, "Accept", StringComparison.OrdinalIgnoreCase))
            {
                hasAccept = true;
            }
            if (!message.Headers.TryAddWithoutValidation(h.Name, h.Value) && message.Content is not null)
            {
                message.Content.Headers.TryAddWithoutValidation(h.Name, h.Value);
            }
        }
        // Without a declared media type the request accepts anything, as the HTTP clients of the
        // other runtimes send it.
        if (!hasAccept)
        {
            message.Headers.TryAddWithoutValidation("Accept", "*/*");
        }
        var attemptCts = CancellationTokenSource.CreateLinkedTokenSource(caller);
        bool timedOut = false;
        using var timer = new Timer(_ =>
        {
            timedOut = true;
            try
            {
                attemptCts.Cancel();
            }
            catch (ObjectDisposedException)
            {
            }
        }, null, req.Timeout, System.Threading.Timeout.InfiniteTimeSpan);
        HttpResponseMessage resp;
        try
        {
            resp = await client.SendAsync(message, HttpCompletionOption.ResponseHeadersRead, attemptCts.Token).ConfigureAwait(false);
        }
        catch (Exception e)
        {
            await timer.DisposeAsync().ConfigureAwait(false);
            attemptCts.Dispose();
            message.Dispose();
            if (timedOut)
            {
                return new AttemptOutcome { Kind = OutcomeKind.Timeout };
            }
            if (caller.IsCancellationRequested)
            {
                return new AttemptOutcome { Kind = OutcomeKind.Aborted, Sent = content is null || content.Started };
            }
            if (NotSent(e))
            {
                return new AttemptOutcome { Kind = OutcomeKind.NotSent, Detail = FailureDetail(e, false) };
            }
            if (e is TaskCanceledException or OperationCanceledException)
            {
                return new AttemptOutcome { Kind = OutcomeKind.Timeout };
            }
            if (e is InvalidOperationException or UriFormatException or ArgumentException)
            {
                return new AttemptOutcome { Kind = OutcomeKind.NotSent, Detail = "INVALID_REQUEST" };
            }
            var detail = FailureDetail(e, true);
            if (detail == "network error")
            {
                detail = "CONNECTION_CLOSED";
            }
            return new AttemptOutcome { Kind = OutcomeKind.Lost, Detail = detail };
        }
        var headers = ResponseHeaders(resp);
        int status = (int)resp.StatusCode;
        if (req.Stream && status >= 200 && status <= 299 && IsEventStream(headers))
        {
            await timer.DisposeAsync().ConfigureAwait(false);
            if (timedOut)
            {
                resp.Dispose();
                attemptCts.Dispose();
                return new AttemptOutcome { Kind = OutcomeKind.Timeout };
            }
            Stream body;
            try
            {
                body = await resp.Content.ReadAsStreamAsync(attemptCts.Token).ConfigureAwait(false);
            }
            catch (Exception)
            {
                resp.Dispose();
                attemptCts.Dispose();
                return new AttemptOutcome { Kind = OutcomeKind.Lost, Detail = "CONNECTION_CLOSED" };
            }
            return new AttemptOutcome
            {
                Kind = OutcomeKind.Streaming,
                Status = status,
                Headers = headers,
                Stream = new EventBody(resp, body, attemptCts, req.Idle ?? req.Timeout),
            };
        }
        var outcome = new AttemptOutcome { Kind = OutcomeKind.Response, Status = status, Headers = headers };
        try
        {
            var data = await resp.Content.ReadAsByteArrayAsync(attemptCts.Token).ConfigureAwait(false);
            outcome = outcome with { Body = data, BodyOk = true };
        }
        catch (Exception)
        {
            outcome = outcome with { BodyFailure = timedOut ? "timeout" : "broken" };
        }
        finally
        {
            await timer.DisposeAsync().ConfigureAwait(false);
            resp.Dispose();
            attemptCts.Dispose();
            message.Dispose();
        }
        return outcome;
    }

    // ------------------------------------------------------------ retry-after

    private static long DaysFromCivil(long year, long month, long day)
    {
        long y = month <= 2 ? year - 1 : year;
        long era = y >= 0 ? y / 400 : (y - 399) / 400;
        long yoe = y - era * 400;
        long mp = (month + 9) % 12;
        long doy = (153 * mp + 2) / 5 + day - 1;
        long doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        return era * 146097 + doe - 719468;
    }

    private static readonly string[] MonthNames = { "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec" };
    private static readonly string[] MonthFull = { "january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december" };

    private static bool MonthNumber(string token, out long month)
    {
        month = 0;
        var lower = token.ToLowerInvariant();
        if (lower.Length < 3)
        {
            return false;
        }
        for (int i = 0; i < MonthNames.Length; i++)
        {
            if (lower[..3] == MonthNames[i] && (lower.Length == 3 || lower == MonthFull[i] || lower == "sept"))
            {
                month = i + 1;
                return true;
            }
        }
        return false;
    }

    private static bool ParseDigits(string s, out long n)
    {
        n = 0;
        return s.Length > 0 && s.All(char.IsAsciiDigit) && long.TryParse(s, NumberStyles.None, CultureInfo.InvariantCulture, out n);
    }

    private static bool ParseClock(string token, out long ms)
    {
        ms = 0;
        var parts = token.Split(':');
        if (parts.Length < 2 || parts.Length > 3)
        {
            return false;
        }
        if (!ParseDigits(parts[0], out var hour) || !ParseDigits(parts[1], out var minute) || hour >= 24 || minute >= 60)
        {
            return false;
        }
        double second = 0;
        if (parts.Length == 3)
        {
            if (!double.TryParse(parts[2], NumberStyles.Float, CultureInfo.InvariantCulture, out second) || second < 0 || second >= 61)
            {
                return false;
            }
        }
        ms = (hour * 3600 + minute * 60) * 1000 + (long)(second * 1000 + 0.5);
        return true;
    }

    private static bool ParseZone(string token, out long ms)
    {
        ms = 0;
        switch (token.ToUpperInvariant())
        {
            case "GMT":
            case "UTC":
            case "UT":
            case "Z":
                return true;
        }
        if (token.Length == 0)
        {
            return false;
        }
        long sign = token[0] switch { '+' => 1, '-' => -1, _ => 0 };
        if (sign == 0)
        {
            return false;
        }
        var digits = token[1..].Replace(":", "", StringComparison.Ordinal);
        if (digits.Length != 4 || !ParseDigits(digits[..2], out var hours) || !ParseDigits(digits[2..], out var minutes))
        {
            return false;
        }
        ms = sign * (hours * 60 + minutes) * 60000;
        return true;
    }

    private static bool ParseIsoDate(string text, out long ms)
    {
        ms = 0;
        string date = text;
        string rest = "";
        bool hasRest = false;
        int i = text.IndexOfAny(new[] { 'T', ' ' });
        if (i >= 0)
        {
            date = text[..i];
            rest = text[(i + 1)..];
            hasRest = true;
        }
        var fields = date.Split('-');
        if (fields.Length != 3 || !ParseDigits(fields[0], out var year) || !ParseDigits(fields[1], out var month) || !ParseDigits(fields[2], out var day) ||
            month < 1 || month > 12 || day < 1 || day > 31 || year > 9999)
        {
            return false;
        }
        long clock = 0, zone = 0;
        if (hasRest)
        {
            int split = rest.IndexOfAny(new[] { 'Z', 'z', '+', '-' });
            if (split < 0)
            {
                split = rest.Length;
            }
            if (!ParseClock(rest[..split], out clock))
            {
                return false;
            }
            if (split < rest.Length && !ParseZone(rest[split..], out zone))
            {
                return false;
            }
        }
        ms = DaysFromCivil(year, month, day) * 86400000 + clock - zone;
        return true;
    }

    /// <summary>An HTTP date (IMF-fixdate, RFC 850, asctime) or an ISO 8601 timestamp as epoch milliseconds.</summary>
    internal static bool ParseHttpDate(string text, out long ms)
    {
        ms = 0;
        text = text.Trim();
        if (text.Length >= 8 && char.IsAsciiDigit(text[0]) && text[4] == '-')
        {
            return ParseIsoDate(text, out ms);
        }
        long day = 0, month = 0, year = 0, clock = 0, zone = 0;
        bool hasDay = false, hasMonth = false, hasYear = false;
        foreach (var token in text.Split(new[] { ' ', ',' }, StringSplitOptions.RemoveEmptyEntries))
        {
            if (token.Contains(':', StringComparison.Ordinal))
            {
                if (!ParseClock(token, out clock))
                {
                    return false;
                }
            }
            else if (token.Contains('-', StringComparison.Ordinal) && char.IsAsciiDigit(token[0]))
            {
                var parts = token.Split('-');
                if (parts.Length < 3 || !ParseDigits(parts[0], out var d) || !MonthNumber(parts[1], out var m) || !ParseDigits(parts[2], out var y))
                {
                    return false;
                }
                if (y < 50)
                {
                    y += 2000;
                }
                else if (y < 100)
                {
                    y += 1900;
                }
                day = d; month = m; year = y;
                hasDay = hasMonth = hasYear = true;
            }
            else if (Util.AllDigits(token))
            {
                ParseDigits(token, out var n);
                if (!hasDay && token.Length <= 2)
                {
                    day = n;
                    hasDay = true;
                }
                else
                {
                    year = n;
                    hasYear = true;
                }
            }
            else if (MonthNumber(token, out var m))
            {
                month = m;
                hasMonth = true;
            }
            else if (ParseZone(token, out var z))
            {
                zone = z;
            }
            else if (!token.All(c => (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '.'))
            {
                return false;
            }
        }
        if (!hasDay || !hasMonth || !hasYear || day < 1 || day > 31 || year > 9999)
        {
            return false;
        }
        ms = DaysFromCivil(year, month, day) * 86400000 + clock - zone;
        return true;
    }

    /// <summary>Retry-After in milliseconds (delta seconds or HTTP date).</summary>
    internal static long? ParseRetryAfter(string? value, long nowMs)
    {
        if (value is null)
        {
            return null;
        }
        var trimmed = value.Trim();
        if (Util.AllDigits(trimmed))
        {
            if (!ulong.TryParse(trimmed, NumberStyles.None, CultureInfo.InvariantCulture, out var n))
            {
                return null;
            }
            if (n > (1UL << 62) / 1000)
            {
                return 1L << 62;
            }
            return (long)(n * 1000);
        }
        if (!ParseHttpDate(trimmed, out var date))
        {
            return null;
        }
        return Math.Max(date - nowMs, 0);
    }

    private static readonly Regex RelNext = new("(?i);\\s*rel\\s*=\\s*\"?([^\";]*\\s)?next(\\s[^\";]*)?\"?\\s*(;|$)", RegexOptions.CultureInvariant);
    private static readonly Regex LinkTarget = new("^\\s*<([^>]*)>(.*)$", RegexOptions.Singleline | RegexOptions.CultureInvariant);

    /// <summary>The next page URL of a Link header (rel="next").</summary>
    internal static string? NextLink(string? header)
    {
        if (string.IsNullOrEmpty(header))
        {
            return null;
        }
        var parts = new List<string>();
        int start = 0;
        for (int i = 0; i < header.Length; i++)
        {
            if (header[i] == ',' && header[(i + 1)..].TrimStart(' ', '\t', '\r', '\n').StartsWith('<'))
            {
                parts.Add(header[start..i]);
                start = i + 1;
            }
        }
        parts.Add(header[start..]);
        foreach (var part in parts)
        {
            var m = LinkTarget.Match(part);
            if (m.Success && RelNext.IsMatch(m.Groups[2].Value))
            {
                return m.Groups[1].Value;
            }
        }
        return null;
    }
}
