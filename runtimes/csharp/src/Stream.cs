// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

/// <summary>One event of a stream, with its data as decoded JSON.</summary>
public sealed record StreamEvent
{
    /// <summary>The event's data, decoded from JSON.</summary>
    public JsonNode? Value { get; init; }
    /// <summary>The event field, "message" when the event has none.</summary>
    public string Event { get; init; } = "message";
    /// <summary>The last event id the stream has set so far (null before any).</summary>
    public string? Id { get; init; }
    /// <summary>The last retry value (milliseconds) set so far.</summary>
    public long? Retry { get; init; }
    /// <summary>The answer of the connection the event arrived on.</summary>
    public ResponseMeta Meta { get; init; } = new();
    /// <summary>How often the stream reconnected up to this event.</summary>
    public int Reconnects { get; init; }
}

/// <summary>One item of a stream: an event, or the failure that ended the stream.</summary>
public abstract record StreamItem<T>
{
    private protected StreamItem() { }

    /// <summary>An event with its typed value.</summary>
    public sealed record Ok(T Value, StreamEvent Raw) : StreamItem<T>;

    /// <summary>The failure that ended the stream (the last item).</summary>
    public sealed record Err(TungstenError Error) : StreamItem<T>;
}

/// <summary>What the consuming helpers of a stream return: the result, or what was gathered before a failure with the failure as Error.</summary>
public sealed record Folded<TResult>(TResult Value, TungstenError? Error, int Reconnects);

/// <summary>
/// The server-sent events of an operation: the request is sent when the stream is first read; a
/// failure ends the stream as its last item. A stream reconnects (with Last-Event-ID) after it
/// dropped when the call is safe to repeat, never delivering an event twice. Dispose it, or read
/// it to its end, to release the connection.
/// </summary>
public sealed class EventStream<T> : IAsyncEnumerable<StreamItem<T>>, IAsyncDisposable
{
    private readonly StreamCore core;
    private readonly Func<StreamEvent, int, T> decode;

    internal EventStream(StreamCore core, Func<StreamEvent, int, T> decode)
    {
        this.core = core;
        this.decode = decode;
    }

    /// <summary>The operation the stream belongs to.</summary>
    public OperationDescriptor Operation => core.Op;

    /// <summary>
    /// The same stream with each event's value as decoded JSON (the typed decoding still runs, so
    /// a value the SDK's type refuses still ends the stream).
    /// </summary>
    public EventStream<JsonNode?> Untyped() => new(core, (ev, index) =>
    {
        decode(ev, index);
        return ev.Value;
    });

    /// <summary>Ends the stream for good, without an error and without a reconnect; safe from any thread.</summary>
    public void Cancel() => core.Cancel();

    public ValueTask DisposeAsync()
    {
        core.Close();
        return ValueTask.CompletedTask;
    }

    public async IAsyncEnumerator<StreamItem<T>> GetAsyncEnumerator(CancellationToken cancellationToken = default)
    {
        int index = 0;
        try
        {
            while (true)
            {
                StreamEvent? ev;
                TungstenError? error = null;
                try
                {
                    ev = await core.NextAsync(cancellationToken).ConfigureAwait(false);
                }
                catch (Failure f)
                {
                    ev = null;
                    error = f.Error;
                }
                if (error is not null)
                {
                    yield return new StreamItem<T>.Err(error);
                    yield break;
                }
                if (ev is null)
                {
                    yield break;
                }
                T value;
                try
                {
                    value = decode(ev, index);
                }
                catch (Failure f)
                {
                    error = f.Error;
                    value = default!;
                }
                if (error is not null)
                {
                    core.Close();
                    yield return new StreamItem<T>.Err(error);
                    yield break;
                }
                index++;
                yield return new StreamItem<T>.Ok(value, ev);
            }
        }
        finally
        {
            core.Close();
        }
    }

    /// <summary>Reads the stream to its end and folds its events into one value, then closes it.</summary>
    public async Task<Folded<TAcc>> ReduceAsync<TAcc>(TAcc initial, Func<TAcc, T, TAcc> fold, CancellationToken cancellationToken = default)
    {
        var acc = initial;
        int reconnects = 0;
        await foreach (var item in WithCancellation(cancellationToken).ConfigureAwait(false))
        {
            switch (item)
            {
                case StreamItem<T>.Ok ok:
                    reconnects = ok.Raw.Reconnects;
                    acc = fold(acc, ok.Value);
                    break;
                case StreamItem<T>.Err err:
                    return new Folded<TAcc>(acc, err.Error, reconnects);
            }
        }
        return new Folded<TAcc>(acc, null, reconnects);
    }

    /// <summary>The first event for which the predicate holds, then closes the stream; null when the stream ends without one.</summary>
    public async Task<Folded<StreamItem<T>.Ok?>> FirstAsync(Func<T, bool> predicate, CancellationToken cancellationToken = default)
    {
        int reconnects = 0;
        await foreach (var item in WithCancellation(cancellationToken).ConfigureAwait(false))
        {
            switch (item)
            {
                case StreamItem<T>.Ok ok:
                    reconnects = ok.Raw.Reconnects;
                    if (predicate(ok.Value))
                    {
                        return new Folded<StreamItem<T>.Ok?>(ok, null, reconnects);
                    }
                    break;
                case StreamItem<T>.Err err:
                    return new Folded<StreamItem<T>.Ok?>(null, err.Error, reconnects);
            }
        }
        return new Folded<StreamItem<T>.Ok?>(null, null, reconnects);
    }

    /// <summary>
    /// Reads the stream to its end and calls the handler named like each event ("message" for an
    /// event without a name), else the one named "*"; the value is the number of events delivered.
    /// </summary>
    public async Task<Folded<int>> OnAsync(IReadOnlyDictionary<string, Action<StreamItem<T>.Ok>> handlers, CancellationToken cancellationToken = default)
    {
        int count = 0;
        int reconnects = 0;
        await foreach (var item in WithCancellation(cancellationToken).ConfigureAwait(false))
        {
            switch (item)
            {
                case StreamItem<T>.Ok ok:
                    reconnects = ok.Raw.Reconnects;
                    count++;
                    if (handlers.TryGetValue(ok.Raw.Event, out var h) || handlers.TryGetValue("*", out h))
                    {
                        h(ok);
                    }
                    break;
                case StreamItem<T>.Err err:
                    return new Folded<int>(count, err.Error, reconnects);
            }
        }
        return new Folded<int>(count, null, reconnects);
    }

    /// <summary>
    /// Every event in order, and the final error if the stream failed. The collection is bounded:
    /// it ends with an UNEXPECTED_RESPONSE error once the kept events hold more than
    /// ClientOptions.MaxCollectBytes of JSON or it ran longer than MaxCollectTime.
    /// </summary>
    public async Task<Folded<List<StreamItem<T>.Ok>>> CollectAsync(CancellationToken cancellationToken = default)
    {
        var events = new List<StreamItem<T>.Ok>();
        using var limit = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        limit.CancelAfter(core.Client.MaxCollectTime);
        var started = DateTime.UtcNow;
        long bytes = 0;
        int reconnects = 0;
        await foreach (var item in WithCancellation(limit.Token).ConfigureAwait(false))
        {
            if (DateTime.UtcNow - started > core.Client.MaxCollectTime)
            {
                core.Close();
                return new Folded<List<StreamItem<T>.Ok>>(events, core.Client.OverBudget(core.Op, false, events.Count), reconnects);
            }
            switch (item)
            {
                case StreamItem<T>.Ok ok:
                    bytes += Util.Utf8.GetByteCount(Json.Text(ok.Raw.Value));
                    if (bytes > core.Client.MaxCollectBytes)
                    {
                        core.Close();
                        return new Folded<List<StreamItem<T>.Ok>>(events, core.Client.OverBudget(core.Op, true, events.Count), reconnects);
                    }
                    reconnects = ok.Raw.Reconnects;
                    events.Add(ok);
                    break;
                case StreamItem<T>.Err err:
                    if (limit.IsCancellationRequested && !cancellationToken.IsCancellationRequested)
                    {
                        return new Folded<List<StreamItem<T>.Ok>>(events, core.Client.OverBudget(core.Op, false, events.Count), reconnects);
                    }
                    return new Folded<List<StreamItem<T>.Ok>>(events, err.Error, reconnects);
            }
        }
        if (limit.IsCancellationRequested && !cancellationToken.IsCancellationRequested)
        {
            return new Folded<List<StreamItem<T>.Ok>>(events, core.Client.OverBudget(core.Op, false, events.Count), reconnects);
        }
        return new Folded<List<StreamItem<T>.Ok>>(events, null, reconnects);
    }

    private ConfiguredCancelableAsyncEnumerable<StreamItem<T>> WithCancellation(CancellationToken ct) => TaskAsyncEnumerableExtensions.WithCancellation(this, ct);
}

/// <summary>An open connection of a stream.</summary>
internal sealed class OpenStream
{
    public required EventBody Body { get; init; }
    public required SseParser Parser { get; init; }
    public Utf8Chunks Decoder { get; } = new();
    public Queue<SseEvent> Queue { get; } = new();
    public bool Ended { get; set; }
    public bool Oversize { get; set; }
    public required ResponseMeta Meta { get; init; }
    public int Count { get; set; }
    public string? Key { get; init; }
    public required string KeyHeader { get; init; }
    public OutcomeCheck? Check { get; init; }
    public required SecretSet Secrets { get; init; }
    public TimeSpan Timeout { get; init; }
    /// <summary>The call is safe to repeat (a read, or a mutation with replay protection).</summary>
    public bool Repeatable { get; init; }
    public int Reconnects { get; set; }
    public string? LastId { get; set; }
    public long? LastRetry { get; set; }
    /// <summary>The ids delivered so far; after a reconnect, events with one of them are skipped until the first new one.</summary>
    public HashSet<string> Delivered { get; set; } = new(StringComparer.Ordinal);
    public bool Replaying { get; set; }
}

internal enum Dropped
{
    None,
    Lost,
    Idle,
    EarlyEnd,
}

/// <summary>The state machine of one stream.</summary>
internal sealed class StreamCore
{
    private readonly StreamDescriptor spec;
    private readonly JsonNode? args;
    private readonly CallOptions opts;
    private readonly object gate = new();
    private OpenStream? open;
    private int state; // 0 idle, 1 open, 2 done
    private int cancelled;

    public StreamCore(ClientCore client, OperationDescriptor op, StreamDescriptor spec, JsonNode? args, CallOptions opts)
    {
        Client = client;
        Op = op;
        this.spec = spec;
        this.args = args;
        this.opts = opts;
    }

    public ClientCore Client { get; }

    public OperationDescriptor Op { get; }

    private bool Cancelled => Volatile.Read(ref cancelled) == 1;

    private void SetOpen(OpenStream? o)
    {
        lock (gate)
        {
            open = o;
        }
        if (Cancelled && o is not null)
        {
            o.Body.Close();
        }
    }

    public void Cancel()
    {
        if (Interlocked.Exchange(ref cancelled, 1) == 0)
        {
            OpenStream? o;
            lock (gate)
            {
                o = open;
            }
            o?.Body.Close();
        }
    }

    public void Close()
    {
        OpenStream? o;
        lock (gate)
        {
            o = open;
            open = null;
        }
        o?.Body.Close();
        state = 2;
    }

    private Failure Fail(Failure f)
    {
        Close();
        return f;
    }

    /// <summary>The next event; null at the end of the stream; throws the failure that ended it.</summary>
    public async Task<StreamEvent?> NextAsync(CancellationToken ct)
    {
        while (true)
        {
            if (state == 2)
            {
                return null;
            }
            // A cancelled stream delivers nothing more, not even events already parsed.
            if (Cancelled)
            {
                Close();
                return null;
            }
            if (state == 0)
            {
                OpenStream fresh;
                try
                {
                    fresh = await Client.OpenStreamAsync(Op, spec, args, opts, null, ct).ConfigureAwait(false);
                }
                catch (Failure f)
                {
                    throw Fail(f);
                }
                SetOpen(fresh);
                state = 1;
                if (Cancelled)
                {
                    Close();
                    return null;
                }
                continue;
            }
            var o = open;
            if (o is null)
            {
                state = 2;
                return null;
            }
            if (o.Queue.Count > 0)
            {
                var ev = o.Queue.Dequeue();
                StreamEvent? item;
                bool done;
                try
                {
                    (item, done) = Client.Accept(Op, spec, o, ev);
                }
                catch (Failure f)
                {
                    throw Fail(f);
                }
                if (done)
                {
                    Close();
                    return null;
                }
                if (item is null)
                {
                    continue;
                }
                return item;
            }
            if (o.Oversize)
            {
                throw Fail(Client.Oversize(Op, o));
            }
            Dropped why = Dropped.None;
            if (o.Ended)
            {
                // A stream that names its terminal event has not finished without it.
                if (spec.Done is null)
                {
                    Close();
                    return null;
                }
                why = Dropped.EarlyEnd;
            }
            else
            {
                var (chunk, kind) = await o.Body.ReadAsync(ct).ConfigureAwait(false);
                if (Cancelled)
                {
                    Close();
                    return null;
                }
                switch (kind)
                {
                    case BodyRead.Chunk:
                        foreach (var e in o.Parser.Push(o.Decoder.Decode(chunk)))
                        {
                            o.Queue.Enqueue(e);
                        }
                        o.Oversize = o.Parser.Exceeded;
                        continue;
                    case BodyRead.End:
                        foreach (var e in o.Parser.Push(o.Decoder.Finish()).Concat(o.Parser.End()))
                        {
                            o.Queue.Enqueue(e);
                        }
                        o.Oversize = o.Parser.Exceeded;
                        o.Ended = true;
                        continue;
                    case BodyRead.Timeout:
                        if (Client.IdleTimeoutValue is null)
                        {
                            throw Fail(Client.Interrupted(Op, o, Dropped.None));
                        }
                        why = Dropped.Idle;
                        break;
                    default:
                        why = Dropped.Lost;
                        break;
                }
            }
            // The caller's token ends the stream for good.
            if (ct.IsCancellationRequested || !Client.MayReconnect(o))
            {
                // Without reconnects a clean end stays a clean end.
                if (why == Dropped.EarlyEnd && o.Reconnects == 0)
                {
                    Close();
                    return null;
                }
                throw Fail(Client.Interrupted(Op, o, why));
            }
            OpenStream next;
            try
            {
                next = await Client.ReconnectAsync(Op, spec, args, opts, o, ct).ConfigureAwait(false);
            }
            catch (Failure f)
            {
                if (Cancelled)
                {
                    Close();
                    return null;
                }
                throw Fail(f);
            }
            if (Cancelled)
            {
                next.Body.Close();
                Close();
                return null;
            }
            SetOpen(next);
        }
    }
}

public sealed partial class ClientCore
{
    /// <summary>The wait before a reconnect when the server sent no retry value.</summary>
    private const int DefaultReconnectMs = 250;

    internal TimeSpan? IdleTimeoutValue => idleTimeout;

    internal TimeSpan MaxCollectTime => maxCollectTime;

    internal int MaxCollectBytes => maxCollectBytes;

    /// <summary>The typed stream of an operation; <paramref name="decode"/> turns an event into the SDK's type.</summary>
    public EventStream<T> StreamTyped<T>(OperationDescriptor op, JsonNode? args, Func<JsonNode?, T> decode, CallOptions? options = null) =>
        new(new StreamCore(this, op, op.Stream ?? new StreamDescriptor(), args, WithCallDeadline(options ?? new CallOptions())), (ev, index) => Typed.DecodeEvent(op.Id, index, ev, decode));

    /// <summary>Sets the stream's request flag to true, in a merged body field or inside an object body argument.</summary>
    private static JsonNode? WithStreamFlag(OperationDescriptor op, StreamDescriptor spec, JsonNode? args)
    {
        if (string.IsNullOrEmpty(spec.Flag) || op.Body is null || args is not JsonObject o)
        {
            return args;
        }
        var outObj = Json.CopyObject(o);
        if (op.Body.Arg is null)
        {
            var f = op.Body.Fields.FirstOrDefault(x => x.Wire == spec.Flag);
            if (f is not null)
            {
                outObj[f.Arg] = true;
            }
        }
        else if (o[op.Body.Arg] is JsonObject inner)
        {
            var copied = Json.CopyObject(inner);
            copied[spec.Flag] = true;
            outObj[op.Body.Arg] = copied;
        }
        return outObj;
    }

    internal async Task<OpenStream> OpenStreamAsync(OperationDescriptor op, StreamDescriptor spec, JsonNode? args, CallOptions opts, string? resumeFrom, CancellationToken ct)
    {
        var streamHeaders = new Dictionary<string, string>(StringComparer.Ordinal);
        var accept = "";
        foreach (var kv in headers)
        {
            if (string.Equals(kv.Key, "accept", StringComparison.OrdinalIgnoreCase))
            {
                accept = kv.Value;
            }
        }
        foreach (var kv in opts.Headers ?? new Dictionary<string, string>())
        {
            if (string.Equals(kv.Key, "accept", StringComparison.OrdinalIgnoreCase))
            {
                accept = kv.Value;
            }
            else
            {
                streamHeaders[kv.Key] = kv.Value;
            }
        }
        streamHeaders["Accept"] = Util.MergeAccept(accept);
        if (resumeFrom is not null)
        {
            foreach (var name in streamHeaders.Keys.Where(k => string.Equals(k, "last-event-id", StringComparison.OrdinalIgnoreCase)).ToList())
            {
                streamHeaders.Remove(name);
            }
            streamHeaders["Last-Event-ID"] = resumeFrom;
        }
        var streamOpts = opts with { Headers = streamHeaders };
        var p = await PrepareAsync(op, WithStreamFlag(op, spec, args), streamOpts, Purpose.Call, null, null, ct).ConfigureAwait(false);
        var result = await SendWithAsync(p, streamOpts, true, ct).ConfigureAwait(false);
        if (result.StreamBody is null || result.StreamMeta is null)
        {
            var response = result.Response!;
            bool hasType = response.Meta.Headers.TryGetValue("content-type", out var contentType);
            var after = Classify.IsMutation(op) ? " The call took effect; do not repeat it." : "";
            throw new Diag(op.Id, Category.UnexpectedResponse).Status(response.Meta.Status).RequestId(response.Meta.RequestId)
                .Param("response").Received(hasType ? Util.EnvelopeValue(JsonValue.Create(contentType), false) : null).Expected("a text/event-stream body")
                .Remedy($"The success response is not an event stream (Content-Type {(hasType ? contentType : "missing")}).{after}")
                .Retry(Retryable.Never).Attempts(response.Meta.Attempts).Fail();
        }
        bool mutation = Classify.IsMutation(op);
        return new OpenStream
        {
            Body = result.StreamBody,
            Parser = new SseParser(maxEventBytes),
            Meta = result.StreamMeta,
            Key = p.Key,
            KeyHeader = p.KeyHeader,
            Check = mutation && !Classify.HasReplayProtection(op, p.HasKey) ? OutcomeCheckOf(op, p.Args) : null,
            Secrets = p.Secrets.Clone(),
            Timeout = AttemptTimeout(streamOpts),
            Repeatable = !mutation || Classify.HasReplayProtection(op, p.HasKey),
        };
    }

    /// <summary>A dropped stream may be reconnected when the call is safe to repeat, reconnects are left, and an event id to resume from is known (or no event was delivered yet).</summary>
    internal bool MayReconnect(OpenStream o) =>
        o.Repeatable && o.Reconnects < maxReconnects && (o.Count == 0 || (o.LastId is not null && Serialize.ValidHeaderValue(o.LastId)));

    /// <summary>Waits as the server asked (capped, shortened by up to 25 % at random), then sends the request again with Last-Event-ID.</summary>
    internal async Task<OpenStream> ReconnectAsync(OperationDescriptor op, StreamDescriptor spec, JsonNode? args, CallOptions opts, OpenStream old, CancellationToken ct)
    {
        old.Body.Close();
        double ceiling = old.LastRetry ?? DefaultReconnectMs;
        ceiling = Math.Min(ceiling, reconnectMax.TotalMilliseconds);
        double wait = ceiling - Random() * 0.25 * ceiling;
        try
        {
            await Task.Delay(Util.DurationMs(wait), ct).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
        }
        var fresh = await OpenStreamAsync(op, spec, args, opts, old.LastId, ct).ConfigureAwait(false);
        fresh.Count = old.Count;
        fresh.Reconnects = old.Reconnects + 1;
        fresh.LastId = old.LastId;
        fresh.LastRetry = old.LastRetry;
        fresh.Delivered = old.Delivered;
        fresh.Replaying = true;
        return fresh;
    }

    /// <summary>The final error of a stream that broke after its events; Dropped.None is a silence past the attempt timeout.</summary>
    internal Failure Interrupted(OperationDescriptor op, OpenStream o, Dropped why)
    {
        var after = $"after {o.Count} {Util.Plural(o.Count, "event", "events")}";
        var retried = o.Reconnects switch
        {
            0 => "",
            1 => " (1 reconnect made)",
            _ => $" ({o.Reconnects} reconnects made)",
        };
        bool idle = why == Dropped.Idle;
        string? code = idle ? "STREAM_IDLE" : null;
        var fields = new UnknownFields { HttpStatus = o.Meta.Status, Code = code, RequestId = o.Meta.RequestId };
        var cctx = new CallContext { Api = api, Op = op, Key = o.Key, HasKey = o.Key is not null, KeyHeader = o.KeyHeader, Attempts = o.Meta.Attempts, Check = o.Check };
        Diagnostic d;
        if (why == Dropped.None || idle)
        {
            var silence = $"No event arrived within {Util.Ms(o.Timeout)} ms {after}";
            var option = "`Timeout`";
            if (idle)
            {
                silence = $"No bytes arrived within {Util.Ms(idleTimeout ?? TimeSpan.Zero)} ms {after}{retried}";
                option = "`IdleTimeout`";
            }
            var cause = silence + "; the stream was abandoned.";
            d = Classify.IsMutation(op)
                ? Classify.OutcomeUnknown(cctx, cause, fields)
                : new Diag(op.Id, Category.UpstreamUnavailable).Status(o.Meta.Status).Code(code).RequestId(o.Meta.RequestId)
                    .Remedy(cause + " This read has no side effects; call again later or with a larger " + option + ".")
                    .Attempts(o.Meta.Attempts).Build();
        }
        else
        {
            var cause = $"The event stream was cut off {after}{retried}: the connection failed before it ended.";
            d = Classify.IsMutation(op)
                ? Classify.OutcomeUnknown(cctx, cause, fields)
                : new Diag(op.Id, Category.TransportFailed).Status(o.Meta.Status).RequestId(o.Meta.RequestId)
                    .Remedy(cause + " This read has no side effects; call again.")
                    .Attempts(o.Meta.Attempts).Build();
        }
        return new Failure(o.Secrets.Scrub(d));
    }

    internal TungstenError OverBudget(OperationDescriptor op, bool bytes, int kept)
    {
        var what = bytes ? $"at most {maxCollectBytes} bytes of events" : $"at most {Util.Ms(maxCollectTime)} ms to collect the events";
        var after = Classify.IsMutation(op) ? " The call took effect; do not repeat it." : "";
        return new TungstenError(new Diag(op.Id, Category.UnexpectedResponse).Param("events").Expected(what)
            .Remedy($"The stream was abandoned after {kept} {Util.Plural(kept, "event", "events")} ({what}; ClientOptions.MaxCollectBytes and MaxCollectTime).{after} The events collected so far precede this error.")
            .Retry(Retryable.Never).Build());
    }

    internal Failure Oversize(OperationDescriptor op, OpenStream o)
    {
        int limit = maxEventBytes;
        var after = Classify.IsMutation(op) ? " The call took effect; do not repeat it." : "";
        return new Failure(o.Secrets.Scrub(new Diag(op.Id, Category.UnexpectedResponse).Status(o.Meta.Status).RequestId(o.Meta.RequestId)
            .Param($"events[{o.Count}]")
            .Expected($"an event of at most {limit} bytes")
            .Remedy($"Event {o.Count} of the stream is larger than the limit of {limit} bytes (ClientOptions.MaxEventBytes); the stream was abandoned.{after} Events before it were delivered. Raise MaxEventBytes if the server sends events this large on purpose.")
            .Retry(Retryable.Never).Attempts(o.Meta.Attempts).Build()));
    }

    /// <summary>Decodes, checks and numbers one event; done is the done sentinel.</summary>
    internal (StreamEvent? Event, bool Done) Accept(OperationDescriptor op, StreamDescriptor spec, OpenStream o, SseEvent ev)
    {
        if (spec.Done is not null && spec.Done == ev.Data)
        {
            return (null, true);
        }
        if (o.Replaying)
        {
            if (ev.Id is not null && o.Delivered.Contains(ev.Id))
            {
                return (null, false);
            }
            o.Replaying = false;
        }
        int index = o.Count;
        var afterEffect = Classify.IsMutation(op) ? " The call took effect; do not repeat it." : "";
        Failure Failed(Diag b) => new(o.Secrets.Scrub(b.Status(o.Meta.Status).RequestId(o.Meta.RequestId).Retry(Retryable.Never).Attempts(o.Meta.Attempts).Build()));
        if (!Json.TryParse(ev.Data, out var value))
        {
            throw Failed(new Diag(op.Id, Category.UnexpectedResponse).Param($"events[{index}]")
                .Received(Util.EnvelopeValue(JsonValue.Create(ev.Data), false)).Expected("JSON in the data of every event")
                .Remedy($"Event {index} of the stream (event \"{ev.Event}\") does not carry JSON in its data.{afterEffect} Events before it were delivered."));
        }
        if (validateResponses != ValidateResponses.Off && spec.Event is { } schema)
        {
            var issues = Judge(op.Id, $"events[{index}]", schema, value);
            if (issues.Count > 0)
            {
                var issue = issues[0];
                var path = Schema.Segments(issue.Path);
                var pathText = Schema.FormatPath(issue.Path);
                var redacted = Util.RedactPaths(value, op.Agent.SensitiveResponseFields);
                Util.GetPath(redacted, true, path, out var received);
                bool sensitive = path.Any(Util.LooksSensitive);
                var e = Failed(new Diag(op.Id, Category.UnexpectedResponse).Param($"events[{index}]{pathText}")
                    .Received(Util.EnvelopeValue(received, sensitive)).Expected(issue.Message)
                    .Remedy($"Event {index} of the stream (event \"{ev.Event}\") does not match the API description at events[{index}]{pathText} ({issue.Message}).{afterEffect}"));
                if (validateResponses == ValidateResponses.Strict)
                {
                    throw e;
                }
                Emit(e.Error.Diagnostic);
            }
        }
        o.Count++;
        if (ev.Id is not null)
        {
            o.Delivered.Add(ev.Id);
            o.LastId = ev.Id;
        }
        if (ev.Retry is long r)
        {
            o.LastRetry = r;
        }
        return (new StreamEvent { Value = value, Event = ev.Event, Id = o.LastId, Retry = o.LastRetry, Meta = o.Meta, Reconnects = o.Reconnects }, false);
    }
}
