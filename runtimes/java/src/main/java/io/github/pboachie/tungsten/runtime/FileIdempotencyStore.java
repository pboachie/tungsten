// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.ByteBuffer;
import java.nio.channels.FileChannel;
import java.nio.charset.StandardCharsets;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.CharacterCodingException;
import java.nio.file.FileAlreadyExistsException;
import java.nio.file.Files;
import java.nio.file.NoSuchFileException;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.nio.file.attribute.FileAttribute;
import java.nio.file.attribute.PosixFilePermissions;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

/**
 * Durable version-1 keys, interoperable with the TypeScript, Python, Go and Rust stores.
 * Writes use a private temporary file and an atomic rename in the same directory. Reserve
 * holds the shared {@code <path>.lock} owner lease, waits up to two seconds for another writer,
 * and fences its token immediately before committing. Only the current owner removes a lease;
 * an abandoned lease requires manual recovery after its owner is known to have stopped.
 * Corrupt or unreadable stores fail closed, rather than generating a second key for an intent.
 */
public final class FileIdempotencyStore implements IdempotencyStore {
    private static final ConcurrentHashMap<Path, Object> FILE_LOCKS = new ConcurrentHashMap<>();
    private static final long LEASE_WAIT_NANOS = TimeUnit.SECONDS.toNanos(2);
    private final Path path;
    private final Object monitor;

    public FileIdempotencyStore(String path) {
        this(Path.of(path));
    }

    public FileIdempotencyStore(Path path) {
        this.path = Objects.requireNonNull(path, "path").toAbsolutePath().normalize();
        if (this.path.getFileName() == null) {
            throw new IllegalArgumentException("the idempotency store path must name a file");
        }
        monitor = FILE_LOCKS.computeIfAbsent(this.path, ignored -> new Object());
    }

    public Path path() {
        return path;
    }

    @Override
    public String get(String scope, String logicalId) {
        synchronized (monitor) {
            try {
                Map<String, String> keys = read().get(scope);
                return keys == null ? null : keys.get(logicalId);
            } catch (IOException e) {
                throw failure(e);
            }
        }
    }

    @Override
    public void put(String scope, String logicalId, String key) {
        Objects.requireNonNull(key, "key");
        synchronized (monitor) {
            try {
                Map<String, Map<String, String>> data = read();
                data.computeIfAbsent(scope, ignored -> new TreeMap<>()).put(logicalId, key);
                write(data, null, null);
            } catch (IOException e) {
                throw failure(e);
            }
        }
    }

    @Override
    public String reserve(String scope, String logicalId, String candidate) {
        Objects.requireNonNull(candidate, "candidate");
        synchronized (monitor) {
            Path lease = Path.of(path + ".lock");
            String token = null;
            try {
                Files.createDirectories(path.getParent());
                token = acquire(lease);
                Map<String, Map<String, String>> data = read();
                Map<String, String> keys = data.computeIfAbsent(scope, ignored -> new TreeMap<>());
                String existing = keys.get(logicalId);
                if (existing != null && !existing.isEmpty()) {
                    return existing;
                }
                keys.put(logicalId, candidate);
                write(data, lease, token);
                return candidate;
            } catch (IOException e) {
                throw failure(e);
            } finally {
                if (token != null) {
                    release(lease, token);
                }
            }
        }
    }

    private static Object parse(byte[] bytes) throws IOException {
        try {
            String text = StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(bytes)).toString();
            return Json.parse(text);
        } catch (CharacterCodingException e) {
            throw new IOException("the idempotency store file is not valid UTF-8");
        }
    }

    private static UncheckedIOException failure(IOException cause) {
        return new UncheckedIOException("the idempotency store could not be accessed safely", cause);
    }

    private Map<String, Map<String, String>> read() throws IOException {
        byte[] bytes;
        try {
            bytes = Files.readAllBytes(path);
        } catch (NoSuchFileException e) {
            return new TreeMap<>();
        }
        Object parsed;
        try {
            parsed = parse(bytes);
        } catch (RuntimeException e) {
            throw new IOException("the idempotency store file is not valid JSON");
        }
        if (!(parsed instanceof JsonObject document)
                || !Long.valueOf(1).equals(document.get("version"))
                || !(document.get("keys") instanceof JsonObject scopes)) {
            throw new IOException("the idempotency store file has an unknown format");
        }
        Map<String, Map<String, String>> result = new TreeMap<>();
        for (String scope : scopes.keys()) {
            if (!(scopes.get(scope) instanceof JsonObject entries)) {
                throw new IOException("the idempotency store file has an unknown format");
            }
            Map<String, String> keys = new TreeMap<>();
            for (String id : entries.keys()) {
                if (!(entries.get(id) instanceof String key)) {
                    throw new IOException("the idempotency store file has an unknown format");
                }
                keys.put(id, key);
            }
            result.put(scope, keys);
        }
        return result;
    }

    private void write(Map<String, Map<String, String>> data, Path lease, String token) throws IOException {
        JsonObject scopes = new JsonObject();
        for (Map.Entry<String, Map<String, String>> scope : data.entrySet()) {
            JsonObject keys = new JsonObject();
            scope.getValue().forEach(keys::set);
            scopes.set(scope.getKey(), keys);
        }
        byte[] bytes = (Json.write(JsonObject.of("version", 1, "keys", scopes)) + "\n")
                .getBytes(StandardCharsets.UTF_8);
        Files.createDirectories(path.getParent());
        Path temporary;
        try {
            temporary = Files.createTempFile(path.getParent(), "." + path.getFileName() + ".", ".tmp", permissions());
        } catch (UnsupportedOperationException e) {
            temporary = Files.createTempFile(path.getParent(), "." + path.getFileName() + ".", ".tmp");
        }
        try {
            try (FileChannel channel = FileChannel.open(temporary, StandardOpenOption.WRITE)) {
                writeAll(channel, bytes);
                channel.force(true);
            }
            if (lease != null && !owns(lease, token)) {
                throw new IOException("the idempotency store lease is no longer this writer's; the key was not stored");
            }
            Files.move(temporary, path, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING);
        } finally {
            try {
                Files.deleteIfExists(temporary);
            } catch (IOException e) {
                // Cleanup cannot change an already committed reservation's outcome.
            }
        }
    }

    private static FileAttribute<?>[] permissions() {
        return new FileAttribute<?>[] {PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------"))};
    }

    private static void writeAll(FileChannel channel, byte[] bytes) throws IOException {
        ByteBuffer buffer = ByteBuffer.wrap(bytes);
        while (buffer.hasRemaining()) {
            channel.write(buffer);
        }
    }

    private static FileChannel createLease(Path lease) throws IOException {
        var options = java.util.Set.of(StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE);
        try {
            return FileChannel.open(lease, options, permissions());
        } catch (UnsupportedOperationException e) {
            return FileChannel.open(lease, options);
        }
    }

    private static String host() {
        String host = System.getenv("COMPUTERNAME");
        if (host == null) {
            host = System.getenv("HOSTNAME");
        }
        return host == null ? "" : host;
    }

    private static String acquire(Path lease) throws IOException {
        String token = Text.hex(Text.randomBytes(16));
        byte[] mine = (Json.write(JsonObject.of("version", 1, "host", host(),
                "pid", ProcessHandle.current().pid(), "token", token)) + "\n").getBytes(StandardCharsets.UTF_8);
        long started = System.nanoTime();
        long delay = TimeUnit.MILLISECONDS.toNanos(5);
        for (;;) {
            boolean created = false;
            try (FileChannel channel = createLease(lease)) {
                created = true;
                writeAll(channel, mine);
                channel.force(true);
                return token;
            } catch (FileAlreadyExistsException busy) {
                // Another owner, including an incomplete or abandoned lease, is never reclaimed.
            } catch (IOException e) {
                if (created) {
                    release(lease, token);
                }
                throw e;
            }
            long left = LEASE_WAIT_NANOS - (System.nanoTime() - started);
            if (left <= 0) {
                JsonObject owner = owner(lease);
                String description = owner == null ? "" : " (process " + owner.get("pid")
                        + " on " + Json.write(owner.get("host")) + ")";
                throw new IOException("the idempotency store is locked by another writer" + description
                        + "; try again, or remove the lease only after its owner no longer runs");
            }
            try {
                TimeUnit.NANOSECONDS.sleep(Math.min(delay, left));
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new IOException("interrupted while waiting for the idempotency store lease");
            }
            delay = Math.min(delay * 2, TimeUnit.MILLISECONDS.toNanos(100));
        }
    }

    private static JsonObject owner(Path lease) {
        try {
            Object parsed = parse(Files.readAllBytes(lease));
            if (parsed instanceof JsonObject owner
                    && Long.valueOf(1).equals(owner.get("version"))
                    && owner.get("host") instanceof String
                    && owner.get("pid") instanceof Long pid && pid > 0 && pid <= 9007199254740991L
                    && owner.get("token") instanceof String token && !token.isEmpty()) {
                return owner;
            }
        } catch (IOException | RuntimeException e) {
            // An unreadable or incomplete lease still belongs to another writer.
        }
        return null;
    }

    private static boolean owns(Path lease, String token) {
        JsonObject owner = owner(lease);
        return owner != null && token.equals(owner.get("token"));
    }

    private static void release(Path lease, String token) {
        if (owns(lease, token)) {
            try {
                Files.deleteIfExists(lease);
            } catch (IOException e) {
                // The reservation's outcome is already decided; cleanup cannot fail it.
            }
        }
    }
}
