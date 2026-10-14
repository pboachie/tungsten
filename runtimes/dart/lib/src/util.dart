// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Helpers shared by the pipeline: field paths, redaction, secret
/// scrubbing, digests.
abstract final class _Util {
  /// The longest string kept in received_value, in UTF-16 code units.
  static const int maxReceivedChars = 200;

  /// The deepest JSON nesting accepted in arguments.
  static const int maxArgDepth = 256;

  /// The largest integer a JavaScript number holds exactly.
  static const int maxSafeInteger = 9007199254740991;

  static const List<String> _sensitiveWords = [
    'secret',
    'password',
    'passwd',
    'passphrase',
    'token',
    'apikey',
    'api-key',
    'api_key',
    'privatekey',
    'private-key',
    'private_key',
    'credential',
    'authorization',
    'cookie',
    'session',
  ];

  static final math.Random _secure = math.Random.secure();

  static String sha256Hex(List<int> data) =>
      crypto.sha256.convert(data).toString();

  static List<int> hmacSha256(List<int> key, List<int> message) =>
      crypto.Hmac(crypto.sha256, key).convert(message).bytes;

  static String base64url(List<int> data) =>
      base64Url.encode(data).replaceAll('=', '');

  static bool timingSafeEqual(String a, String b) {
    if (a.length != b.length) return false;
    var diff = 0;
    for (var i = 0; i < a.length; i++) {
      diff |= a.codeUnitAt(i) ^ b.codeUnitAt(i);
    }
    return diff == 0;
  }

  static List<int> randomBytes(int n) =>
      List<int>.generate(n, (_) => _secure.nextInt(256));

  /// A random UUID version 4.
  static String uuidV4() {
    final b = randomBytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    final h = [for (final x in b) x.toRadixString(16).padLeft(2, '0')].join();
    return '${h.substring(0, 8)}-${h.substring(8, 12)}-${h.substring(12, 16)}-'
        '${h.substring(16, 20)}-${h.substring(20)}';
  }

  /// A field path (a.b, items[0].id, items.0.id) as segments.
  static List<String> splitPath(String? path) {
    if (path == null || path.isEmpty || path == '.') return const [];
    final dotted =
        path.replaceAllMapped(RegExp(r'\[(\d+)\]'), (m) => '.${m[1]}');
    return [
      for (final s in dotted.split('.'))
        if (s.isNotEmpty) s,
    ];
  }

  static bool isIndex(Object? segment) =>
      segment is int || (segment is String && RegExp(r'^\d+$').hasMatch(segment));

  /// The value at a path of segments (strings or ints), or [unset].
  static Object? getPath(Object? value, List<Object> segments) {
    if (identical(value, unset)) return unset;
    var current = value;
    for (final seg in segments) {
      if (current is List) {
        final key = seg is int ? seg : int.tryParse('$seg');
        if (key == null || !isIndex(seg) || key >= current.length) return unset;
        current = current[key];
      } else if (current is Map) {
        final key = '$seg';
        if (!current.containsKey(key)) return unset;
        current = current[key];
      } else {
        return unset;
      }
    }
    return current;
  }

  static Object? getPathStr(Object? value, String path) =>
      getPath(value, splitPath(path));

  /// Whether a name very likely holds a secret.
  static bool looksSensitive(String name) {
    final lower = name.toLowerCase();
    return _sensitiveWords.any(lower.contains);
  }

  static Object? redactSensitiveKeys(Object? value, [int depth = 0]) {
    if (depth > 32) return null;
    if (value is List) {
      return [for (final v in value) redactSensitiveKeys(v, depth + 1)];
    }
    if (value is Map) {
      return <String, Object?>{
        for (final e in value.entries)
          '${e.key}': looksSensitive('${e.key}')
              ? redacted
              : redactSensitiveKeys(e.value, depth + 1),
      };
    }
    return value;
  }

  static Object? _redactAt(Object? value, List<String> segments) {
    if (segments.isEmpty) return redacted;
    final head = segments.first;
    final rest = segments.sublist(1);
    if (value is List) return [for (final v in value) _redactAt(v, segments)];
    if (value is Map) {
      if (!value.containsKey(head)) return value;
      final out = <String, Object?>{
        for (final e in value.entries) '${e.key}': e.value,
      };
      out[head] = _redactAt(value[head], rest);
      return out;
    }
    return value;
  }

  /// A copy of a value with every listed field path redacted; arrays on the
  /// way fan out.
  static Object? redactPaths(Object? value, List<String> paths) {
    var out = Json.copy(value);
    for (final p in paths) {
      final segs = splitPath(p);
      if (segs.isNotEmpty) out = _redactAt(out, segs);
    }
    return out;
  }

  static String cut(String text) {
    if (text.length <= maxReceivedChars) return text;
    return '${Json.takeUnits(text, maxReceivedChars - 1)}…';
  }

  /// A value made safe for received_value: redacted when sensitive, strings
  /// cut to 200 UTF-16 code units, containers with sensitive members
  /// redacted and cut as JSON text when long.
  static Object? envelopeValue(Object? value, bool sensitive) {
    if (sensitive) return redacted;
    if (identical(value, unset)) return null;
    if (value is String) return cut(value);
    if (value is Binary) return '<${value.length} bytes>';
    if (value is Uint8List) return '<${value.length} bytes>';
    if (value is List || value is Map) {
      final bin = Binary.tryFrom(value);
      if (bin != null) return '<${bin.length} bytes>';
      final text = Json.text(redactSensitiveKeys(value));
      if (text.length > maxReceivedChars) return cut(text);
      final parsed = Json.tryParse(text);
      return identical(parsed, unset) ? null : parsed;
    }
    return value;
  }

  /// Every occurrence of the secrets (four characters or more) replaced.
  static String? scrubText(String? text, _Secrets? secrets) {
    if (text == null || secrets == null) return text;
    var out = text;
    for (final secret in secrets.items) {
      if (secret.length >= 4 && out.contains(secret)) {
        out = out.replaceAll(secret, redacted);
      }
    }
    return out;
  }

  static Object? scrubValue(Object? value, _Secrets secrets) {
    if (value is String) return scrubText(value, secrets);
    if (value is List || value is Map) {
      final parsed = Json.tryParse(scrubText(Json.text(value), secrets)!);
      return identical(parsed, unset) ? redacted : parsed;
    }
    return value;
  }

  /// The envelope with the secrets scrubbed from its text members: a last
  /// line of defence.
  static Diagnostic scrubDiagnostic(Diagnostic d, _Secrets? secrets) {
    if (secrets == null || secrets.items.isEmpty) return d;
    return Diagnostic(
      category: d.category,
      operation: d.operation,
      httpStatus: d.httpStatus,
      code: scrubText(d.code, secrets),
      failedParameter: scrubText(d.failedParameter, secrets),
      receivedValue: scrubValue(d.receivedValue, secrets),
      expected: scrubText(d.expected, secrets),
      remediation: scrubText(d.remediation, secrets)!,
      retryable: d.retryable,
      retryAfterMs: d.retryAfterMs,
      nextAction: scrubText(d.nextAction, secrets),
      requestId: scrubText(d.requestId, secrets),
      attempts: d.attempts,
    );
  }

  /// The Accept of a stream call: the caller's media ranges in order
  /// without repeats, text/event-stream first when they do not name it.
  static String mergeAccept(String value) {
    final ranges = <String>[];
    for (final part in value.split(',')) {
      final r = part.trim();
      if (r.isNotEmpty && !ranges.contains(r)) ranges.add(r);
    }
    final names = ranges.any(
      (r) => r.split(';').first.trim().toLowerCase() == 'text/event-stream',
    );
    if (!names) ranges.insert(0, 'text/event-stream');
    return ranges.join(', ');
  }

  /// Whether a value nests deeper than [limit].
  static bool nestsDeeper(Object? value, int limit) {
    if (value is List) {
      return limit == 0 || value.any((v) => nestsDeeper(v, limit - 1));
    }
    if (value is Map) {
      return limit == 0 || value.values.any((v) => nestsDeeper(v, limit - 1));
    }
    return false;
  }

  /// A finite number within [min, max], else the fallback.
  static num bounded(Object? value, num fallback, num min, num max) {
    if (value is! num) return fallback;
    if (value is double && (value.isNaN || value.isInfinite)) return fallback;
    return value.clamp(min, max);
  }

  /// A number as JSON writes it, integral doubles as ints.
  static Object asNumber(num value) {
    if (value is double &&
        value == value.truncateToDouble() &&
        value.abs() < maxSafeInteger) {
      return value.toInt();
    }
    return value;
  }

  /// A string literal as JSON.stringify writes it.
  static String jsString(String text) => Json.quote(text);

  /// Remediation JSON, cut to 80 characters.
  static String shortJson(Object? value) {
    final text = Json.text(value);
    return text.length > 80 ? '${text.substring(0, 77)}...' : text;
  }

  /// " with name value, ..." for remediation text.
  static String withArguments(Object? args) {
    if (args is! Map || args.isEmpty) return '';
    final parts = [
      for (final e in args.entries) '${e.key} ${shortJson(e.value)}',
    ];
    return ' with ${parts.join(', ')}';
  }

  static String plural(int n, String one, String many) => n == 1 ? one : many;
}

/// Secret values to scrub from texts, in insertion order.
final class _Secrets {
  _Secrets([Iterable<String> items = const []]) : items = [...items];

  final List<String> items;

  void add(Object? secret) {
    if (secret == null) return;
    final text = '$secret';
    if (text.isNotEmpty && !items.contains(text)) items.add(text);
  }

  _Secrets copy() => _Secrets(items);
}

/// One async critical section at a time (key generation, token refresh).
final class _Lock {
  Future<void> _tail = Future<void>.value();

  Future<T> run<T>(Future<T> Function() body) {
    final previous = _tail;
    final done = Completer<void>();
    _tail = done.future;
    return previous.then((_) => body()).whenComplete(done.complete);
  }
}
