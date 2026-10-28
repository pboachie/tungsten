// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Keeps the idempotency key of each logical call, so a retry of the same
/// intent reuses it ([ClientOptions.idempotencyStore]).
///
/// A store needs only [get] and [put]. Allocating an automatic key goes
/// through `reserve` (see [IdempotencyStoreReserve] and
/// [AtomicIdempotencyStore]): a get/put-only store is serialized per store
/// instance, a store shared with other clients or processes should implement
/// [AtomicIdempotencyStore].
abstract interface class IdempotencyStore {
  /// The key stored for a logical call, or null.
  FutureOr<String?> get(String scope, String logicalId);

  /// Stores the key of a logical call.
  FutureOr<void> put(String scope, String logicalId, String key);
}

/// An [IdempotencyStore] that chooses a key atomically.
///
/// Implement it, rather than relying on the get/put fallback of
/// [IdempotencyStoreReserve.reserve], when the storage is shared with other
/// clients or processes: the check and the write must be one transaction or
/// one lease. [MemoryIdempotencyStore] and [FileIdempotencyStore] are atomic.
abstract interface class AtomicIdempotencyStore implements IdempotencyStore {
  /// Returns the existing non-empty key of a logical call, or stores
  /// [candidate] as its key and returns it.
  FutureOr<String> reserve(String scope, String logicalId, String candidate);
}

/// `reserve` for every [IdempotencyStore], so a custom store written against
/// get and put alone keeps working.
extension IdempotencyStoreReserve on IdempotencyStore {
  static final Expando<Future<void>> _queues = Expando('idempotency reserve');

  /// Returns the existing non-empty key of a logical call, or persists
  /// [candidate] and returns it.
  ///
  /// An [AtomicIdempotencyStore] decides itself. Any other store is read and
  /// written in turn, one reservation at a time per store instance, and the
  /// write is read back; that makes the choice safe among the users of this
  /// instance only. Throws a [StateError] when the key cannot be read back.
  Future<String> reserve(String scope, String logicalId, String candidate) {
    final store = this;
    if (store is AtomicIdempotencyStore) {
      return Future.sync(() => store.reserve(scope, logicalId, candidate));
    }
    final result = (_queues[store] ?? Future<void>.value()).then((_) async {
      final existing = await store.get(scope, logicalId);
      if (existing != null && existing.isNotEmpty) return existing;
      await store.put(scope, logicalId, candidate);
      if (await store.get(scope, logicalId) != candidate) {
        throw StateError('the idempotency key could not be stored');
      }
      return candidate;
    });
    _queues[store] = result.then<void>((_) {}, onError: (Object _) {});
    return result;
  }
}

/// The default [IdempotencyStore]: in memory, for the life of the client.
final class MemoryIdempotencyStore implements AtomicIdempotencyStore {
  final Map<String, String> _keys = {};

  @override
  String? get(String scope, String logicalId) => _keys['$scope\n$logicalId'];

  @override
  void put(String scope, String logicalId, String key) {
    _keys['$scope\n$logicalId'] = key;
  }

  @override
  String reserve(String scope, String logicalId, String candidate) {
    final existing = _keys['$scope\n$logicalId'];
    if (existing != null && existing.isNotEmpty) return existing;
    _keys['$scope\n$logicalId'] = candidate;
    return candidate;
  }
}

/// A durable [IdempotencyStore] in one JSON file, so a client restarted after
/// a crash reuses the key of an intent it had already started.
///
/// The version-1 file and the `<path>.lock` owner lease are the ones of the
/// TypeScript, Python, Rust, Go, Java, C# and Ruby `FileIdempotencyStore`s, so
/// processes written in any of these languages can share one store. The file
/// is `{"version": 1, "keys": {scope: {logicalId: key}}}`, scopes and ids
/// sorted, written by an exclusive mode-0600 temporary file in the same
/// directory, flushed and renamed over the store. An unreadable or malformed
/// file (invalid UTF-8 or JSON, an unknown version or shape) makes every call
/// fail with a [FileSystemException] and is never overwritten: the client then
/// does not send the call rather than issue a second key for the same intent.
///
/// [reserve] holds the lease `<path>.lock` (a JSON owner record with the
/// version, host, process id and a random token), waits up to two seconds for
/// another writer, then fails, and checks that the lease is still its own just
/// before the rename. Only the owner removes a lease: one left by a process
/// that died is never reclaimed automatically; remove it by hand once its owner
/// (named in the error) no longer runs. [put] writes directly and neither
/// waits for nor takes the lease; the client allocates keys with [reserve].
///
/// Instances of one path in a process also take turns. The mode 0600 of the
/// files is set with `chmod` on POSIX systems; where it cannot be set, files
/// readable by others are refused.
final class FileIdempotencyStore implements AtomicIdempotencyStore {
  /// A store in the file at [path]; the file and its directory are created
  /// on the first write.
  FileIdempotencyStore(String path) : path = _normalize(path);

  /// The absolute, normalized path of the store file.
  final String path;

  static const Duration _leaseWait = Duration(seconds: 2);
  static final Map<String, Future<void>> _tails = {};

  static String _normalize(String path) {
    if (path.isEmpty) {
      throw ArgumentError.value(path, 'path', 'must name a file');
    }
    return File(path).absolute.uri.normalizePath().toFilePath();
  }

  String get _lease => '$path.lock';

  Future<T> _turn<T>(Future<T> Function() action) {
    final previous = _tails[path] ?? Future<void>.value();
    final run = previous.then((_) => action());
    final tail = run.then<void>((_) {}, onError: (Object _) {});
    _tails[path] = tail;
    tail.whenComplete(() {
      if (identical(_tails[path], tail)) _tails.remove(path);
    });
    return run;
  }

  @override
  Future<String?> get(String scope, String logicalId) =>
      _turn(() async => (await _read())[scope]?[logicalId]);

  @override
  Future<void> put(String scope, String logicalId, String key) =>
      _turn(() async {
        final data = await _read();
        (data[scope] ??= {})[logicalId] = key;
        await _write(data, null);
      });

  @override
  Future<String> reserve(String scope, String logicalId, String candidate) =>
      _turn(() async {
        await File(path).parent.create(recursive: true);
        final token = await _acquire();
        try {
          final data = await _read();
          final existing = data[scope]?[logicalId];
          if (existing != null && existing.isNotEmpty) return existing;
          (data[scope] ??= {})[logicalId] = candidate;
          await _write(data, token);
          return candidate;
        } finally {
          await _release(token);
        }
      });

  FileSystemException _fail(String message, [String? at]) =>
      FileSystemException(message, at ?? path);

  Future<Map<String, Map<String, String>>> _read() async {
    final List<int> bytes;
    try {
      bytes = await File(path).readAsBytes();
    } on PathNotFoundException {
      return {};
    }
    final String text;
    try {
      text = utf8.decode(bytes);
    } on FormatException {
      throw _fail('the idempotency store file is not valid UTF-8');
    }
    final Object? document;
    try {
      // utf8.decode drops a byte order mark, which is not JSON.
      if (bytes.length >= 3 &&
          bytes[0] == 0xEF &&
          bytes[1] == 0xBB &&
          bytes[2] == 0xBF) {
        throw const FormatException('byte order mark');
      }
      document = jsonDecode(text);
    } on FormatException {
      throw _fail('the idempotency store file is not valid JSON');
    }
    const unknown = 'the idempotency store file has an unknown format';
    if (document is! Map || document['version'] is! int) throw _fail(unknown);
    final scopes = document['keys'];
    if (document['version'] != 1 || scopes is! Map) throw _fail(unknown);
    final result = <String, Map<String, String>>{};
    for (final MapEntry(key: scope, value: entries) in scopes.entries) {
      if (scope is! String || entries is! Map) throw _fail(unknown);
      final keys = <String, String>{};
      for (final MapEntry(key: id, value: key) in entries.entries) {
        if (id is! String || key is! String) throw _fail(unknown);
        keys[id] = key;
      }
      result[scope] = keys;
    }
    return result;
  }

  /// Writes the whole store through a private temporary file. With a lease
  /// [token], the lease must still be this writer's just before the rename.
  Future<void> _write(
    Map<String, Map<String, String>> data,
    String? token,
  ) async {
    final sorted = SplayTreeMap<String, Object?>();
    for (final scope in data.keys) {
      sorted[scope] = SplayTreeMap<String, String>.of(data[scope]!);
    }
    final text = '${const JsonEncoder.withIndent('  ').convert({
          'version': 1,
          'keys': sorted,
        })}\n';
    await File(path).parent.create(recursive: true);
    final temporary = File('$path.${_hex(6)}.tmp');
    var created = false;
    try {
      await temporary.create(exclusive: true);
      created = true;
      await _restrict(temporary);
      final file = await temporary.open(mode: FileMode.writeOnly);
      try {
        await file.writeFrom(utf8.encode(text));
        await file.flush();
      } finally {
        await file.close();
      }
      if (token != null && !await _owns(token)) {
        throw _fail(
          'the idempotency store lease is no longer this writer\'s; the key was not stored',
          _lease,
        );
      }
      await temporary.rename(path);
    } finally {
      if (created) {
        try {
          await temporary.delete();
        } on FileSystemException {
          // Cleanup cannot change an already committed reservation.
        }
      }
    }
  }

  /// Gives a file the owner-only mode 0600 where the platform has modes.
  Future<void> _restrict(File file) async {
    if (Platform.isWindows) return;
    try {
      await Process.run('/bin/chmod', ['600', file.path]);
    } on ProcessException {
      // The mode is checked below.
    }
    if ((await file.stat()).mode & 0x3f != 0) {
      throw _fail(
        'the idempotency store file could not be restricted to its owner (mode 0600)',
        file.path,
      );
    }
  }

  static String _hex(int bytes) => [
        for (final b in _Util.randomBytes(bytes))
          b.toRadixString(16).padLeft(2, '0'),
      ].join();

  /// Takes the lease, waiting up to two seconds. Never reclaims one.
  Future<String> _acquire() async {
    final token = _hex(16);
    final mine = '${jsonEncode({
          'version': 1,
          'host': Platform.localHostname,
          'pid': pid,
          'token': token,
        })}\n';
    final lease = File(_lease);
    final clock = Stopwatch()..start();
    var delay = const Duration(milliseconds: 5);
    for (;;) {
      var created = false;
      try {
        await lease.create(exclusive: true);
        created = true;
        await _restrict(lease);
        final file = await lease.open(mode: FileMode.writeOnly);
        try {
          await file.writeFrom(utf8.encode(mine));
          await file.flush();
        } finally {
          await file.close();
        }
        return token;
      } on FileSystemException {
        if (created) {
          // The lease is this call's own, but not yet complete: remove it.
          try {
            await lease.delete();
          } on FileSystemException {
            // Nothing more to do.
          }
          rethrow;
        }
        // Another owner, including an incomplete or abandoned lease, is
        // never reclaimed; any other failure to create it is final.
        if (await FileSystemEntity.type(_lease, followLinks: false) ==
            FileSystemEntityType.notFound) {
          rethrow;
        }
      }
      final left = _leaseWait - clock.elapsed;
      if (left <= Duration.zero) {
        final owner = await _owner();
        final who = owner == null
            ? ''
            : ' (process ${owner['pid']} on ${jsonEncode(owner['host'])})';
        throw _fail(
          'the idempotency store is locked by another writer$who; try again, '
          'or remove the lease only after its owner no longer runs',
          _lease,
        );
      }
      await Future<void>.delayed(delay < left ? delay : left);
      delay = delay * 2 > const Duration(milliseconds: 100)
          ? const Duration(milliseconds: 100)
          : delay * 2;
    }
  }

  /// The owner record of the lease, or null when it is missing or not a
  /// complete record (which still belongs to another writer).
  Future<Map<String, Object?>?> _owner() async {
    try {
      final owner = jsonDecode(utf8.decode(await File(_lease).readAsBytes()));
      if (owner is Map<String, Object?> &&
          owner['version'] == 1 &&
          owner['version'] is int &&
          owner['host'] is String &&
          owner['pid'] is int &&
          (owner['pid']! as int) > 0 &&
          (owner['pid']! as int) <= 9007199254740991 &&
          owner['token'] is String &&
          (owner['token']! as String).isNotEmpty) {
        return owner;
      }
    } on FormatException {
      // An unreadable or incomplete lease.
    } on FileSystemException {
      // Gone or unreadable.
    }
    return null;
  }

  Future<bool> _owns(String token) async => (await _owner())?['token'] == token;

  /// Removes the lease if it is still this writer's.
  Future<void> _release(String token) async {
    try {
      if (await _owns(token)) await File(_lease).delete();
    } on FileSystemException {
      // The reservation's outcome is decided; cleanup cannot fail it.
    }
  }
}

/// Idempotency key formats and confirmation tokens.
abstract final class _Keys {
  static String formatKind(Object? format) {
    final f = _str(format).toLowerCase().replaceAll(RegExp('[^a-z0-9]'), '');
    return switch (f) {
      'uuidv4' || 'uuid4' => 'uuid_v4',
      'uuid' => 'uuid',
      'asciitoken128' => 'ascii_token_128',
      _ => 'token',
    };
  }

  /// What a policy's key format expects, in words.
  static String formatDescription(Object? format) =>
      switch (formatKind(format)) {
        'uuid_v4' => 'UUIDv4 string',
        'uuid' => 'UUID string',
        'ascii_token_128' =>
          '1-128 ASCII letters, digits, dots, underscores or hyphens',
        _ => 'a non-empty printable ASCII string of at most 255 characters',
      };

  static bool _uuid(String key, bool v4Only) {
    final version = v4Only ? '4' : '[1-8]';
    return RegExp(
      '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-$version[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}\$',
    ).hasMatch(key);
  }

  /// The expected format when the key does not match it, else null.
  static String? checkFormat(String? key, Object? format) {
    final k = key ?? '';
    final valid = switch (formatKind(format)) {
      'uuid_v4' => _uuid(k, true),
      'uuid' => _uuid(k, false),
      'ascii_token_128' => RegExp(r'^[A-Za-z0-9._-]{1,128}$').hasMatch(k),
      _ => k.isNotEmpty &&
          utf8.encode(k).length <= 255 &&
          RegExp(r'^[\x21-\x7e]+$').hasMatch(k),
    };
    return valid ? null : formatDescription(format);
  }

  /// The wire header of an operation's key.
  static String header(OperationDescriptor op) {
    final h = op._idempotency['header'];
    if (h is String && h.isNotEmpty) return h;
    for (final p in op.params) {
      if (p.role == 'idempotency_key') return p.wire;
    }
    return 'Idempotency-Key';
  }

  /// The lifetime of a confirmation token.
  static const int ttlMs = 5 * 60 * 1000;
  static const String _prefix = 'tgc1';

  /// The SHA-256 of the canonical JSON of the arguments.
  static String argsDigest(Object? args) =>
      _Util.sha256Hex(utf8.encode(Json.canonical(args)));

  static String _signature(
    List<int> key,
    String operation,
    String digest,
    int expiry,
  ) =>
      _Util.base64url(
        _Util.hmacSha256(key, utf8.encode('$operation\n$digest\n$expiry')),
      );

  /// tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, operation, args digest,
  /// expiry))>: valid only for the operation and the exact arguments.
  static String issueToken(
    List<int> key,
    String operation,
    Object? args,
    int now,
  ) {
    final expiry = now + ttlMs;
    return '$_prefix.$expiry.${_signature(key, operation, argsDigest(args), expiry)}';
  }

  static final RegExp _tokenShape =
      RegExp(r'^tgc1\.(\d{1,16})\.([A-Za-z0-9_-]{43})$');

  /// (expiry, signature) of a token of the right shape, else null.
  static (int, String)? tokenExpiry(String token) {
    final m = _tokenShape.firstMatch(token);
    if (m == null) return null;
    final expiry = int.parse(m[1]!);
    // A leading zero is another spelling of the same expiry: not canonical.
    if ('$expiry' != m[1]) return null;
    return (expiry, m[2]!);
  }

  /// valid, expired, mismatch or malformed.
  static String checkToken(
    List<int> key,
    String token,
    String operation,
    Object? args,
    int now,
  ) {
    final parsed = tokenExpiry(token);
    if (parsed == null) return 'malformed';
    final (expiry, given) = parsed;
    final expected = _signature(key, operation, argsDigest(args), expiry);
    if (!_Util.timingSafeEqual(expected, given)) return 'mismatch';
    return expiry > now ? 'valid' : 'expired';
  }
}
