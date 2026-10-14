// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Argument paths, sensitivity and the texts built from arguments.
abstract final class _Helpers {
  /// A structural problem that makes a descriptor unusable, or null.
  static String? descriptorProblem(OperationDescriptor op) {
    if (op.id.isEmpty) return '`id` must be a non-empty string';
    if (op.params.any((p) => p.name.isEmpty || p.wire.isEmpty)) {
      return 'every parameter needs a `name` and a `wire`';
    }
    final rpc = op.rpc;
    if (rpc != null &&
        (_str(rpc['field']).isEmpty || _str(rpc['params_field']).isEmpty)) {
      return '`rpc` needs `field` and `params_field`';
    }
    return null;
  }

  /// The path of an argument issue: body.<wire> for a merged body field,
  /// body for the whole-body argument, else args.<name>.
  static String argumentPath(OperationDescriptor op, List<Object> path) {
    final body = op.body;
    if (body != null && path.isNotEmpty) {
      final head = '${path.first}';
      final rest = path.sublist(1);
      if (body.arg != null) {
        if (body.arg == head) return 'body${Schema.issuePath(rest)}';
      } else {
        for (final (name, wire) in body.fields) {
          if (name == head) return 'body.$wire${Schema.issuePath(rest)}';
        }
      }
    }
    if (path.isEmpty) return 'args';
    return 'args${Schema.issuePath(path)}';
  }

  static List<String> sensitiveRequestPaths(OperationDescriptor op) => [
        for (final f in _list(op.agent['sensitive_request_fields']))
          if ('$f'.isNotEmpty) '$f',
      ];

  /// An argument path without array indexes.
  static String dotted(List<Object> path) => [
        for (final s in path)
          if (s is! int && !RegExp(r'^\d*$').hasMatch('$s')) '$s',
      ].join('.');

  static bool sensitiveArg(OperationDescriptor op, List<Object> path) {
    final d = dotted(path);
    if (d.isNotEmpty &&
        sensitiveRequestPaths(op).any((f) => d == f || d.startsWith('$f.'))) {
      return true;
    }
    return path.any((s) => s is String && _Util.looksSensitive(s));
  }

  /// The sensitive fields below the argument at [path], redacted.
  static Object? redactBelow(
    OperationDescriptor op,
    List<Object> path,
    Object? value,
  ) {
    final d = dotted(path);
    final prefix = d.isEmpty ? '' : '$d.';
    final below = [
      for (final f in sensitiveRequestPaths(op))
        if (f.startsWith(prefix)) f.substring(prefix.length),
    ];
    return below.isEmpty ? value : _Util.redactPaths(value, below);
  }

  /// The sensitive argument paths relative to the body as sent; "" is the
  /// whole body.
  static List<String> sensitiveBodyPaths(OperationDescriptor op) {
    final paths = sensitiveRequestPaths(op);
    final relative = <String>[];
    final body = op.body;
    if (body != null && body.arg == null) {
      for (final p in paths) {
        final segs = _Util.splitPath(p);
        if (segs.isEmpty) continue;
        for (final (name, wire) in body.fields) {
          if (name != segs.first) continue;
          final rest = segs.sublist(1).join('.');
          relative.add(rest.isEmpty ? wire : '$wire.$rest');
          break;
        }
      }
    } else if (body != null) {
      final arg = body.arg!;
      for (final p in paths) {
        if (p == arg) {
          relative.add('');
        } else if (p.startsWith('$arg.')) {
          relative.add(p.substring(arg.length + 1));
        }
      }
    } else if (op.rpc != null) {
      relative.addAll(paths);
    }
    final rpc = op.rpc;
    if (rpc == null) return relative;
    final field = _str(rpc['params_field']);
    return [for (final p in relative) p.isEmpty ? field : '$field.$p'];
  }

  /// Every string or number found at a path, through arrays.
  static void valuesAt(
    Object? value,
    List<String> segments,
    List<String> out, [
    int depth = 0,
  ]) {
    if (depth > 64) return;
    if (value is List) {
      for (final item in value) {
        valuesAt(item, segments, out, depth + 1);
      }
      return;
    }
    if (segments.isEmpty) {
      if (value is String) {
        out.add(value);
      } else if (value is num) {
        out.add(Json.numberText(value));
      } else if (value is Map) {
        for (final v in value.values) {
          valuesAt(v, const [], out, depth + 1);
        }
      }
      return;
    }
    if (value is Map && value.containsKey(segments.first)) {
      valuesAt(value[segments.first], segments.sublist(1), out, depth + 1);
    }
  }

  /// The arguments with each parameter's value also under its wire name,
  /// so references written against the API find arguments whose SDK name
  /// differs.
  static Map<String, Object?> withWireNames(
    OperationDescriptor op,
    Map<String, Object?> args,
  ) {
    final out = <String, Object?>{...args};
    for (final p in op.params) {
      if (args.containsKey(p.name) && !out.containsKey(p.wire)) {
        out[p.wire] = args[p.name];
      }
    }
    return out;
  }

  /// A path with a leading parameter wire name rewritten to its name.
  static List<Object> argPath(OperationDescriptor op, List<Object> path) {
    if (path.isEmpty) return path;
    for (final p in op.params) {
      if (p.wire == path.first && p.name != path.first) {
        return [p.name, ...path.sublist(1)];
      }
    }
    return path;
  }

  /// {field} references filled from the arguments; sensitive ones are
  /// redacted, unset ones shown as <unset>.
  static String interpolate(
    Object? template,
    Map<String, Object?> args,
    OperationDescriptor op,
  ) =>
      _str(template).replaceAllMapped(RegExp(r'\{([^{}]+)\}'), (m) {
        final segments = argPath(op, m[1]!.trim().split('.'));
        final value = _Util.getPath(args, segments);
        if (identical(value, unset) || value == null) return '<unset>';
        if (sensitiveArg(op, segments)) return redacted;
        final shown = _Util.envelopeValue(value, false);
        return shown is String ? shown : Json.text(shown);
      });

  static final RegExp _placeholder = RegExp(r'^<from step [^<>]+>$');

  static void placeholdersIn(
    Object? value,
    List<String> out, [
    int depth = 0,
  ]) {
    if (depth > 64) return;
    if (value is String) {
      if (_placeholder.hasMatch(value)) out.add(value);
    } else if (value is List) {
      for (final v in value) {
        placeholdersIn(v, out, depth + 1);
      }
    } else if (value is Map) {
      for (final v in value.values) {
        placeholdersIn(v, out, depth + 1);
      }
    }
  }

  static bool isPlaceholder(Object? value) {
    final out = <String>[];
    placeholdersIn(value, out);
    return out.isNotEmpty;
  }

  /// " (name value, ...)": the scalar, non-sensitive arguments a call was
  /// made with (at most four, by wire name).
  static String sentArguments(
    OperationDescriptor op,
    Map<String, Object?> args,
  ) {
    final shown = <String>[];
    for (final e in args.entries) {
      if (shown.length == 4) break;
      final v = e.value;
      if (v is! String && v is! num && v is! bool) continue;
      if (sensitiveArg(op, [e.key])) continue;
      var wire = e.key;
      for (final p in op.params) {
        if (p.name == e.key) {
          wire = p.wire;
          break;
        }
      }
      shown.add('$wire ${_Util.shortJson(v)}');
    }
    return shown.isEmpty ? '' : ' (${shown.join(', ')})';
  }
}
