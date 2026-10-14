// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

/// <summary>A value that does not decode into the SDK's type: where and why.</summary>
public sealed class DecodeException : Exception
{
    public DecodeException(IReadOnlyList<object> path, string message) : base(message)
    {
        Path = path;
    }

    /// <summary>Member names and array indexes from the decoded root.</summary>
    public IReadOnlyList<object> Path { get; }
}

/// <summary>The result type of an operation that answers without a body.</summary>
public sealed record NoContent
{
    public static readonly NoContent Value = new();
}

/// <summary>
/// Decodes the runtime's JSON tree into generated types, without reflection. Strict decoding
/// (arguments handed to <c>Dispatch</c>) refuses members the type does not declare; lenient
/// decoding (responses) ignores them. A JSON type mismatch or a missing required member always
/// fails, with the path of the value.
/// </summary>
public sealed class Decoder
{
    private readonly List<object> path = new();

    public Decoder(bool strict)
    {
        Strict = strict;
    }

    /// <summary>Unknown members are refused.</summary>
    public bool Strict { get; }

    /// <summary>A lenient decoder (response bodies).</summary>
    public static Decoder Lenient() => new(false);

    private DecodeException Fail(string expected, JsonNode? found) =>
        new(path.ToList(), $"expected {expected}, found {Schema.Describe(found)}");

    /// <summary>Decodes the value at a member name or index of the current value.</summary>
    public T At<T>(object segment, JsonNode? node, Func<JsonNode?, Decoder, T> decode)
    {
        path.Add(segment);
        try
        {
            return decode(node, this);
        }
        finally
        {
            path.RemoveAt(path.Count - 1);
        }
    }

    /// <summary>The value as an object.</summary>
    public JsonObject Object(JsonNode? node) => node as JsonObject ?? throw Fail("an object", node);

    /// <summary>Refuses members other than the listed ones when strict.</summary>
    public void Known(JsonObject o, params string[] names)
    {
        if (!Strict)
        {
            return;
        }
        foreach (var kv in o)
        {
            if (System.Array.IndexOf(names, kv.Key) < 0)
            {
                path.Add(kv.Key);
                var e = new DecodeException(path.ToList(), "unknown member " + kv.Key);
                path.RemoveAt(path.Count - 1);
                throw e;
            }
        }
    }

    /// <summary>A required member.</summary>
    public T Required<T>(JsonObject o, string name, Func<JsonNode?, Decoder, T> decode)
    {
        if (!o.TryGetPropertyValue(name, out var v))
        {
            path.Add(name);
            var e = new DecodeException(path.ToList(), "missing required member " + name);
            path.RemoveAt(path.Count - 1);
            throw e;
        }
        return At(name, v, decode);
    }

    /// <summary>A required member that may be null (a reference type).</summary>
    public T? RequiredNullable<T>(JsonObject o, string name, Func<JsonNode?, Decoder, T> decode) where T : class
    {
        if (!o.TryGetPropertyValue(name, out var v))
        {
            path.Add(name);
            var e = new DecodeException(path.ToList(), "missing required member " + name);
            path.RemoveAt(path.Count - 1);
            throw e;
        }
        return Json.IsNull(v) ? null : At(name, v, decode);
    }

    /// <summary>A required member that may be null (a value type).</summary>
    public T? RequiredNullableValue<T>(JsonObject o, string name, Func<JsonNode?, Decoder, T> decode) where T : struct
    {
        if (!o.TryGetPropertyValue(name, out var v))
        {
            path.Add(name);
            var e = new DecodeException(path.ToList(), "missing required member " + name);
            path.RemoveAt(path.Count - 1);
            throw e;
        }
        return Json.IsNull(v) ? null : At(name, v, decode);
    }

    /// <summary>An optional member (a reference type): null when absent.</summary>
    public T? Optional<T>(JsonObject o, string name, Func<JsonNode?, Decoder, T> decode) where T : class =>
        o.TryGetPropertyValue(name, out var v) ? At(name, v, decode) : null;

    /// <summary>An optional member (a value type): null when absent.</summary>
    public T? OptionalValue<T>(JsonObject o, string name, Func<JsonNode?, Decoder, T> decode) where T : struct =>
        o.TryGetPropertyValue(name, out var v) ? At(name, v, decode) : null;

    /// <summary>An optional member that may be null: absent, null or a value.</summary>
    public Patch<T> Patch<T>(JsonObject o, string name, Func<JsonNode?, Decoder, T> decode)
    {
        if (!o.TryGetPropertyValue(name, out var v))
        {
            return global::Tungsten.Runtime.Patch<T>.Absent;
        }
        return Json.IsNull(v) ? global::Tungsten.Runtime.Patch<T>.Null : global::Tungsten.Runtime.Patch<T>.Of(At(name, v, decode));
    }

    // ------------------------------------------------------------ scalars

    public static string String(JsonNode? node, Decoder d) => Json.TryString(node, out var s) ? s : throw d.Fail("a string", node);

    public static bool Boolean(JsonNode? node, Decoder d) => Json.TryBoolean(node, out var b) ? b : throw d.Fail("a boolean", node);

    public static long Int64(JsonNode? node, Decoder d) => Json.TryInteger(node, out var l) ? l : throw d.Fail("an integer", node);

    public static int Int32(JsonNode? node, Decoder d) =>
        Json.TryInteger(node, out var l) && l >= int.MinValue && l <= int.MaxValue ? (int)l : throw d.Fail("a 32-bit integer", node);

    public static double Double(JsonNode? node, Decoder d) => Json.TryNumber(node, out var x) ? x : throw d.Fail("a number", node);

    /// <summary>Base64 text (format: byte).</summary>
    public static byte[] Bytes(JsonNode? node, Decoder d)
    {
        if (Json.TryString(node, out var s))
        {
            try
            {
                return Convert.FromBase64String(s);
            }
            catch (FormatException)
            {
                throw d.Fail("base64 text", node);
            }
        }
        throw d.Fail("base64 text", node);
    }

    /// <summary>A binary body or multipart field (the tagged object, or text as UTF-8 bytes).</summary>
    public static Binary Binary(JsonNode? node, Decoder d)
    {
        if (Runtime.Binary.TryFrom(node, out var bin))
        {
            return bin;
        }
        if (Json.TryString(node, out var s))
        {
            return new Binary(Util.Utf8.GetBytes(s));
        }
        throw d.Fail("binary data", node);
    }

    /// <summary>Any JSON value, kept as it is.</summary>
    public static JsonNode? Any(JsonNode? node, Decoder d) => Json.Copy(node);

    /// <summary>Exactly this JSON value.</summary>
    public static JsonNode? Const(JsonNode? node, Decoder d, JsonNode? expected) =>
        Json.Equal(node, expected) ? Json.Copy(node) : throw new DecodeException(d.path.ToList(), "expected " + Json.Text(expected) + ", found " + Schema.Describe(node));

    public static IReadOnlyList<T> List<T>(JsonNode? node, Decoder d, Func<JsonNode?, Decoder, T> item)
    {
        if (node is not JsonArray a)
        {
            throw d.Fail("an array", node);
        }
        var outList = new List<T>(a.Count);
        for (int i = 0; i < a.Count; i++)
        {
            outList.Add(d.At(i, a[i], item));
        }
        return outList;
    }

    public static IReadOnlyDictionary<string, T> Map<T>(JsonNode? node, Decoder d, Func<JsonNode?, Decoder, T> value)
    {
        if (node is not JsonObject o)
        {
            throw d.Fail("an object", node);
        }
        var outMap = new Dictionary<string, T>(StringComparer.Ordinal);
        foreach (var kv in o)
        {
            outMap[kv.Key] = d.At(kv.Key, kv.Value, value);
        }
        return outMap;
    }

    /// <summary>A nullable value of a reference type.</summary>
    public static T? Nullable<T>(JsonNode? node, Decoder d, Func<JsonNode?, Decoder, T> inner) where T : class =>
        Json.IsNull(node) ? null : inner(node, d);

    /// <summary>A nullable value of a value type.</summary>
    public static T? NullableValue<T>(JsonNode? node, Decoder d, Func<JsonNode?, Decoder, T> inner) where T : struct =>
        Json.IsNull(node) ? null : inner(node, d);

    /// <summary>The members of an open record the type does not declare; null when there are none.</summary>
    public JsonObject? Extra(JsonObject o, params string[] known)
    {
        JsonObject? extra = null;
        foreach (var kv in o)
        {
            if (System.Array.IndexOf(known, kv.Key) < 0)
            {
                extra ??= new JsonObject();
                Json.Put(extra, kv.Key, kv.Value);
            }
        }
        return extra;
    }

    /// <summary>The members of a record with typed additional members the type does not declare; null when there are none.</summary>
    public IReadOnlyDictionary<string, T>? ExtraTyped<T>(JsonObject o, Func<JsonNode?, Decoder, T> decode, params string[] known)
    {
        Dictionary<string, T>? extra = null;
        foreach (var kv in o)
        {
            if (System.Array.IndexOf(known, kv.Key) < 0)
            {
                extra ??= new Dictionary<string, T>(StringComparer.Ordinal);
                extra[kv.Key] = At(kv.Key, kv.Value, decode);
            }
        }
        return extra;
    }

    /// <summary>The value of a discriminator member, or a failure at its path.</summary>
    public string Tag(JsonObject o, string tag)
    {
        if (o.TryGetPropertyValue(tag, out var v) && Json.TryString(v, out var s))
        {
            return s;
        }
        path.Add(tag);
        var e = new DecodeException(path.ToList(), "missing the discriminator " + tag);
        path.RemoveAt(path.Count - 1);
        throw e;
    }

    /// <summary>An unknown tag value.</summary>
    public DecodeException UnknownTag(string tag, string value, params string[] known)
    {
        var at = path.ToList();
        at.Add(tag);
        return new DecodeException(at, $"unknown {tag} {Json.Quote(value)}, expected one of {Json.Text(Json.Strings(known))}");
    }

    /// <summary>Tries the variants of an untagged union in order; the first that decodes wins.</summary>
    public T FirstOf<T>(JsonNode? node, string what, params Func<JsonNode?, Decoder, T>[] variants)
    {
        foreach (var variant in variants)
        {
            var trial = new Decoder(true);
            trial.path.AddRange(path);
            try
            {
                return variant(node, trial);
            }
            catch (DecodeException)
            {
            }
        }
        if (!Strict)
        {
            foreach (var variant in variants)
            {
                var trial = new Decoder(false);
                trial.path.AddRange(path);
                try
                {
                    return variant(node, trial);
                }
                catch (DecodeException)
                {
                }
            }
        }
        throw new DecodeException(path.ToList(), $"a value of none of the variants of {what}");
    }

    /// <summary>An enum value that is not one of the known ones.</summary>
    public DecodeException NotOneOf(JsonNode? node, IEnumerable<JsonNode?> values) =>
        new(path.ToList(), "must be one of " + Json.Text(Json.Array(values)) + ", found " + Json.Text(node));
}

/// <summary>Encodes generated types into the runtime's JSON tree.</summary>
public static class Encode
{
    public static JsonNode String(string v) => JsonValue.Create(v);

    public static JsonNode Boolean(bool v) => JsonValue.Create(v);

    public static JsonNode Int64(long v) => JsonValue.Create(v);

    public static JsonNode Int32(int v) => JsonValue.Create((long)v);

    public static JsonNode Double(double v) => Json.Number(v);

    public static JsonNode Bytes(byte[] v) => JsonValue.Create(Convert.ToBase64String(v));

    public static JsonNode Binary(Binary v) => v.ToJson();

    public static JsonNode? Any(JsonNode? v) => Json.Copy(v);

    public static JsonArray List<T>(IEnumerable<T> items, Func<T, JsonNode?> item) => Json.Array(items.Select(item));

    public static JsonObject Map<T>(IReadOnlyDictionary<string, T> map, Func<T, JsonNode?> value)
    {
        var o = new JsonObject();
        foreach (var kv in map)
        {
            Json.Put(o, kv.Key, value(kv.Value));
        }
        return o;
    }

    /// <summary>A member that is always written.</summary>
    public static void Put(JsonObject o, string name, JsonNode? value) => Json.Put(o, name, value);

    /// <summary>An optional member (a reference type): written when not null.</summary>
    public static void Optional<T>(JsonObject o, string name, T? value, Func<T, JsonNode?> encode) where T : class
    {
        if (value is not null)
        {
            Json.Put(o, name, encode(value));
        }
    }

    /// <summary>An optional member (a value type): written when not null.</summary>
    public static void OptionalValue<T>(JsonObject o, string name, T? value, Func<T, JsonNode?> encode) where T : struct
    {
        if (value is T v)
        {
            Json.Put(o, name, encode(v));
        }
    }

    /// <summary>A required member that may be null (a reference type).</summary>
    public static void Nullable<T>(JsonObject o, string name, T? value, Func<T, JsonNode?> encode) where T : class =>
        Json.Put(o, name, value is null ? null : encode(value));

    /// <summary>A required member that may be null (a value type).</summary>
    public static void NullableValue<T>(JsonObject o, string name, T? value, Func<T, JsonNode?> encode) where T : struct =>
        Json.Put(o, name, value is T v ? encode(v) : null);

    /// <summary>The additional members of an open record.</summary>
    public static void Extra(JsonObject o, JsonObject? extra)
    {
        if (extra is null)
        {
            return;
        }
        foreach (var kv in extra)
        {
            if (!o.ContainsKey(kv.Key))
            {
                Json.Put(o, kv.Key, kv.Value);
            }
        }
    }

    /// <summary>The typed additional members of a record.</summary>
    public static void ExtraTyped<T>(JsonObject o, IReadOnlyDictionary<string, T>? extra, Func<T, JsonNode?> encode)
    {
        if (extra is null)
        {
            return;
        }
        foreach (var kv in extra)
        {
            if (!o.ContainsKey(kv.Key))
            {
                Json.Put(o, kv.Key, encode(kv.Value));
            }
        }
    }

    /// <summary>An optional member that may be null.</summary>
    public static void Patch<T>(JsonObject o, string name, Patch<T> value, Func<T, JsonNode?> encode)
    {
        if (!value.IsPresent)
        {
            return;
        }
        Json.Put(o, name, value.IsNull ? null : encode(value.Value));
    }
}

/// <summary>The helpers generated SDKs are written with.</summary>
public static class Typed
{
    private static TungstenError Mismatch(string operation, string prefix, DecodeException e, JsonNode? root, ResponseMeta meta)
    {
        var segments = Schema.Segments(e.Path);
        JsonNode? shown;
        if (segments.Count == 0 && prefix.Length == 0)
        {
            shown = Util.EnvelopeValue(root, false);
        }
        else
        {
            Util.GetPath(root, true, segments, out var found);
            shown = Util.EnvelopeValue(found, segments.Any(Util.LooksSensitive));
        }
        var location = "response" + prefix + Schema.FormatPath(e.Path);
        var b = new Diag(operation, Category.UnexpectedResponse);
        if (meta.Status > 0)
        {
            b.Status(meta.Status);
        }
        var d = b.RequestId(meta.RequestId).Param(location).Received(shown).Expected(e.Message).Retry(Retryable.Never)
            .Remedy($"The response of {operation} could not be decoded into the SDK's type at {location} ({e.Message}). If the call changed state it already took effect; do not repeat it.")
            .Attempts(meta.Attempts).Build();
        return new TungstenError(d) { Partial = Json.Copy(root), HasPartial = true };
    }

    /// <summary>A dynamic result as a typed one. A missing body decodes as JSON null.</summary>
    public static Result<T> Decode<T>(string operation, Result<JsonNode?> result, Func<JsonNode?, Decoder, T> decode)
    {
        switch (result)
        {
            case Result<JsonNode?>.Ok ok:
                try
                {
                    var value = decode(ok.Value, Decoder.Lenient());
                    return new Result<T>.Ok(value) { HasValue = ok.HasValue, Raw = ok.Raw, Meta = ok.Meta, Verification = ok.Verification };
                }
                catch (DecodeException e)
                {
                    return new Result<T>.Err(Mismatch(operation, "", e, ok.Value, ok.Meta));
                }
            case Result<JsonNode?>.Err err:
                return new Result<T>.Err(err.Error);
        }
        throw new InvalidOperationException();
    }

    /// <summary>A typed result back as a dynamic one whose value is the decoded wire body.</summary>
    public static Result<JsonNode?> Untyped<T>(Result<T> result) => result switch
    {
        Result<T>.Ok ok => new Result<JsonNode?>.Ok(ok.Raw) { HasValue = ok.HasValue, Raw = ok.Raw, Meta = ok.Meta, Verification = ok.Verification },
        Result<T>.Err err => new Result<JsonNode?>.Err(err.Error),
        _ => throw new InvalidOperationException(),
    };

    /// <summary>Calls an operation with an encoded request and decodes the body.</summary>
    public static async Task<Result<T>> CallAsync<T>(ClientCore core, OperationDescriptor op, JsonObject args, Func<JsonNode?, Decoder, T> decode, CallOptions? options, CancellationToken cancellationToken)
    {
        var result = await core.CallAsync(op, args, options, cancellationToken).ConfigureAwait(false);
        return Decode(op.Id, result, decode);
    }

    /// <summary>Calls an operation that answers without a body.</summary>
    public static async Task<Result<NoContent>> CallNoContentAsync(ClientCore core, OperationDescriptor op, JsonObject args, CallOptions? options, CancellationToken cancellationToken)
    {
        var result = await core.CallAsync(op, args, options, cancellationToken).ConfigureAwait(false);
        return Decode(op.Id, result, (_, _) => NoContent.Value);
    }

    /// <summary>The pages of an operation with items typed as T; an item that does not decode ends the sequence with UNEXPECTED_RESPONSE.</summary>
    public static async IAsyncEnumerable<Result<Page<T>>> PagesAsync<T>(ClientCore core, OperationDescriptor op, JsonObject args, Func<JsonNode?, Decoder, T> decode, CallOptions? options, [EnumeratorCancellation] CancellationToken cancellationToken)
    {
        await foreach (var page in core.PagesAsync(op, args, options, cancellationToken).ConfigureAwait(false))
        {
            switch (page)
            {
                case Result<Page<JsonNode?>>.Ok ok:
                    {
                        var items = new List<T>();
                        TungstenError? error = null;
                        for (int i = 0; i < ok.Value.Items.Count; i++)
                        {
                            try
                            {
                                items.Add(decode(ok.Value.Items[i], Decoder.Lenient()));
                            }
                            catch (DecodeException e)
                            {
                                error = Mismatch(op.Id, ".items[" + i.ToString(CultureInfo.InvariantCulture) + "]", e, ok.Value.Items[i], ok.Meta);
                                break;
                            }
                        }
                        if (error is not null)
                        {
                            yield return new Result<Page<T>>.Err(error);
                            yield break;
                        }
                        yield return new Result<Page<T>>.Ok(new Page<T> { Items = items, RawItems = ok.Value.Items, Body = ok.Value.Body, Next = ok.Value.Next })
                        {
                            HasValue = true,
                            Raw = ok.Raw,
                            Meta = ok.Meta,
                        };
                        break;
                    }
                case Result<Page<JsonNode?>>.Err err:
                    yield return new Result<Page<T>>.Err(err.Error);
                    yield break;
            }
        }
    }

    /// <summary>A typed page back as a dynamic one whose items are the decoded wire items.</summary>
    public static Result<Page<JsonNode?>> UntypedPage<T>(Result<Page<T>> page) => page switch
    {
        Result<Page<T>>.Ok ok => new Result<Page<JsonNode?>>.Ok(new Page<JsonNode?> { Items = ok.Value.RawItems, RawItems = ok.Value.RawItems, Body = ok.Value.Body, Next = ok.Value.Next })
        {
            HasValue = true,
            Raw = ok.Raw,
            Meta = ok.Meta,
        },
        Result<Page<T>>.Err err => new Result<Page<JsonNode?>>.Err(err.Error),
        _ => throw new InvalidOperationException(),
    };

    /// <summary>Types the value of the event at an index of a stream; a mismatch ends the stream.</summary>
    internal static T DecodeEvent<T>(string operation, int index, StreamEvent ev, Func<JsonNode?, T> decode)
    {
        try
        {
            return decode(ev.Value);
        }
        catch (DecodeException e)
        {
            var segments = Schema.Segments(e.Path);
            var location = $"events[{index}]" + Schema.FormatPath(e.Path);
            Util.GetPath(ev.Value, true, segments, out var found);
            var b = new Diag(operation, Category.UnexpectedResponse);
            if (ev.Meta.Status > 0)
            {
                b.Status(ev.Meta.Status);
            }
            throw b.RequestId(ev.Meta.RequestId).Param(location).Received(Util.EnvelopeValue(found, segments.Any(Util.LooksSensitive)))
                .Expected(e.Message).Retry(Retryable.Never)
                .Remedy($"Event {index} of the stream of {operation} could not be decoded into the SDK's type at {location} ({e.Message}). If the call changed state it already took effect; do not repeat it.")
                .Attempts(ev.Meta.Attempts).Fail();
        }
    }

    /// <summary>The typed stream of an operation.</summary>
    public static EventStream<T> Stream<T>(ClientCore core, OperationDescriptor op, JsonObject args, Func<JsonNode?, Decoder, T> decode, CallOptions? options) =>
        core.StreamTyped(op, args, v => decode(v, Decoder.Lenient()), options);

    /// <summary>Typed pages back as dynamic ones.</summary>
    public static async IAsyncEnumerable<Result<Page<JsonNode?>>> UntypedPagesAsync<T>(IAsyncEnumerable<Result<Page<T>>> pages, [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        await foreach (var page in pages.WithCancellation(cancellationToken).ConfigureAwait(false))
        {
            yield return UntypedPage(page);
        }
    }

    /// <summary>A sequence of one item (a failure in place of pages).</summary>
    public static async IAsyncEnumerable<T> Once<T>(T item)
    {
        await Task.CompletedTask.ConfigureAwait(false);
        yield return item;
    }

    /// <summary>Decodes arguments strictly into a request type; false when they do not decode.</summary>
    public static bool TryDecodeArgs<T>(JsonObject args, Func<JsonNode?, Decoder, T> decode, out T request)
    {
        try
        {
            request = decode(args, new Decoder(true));
            return true;
        }
        catch (DecodeException)
        {
            request = default!;
            return false;
        }
    }

    /// <summary>Parses JSON text the generator wrote.</summary>
    public static JsonNode? Parse(string text) => Json.Parse(text);

    // ------------------------------------------------------------ dispatch errors

    private static Result<T> Invalid<T>(string target, string param, string expected, string remediation) =>
        new Result<T>.Err(new TungstenError(new Diag(target, Category.ValidationFailed).Param(param).Expected(expected).Remedy(remediation).Build()));

    /// <summary>VALIDATION_FAILED for an operation without an event stream.</summary>
    public static Result<T> NoStream<T>(string operation) => Invalid<T>(operation, "operation", "an operation with an event stream",
        $"{operation} has no event stream (its success response is not text/event-stream); use InvokeAsync instead. Nothing was sent.");

    /// <summary>VALIDATION_FAILED for an operation id the client does not have.</summary>
    public static Result<T> UnknownOperation<T>(string operation) => Invalid<T>(operation, "operation", "an operation id of this SDK",
        $"This SDK has no operation {operation}; check the id (Operations lists them). Nothing was sent.");

    /// <summary>VALIDATION_FAILED for a macro name the client does not have.</summary>
    public static Result<T> UnknownMacro<T>(string name) => Invalid<T>(name, "macro", "a macro name of this SDK",
        $"This SDK has no macro {name}; check the name (Macros lists them). Nothing was sent.");

    /// <summary>VALIDATION_FAILED for an operation without pagination.</summary>
    public static Result<T> NotPaginated<T>(string operation) => Invalid<T>(operation, "operation", "a paginated operation",
        $"{operation} is not paginated; use InvokeAsync instead. Nothing was sent.");
}

/// <summary>
/// Implemented by every generated client. It lets drivers that only know operation ids and JSON
/// (the contract suite, tool servers) run the same typed methods as application code:
/// <see cref="InvokeAsync"/> decodes the arguments into the operation's request type, calls the
/// typed method and returns its result with the decoded wire body as the value. When the
/// arguments do not decode, it calls <see cref="ClientCore.CallAsync"/> with them, so the
/// VALIDATION_FAILED envelope is the pre-flight one. Arguments are keyed by arguments key.
/// </summary>
public interface IDispatch
{
    ClientCore Core { get; }

    /// <summary>Every callable operation, in IR order.</summary>
    IReadOnlyList<OperationDescriptor> Operations { get; }

    /// <summary>Every macro, in IR order.</summary>
    IReadOnlyList<MacroDescriptor> Macros { get; }

    Task<Result<JsonNode?>> InvokeAsync(string operation, JsonObject args, CallOptions? options = null, CancellationToken cancellationToken = default);

    Task<Result<PreviewResult>> PreviewOperationAsync(string operation, JsonObject args, CallOptions? options = null, CancellationToken cancellationToken = default);

    /// <summary>The pages of a paginated operation; a failure ends the sequence.</summary>
    IAsyncEnumerable<Result<Page<JsonNode?>>> PaginateAsync(string operation, JsonObject args, CallOptions? options = null, CancellationToken cancellationToken = default);

    /// <summary>The event stream of an operation, values as decoded JSON; null when the operation has none.</summary>
    EventStream<JsonNode?>? StreamEvents(string operation, JsonObject args, CallOptions? options = null);

    Task<Result<JsonNode?>> RunMacroAsync(string name, JsonObject input, CallOptions? options = null, CancellationToken cancellationToken = default);

    Task<Result<PreviewResult>> PreviewMacroAsync(string name, JsonObject input, CallOptions? options = null, CancellationToken cancellationToken = default);
}
