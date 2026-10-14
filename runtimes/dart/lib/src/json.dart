// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// The type of [unset].
final class Unset {
  const Unset._();

  @override
  String toString() => 'unset';
}

/// The marker of an absent value: an optional argument or member that was
/// not given, a path that leads nowhere. It is neither `null` (JSON null)
/// nor any JSON value.
const Unset unset = Unset._();

/// Binary data: the bytes of a bytes or text body, a multipart file part
/// (with its optional file name and media type) or a binary response.
/// [toString] never shows the bytes.
final class Binary {
  /// Binary data with an optional file name and media type (multipart).
  Binary(List<int> data, {this.filename, this.contentType})
      : data = Uint8List.fromList(data);

  /// The bytes.
  final Uint8List data;

  /// The file name of a multipart file part.
  final String? filename;

  /// The media type of a multipart file part.
  final String? contentType;

  /// The number of bytes.
  int get length => data.length;

  @override
  bool operator ==(Object other) =>
      other is Binary &&
      other.filename == filename &&
      other.contentType == contentType &&
      _sameBytes(other.data, data);

  @override
  int get hashCode => Object.hash(data.length, filename, contentType);

  @override
  String toString() => 'Binary(${data.length} bytes)';

  /// The tagged object form (`{"$tungsten_binary": <base64>, "filename"?,
  /// "content_type"?}`) of the JSON text of a value.
  Map<String, Object?> toJson() => {
        Json.binaryKey: base64Encode(data),
        if (filename != null) 'filename': filename,
        if (contentType != null) 'content_type': contentType,
      };

  /// The tagged object form (or a [Binary] or [Uint8List]) as a [Binary];
  /// null when the value is not one.
  static Binary? tryFrom(Object? value) {
    if (value is Binary) return value;
    if (value is Uint8List) return Binary(value);
    if (value is! Map || !value.containsKey(Json.binaryKey)) return null;
    final data = value[Json.binaryKey];
    if (data is! String) return null;
    for (final k in value.keys) {
      if (k != Json.binaryKey && k != 'filename' && k != 'content_type') {
        return null;
      }
    }
    final name = value['filename'];
    final type = value['content_type'];
    if ((name != null && name is! String) || (type != null && type is! String)) {
      return null;
    }
    try {
      return Binary(base64Decode(data),
          filename: name as String?, contentType: type as String?);
    } on FormatException {
      return null;
    }
  }
}

bool _sameBytes(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

/// A text that is not one JSON document.
final class JsonParseException implements Exception {
  /// A text that is not one JSON document.
  const JsonParseException(this.message);

  /// Why it is not.
  final String message;

  @override
  String toString() => 'JsonParseException: $message';
}

/// JSON values as the runtime handles them: `null`, `bool`, `String`, `int`,
/// `double`, `List<Object?>`, `Map<String, Object?>` (in member order) and
/// [Binary]. A number without a fractional part and below 2^53 in magnitude
/// is an `int`, as every tungsten runtime reads it; numbers are finite.
abstract final class Json {
  /// 2^53: integers below it in magnitude are exact in every runtime.
  static const int maxSafe = 9007199254740992;

  /// The deepest nesting parsed or traversed.
  static const int maxDepth = 512;

  /// The member that tags bytes in the JSON text of a [Binary].
  static const String binaryKey = r'$tungsten_binary';

  /// Parses one JSON document (a leading byte order mark dropped). Throws
  /// [JsonParseException].
  static Object? parse(String text) {
    final source = text.startsWith('﻿') ? text.substring(1) : text;
    Object? decoded;
    try {
      decoded = jsonDecode(source);
    } on FormatException catch (e) {
      throw JsonParseException(e.message);
    }
    try {
      return normalize(decoded);
    } on ArgumentError catch (e) {
      throw JsonParseException('${e.message}');
    }
  }

  /// Parses UTF-8 bytes (invalid sequences replaced); [unset] when they are
  /// not JSON.
  static Object? tryParseBytes(List<int> bytes) =>
      tryParse(utf8.decode(bytes, allowMalformed: true));

  /// Parses a text, or [unset] when it is not JSON.
  static Object? tryParse(String text) {
    try {
      return parse(text);
    } on JsonParseException {
      return unset;
    }
  }

  /// A decoded value with the number rules applied.
  static Object? normalize(Object? value, [int depth = 0]) {
    _checkDepth(depth);
    if (value is Map) {
      final out = <String, Object?>{};
      value.forEach((k, v) => out['$k'] = normalize(v, depth + 1));
      return out;
    }
    if (value is List) {
      return [for (final v in value) normalize(v, depth + 1)];
    }
    if (value is double) return number(value);
    return value;
  }

  /// A number: an integral double below 2^53 becomes an `int`.
  static Object number(num value) {
    if (value is int) return value;
    final d = value.toDouble();
    if (d.isNaN || d.isInfinite) return d;
    if (d == d.truncateToDouble() && d.abs() < maxSafe) return d.toInt();
    return d;
  }

  /// A Dart value as a JSON value: maps with string keys, lists, numbers,
  /// strings, booleans, null, [Binary] and `Uint8List` (as [Binary]);
  /// objects with a `toJson()` method (generated models) as their JSON;
  /// [unset] members and items dropped. Throws [ArgumentError] for a value
  /// without a JSON form (a non-finite number, an object).
  static Object? from(Object? value, [int depth = 0]) {
    _checkDepth(depth);
    if (value == null || value is bool || value is String) return value;
    if (value is Binary) return value;
    if (value is Uint8List) return Binary(value);
    if (value is int) return value;
    if (value is double) {
      if (value.isNaN || value.isInfinite) {
        throw ArgumentError('a non-finite number is not JSON');
      }
      return number(value);
    }
    if (value is Map) {
      final out = <String, Object?>{};
      for (final e in value.entries) {
        if (identical(e.value, unset)) continue;
        out['${e.key}'] = from(e.value, depth + 1);
      }
      return out;
    }
    if (value is Iterable) {
      return [
        for (final v in value)
          if (!identical(v, unset)) from(v, depth + 1),
      ];
    }
    if (value is JsonEncodable) return from(value.toJson(), depth + 1);
    throw ArgumentError('${value.runtimeType} has no JSON form');
  }

  /// `JSON.stringify(value)`: members in order, no whitespace, numbers as
  /// JavaScript writes them; a [Binary] is its tagged form.
  static String text(Object? value) {
    final out = StringBuffer();
    _write(out, value, false, 0);
    return out.toString();
  }

  /// The canonical JSON every tungsten runtime digests (confirmation
  /// tokens, automatic idempotency keys, content hashes): members sorted by
  /// the UTF-16 code units of their names, binary values as
  /// `{"$bytes": <base64>}` or `{"$file": {"bytes", "name", "type"}}`.
  static String canonical(Object? value) {
    final out = StringBuffer();
    _write(out, value, true, 0);
    return out.toString();
  }

  static void _write(StringBuffer out, Object? value, bool canonical, int depth) {
    _checkDepth(depth);
    if (value == null) {
      out.write('null');
    } else if (value is bool) {
      out.write(value ? 'true' : 'false');
    } else if (value is num) {
      out.write(numberText(value));
    } else if (value is String) {
      out.write(quote(value));
    } else if (value is List) {
      out.write('[');
      for (var i = 0; i < value.length; i++) {
        if (i > 0) out.write(',');
        _write(out, value[i], canonical, depth + 1);
      }
      out.write(']');
    } else if (value is Map) {
      final keys = [for (final k in value.keys) '$k'];
      if (canonical) keys.sort();
      out.write('{');
      for (var i = 0; i < keys.length; i++) {
        if (i > 0) out.write(',');
        out
          ..write(quote(keys[i]))
          ..write(':');
        _write(out, value[keys[i]], canonical, depth + 1);
      }
      out.write('}');
    } else if (value is Binary) {
      _writeBinary(out, value, canonical);
    } else if (value is Uint8List) {
      _writeBinary(out, Binary(value), canonical);
    } else {
      _write(out, from(value), canonical, depth + 1);
    }
  }

  static void _writeBinary(StringBuffer out, Binary bin, bool canonical) {
    final encoded = base64Encode(bin.data);
    if (!canonical) {
      _write(out, bin.toJson(), false, 0);
      return;
    }
    if (bin.filename == null && bin.contentType == null) {
      out
        ..write(r'{"$bytes":')
        ..write(quote(encoded))
        ..write('}');
      return;
    }
    out
      ..write(r'{"$file":{"bytes":')
      ..write(quote(encoded))
      ..write(',"name":')
      ..write(bin.filename == null ? 'null' : quote(bin.filename!))
      ..write(',"type":')
      ..write(bin.contentType == null ? 'null' : quote(bin.contentType!))
      ..write('}}');
  }

  /// A string literal as `JSON.stringify` writes it (lone surrogates
  /// escaped, as ES2019 does).
  static String quote(String text) {
    final out = StringBuffer('"');
    for (var i = 0; i < text.length; i++) {
      final c = text.codeUnitAt(i);
      switch (c) {
        case 0x22:
          out.write(r'\"');
        case 0x5c:
          out.write(r'\\');
        case 0x08:
          out.write(r'\b');
        case 0x0c:
          out.write(r'\f');
        case 0x0a:
          out.write(r'\n');
        case 0x0d:
          out.write(r'\r');
        case 0x09:
          out.write(r'\t');
        default:
          if (c < 0x20) {
            out.write('\\u${c.toRadixString(16).padLeft(4, '0')}');
          } else if (c >= 0xd800 && c <= 0xdbff) {
            final next = i + 1 < text.length ? text.codeUnitAt(i + 1) : 0;
            if (next >= 0xdc00 && next <= 0xdfff) {
              out
                ..writeCharCode(c)
                ..writeCharCode(next);
              i++;
            } else {
              out.write('\\u${c.toRadixString(16)}');
            }
          } else if (c >= 0xdc00 && c <= 0xdfff) {
            out.write('\\u${c.toRadixString(16)}');
          } else {
            out.writeCharCode(c);
          }
      }
    }
    out.write('"');
    return out.toString();
  }

  /// A number as JavaScript writes it (`String(n)`): 1.0 is "1", 1e-7 is
  /// "1e-7", 1e21 is "1e+21".
  static String numberText(num value) {
    if (value is int) return '$value';
    final d = value.toDouble();
    if (d.isNaN || d.isInfinite) return 'null';
    if (d == 0) return '0';
    final s = d.toString();
    return s.endsWith('.0') ? s.substring(0, s.length - 2) : s;
  }

  /// Structural equality: 1 equals 1.0, member order does not matter.
  static bool same(Object? a, Object? b) {
    if (a is num) return b is num && a == b;
    if (a is List) {
      if (b is! List || a.length != b.length) return false;
      for (var i = 0; i < a.length; i++) {
        if (!same(a[i], b[i])) return false;
      }
      return true;
    }
    if (a is Map) {
      if (b is! Map || a.length != b.length) return false;
      for (final e in a.entries) {
        if (!b.containsKey(e.key) || !same(e.value, b[e.key])) return false;
      }
      return true;
    }
    if (a is Binary) return b is Binary && _sameBytes(a.data, b.data);
    if (a is bool) return b is bool && a == b;
    if (a is String) return b is String && a == b;
    return a == null ? b == null : identical(a, b) || a == b;
  }

  /// [same] where either side may be [unset].
  static bool sameOpt(Object? a, Object? b) {
    if (identical(a, unset) || identical(b, unset)) {
      return identical(a, unset) && identical(b, unset);
    }
    return same(a, b);
  }

  /// A deep copy of a JSON value.
  static Object? copy(Object? value, [int depth = 0]) {
    _checkDepth(depth);
    if (value is Map) {
      return <String, Object?>{
        for (final e in value.entries) '${e.key}': copy(e.value, depth + 1),
      };
    }
    if (value is List) return [for (final v in value) copy(v, depth + 1)];
    return value;
  }

  static void _checkDepth(int depth) {
    if (depth > maxDepth) {
      throw ArgumentError('the value is nested too deeply');
    }
  }

  /// The JSON type of a value: null, boolean, integer, number, string,
  /// array, object or binary.
  static String typeOf(Object? value) {
    if (value == null) return 'null';
    if (value is bool) return 'boolean';
    if (value is int) return 'integer';
    if (value is double) return 'number';
    if (value is String) return 'string';
    if (value is List) return 'array';
    if (value is Map) return 'object';
    if (value is Binary || value is Uint8List) return 'binary';
    return 'value';
  }

  /// The first [max] UTF-16 code units of a text, never splitting a
  /// surrogate pair.
  static String takeUnits(String text, int max) {
    if (text.length <= max) return text;
    var end = max;
    if (end > 0) {
      final last = text.codeUnitAt(end - 1);
      final next = text.codeUnitAt(end);
      if (last >= 0xd800 && last <= 0xdbff && next >= 0xdc00 && next <= 0xdfff) {
        end--;
      }
    }
    return text.substring(0, end);
  }
}

/// A value with a JSON form of its own (generated models).
abstract interface class JsonEncodable {
  /// The value as JSON.
  Object? toJson();
}
