// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// A value that cannot be encoded becomes VALIDATION_FAILED.
final class _SerializationError {
  const _SerializationError(this.parameter, this.expected, this.value);

  final String parameter;
  final String expected;
  final Object? value;
}

/// A body ready for the wire: [bytes] or [parts] (multipart), the content
/// type, the redacted [display] and the bytes the content hash is computed
/// over ([hashed], null without a body).
final class _Encoded {
  const _Encoded({
    required this.empty,
    this.bytes,
    this.parts,
    this.contentType,
    this.display,
    this.hashed,
  });

  static const _Encoded none = _Encoded(empty: true);

  final bool empty;
  final List<int>? bytes;
  final List<_Part>? parts;
  final String? contentType;
  final Object? display;
  final List<int>? hashed;
}

enum _PartKind { text, json, file }

final class _Part {
  const _Part(this.name, this.kind,
      {this.text, this.data, this.filename, this.contentType});

  final String name;
  final _PartKind kind;
  final String? text;
  final List<int>? data;
  final String? filename;
  final String? contentType;
}

abstract final class _Serialize {
  /// String(value) for a scalar, JSON text for anything else.
  static String scalar(Object? value) {
    if (value is String) return value;
    if (value is num) return Json.numberText(value);
    if (value is bool) return '$value';
    return Json.text(value);
  }

  static final RegExp _alnum = RegExp('[A-Za-z0-9]');

  static String _escape(String text, String keep) {
    final out = StringBuffer();
    for (final b in utf8.encode(text)) {
      final c = String.fromCharCode(b);
      if (b < 0x80 && (_alnum.hasMatch(c) || keep.contains(c))) {
        out.write(c);
      } else {
        out.write('%${b.toRadixString(16).toUpperCase().padLeft(2, '0')}');
      }
    }
    return out.toString();
  }

  /// encodeURIComponent.
  static String encodeComponent(String text) => _escape(text, "-_.!~*'()");

  /// One component of a form body (URLSearchParams): spaces as `+`.
  static String encodeFormComponent(String text) {
    final out = StringBuffer();
    for (final b in utf8.encode(text)) {
      final c = String.fromCharCode(b);
      if (b < 0x80 && (_alnum.hasMatch(c) || '*-._'.contains(c))) {
        out.write(c);
      } else if (c == ' ') {
        out.write('+');
      } else {
        out.write('%${b.toRadixString(16).toUpperCase().padLeft(2, '0')}');
      }
    }
    return out.toString();
  }

  /// RFC 3986 percent-encoding.
  static String percentEncode(String text) => _escape(text, '-._~');

  static List<String> _encodedItems(List<Object?> items) => [
        for (final v in items)
          if (v != null) encodeComponent(scalar(v)),
      ];

  static List<(String, String)> _encodedPairs(Map<Object?, Object?> object) => [
        for (final e in object.entries)
          if (e.value != null)
            (encodeComponent('${e.key}'), encodeComponent(scalar(e.value))),
      ];

  static List<String> _flat(List<(String, String)> pairs) => [
        for (final (k, v) in pairs) ...[k, v]
      ];

  /// One path parameter value, percent-encoded.
  static String pathParam(ParamDescriptor p, Object? value) {
    final name = encodeComponent(p.wire);
    final style =
        p.style == 'label' || p.style == 'matrix' ? p.style : 'simple';
    if (value is List) {
      final items = _encodedItems(value);
      if (style == 'label') return '.${items.join(p.explode ? '.' : ',')}';
      if (style == 'matrix' && p.explode) {
        return items.map((i) => ';$name=$i').join();
      }
      if (style == 'matrix') return ';$name=${items.join(',')}';
      return items.join(',');
    }
    if (value is Map) {
      final pairs = _encodedPairs(value);
      final exploded = [for (final (k, v) in pairs) '$k=$v'];
      final flat = _flat(pairs);
      if (style == 'label' && p.explode) return '.${exploded.join('.')}';
      if (style == 'label') return '.${flat.join(',')}';
      if (style == 'matrix' && p.explode) {
        return exploded.map((e) => ';$e').join();
      }
      if (style == 'matrix') return ';$name=${flat.join(',')}';
      return p.explode ? exploded.join(',') : flat.join(',');
    }
    final val = encodeComponent(scalar(value));
    return switch (style) {
      'label' => '.$val',
      'matrix' => ';$name=$val',
      _ => val,
    };
  }

  static void _deepObjectPairs(
    String prefix,
    Object? value,
    List<String> out,
    int depth,
  ) {
    if (depth > 16 || value == null) return;
    if (value is List) {
      for (final item in value) {
        _deepObjectPairs(prefix, item, out, depth + 1);
      }
    } else if (value is Map) {
      for (final e in value.entries) {
        if (e.value != null) {
          _deepObjectPairs(
            '$prefix[${encodeComponent('${e.key}')}]',
            e.value,
            out,
            depth + 1,
          );
        }
      }
    } else {
      out.add('$prefix=${encodeComponent(scalar(value))}');
    }
  }

  /// The query parts (name=value, encoded) of one parameter.
  static List<String> queryParam(ParamDescriptor p, Object? value) {
    if (value == null) return const [];
    final name = encodeComponent(p.wire);
    final delimiter = switch (p.style) {
      'space_delimited' => '%20',
      'pipe_delimited' => '|',
      _ => ',',
    };
    if (value is Map && p.style == 'deep_object') {
      final out = <String>[];
      _deepObjectPairs(name, value, out, 0);
      return out;
    }
    if (value is List) {
      final items = _encodedItems(value);
      if (items.isEmpty) return const [];
      if (p.explode || p.style == 'deep_object') {
        return [for (final i in items) '$name=$i'];
      }
      return ['$name=${items.join(delimiter)}'];
    }
    if (value is Map) {
      final pairs = _encodedPairs(value);
      if (p.explode) return [for (final (k, v) in pairs) '$k=$v'];
      return ['$name=${_flat(pairs).join(delimiter)}'];
    }
    return ['$name=${encodeComponent(scalar(value))}'];
  }

  /// A header parameter (simple style, not encoded).
  static String headerParam(ParamDescriptor p, Object? value) {
    if (value is List) {
      return [
        for (final v in value)
          if (v != null) scalar(v)
      ].join(',');
    }
    if (value is Map) {
      return [
        for (final e in value.entries)
          if (e.value != null)
            ...(p.explode
                ? ['${e.key}=${scalar(e.value)}']
                : ['${e.key}', scalar(e.value)]),
      ].join(',');
    }
    return scalar(value);
  }

  /// name=value of a cookie parameter.
  static String cookieParam(ParamDescriptor p, Object? value) {
    if (value is List) return '${p.wire}=${_encodedItems(value).join(',')}';
    if (value is Map)
      return '${p.wire}=${_flat(_encodedPairs(value)).join(',')}';
    return '${p.wire}=${encodeComponent(scalar(value))}';
  }

  /// The body before encoding: merged fields picked from the arguments
  /// (under their wire names) or the body argument; wrapped in the rpc
  /// envelope when the operation has one. [unset] without a body.
  static Object? bodyValue(
    OperationDescriptor op,
    Map<String, Object?> args,
    List<String> paramNames,
  ) {
    Object? value = unset;
    final b = op.body;
    if (b != null) {
      if (b.arg != null) {
        value = args.containsKey(b.arg) ? args[b.arg] : unset;
      } else {
        final picked = <String, Object?>{};
        for (final (name, wire) in b.fields) {
          if (args.containsKey(name)) picked[wire] = args[name];
        }
        if (picked.isNotEmpty || b.required || op.rpc != null) value = picked;
      }
    }
    final rpc = op.rpc;
    if (rpc == null) return value;
    final Object? params;
    if (op.body == null) {
      params = <String, Object?>{
        for (final e in args.entries)
          if (!paramNames.contains(e.key)) e.key: e.value,
      };
    } else if (!identical(value, unset)) {
      params = value;
    } else {
      params = <String, Object?>{};
    }
    final envelope = <String, Object?>{};
    _map(rpc['constants']).forEach((k, v) => envelope[k] = v);
    envelope[_str(rpc['field'])] = rpc['value'];
    envelope[_str(rpc['params_field'])] = params;
    return envelope;
  }

  static String binaryDisplay(int n, String? media) =>
      media == null || media.isEmpty ? '<$n bytes>' : '<$n bytes of $media>';

  static Object _formPairs(Object? value) {
    if (value is! Map) {
      return _SerializationError('body', 'an object of form fields', value);
    }
    final out = <(String, String)>[];
    for (final e in value.entries) {
      final item = e.value;
      if (item == null) continue;
      if (item is List) {
        for (final x in item) {
          if (x != null) out.add(('${e.key}', scalar(x)));
        }
      } else {
        out.add(('${e.key}', scalar(item)));
      }
    }
    return out;
  }

  /// Encodes a body per its descriptor; a [_SerializationError] when the
  /// value does not fit. [redact] renders the shown body.
  static Object encodeBody(
    BodyDescriptor? desc,
    Object? value,
    Object? Function(Object? display) redact,
  ) {
    if (identical(value, unset)) return _Encoded.none;
    final encoding = desc?.encoding ?? 'json';
    final media = desc?.mediaType ?? 'application/json';
    switch (encoding) {
      case 'form':
        final pairs = _formPairs(value);
        if (pairs is _SerializationError) return pairs;
        final list = pairs as List<(String, String)>;
        final text = [
          for (final (k, v) in list)
            '${encodeFormComponent(k)}=${encodeFormComponent(v)}',
        ].join('&');
        final shown = <String, Object?>{for (final (k, v) in list) k: v};
        final bytes = utf8.encode(text);
        return _Encoded(
          empty: false,
          bytes: bytes,
          contentType: media,
          display: redact(shown),
          hashed: bytes,
        );
      case 'multipart':
        return _multipart(value, redact);
      case 'bytes':
        final List<int> data;
        final bin = Binary.tryFrom(value);
        if (value is String) {
          data = utf8.encode(value);
        } else if (bin != null) {
          data = bin.data;
        } else {
          return _SerializationError(
            'body',
            'binary data (a Binary value)',
            value,
          );
        }
        return _Encoded(
          empty: false,
          bytes: data,
          contentType: media,
          display: binaryDisplay(data.length, media),
          hashed: data,
        );
      case 'text':
        if (value is! String && value is! num && value is! bool) {
          return _SerializationError('body', 'a string', value);
        }
        final text = scalar(value);
        final bytes = utf8.encode(text);
        return _Encoded(
          empty: false,
          bytes: bytes,
          contentType: media,
          display: text,
          hashed: bytes,
        );
      default:
        final String text;
        try {
          text = Json.text(value);
        } on ArgumentError {
          return _SerializationError(
            'body',
            'a JSON-serializable value',
            value,
          );
        }
        final raw = Json.tryParse(text);
        if (identical(raw, unset)) {
          return _SerializationError(
            'body',
            'a JSON-serializable value',
            value,
          );
        }
        return _Encoded(
          empty: false,
          bytes: utf8.encode(text),
          contentType: media,
          display: redact(raw),
          hashed: utf8.encode(Json.canonical(raw)),
        );
    }
  }

  static Object _multipart(
    Object? value,
    Object? Function(Object? display) redact,
  ) {
    if (value is! Map) {
      return _SerializationError(
          'body', 'an object of multipart fields', value);
    }
    final parts = <_Part>[];
    final display = <String, Object?>{};
    for (final e in value.entries) {
      final k = '${e.key}';
      final item = e.value;
      if (item == null) continue;
      final list = item is List
          ? [
              for (final x in item)
                if (x != null) x
            ]
          : [item];
      final shown = <Object?>[];
      for (final x in list) {
        final bin = Binary.tryFrom(x);
        if (bin != null) {
          final contentType = bin.contentType ?? 'application/octet-stream';
          if (!validHeaderValue(contentType)) {
            return _SerializationError(
              'body',
              'multipart file content types without header control characters',
              value,
            );
          }
          shown.add(binaryDisplay(bin.length, bin.contentType));
          parts.add(_Part(
            k,
            _PartKind.file,
            data: bin.data,
            filename: bin.filename ?? k,
            contentType: contentType,
          ));
        } else if (x is Map) {
          parts.add(_Part(k, _PartKind.json, text: Json.text(x)));
          shown.add(x);
        } else {
          parts.add(_Part(k, _PartKind.text, text: scalar(x)));
          shown.add(scalar(x));
        }
      }
      display[k] = item is List ? shown : (shown.isEmpty ? null : shown.first);
    }
    return _Encoded(
      empty: false,
      parts: parts,
      display: redact(display),
      hashed: utf8.encode(Json.canonical(value)),
    );
  }

  /// The bytes of a multipart body and its content type.
  static (List<int>, String) multipartBody(List<_Part> parts) {
    final boundary = [
      for (final b in _Util.randomBytes(16))
        b.toRadixString(16).padLeft(2, '0'),
    ].join();
    final out = BytesBuilder(copy: false);
    String quote(String s) => s
        .replaceAll(r'\', r'\\')
        .replaceAll('"', r'\"')
        .replaceAll('\r', '%0D')
        .replaceAll('\n', '%0A');
    void add(String s) => out.add(utf8.encode(s));
    for (final p in parts) {
      add('--$boundary\r\n');
      switch (p.kind) {
        case _PartKind.file:
          add('Content-Disposition: form-data; name="${quote(p.name)}"; '
              'filename="${quote(p.filename ?? p.name)}"\r\n');
          add('Content-Type: ${p.contentType}\r\n\r\n');
          out.add(p.data ?? const []);
        case _PartKind.json:
          add('Content-Disposition: form-data; name="${quote(p.name)}"\r\n');
          add('Content-Type: application/json\r\n\r\n');
          add(p.text ?? '');
        case _PartKind.text:
          add('Content-Disposition: form-data; name="${quote(p.name)}"\r\n\r\n');
          add(p.text ?? '');
      }
      add('\r\n');
    }
    add('--$boundary--\r\n');
    return (out.takeBytes(), 'multipart/form-data; boundary=$boundary');
  }

  static final RegExp _token = RegExp(r"^[A-Za-z0-9!#$%&'*+\-.^_`|~]+$");

  /// An RFC 9110 token.
  static bool validHeaderName(String name) => _token.hasMatch(name);

  /// Visible ASCII, tab, space and Latin-1 characters.
  static bool validHeaderValue(String value) {
    for (final c in value.codeUnits) {
      final ok =
          c == 0x09 || (c >= 0x20 && c <= 0x7e) || (c >= 0x80 && c <= 0xff);
      if (!ok) return false;
    }
    return true;
  }
}

/// A case-insensitive header collection that remembers which values are
/// secrets, in insertion order.
final class _HeaderBag {
  _HeaderBag();

  final List<_HeaderEntry> entries = [];

  void set(String name, String value, bool secret) {
    final lower = name.toLowerCase();
    for (final e in entries) {
      if (e.name.toLowerCase() == lower) {
        e
          ..name = name
          ..value = value
          ..secret = secret;
        return;
      }
    }
    entries.add(_HeaderEntry(name, value, secret));
  }

  String? get(String name) {
    final lower = name.toLowerCase();
    for (final e in entries) {
      if (e.name.toLowerCase() == lower) return e.value;
    }
    return null;
  }

  Map<String, String> toMap(bool redact) => {
        for (final e in entries)
          e.name: redact && e.secret ? redacted : e.value,
      };

  List<String> get secrets => [
        for (final e in entries)
          if (e.secret) e.value
      ];
}

final class _HeaderEntry {
  _HeaderEntry(this.name, this.value, this.secret);

  String name;
  String value;
  bool secret;
}
