// SPDX-License-Identifier: Apache-2.0
using System;
using System.Text.Json.Nodes;

namespace Tungsten.Runtime;

/// <summary>
/// The value of a bytes or multipart body field: the bytes, and for a multipart part its optional
/// file name and media type. Inside an arguments object (which is JSON) it is the tagged object
/// <c>{"$tungsten_binary": "&lt;base64&gt;", "filename": ..., "content_type": ...}</c>; only the
/// request encoder interprets it. Base64 text inside JSON models (format: byte) is a plain
/// <c>byte[]</c> instead.
/// </summary>
public sealed record Binary
{
    /// <summary>The member that marks bytes inside an arguments object.</summary>
    public const string Key = "$tungsten_binary";

    public Binary(byte[] data)
    {
        Data = data;
    }

    public byte[] Data { get; init; }

    /// <summary>The multipart file name.</summary>
    public string? Filename { get; init; }

    /// <summary>The multipart part's media type.</summary>
    public string? ContentType { get; init; }

    /// <summary>Never shows the bytes.</summary>
    public override string ToString() => $"Binary({Data.Length} bytes)";

    /// <summary>The tagged object that stands for these bytes in an arguments object.</summary>
    public JsonObject ToJson()
    {
        var o = new JsonObject { [Key] = Convert.ToBase64String(Data) };
        if (Filename is not null)
        {
            o["filename"] = Filename;
        }
        if (ContentType is not null)
        {
            o["content_type"] = ContentType;
        }
        return o;
    }

    /// <summary>The bytes a tagged object stands for.</summary>
    public static bool TryFrom(JsonNode? value, out Binary binary)
    {
        binary = new Binary(System.Array.Empty<byte>());
        if (value is not JsonObject o || !o.ContainsKey(Key))
        {
            return false;
        }
        byte[] data = System.Array.Empty<byte>();
        string? filename = null;
        string? contentType = null;
        foreach (var kv in o)
        {
            switch (kv.Key)
            {
                case Key:
                    if (!Json.TryString(kv.Value, out var text))
                    {
                        return false;
                    }
                    try
                    {
                        data = Convert.FromBase64String(text);
                    }
                    catch (FormatException)
                    {
                        return false;
                    }
                    break;
                case "filename":
                    if (kv.Value is null)
                    {
                        continue;
                    }
                    if (!Json.TryString(kv.Value, out var name))
                    {
                        return false;
                    }
                    filename = name;
                    break;
                case "content_type":
                    if (kv.Value is null)
                    {
                        continue;
                    }
                    if (!Json.TryString(kv.Value, out var media))
                    {
                        return false;
                    }
                    contentType = media;
                    break;
                default:
                    return false;
            }
        }
        binary = new Binary(data) { Filename = filename, ContentType = contentType };
        return true;
    }
}

/// <summary>
/// A field that may be absent, null or a value (presence optional-nullable). The default value
/// is absent; <see cref="Null"/> is an explicit null and <see cref="Of"/> a value.
/// </summary>
public readonly struct Patch<T> : IEquatable<Patch<T>>
{
    private readonly T? value;

    private Patch(bool present, bool isNull, T? value)
    {
        IsPresent = present;
        IsNull = isNull;
        this.value = value;
    }

    /// <summary>The field is absent.</summary>
    public static Patch<T> Absent => default;

    /// <summary>An explicit null.</summary>
    public static Patch<T> Null => new(true, true, default);

    /// <summary>A present value.</summary>
    public static Patch<T> Of(T value) => new(true, false, value);

    public static implicit operator Patch<T>(T value) => Of(value);

    /// <summary>The field is present (null or a value).</summary>
    public bool IsPresent { get; }

    /// <summary>The field is an explicit null.</summary>
    public bool IsNull { get; }

    /// <summary>The field holds a value.</summary>
    public bool HasValue => IsPresent && !IsNull;

    /// <summary>The value; throws when the field is absent or null.</summary>
    public T Value => HasValue ? value! : throw new InvalidOperationException("the field is absent or null");

    /// <summary>The value and whether the field holds one.</summary>
    public bool TryGetValue(out T result)
    {
        result = value!;
        return HasValue;
    }

    public bool Equals(Patch<T> other) =>
        IsPresent == other.IsPresent && IsNull == other.IsNull &&
        System.Collections.Generic.EqualityComparer<T?>.Default.Equals(value, other.value);

    public override bool Equals(object? obj) => obj is Patch<T> p && Equals(p);

    public override int GetHashCode() => HashCode.Combine(IsPresent, IsNull, value);

    public static bool operator ==(Patch<T> a, Patch<T> b) => a.Equals(b);

    public static bool operator !=(Patch<T> a, Patch<T> b) => !a.Equals(b);

    public override string ToString() => !IsPresent ? "Patch.Absent" : IsNull ? "Patch.Null" : $"Patch.Of({value})";
}
