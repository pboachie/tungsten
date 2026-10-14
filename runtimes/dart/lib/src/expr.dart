// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Predicates and expressions of verification hooks, polls and macros.
///
/// Predicates (verify `expect` and `terminal`, poll `until`): field path to
/// `{"in": [...]}` | `{"equals": x}` | `{"contains": ...}`; any other value
/// is compared for equality. Every entry must hold.
///
/// Expressions: JSON in which a string starting with `$` is a reference
/// resolved against a scope (`$input.a.b`, `$<name>.a`, `$response.x`,
/// `$args.y`), an object `{"expr": "<ref> in [..]" | "<ref> == x" |
/// "<ref> != x"}` is a boolean, anything else a literal. A dry evaluation
/// (macro previews) knows the input but not the earlier results: a
/// reference to one is the placeholder `<from step NAME: path>`.
abstract final class Expr {
  /// Whether every entry of [predicate] holds on [value] ([unset] when
  /// absent). A null predicate always holds; a malformed one never does.
  static bool predicate(Object? predicate, Object? value) {
    if (predicate == null) return true;
    if (predicate is! Map) return false;
    for (final e in predicate.entries) {
      if (!_holds(e.value, _Util.getPathStr(value, '${e.key}'))) return false;
    }
    return true;
  }

  static bool _subset(Object? value, Object? pattern) {
    if (pattern is Map) {
      if (value is! Map) return false;
      for (final e in pattern.entries) {
        if (!value.containsKey(e.key) || !_subset(value[e.key], e.value)) {
          return false;
        }
      }
      return true;
    }
    if (pattern is List) {
      if (value is! List) return false;
      return pattern.every((want) => value.any((have) => _subset(have, want)));
    }
    return !identical(value, unset) && Json.same(value, pattern);
  }

  static bool _holds(Object? test, Object? actual) {
    if (test is Map && test.length == 1) {
      if (test.containsKey('in')) {
        final list = test['in'];
        return list is List && list.any((c) => Json.sameOpt(c, actual));
      }
      if (test.containsKey('equals')) {
        return Json.sameOpt(test['equals'], actual);
      }
      if (test.containsKey('contains')) {
        final pattern = test['contains'];
        if (pattern is String) {
          if (actual is String) return actual.contains(pattern);
          if (actual is List) return actual.any((i) => i == pattern);
          return false;
        }
        if (actual is List && pattern is! List) {
          return actual.any((item) => _subset(item, pattern));
        }
        return _subset(actual, pattern);
      }
    }
    return Json.sameOpt(test, actual);
  }

  /// One `$name.path` reference, or [unset].
  static Object? resolveRef(String reference, Map<String, Object?> scope) {
    final segments = _Util.splitPath(
      reference.startsWith(r'$') ? reference.substring(1) : reference,
    );
    if (segments.isEmpty || !scope.containsKey(segments.first)) return unset;
    return _Util.getPath(scope[segments.first], segments.sublist(1));
  }

  /// An expression's value; unknown references are [unset] (dropped from
  /// objects, null in arrays); a malformed expr string is null.
  static Object? evaluate(
    Object? expr,
    Map<String, Object?> scope, [
    int depth = 0,
  ]) {
    if (depth > 64) return null;
    if (expr is String) {
      return expr.startsWith(r'$') ? resolveRef(expr, scope) : expr;
    }
    if (expr is List) {
      return [
        for (final item in expr) _orNull(evaluate(item, scope, depth + 1)),
      ];
    }
    if (expr is Map) {
      final source = expr['expr'];
      if (expr.length == 1 && source is String) return _boolean(source, scope);
      final out = <String, Object?>{};
      for (final e in expr.entries) {
        final value = evaluate(e.value, scope, depth + 1);
        if (!identical(value, unset)) out['${e.key}'] = value;
      }
      return out;
    }
    return expr;
  }

  static Object? _orNull(Object? v) => identical(v, unset) ? null : v;

  /// The placeholder of a pending step result.
  static String placeholder(String name, String path) =>
      path.isEmpty ? '<from step $name>' : '<from step $name: $path>';

  static String _dropDot(String s) => s.startsWith('.') ? s.substring(1) : s;

  /// (name, path) of a reference to a pending binding, else null.
  static (String, String)? pendingRef(Object? reference, Set<String> pending) {
    if (reference is! String || !reference.startsWith(r'$')) return null;
    final segs = _Util.splitPath(reference.substring(1));
    if (segs.isEmpty) return null;
    final name = segs.first;
    if (!pending.contains(name) || !reference.startsWith('\$$name')) {
      return null;
    }
    return (name, _dropDot(reference.substring(name.length + 1)));
  }

  /// A dry evaluation: references to the [pending] bindings become
  /// placeholders.
  static Object? evaluateDry(
    Object? expr,
    Map<String, Object?> scope,
    Set<String> pending, [
    int depth = 0,
  ]) {
    if (depth > 64) return null;
    if (expr is String) {
      if (!expr.startsWith(r'$')) return expr;
      final ref = pendingRef(expr, pending);
      return ref != null
          ? placeholder(ref.$1, ref.$2)
          : resolveRef(expr, scope);
    }
    if (expr is List) {
      return [
        for (final item in expr)
          _orNull(evaluateDry(item, scope, pending, depth + 1)),
      ];
    }
    if (expr is Map) {
      final raw = expr['expr'];
      if (expr.length == 1 && raw is String) {
        final source = raw.trim();
        final head = RegExp(r'^\$[^\s=!]+').firstMatch(source)?[0];
        final ref = head == null ? null : pendingRef(head, pending);
        if (ref != null) {
          return placeholder(
            ref.$1,
            _dropDot(source.substring(ref.$1.length + 1)),
          );
        }
        return _boolean(source, scope);
      }
      final out = <String, Object?>{};
      for (final e in expr.entries) {
        final value = evaluateDry(e.value, scope, pending, depth + 1);
        if (!identical(value, unset)) out['${e.key}'] = value;
      }
      return out;
    }
    return expr;
  }

  /// A predicate in words: `state in ["a", "b"]`, `endpoints contains
  /// {...}`, `a = 1`, joined with "and".
  static String describe(Object? predicate) {
    if (predicate is! Map) return '';
    return [
      for (final e in predicate.entries) _describeOne('${e.key}', e.value),
    ].join(' and ');
  }

  static String _describeOne(String path, Object? test) {
    if (test is Map && test.length == 1) {
      final list = test['in'];
      if (list is List) {
        return '$path in [${[for (final v in list) Json.text(v)].join(', ')}]';
      }
      if (test.containsKey('equals')) {
        return '$path = ${Json.text(test['equals'])}';
      }
      if (test.containsKey('contains')) {
        return '$path contains ${Json.text(test['contains'])}';
      }
    }
    return '$path = ${Json.text(test)}';
  }

  static Object? _literal(String text) {
    final trimmed = text.trim();
    final value = Json.tryParse(trimmed);
    if (!identical(value, unset)) return value;
    final normalized = trimmed.replaceAllMapped(
      RegExp(r"'((?:[^'\\]|\\.)*)'"),
      (m) => Json.quote(m[1]!.replaceAll(r"\'", "'")),
    );
    return Json.tryParse(normalized);
  }

  static final RegExp _booleanRe =
      RegExp(r'^\s*(\$[^\s=!]+)\s+(in|==|!=)\s+(.+)$', dotAll: true);

  /// `<ref> in [..]`, `<ref> == x` or `<ref> != x`; null when malformed.
  static bool? _boolean(String source, Map<String, Object?> scope) {
    final m = _booleanRe.firstMatch(source);
    if (m == null) return null;
    final lit = _literal(m[3]!);
    if (identical(lit, unset)) return null;
    var actual = resolveRef(m[1]!, scope);
    if (m[2] == 'in') {
      if (lit is! List) return null;
      return lit.any((v) => Json.sameOpt(v, actual));
    }
    if (identical(actual, unset)) actual = null;
    final same = Json.same(actual, lit);
    return m[2] == '==' ? same : !same;
  }
}
