// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;
using System.Threading;

namespace Tungsten.Runtime;

/// <summary>
/// Optional transactional extension for stores shared by multiple clients or processes.
/// Returns a previously stored nonempty key, or atomically persists candidate.
/// </summary>
public interface IAtomicIdempotencyStore : IIdempotencyStore
{
    string Reserve(string scope, string logicalId, string candidate);
}

internal static class IdempotencyReservation
{
    private static readonly object LegacyLock = new();

    internal static string Reserve(IIdempotencyStore store, string scope, string id, string candidate)
    {
        if (store is IAtomicIdempotencyStore atomic)
        {
            return atomic.Reserve(scope, id, candidate);
        }
        // Backwards-compatible get/put stores remain usable. Only an atomic
        // implementation can coordinate a backend shared outside this runtime.
        lock (LegacyLock)
        {
            string? existing = store.Get(scope, id);
            if (!string.IsNullOrEmpty(existing)) return existing;
            store.Put(scope, id, candidate);
            if (store.Get(scope, id) != candidate)
                throw new InvalidOperationException("the idempotency key could not be stored");
            return candidate;
        }
    }
}

/// <summary>
/// Durable version-1 keys shared with the TypeScript, Python, Go and Rust stores.
/// Writes use a mode-0600 temporary file where Unix permissions are available,
/// then an atomic same-directory rename. Reserve waits up to two seconds for
/// the shared owner lease, fences its token before commit, and releases only
/// its own lease. Abandoned leases require manual recovery; none are reclaimed.
/// </summary>
public sealed class FileIdempotencyStore : IAtomicIdempotencyStore
{
    private static readonly ConcurrentDictionary<string, object> FileLocks = new(
        OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal);
    private static readonly UTF8Encoding Utf8 = new(false, true);
    private const UnixFileMode PrivateMode = UnixFileMode.UserRead | UnixFileMode.UserWrite;
    private readonly object monitor;

    public string Path { get; }

    public FileIdempotencyStore(string path)
    {
        Path = System.IO.Path.GetFullPath(path);
        if (System.IO.Path.GetFileName(Path).Length == 0)
            throw new ArgumentException("the idempotency store path must name a file", nameof(path));
        monitor = FileLocks.GetOrAdd(Path, _ => new object());
    }

    public string? Get(string scope, string logicalId)
    {
        lock (monitor)
        {
            var data = Read();
            return data.TryGetValue(scope, out var keys) && keys.TryGetValue(logicalId, out var key) ? key : null;
        }
    }

    public void Put(string scope, string logicalId, string key)
    {
        ArgumentNullException.ThrowIfNull(key);
        lock (monitor)
        {
            var data = Read();
            if (!data.TryGetValue(scope, out var keys)) data[scope] = keys = new(StringComparer.Ordinal);
            keys[logicalId] = key;
            Write(data, null, null);
        }
    }

    public string Reserve(string scope, string logicalId, string candidate)
    {
        ArgumentNullException.ThrowIfNull(candidate);
        lock (monitor)
        {
            Directory.CreateDirectory(System.IO.Path.GetDirectoryName(Path)!);
            string lease = Path + ".lock";
            string token = Acquire(lease);
            try
            {
                var data = Read();
                if (!data.TryGetValue(scope, out var keys)) data[scope] = keys = new(StringComparer.Ordinal);
                if (keys.TryGetValue(logicalId, out var existing) && existing.Length > 0) return existing;
                keys[logicalId] = candidate;
                Write(data, lease, token);
                return candidate;
            }
            finally
            {
                Release(lease, token);
            }
        }
    }

    private SortedDictionary<string, SortedDictionary<string, string>> Read()
    {
        byte[] bytes;
        try { bytes = File.ReadAllBytes(Path); }
        catch (FileNotFoundException) { return new(StringComparer.Ordinal); }
        catch (DirectoryNotFoundException) { return new(StringComparer.Ordinal); }
        JsonNode? parsed;
        try { parsed = Json.Parse(bytes); }
        catch (JsonParseException) { throw new IOException("the idempotency store file is not valid JSON"); }
        if (parsed is not JsonObject document || !Json.TryInteger(document["version"], out var version) || version != 1 ||
            document["keys"] is not JsonObject scopes)
            throw new IOException("the idempotency store file has an unknown format");
        var result = new SortedDictionary<string, SortedDictionary<string, string>>(StringComparer.Ordinal);
        foreach (var scope in scopes)
        {
            if (scope.Value is not JsonObject entries)
                throw new IOException("the idempotency store file has an unknown format");
            var keys = new SortedDictionary<string, string>(StringComparer.Ordinal);
            foreach (var entry in entries)
            {
                if (!Json.TryString(entry.Value, out var key))
                    throw new IOException("the idempotency store file has an unknown format");
                keys.Add(entry.Key, key);
            }
            result.Add(scope.Key, keys);
        }
        return result;
    }

    private void Write(SortedDictionary<string, SortedDictionary<string, string>> data, string? lease, string? token)
    {
        var scopes = new JsonObject();
        foreach (var scope in data)
        {
            var keys = new JsonObject();
            foreach (var entry in scope.Value) keys[entry.Key] = entry.Value;
            scopes[scope.Key] = keys;
        }
        byte[] bytes = Utf8.GetBytes(Json.Text(new JsonObject { ["version"] = 1, ["keys"] = scopes }) + "\n");
        string directory = System.IO.Path.GetDirectoryName(Path)!;
        Directory.CreateDirectory(directory);
        string temporary = System.IO.Path.Combine(directory, "." + System.IO.Path.GetFileName(Path) + "." + Token() + ".tmp");
        bool created = false;
        try
        {
            using (var stream = Create(temporary))
            {
                created = true;
                stream.Write(bytes);
                stream.Flush(true);
            }
            if (lease is not null && !Owns(lease, token!))
                throw new IOException("the idempotency store lease is no longer this writer's; the key was not stored");
            File.Move(temporary, Path, true);
        }
        finally
        {
            if (created) TryDelete(temporary);
        }
    }

    private static FileStream Create(string path)
    {
        var options = new FileStreamOptions { Mode = FileMode.CreateNew, Access = FileAccess.Write, Share = FileShare.None };
        if (!OperatingSystem.IsWindows()) options.UnixCreateMode = PrivateMode;
        return new FileStream(path, options);
    }

    private static string Token() => Convert.ToHexString(RandomNumberGenerator.GetBytes(16)).ToLowerInvariant();

    private static string Host()
    {
        try { return Environment.MachineName; }
        catch (InvalidOperationException) { return ""; }
    }

    private static string Acquire(string lease)
    {
        string token = Token();
        byte[] mine = Utf8.GetBytes(Json.Text(new JsonObject
        {
            ["version"] = 1, ["host"] = Host(), ["pid"] = Environment.ProcessId, ["token"] = token,
        }) + "\n");
        long started = Stopwatch.GetTimestamp();
        int delay = 5;
        while (true)
        {
            bool created = false;
            try
            {
                using (var stream = Create(lease))
                {
                    created = true;
                    stream.Write(mine);
                    stream.Flush(true);
                }
                return token;
            }
            catch (IOException) when (!created && File.Exists(lease))
            {
                // A busy, incomplete or abandoned lease is never reclaimed.
            }
            catch
            {
                if (created) Release(lease, token);
                throw;
            }
            double left = 2000 - Stopwatch.GetElapsedTime(started).TotalMilliseconds;
            if (left <= 0)
            {
                var owner = Owner(lease);
                string description = owner is null ? "" : $" (process {owner["pid"]} on {Json.Text(owner["host"])})";
                throw new IOException("the idempotency store is locked by another writer" + description +
                    "; try again, or remove the lease only after its owner no longer runs");
            }
            Thread.Sleep(TimeSpan.FromMilliseconds(Math.Min(delay, left)));
            delay = Math.Min(delay * 2, 100);
        }
    }

    private static JsonObject? Owner(string lease)
    {
        try
        {
            var parsed = Json.Parse(File.ReadAllBytes(lease));
            if (parsed is JsonObject owner && Json.TryInteger(owner["version"], out var version) && version == 1 &&
                Json.TryString(owner["host"], out _) && Json.TryInteger(owner["pid"], out var pid) && pid > 0 && pid <= 9007199254740991 &&
                Json.TryString(owner["token"], out var token) && token.Length > 0)
                return owner;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonParseException)
        {
            // A malformed lease still belongs to another writer.
        }
        return null;
    }

    private static bool Owns(string lease, string token) => Owner(lease) is { } owner && Json.AsString(owner["token"]) == token;

    private static void Release(string lease, string token)
    {
        if (Owns(lease, token)) TryDelete(lease);
    }

    private static void TryDelete(string path)
    {
        try { File.Delete(path); }
        catch (IOException) { }
        catch (UnauthorizedAccessException) { }
    }
}
