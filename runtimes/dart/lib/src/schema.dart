// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// One validation problem: the path of the offending value (member names
/// and array indexes) and a message.
final class Issue {
  /// A validation problem.
  const Issue(this.path, this.message);

  /// Member names (strings) and array indexes (ints) from the root.
  final List<Object> path;

  /// What is wrong, in words.
  final String message;
}

/// A `pattern` the runtime could not check, so the value was accepted
/// without it (see [Schema.validate]).
final class UncheckedPattern {
  /// A pattern that was not checked.
  const UncheckedPattern(this.path, this.pattern, this.reason);

  /// Member names (strings) and array indexes (ints) from the root.
  final List<Object> path;

  /// The pattern source.
  final String pattern;

  /// Why it was not checked, in words.
  final String reason;
}

/// The validator of the runtime schema form of the SDK descriptor document:
/// arguments before any network call, success bodies, page items and events
/// after.
///
/// Issues come structure first, then constraint violations, then unknown
/// members. Types are strict (no coercion); string lengths count Unicode
/// code points, so an emoji is one character, not two UTF-16 code units.
///
/// `pattern` is an ECMAScript expression, searched anywhere in the string,
/// run by Dart's `RegExp` with Unicode semantics (the `u` flag): `\p{...}`,
/// `\u{...}` and astral-plane characters match as whole code points. A
/// pattern the `u` flag rejects only for its legacy syntax (an identity
/// escape such as `\_`) is run without the flag. A pattern neither mode can
/// compile accepts the value, as in the other runtimes, but never silently:
/// the caller of [validate] is told through its `onUnchecked` observer, and
/// the client reports a `PATTERN_UNCHECKED` diagnostic.
final class Schema {
  /// A validator of [schema] (a map of the runtime schema form) whose `ref`s
  /// resolve in [defs].
  Schema(this.schema, Map<String, Object?>? defs) : defs = defs ?? const {};

  /// The schema.
  final Map<String, Object?> schema;

  /// The named types' schemas.
  final Map<String, Object?> defs;

  static const String _date =
      '(?:(?:[0-9][0-9][2468][048]|[0-9][0-9][13579][26]|[0-9][0-9]0[48]|[02468][048]00'
      '|[13579][26]00)-02-29|[0-9]{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12][0-9]|3[01])'
      '|(?:0[469]|11)-(?:0[1-9]|[12][0-9]|30)|(?:02)-(?:0[1-9]|1[0-9]|2[0-8])))';
  static const String _ipv4Part =
      '(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])';
  static const String _ipv6 =
      '(?:(?:H:){7}H|(?:H:){1,7}:|(?:H:){1,6}:H|(?:H:){1,5}(?::H){1,2}'
      '|(?:H:){1,4}(?::H){1,3}|(?:H:){1,3}(?::H){1,4}|(?:H:){1,2}(?::H){1,5}'
      '|H:(?:(?::H){1,6})|:(?:(?::H){1,7}|:))';

  static final Map<String, RegExp> _formats = {
    'uuid': RegExp(
      r'^(?:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$',
    ),
    'email': RegExp(r'^(?:[^\s@"]{1,64}@[^\s@]{1,255})$'),
    'date-time': RegExp(
      '^(?:$_date'
      r'T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](?:\.[0-9]+)?(?:Z|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9]))$',
    ),
    'date': RegExp('^(?:$_date)\$'),
    'ipv4': RegExp('^(?:(?:$_ipv4Part\\.){3}$_ipv4Part)\$'),
    'ipv6': RegExp('^(?:${_ipv6.replaceAll('H', '[0-9a-fA-F]{1,4}')})\$'),
  };

  static final Map<String, RegExp?> _patterns = {};

  /// The compiled pattern of an ECMAScript source, with Unicode semantics
  /// when it compiles with them, else as legacy syntax; null when Dart cannot
  /// compile it either way.
  static RegExp? pattern(String source) => _patterns.putIfAbsent(source, () {
        try {
          return RegExp(source, unicode: true);
        } on FormatException {
          // Fall through to the legacy syntax.
        }
        try {
          return RegExp(source);
        } on FormatException {
          return null;
        }
      });

  /// The length of a string in Unicode code points: a surrogate pair counts
  /// once.
  static int codePointCount(String text) {
    var n = text.length;
    for (var i = 0; i + 1 < text.length; i++) {
      final unit = text.codeUnitAt(i);
      if (unit >= 0xD800 && unit <= 0xDBFF) {
        final next = text.codeUnitAt(i + 1);
        if (next >= 0xDC00 && next <= 0xDFFF) {
          n--;
          i++;
        }
      }
    }
    return n;
  }

  /// The issues of a value, none when it is valid.
  ///
  /// A `pattern` that cannot be compiled adds no issue. When [onUnchecked] is
  /// given it receives each such pattern with its path (the value is never
  /// passed); without it the pattern is skipped silently.
  List<Issue> validate(
    Object? value, {
    void Function(UncheckedPattern note)? onUnchecked,
  }) =>
      _Run(defs, onUnchecked).run(schema, value);

  /// Whether the value is valid.
  bool accepts(Object? value) => validate(value).isEmpty;

  /// The path of an issue as `.key` and `[index]` segments.
  static String issuePath(List<Object> path) => [
        for (final seg in path)
          seg is int || _Util.isIndex(seg) ? '[$seg]' : '.$seg',
      ].join();

  /// A value in words, for messages.
  static String describe(Object? value) {
    if (value == null) return 'null';
    if (value is bool) return 'a boolean';
    if (value is num) return 'a number';
    if (value is String) return 'a string';
    if (value is List) return 'an array';
    if (value is Map) return 'an object';
    if (value is Binary || value is Uint8List) return 'binary data';
    return 'a value';
  }
}

final class _Run {
  _Run(this.defs, [this.onUnchecked]);

  final Map<String, Object?> defs;
  final void Function(UncheckedPattern note)? onUnchecked;
  final List<Issue> _structure = [];
  final List<Issue> _constraints = [];
  final List<Issue> _unknown = [];

  List<Issue> run(Map<String, Object?>? schema, Object? value) {
    _check(schema, value, const [], 0);
    return [..._structure, ..._constraints, ..._unknown];
  }

  bool _admits(Object? schema, Object? value) =>
      _Run(defs).run(_asSchema(schema), value).isEmpty;

  static Map<String, Object?>? _asSchema(Object? s) =>
      s is Map<String, Object?> ? s : null;

  void _bad(List<Object> path, String message) =>
      _structure.add(Issue(path, message));

  void _limit(List<Object> path, String message) =>
      _constraints.add(Issue(path, message));

  List<Object> _at(List<Object> path, Object seg) => [...path, seg];

  void _check(
    Map<String, Object?>? schema,
    Object? value,
    List<Object> path,
    int depth,
  ) {
    if (schema == null) return;
    if (depth > 512) return _bad(path, 'the value is nested too deeply');
    final describe = Schema.describe;
    switch (schema['kind']) {
      case null || 'any':
        return;
      case 'never':
        return _bad(path, 'no value is allowed here');
      case 'nullable':
        if (value != null) {
          _check(_asSchema(schema['inner']), value, path, depth + 1);
        }
      case 'ref':
        final target = _asSchema(defs[schema['ref']]);
        if (target != null) _check(target, value, path, depth + 1);
      case 'string':
        if (value is! String) {
          return _bad(path, 'expected a string, found ${describe(value)}');
        }
        _stringLimits(schema, value, path);
      case 'bytes':
        if (value is! String && Binary.tryFrom(value) == null) {
          _bad(path, 'expected binary data, found ${describe(value)}');
        }
      case 'boolean':
        if (value is! bool) {
          _bad(path, 'expected a boolean, found ${describe(value)}');
        }
      case 'integer':
        _integer(schema, value, path);
      case 'number':
        if (value is! num) {
          return _bad(path, 'expected a number, found ${describe(value)}');
        }
        _numberLimits(schema, value, path);
      case 'enum':
        final values = schema['values'];
        final list = values is List ? values : const <Object?>[];
        if (!list.any((v) => Json.same(v, value))) {
          _bad(path, 'must be one of ${Json.text(list)}');
        }
      case 'const':
        final values = schema['values'];
        final list = values is List ? values : const <Object?>[];
        if (list.isNotEmpty && !Json.same(list[0], value)) {
          _bad(path, 'must be ${Json.text(list[0])}');
        }
      case 'array':
        _array(schema, value, path, depth);
      case 'map':
        if (value is! Map) {
          return _bad(path, 'expected an object, found ${describe(value)}');
        }
        final items = _asSchema(schema['items']);
        for (final e in value.entries) {
          _check(items, e.value, _at(path, '${e.key}'), depth + 1);
        }
      case 'object':
        if (value is! Map) {
          return _bad(path, 'expected an object, found ${describe(value)}');
        }
        _object(schema, value, path, depth);
      case 'union':
        _union(schema, value, path, depth);
      case 'all':
        final variants = schema['variants'];
        if (variants is List) {
          for (final m in variants) {
            _check(_asSchema(m), value, path, depth + 1);
          }
        }
      case 'limits':
        if (value is String) {
          _stringLimits(schema, value, path);
        } else if (value is num) {
          _numberLimits(schema, value, path);
        }
      default:
        _bad(path, 'unknown schema kind ${schema['kind']}');
    }
  }

  void _integer(Map<String, Object?> schema, Object? value, List<Object> path) {
    if (value is! int) {
      if (value is double) {
        return _bad(
          path,
          'expected an integer, found ${Json.numberText(value)}',
        );
      }
      return _bad(
        path,
        'expected an integer, found ${Schema.describe(value)}',
      );
    }
    if (schema['bits'] == 32 && (value < -0x80000000 || value > 0x7fffffff)) {
      return _bad(path, 'expected a 32-bit integer');
    }
    _numberLimits(schema, value, path);
  }

  void _array(
    Map<String, Object?> schema,
    Object? value,
    List<Object> path,
    int depth,
  ) {
    if (value is! List) {
      return _bad(path, 'expected an array, found ${Schema.describe(value)}');
    }
    final n = value.length;
    final min = schema['min_items'];
    final max = schema['max_items'];
    if (min is num && n < min) _limit(path, 'must have at least $min items');
    if (max is num && n > max) {
      // Reject an oversized array before traversing or comparing its items.
      return _limit(path, 'must have at most $max items');
    }
    final items = _asSchema(schema['items']);
    for (var i = 0; i < n; i++) {
      _check(items, value[i], _at(path, i), depth + 1);
    }
    if (schema['unique'] != true) return;
    final seen = <int, List<Object?>>{};
    var repeated = false;
    for (final item in value) {
      final bucket = seen.putIfAbsent(_uniquenessHash(item), () => []);
      if (bucket.any((other) => Json.same(item, other))) {
        repeated = true;
        break;
      }
      bucket.add(item);
    }
    if (repeated) _limit(path, 'items must be unique');
  }

  // Structurally equal JSON values hash alike; same() confirms a match.
  static int _uniquenessHash(Object? value) {
    if (value is num) return Object.hash('n', value.toDouble());
    if (value is Binary) return Object.hash('b', value.length);
    if (value is List)
      return Object.hashAll(['a', ...value.map(_uniquenessHash)]);
    if (value is Map) {
      var h = 0;
      for (final e in value.entries) {
        h ^= Object.hash(e.key, _uniquenessHash(e.value));
      }
      return Object.hash('o', h);
    }
    return Object.hash(Json.typeOf(value), value);
  }

  void _object(
    Map<String, Object?> schema,
    Map<Object?, Object?> value,
    List<Object> path,
    int depth,
  ) {
    final fields = schema['fields'];
    final known = <String>{};
    if (fields is List) {
      for (final f in fields) {
        if (f is! Map) continue;
        final name = '${f['name']}';
        known.add(name);
        final at = _at(path, name);
        if (!value.containsKey(name)) {
          if (f['required'] == true) _bad(at, 'missing required member $name');
        } else if (value[name] == null && f['nullable'] == true) {
          // null is a value of this member
        } else {
          _check(_asSchema(f['schema']), value[name], at, depth + 1);
        }
      }
    }
    final extra = _asSchema(schema['extra']);
    for (final e in value.entries) {
      final k = '${e.key}';
      if (known.contains(k)) continue;
      if (extra != null) {
        _check(extra, e.value, _at(path, k), depth + 1);
      } else if (schema['additional'] == 'closed') {
        _unknown.add(Issue(_at(path, k), 'unknown member $k'));
      }
    }
  }

  void _union(
    Map<String, Object?> schema,
    Object? value,
    List<Object> path,
    int depth,
  ) {
    final raw = schema['variants'];
    final variants = raw is List ? raw : const <Object?>[];
    final tag = schema['tag'];
    if (tag is String) {
      if (value is! Map) {
        return _bad(
            path, 'expected an object, found ${Schema.describe(value)}');
      }
      final text = value[tag];
      if (text is! String) {
        return _bad(_at(path, tag), 'missing the discriminator $tag');
      }
      final tagsRaw = schema['tags'];
      final tags = tagsRaw is List ? tagsRaw : const <Object?>[];
      final index = tags.indexOf(text);
      if (index >= 0 && index < variants.length) {
        return _check(_asSchema(variants[index]), value, path, depth + 1);
      }
      return _bad(_at(path, tag), 'must be one of ${Json.text(tags)}');
    }
    if (variants.any((v) => _admits(v, value))) return;
    _bad(path, 'does not match any variant');
  }

  static String _boundText(Object? value) {
    if (value is double &&
        value == value.truncateToDouble() &&
        value.abs() < Json.maxSafe) {
      return '${value.toInt()}';
    }
    return value is num ? Json.numberText(value) : '$value';
  }

  void _stringLimits(
      Map<String, Object?> schema, String text, List<Object> path) {
    final n = Schema.codePointCount(text);
    final min = schema['min_length'];
    final max = schema['max_length'];
    if (min is num && n < min) {
      _limit(path, 'must have at least $min characters');
    }
    if (max is num && n > max) {
      _limit(path, 'must have at most $max characters');
    }
    final source = schema['pattern'];
    if (source is String) {
      final re = Schema.pattern(source);
      if (re == null) {
        onUnchecked?.call(
          UncheckedPattern(
            path,
            source,
            'Dart\'s RegExp cannot compile it',
          ),
        );
      } else if (!re.hasMatch(text)) {
        _limit(path, 'must match the pattern /$source/');
      }
    }
    final format = Schema._formats[schema['format']];
    if (format != null && !format.hasMatch(text)) {
      _limit(path, 'must be a valid ${schema['format']}');
    }
  }

  void _numberLimits(
      Map<String, Object?> schema, num number, List<Object> path) {
    final minimum = schema['minimum'];
    final maximum = schema['maximum'];
    final exMin = schema['exclusive_minimum'];
    final exMax = schema['exclusive_maximum'];
    if (minimum is num && number < minimum) {
      _limit(path, 'must be at least ${_boundText(minimum)}');
    }
    if (maximum is num && number > maximum) {
      _limit(path, 'must be at most ${_boundText(maximum)}');
    }
    if (exMin is num && number <= exMin) {
      _limit(path, 'must be greater than ${_boundText(exMin)}');
    }
    if (exMax is num && number >= exMax) {
      _limit(path, 'must be less than ${_boundText(exMax)}');
    }
    final step = schema['multiple_of'];
    if (step is! num || step == 0) return;
    final q = number / step;
    if ((q - q.roundToDouble()).abs() > 1e-9 * math.max(q.abs(), 1)) {
      _limit(path, 'must be a multiple of ${_boundText(step)}');
    }
  }
}
