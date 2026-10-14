// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Keeps the idempotency key of each logical call, so a retry of the same
/// intent reuses it ([ClientOptions.idempotencyStore]).
abstract interface class IdempotencyStore {
  /// The key stored for a logical call, or null.
  FutureOr<String?> get(String scope, String logicalId);

  /// Stores the key of a logical call.
  FutureOr<void> put(String scope, String logicalId, String key);
}

/// The default [IdempotencyStore]: in memory, for the life of the client.
final class MemoryIdempotencyStore implements IdempotencyStore {
  final Map<String, String> _keys = {};

  @override
  String? get(String scope, String logicalId) => _keys['$scope\n$logicalId'];

  @override
  void put(String scope, String logicalId, String key) {
    _keys['$scope\n$logicalId'] = key;
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
    return (int.parse(m[1]!), m[2]!);
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
