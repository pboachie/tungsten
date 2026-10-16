// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

enum _Kind { response, streaming, notSent, lost, timeout }

/// What one attempt gave. A [_Kind.response] has [status], [headers]
/// (lower-cased names) and [body] (null with [bodyFailure] `timeout` or
/// `broken` when reading it failed); a [_Kind.streaming] one has [stream].
/// [_Kind.notSent]: the request never left (DNS, connect, TLS);
/// [_Kind.lost]: the connection failed after the request was written.
final class _Outcome {
  const _Outcome(
    this.kind, {
    this.status = 0,
    this.headers = const {},
    this.body,
    this.bodyFailure,
    this.stream,
    this.detail,
  });

  final _Kind kind;
  final int status;
  final Map<String, String> headers;
  final List<int>? body;
  final String? bodyFailure;
  final _EventBody? stream;
  final String? detail;
}

/// The unread body of an event stream: chunks as they arrive.
final class _EventBody {
  _EventBody(this._client, Stream<List<int>> body, this._idleMs) {
    _sub = body.listen(
      (chunk) => _push(('chunk', chunk)),
      onError: (Object _) => _push(('lost', null)),
      onDone: () => _push(('end', null)),
      cancelOnError: true,
    );
  }

  final HttpClient _client;
  final int _idleMs;
  late final StreamSubscription<List<int>> _sub;
  final Queue<(String, List<int>?)> _items = Queue();
  Completer<void>? _waiting;
  bool _closed = false;

  void _push((String, List<int>?) item) {
    _items.add(item);
    final w = _waiting;
    _waiting = null;
    if (w != null && !w.isCompleted) w.complete();
  }

  /// The next item: (`chunk`, bytes), (`end`), (`timeout`) or (`lost`).
  Future<(String, List<int>?)> read() async {
    if (_closed) return ('lost', null);
    if (_items.isEmpty) {
      final w = Completer<void>();
      _waiting = w;
      final timer = Timer(Duration(milliseconds: _idleMs), () {
        if (!w.isCompleted) w.complete();
      });
      await w.future;
      timer.cancel();
      if (_closed) return ('lost', null);
      if (_items.isEmpty) return ('timeout', null);
    }
    return _items.removeFirst();
  }

  /// Closes the connection; safe more than once.
  void close() {
    if (_closed) return;
    _closed = true;
    unawaited(_sub.cancel());
    _client.close(force: true);
    final w = _waiting;
    _waiting = null;
    if (w != null && !w.isCompleted) w.complete();
  }
}

abstract final class _Transport {
  static final Stopwatch _clock = Stopwatch()..start();

  static int monotonicMs() => _clock.elapsedMilliseconds;

  static const Map<int, String> _errno = {
    104: 'ECONNRESET',
    111: 'ECONNREFUSED',
    103: 'ECONNABORTED',
    107: 'ENOTCONN',
    32: 'EPIPE',
    110: 'ETIMEDOUT',
    99: 'EADDRNOTAVAIL',
    113: 'EHOSTUNREACH',
    101: 'ENETUNREACH',
  };

  /// Short, secret-free codes of a transport error.
  static String _detail(Object error, bool sent) {
    if (error is SocketException) {
      final code = error.osError?.errorCode;
      final name = code == null ? null : _errno[code];
      if (name != null) return name;
      if (error.osError == null || error.message.contains('lookup')) {
        if (!sent) return 'ENOTFOUND';
      }
      return sent ? 'CONNECTION_CLOSED' : 'connect error';
    }
    if (error is HandshakeException || error is TlsException) {
      return '$error'.contains('CERTIFICATE_VERIFY_FAILED')
          ? 'CERT_VERIFY_FAILED'
          : 'TLS_ERROR';
    }
    if (error is HttpException) return 'CONNECTION_CLOSED';
    return sent ? 'network error' : 'connect error';
  }

  static Map<String, String> _headersOf(HttpHeaders h) {
    final out = <String, String>{};
    h.forEach((name, values) {
      final lower = name.toLowerCase();
      out[lower] = lower == 'set-cookie'
          ? (values.isEmpty ? '' : values.last)
          : values.join(', ');
    });
    return out;
  }

  static bool _eventStream(Map<String, String> headers) =>
      (headers['content-type'] ?? '').split(';').first.trim().toLowerCase() ==
      'text/event-stream';

  /// Sends once. Never throws: every outcome is an [_Outcome].
  static Future<_Outcome> attempt({
    required String url,
    required String method,
    required List<(String, String)> headers,
    required _Encoded? body,
    required int timeoutMs,
    bool stream = false,
    int? idleMs,
    int? maxResponseBytes,
  }) async {
    final started = monotonicMs();
    int left() => math.max(timeoutMs - (monotonicMs() - started), 1);
    final client = HttpClient()
      ..autoUncompress = false
      ..userAgent = null
      ..connectionTimeout = Duration(milliseconds: timeoutMs);
    var handedOff = false;
    try {
      final Uri uri;
      try {
        uri = Uri.parse(url);
      } on FormatException {
        return const _Outcome(_Kind.notSent, detail: 'INVALID_REQUEST');
      }
      final HttpClientRequest request;
      try {
        request = await client
            .openUrl(method, uri)
            .timeout(Duration(milliseconds: left()));
      } on TimeoutException {
        return const _Outcome(_Kind.timeout);
      } catch (e) {
        return _Outcome(_Kind.notSent, detail: _detail(e, false));
      }
      List<int>? data;
      String? multipartType;
      if (body != null && !body.empty) {
        if (body.parts != null) {
          final (bytes, type) = _Serialize.multipartBody(body.parts!);
          data = bytes;
          multipartType = type;
        } else {
          data = body.bytes ?? const [];
        }
      }
      try {
        request
          ..followRedirects = false
          ..maxRedirects = 0;
        request.headers.removeAll(HttpHeaders.acceptEncodingHeader);
        for (final (name, value) in headers) {
          request.headers.set(name, value, preserveHeaderCase: true);
        }
        // Like a browser's fetch: any media type when the operation declares
        // none (dart:io sends no Accept header of its own).
        if (request.headers.value(HttpHeaders.acceptHeader) == null) {
          request.headers.set(HttpHeaders.acceptHeader, '*/*');
        }
        if (multipartType != null) {
          request.headers.set(
            'Content-Type',
            multipartType,
            preserveHeaderCase: true,
          );
        }
        if (data != null) {
          request.contentLength = data.length;
          request.add(data);
        } else if (method == 'POST' || method == 'PUT' || method == 'PATCH') {
          request.contentLength = 0;
        }
      } catch (_) {
        request.abort();
        return const _Outcome(_Kind.notSent, detail: 'INVALID_REQUEST');
      }
      final HttpClientResponse response;
      try {
        response = await request.close().timeout(
              Duration(milliseconds: left()),
            );
      } on TimeoutException {
        request.abort();
        return const _Outcome(_Kind.timeout);
      } catch (e) {
        return _Outcome(_Kind.lost, detail: _detail(e, true));
      }
      final status = response.statusCode;
      final responseHeaders = _headersOf(response.headers);
      if (stream &&
          status >= 200 &&
          status <= 299 &&
          _eventStream(responseHeaders)) {
        handedOff = true;
        return _Outcome(
          _Kind.streaming,
          status: status,
          headers: responseHeaders,
          stream: _EventBody(client, response, idleMs ?? timeoutMs),
        );
      }
      if (maxResponseBytes != null &&
          response.contentLength > maxResponseBytes) {
        return _Outcome(
          _Kind.response,
          status: status,
          headers: responseHeaders,
          bodyFailure: 'oversized',
        );
      }
      final (bytes, failure) = await _readBody(
        response,
        left(),
        maxResponseBytes,
      );
      return _Outcome(
        _Kind.response,
        status: status,
        headers: responseHeaders,
        body: failure == null ? bytes : null,
        bodyFailure: failure,
      );
    } finally {
      if (!handedOff) client.close(force: true);
    }
  }

  static Future<(List<int>, String?)> _readBody(
    Stream<List<int>> response,
    int leftMs,
    int? maximum,
  ) {
    final done = Completer<(List<int>, String?)>();
    final out = BytesBuilder(copy: false);
    late final StreamSubscription<List<int>> sub;
    final timer = Timer(Duration(milliseconds: leftMs), () {
      if (done.isCompleted) return;
      unawaited(sub.cancel());
      done.complete((const <int>[], 'timeout'));
    });
    sub = response.listen(
      (chunk) {
        if (done.isCompleted) return;
        if (maximum != null && out.length + chunk.length > maximum) {
          timer.cancel();
          unawaited(sub.cancel());
          done.complete((const <int>[], 'oversized'));
          return;
        }
        out.add(chunk);
      },
      onError: (Object _) {
        if (done.isCompleted) return;
        timer.cancel();
        done.complete((const <int>[], 'broken'));
      },
      onDone: () {
        if (done.isCompleted) return;
        timer.cancel();
        done.complete((out.takeBytes(), null));
      },
      cancelOnError: true,
    );
    return done.future;
  }

  // ------------------------------------------------------ retry-after

  static const List<String> _months = [
    'jan', 'feb', 'mar', 'apr', 'may', 'jun', //
    'jul', 'aug', 'sep', 'oct', 'nov', 'dec',
  ];
  static const List<String> _monthsFull = [
    'january', 'february', 'march', 'april', 'may', 'june', //
    'july', 'august', 'september', 'october', 'november', 'december',
  ];

  static int _daysFromCivil(int year, int month, int day) {
    final y = month <= 2 ? year - 1 : year;
    final era = (y >= 0 ? y : y - 399) ~/ 400;
    final yoe = y - era * 400;
    final mp = (month + 9) % 12;
    final doy = (153 * mp + 2) ~/ 5 + day - 1;
    final doe = yoe * 365 + yoe ~/ 4 - yoe ~/ 100 + doy;
    return era * 146097 + doe - 719468;
  }

  static int? _monthNumber(String token) {
    final lower = token.toLowerCase();
    if (lower.length < 3) return null;
    final i = _months.indexOf(lower.substring(0, 3));
    if (i < 0) return null;
    if (lower.length == 3 || lower == _monthsFull[i] || lower == 'sept') {
      return i + 1;
    }
    return null;
  }

  static final RegExp _digitsRe = RegExp(r'^\d+$');

  static bool _digits(String text) => _digitsRe.hasMatch(text);

  static int? _parseClock(String token) {
    final parts = token.split(':');
    if (parts.length < 2 || parts.length > 3) return null;
    if (!_digits(parts[0]) || !_digits(parts[1])) return null;
    final hour = int.parse(parts[0]);
    final minute = int.parse(parts[1]);
    if (hour >= 24 || minute >= 60) return null;
    var second = 0.0;
    if (parts.length == 3) {
      final s = double.tryParse(parts[2]);
      if (s == null ||
          s < 0 ||
          s >= 61 ||
          !RegExp(r'^[0-9.]+$').hasMatch(parts[2])) {
        return null;
      }
      second = s;
    }
    return (hour * 3600 + minute * 60) * 1000 + (second * 1000 + 0.5).floor();
  }

  static int? _parseZone(String token) {
    if (const ['GMT', 'UTC', 'UT', 'Z'].contains(token.toUpperCase())) return 0;
    final m = RegExp(r'^([+-])(\d\d):?(\d\d)$').firstMatch(token);
    if (m == null) return null;
    final sign = m[1] == '-' ? -1 : 1;
    return sign * (int.parse(m[2]!) * 60 + int.parse(m[3]!)) * 60000;
  }

  static int? _parseIsoDate(String text) {
    final split = text.indexOf(RegExp('[T ]'));
    final date = split < 0 ? text : text.substring(0, split);
    final rest = split < 0 ? null : text.substring(split + 1);
    final fields = date.split('-');
    if (fields.length != 3 || !fields.every(_digits)) return null;
    final year = int.parse(fields[0]);
    final month = int.parse(fields[1]);
    final day = int.parse(fields[2]);
    if (month < 1 || month > 12 || day < 1 || day > 31 || year > 9999) {
      return null;
    }
    var clock = 0;
    var zone = 0;
    if (rest != null) {
      var at = rest.indexOf(RegExp('[Zz+-]'));
      if (at < 0) at = rest.length;
      final c = _parseClock(rest.substring(0, at));
      if (c == null) return null;
      clock = c;
      if (at < rest.length) {
        final z = _parseZone(rest.substring(at));
        if (z == null) return null;
        zone = z;
      }
    }
    return _daysFromCivil(year, month, day) * 86400000 + clock - zone;
  }

  /// An HTTP date (IMF-fixdate, RFC 850, asctime) or an ISO 8601 timestamp
  /// as epoch milliseconds, or null.
  static int? parseHttpDate(String raw) {
    final text = raw.trim();
    if (text.length >= 8 &&
        RegExp(r'^\d').hasMatch(text) &&
        text.length > 4 &&
        text[4] == '-') {
      return _parseIsoDate(text);
    }
    int? day;
    int? month;
    int? year;
    var clock = 0;
    var zone = 0;
    for (final token in text.split(RegExp('[ ,]+'))) {
      if (token.isEmpty) continue;
      int? m;
      int? z;
      if (token.contains(':')) {
        final c = _parseClock(token);
        if (c == null) return null;
        clock = c;
      } else if (token.contains('-') && RegExp(r'^\d').hasMatch(token)) {
        final parts = token.split('-');
        if (parts.length < 3 || !_digits(parts[0]) || !_digits(parts[2])) {
          return null;
        }
        month = _monthNumber(parts[1]);
        if (month == null) return null;
        day = int.parse(parts[0]);
        var y = int.parse(parts[2]);
        if (y < 50) {
          y += 2000;
        } else if (y <= 99) {
          y += 1900;
        }
        year = y;
      } else if (_digits(token)) {
        if (day == null && token.length <= 2) {
          day = int.parse(token);
        } else {
          year = int.parse(token);
        }
      } else if ((m = _monthNumber(token)) != null) {
        month = m;
      } else if ((z = _parseZone(token)) != null) {
        zone = z!;
      } else if (!RegExp(r'^[A-Za-z.]+$').hasMatch(token)) {
        return null;
      }
    }
    if (day == null || month == null || year == null) return null;
    if (day < 1 || day > 31 || year > 9999) return null;
    return _daysFromCivil(year, month, day) * 86400000 + clock - zone;
  }

  /// Retry-After in milliseconds (delta seconds or an HTTP date), or null.
  static int? parseRetryAfter(String? value, int nowMs) {
    if (value == null) return null;
    final trimmed = value.trim();
    if (_digits(trimmed)) {
      final seconds = int.tryParse(trimmed);
      if (seconds == null || seconds > 4611686018427387) {
        return 4611686018427387904;
      }
      return math.min(seconds * 1000, 4611686018427387904);
    }
    final date = parseHttpDate(trimmed);
    return date == null ? null : math.max(date - nowMs, 0);
  }
}
