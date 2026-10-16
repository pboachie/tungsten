// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// A decoded body: [value] is parsed JSON, text, or a [Binary] ([unset]
/// for an empty body), with what it was.
final class _Decoded {
  const _Decoded(
    this.value, {
    this.json = false,
    this.invalidJson = false,
    this.empty = false,
    this.jsonl = false,
    this.badLine = 0,
  });

  final Object? value;
  final bool json;
  final bool invalidJson;
  final bool empty;
  final bool jsonl;
  final int badLine;

  bool get present => !identical(value, unset);
}

/// How to find out whether an unprotected mutation took effect: the call
/// to make and what to look for, in words.
final class _OutcomeCheck {
  const _OutcomeCheck(this.call, this.shows);

  final String call;
  final String shows;
}

/// Everything one call's classification shares: the operation, its key
/// and header, the attempts so far and how to check a lost mutation.
final class _CallContext {
  const _CallContext({
    required this.api,
    required this.op,
    required this.key,
    required this.keyHeader,
    required this.attempts,
    this.check,
  });

  final Map<String, Object?> api;
  final OperationDescriptor op;
  final String? key;
  final String keyHeader;
  final int attempts;
  final _OutcomeCheck? check;

  bool get hasKey => key != null;
}

/// Decoding answers and classifying failures into the envelope.
abstract final class _Classify {
  static const List<String> _requestIdHeaders = [
    'x-request-id',
    'request-id',
    'x-correlation-id',
    'x-amzn-requestid',
    'cf-ray',
  ];

  static const List<String> _messageFields = [
    'message',
    'error.message',
    'detail',
    'error_description',
    'title',
    'error',
  ];

  static const List<String> _definite = [
    'VALIDATION_FAILED',
    'MALFORMED_REQUEST',
    'REQUEST_TOO_LARGE',
    'AUTH_FAILED',
    'NOT_FOUND',
    'CONFLICT',
    'PRECONDITION_FAILED',
    'RATE_LIMITED',
    'GATE_DISABLED',
  ];

  static String? requestId(Map<String, String> headers) {
    for (final name in _requestIdHeaders) {
      final v = headers[name];
      if (v != null && v.isNotEmpty) return Json.takeUnits(v, 200);
    }
    return null;
  }

  static String mediaType(Map<String, String> headers) =>
      (headers['content-type'] ?? '').split(';').first.trim().toLowerCase();

  static bool jsonMedia(String media) =>
      media == 'application/json' ||
      media.endsWith('+json') ||
      media == 'text/json';

  static bool jsonlMedia(String media) => const [
        'application/jsonl',
        'application/x-jsonl',
        'application/ndjson',
        'application/x-ndjson',
      ].contains(media);

  static final RegExp _statusClass = RegExp(r'^[1-5]XX$');

  /// The declared response of a status: exact, then by class, then
  /// `default`.
  static ResponseDescriptor? matchResponse(
    List<ResponseDescriptor> responses,
    int status,
  ) {
    for (final r in responses) {
      if (r.status == status) return r;
    }
    for (final r in responses) {
      final s = r.status;
      if (s is String &&
          _statusClass.hasMatch(s) &&
          int.parse(s[0]) == status ~/ 100) {
        return r;
      }
    }
    for (final r in responses) {
      if (r.status == 'default') return r;
    }
    return null;
  }

  /// UTF-8 text without a byte order mark, invalid sequences replaced.
  static String decodeText(List<int> data) {
    final text = utf8.decode(data, allowMalformed: true);
    return text.startsWith('﻿') ? text.substring(1) : text;
  }

  static _Decoded decodeBody(
    List<int> data,
    Map<String, String> headers,
    String? declared,
  ) {
    final announced = mediaType(headers);
    if (data.isEmpty && !jsonlMedia(announced)) {
      return const _Decoded(unset, empty: true);
    }
    final media =
        announced.isEmpty ? (declared ?? '').toLowerCase() : announced;
    final jsonlType =
        announced.isEmpty ? media.split(';').first.trim() : announced;
    if (jsonlMedia(jsonlType)) {
      final raw = decodeText(data);
      final values = <Object?>[];
      final lines = raw.split('\n');
      for (var i = 0; i < lines.length; i++) {
        if (lines[i].trim().isEmpty) continue;
        final v = Json.tryParse(lines[i]);
        if (identical(v, unset)) {
          return _Decoded(raw, invalidJson: true, jsonl: true, badLine: i + 1);
        }
        values.add(v);
      }
      return _Decoded(values, jsonl: true);
    }
    if (jsonMedia(media)) {
      final raw = decodeText(data);
      final v = Json.tryParse(raw);
      return identical(v, unset)
          ? _Decoded(raw, invalidJson: true)
          : _Decoded(v, json: true);
    }
    if (media.isEmpty ||
        media.startsWith('text/') ||
        media == 'application/problem+xml' ||
        media.endsWith('+xml') ||
        media == 'application/xml') {
      final raw = decodeText(data);
      final lead = raw.trimLeft();
      if (media.isEmpty && (lead.startsWith('[') || lead.startsWith('{'))) {
        final v = Json.tryParse(raw);
        if (!identical(v, unset)) return _Decoded(v, json: true);
      }
      return _Decoded(raw);
    }
    return _Decoded(Binary(Uint8List.fromList(data)));
  }

  static String? serverMessage(Object? body) {
    for (final field in _messageFields) {
      final v = _Util.getPathStr(body, field);
      if (v is! String) continue;
      final text = v.trim();
      if (text.isNotEmpty) return Json.takeUnits(text, 200);
    }
    return null;
  }

  static String? errorCode(OperationDescriptor op, Object? body) {
    final field = op.errorCodeField;
    if (field == null || field.isEmpty) return null;
    final v = _Util.getPathStr(body, field);
    if (v is String) return v.isEmpty ? null : Json.takeUnits(v, 200);
    if (v is num) return Json.numberText(v);
    return null;
  }

  static bool _refersToResponse(Object? node, [int depth = 0]) {
    if (depth > 32) return true;
    if (node is String) {
      return node == r'$response' || node.startsWith(r'$response.');
    }
    if (node is List) return node.any((n) => _refersToResponse(n, depth + 1));
    if (node is Map) {
      return node.values.any((n) => _refersToResponse(n, depth + 1));
    }
    return false;
  }

  /// After a lost answer there is no success body, so a hook whose
  /// arguments reference `$response` cannot be called.
  static bool verifyCallable(Map<String, Object?> verify) =>
      !_refersToResponse(verify['args']);

  static String? nextActionHint(_CallContext ctx) {
    final op = ctx.op;
    final v = op.agent['verify'];
    if (v is Map<String, Object?> &&
        _str(v['operation']).isNotEmpty &&
        verifyCallable(v)) {
      final args = v['args'];
      final with_ =
          args is Map && args.isNotEmpty ? ' with ${args.keys.join(', ')}' : '';
      return 'Call ${v['operation']}$with_ to check whether ${op.id} took effect before doing anything else.';
    }
    if (op._idempotency['policy'] == 'content_identity') {
      return 'Call ${op.id} again with the identical body; the body is its own identity, so the server answers the original result instead of applying it twice.';
    }
    if (!ctx.hasKey) return null;
    return 'Call ${op.id} again with the same ${ctx.keyHeader} value and identical arguments; the server answers the original result instead of applying it twice.';
  }

  /// The change an operation makes, in words.
  static String changeOf(OperationDescriptor op) {
    var summary = op.summary.trim();
    if (summary.endsWith('.')) {
      summary = summary.substring(0, summary.length - 1);
    }
    return summary.isEmpty
        ? 'the change ${op.id} makes'
        : 'the change ${_Util.jsString(summary)}';
  }

  static bool replayProtection(OperationDescriptor op, bool hasKey) =>
      op._idempotency['policy'] == 'content_identity' || hasKey;

  /// OUTCOME_UNKNOWN for a mutation whose effect cannot be known. Without
  /// replay protection a repeat can apply the effect twice, so the
  /// envelope never offers one.
  static Diagnostic outcomeUnknown(
    _CallContext ctx,
    String cause, {
    String? nextAction,
    int? httpStatus,
    String? code,
    String? requestId,
    int? retryAfterMs,
  }) {
    final op = ctx.op;
    String? hint;
    final String rule;
    if (op._idempotency['policy'] == 'content_identity') {
      rule =
          'If you retry, resend the identical bytes only; the body is its own identity.';
    } else if (ctx.hasKey) {
      rule =
          'If you retry, reuse the SAME ${ctx.keyHeader} value; a new key can apply the effect twice.';
    } else if (ctx.check != null) {
      final c = ctx.check!;
      hint =
          'Call ${c.call} and check ${c.shows}; call ${op.id} again only if it did not take effect.';
      rule =
          'This operation has no idempotency key, so repeating it can apply the effect twice. Before calling ${op.id} again, '
          'call ${c.call} and check ${c.shows}: if it does, ${op.id} took effect and must not be repeated; call it again only if it did not.';
    } else {
      final change = changeOf(op);
      hint =
          'Do not retry ${op.id} blindly: it may have taken effect. Check for $change if you can; call ${op.id} '
          'again only if it is not there, or after the task owner accepts that it may apply twice.';
      rule =
          'This operation has no idempotency key and no registered way to verify it, so ${op.id} may have taken effect and '
          'a repeat can apply the effect twice: do not retry it blindly. Check the outcome by other means if you can '
          '(look for $change); if you cannot, ask whoever owns the task before calling it again.';
    }
    final retryable = replayProtection(op, ctx.hasKey)
        ? 'same_key_only'
        : 'after_remediation';
    return _diag(op.id, 'OUTCOME_UNKNOWN')
        .remedy(
          '$cause The server may or may not have applied ${op.id}. $rule',
        )
        .retrying(retryable)
        .next(nextAction ?? hint ?? nextActionHint(ctx))
        .status(httpStatus)
        .code(code)
        .requestId(requestId)
        .retryAfter(retryAfterMs)
        .attempts(ctx.attempts)
        .build();
  }

  /// A manifest entry with one of these categories (or retryable never /
  /// after_remediation) is a definite answer even for an ambiguous status.
  static bool definite(Object? category, Object? retryable) =>
      retryable == 'never' ||
      retryable == 'after_remediation' ||
      _definite.contains(category);

  /// The envelope of an HTTP error status.
  static Diagnostic classifyError(
    _CallContext ctx,
    int status,
    Map<String, String> headers,
    _Decoded decoded,
    int? retryAfter,
  ) {
    final api = ctx.api;
    final op = ctx.op;
    final rid = requestId(headers);
    _Diag base(_Diag d) => d
        .status(status)
        .requestId(rid)
        .retryAfter(retryAfter)
        .attempts(ctx.attempts);
    final code = decoded.json ? errorCode(op, decoded.value) : null;
    final gate = op.gate;
    if (gate != null && status == gate['disabled_status'] && code == null) {
      final gates = _map(api['gates']);
      final text = gates[gate['env_var']] is String
          ? gates[gate['env_var']]! as String
          : 'This deployment disables ${op.id} because ${gate['env_var']} is off (HTTP $status). It is a deployment setting, not a missing resource; do not retry.';
      return base(_diag(op.id, 'GATE_DISABLED'))
          .code(code)
          .remedy(text)
          .retrying('never')
          .build();
    }
    Map<String, Object?>? entry;
    if (code != null) {
      final own = _map(op.agent['remediation'])[code];
      final shared = _map(api['error_codes'])[code];
      if (own is Map) {
        entry = _map(own);
      } else if (shared is Map) {
        entry = _map(shared);
      }
    }
    var media = 'none';
    if (!decoded.empty) {
      final m = mediaType(headers);
      if (m.isNotEmpty) media = m;
    }
    Map<String, Object?>? nonJson;
    if (!decoded.json) {
      for (final n in _list(api['non_json'])) {
        final m = _map(n);
        if (m['status'] == status &&
            (m['media'] == media || (m['media'] == 'none' && decoded.empty))) {
          nonJson = m;
          break;
        }
      }
    }
    final declared = matchResponse(op.responses, status);
    final ambiguous = _list(api['ambiguous_statuses']).contains(status) ||
        declared?.kind == 'ambiguous' ||
        (status >= 500 && status <= 599);
    final isDefinite =
        (entry != null && definite(entry['category'], entry['retryable'])) ||
            (entry == null &&
                nonJson != null &&
                definite(nonJson['category'], nonJson['retryable']));
    if (ambiguous && op.isMutation && !isDefinite) {
      String? said;
      if (entry != null && _str(entry['text']).isNotEmpty) {
        said = _str(entry['text']);
      } else if (nonJson != null && _str(nonJson['text']).isNotEmpty) {
        said = _str(nonJson['text']);
      }
      var cause = 'HTTP $status leaves the outcome of this call unknown.';
      if (said != null) cause += ' $said';
      return outcomeUnknown(
        ctx,
        cause,
        nextAction: entry?['next_action'] as String?,
        httpStatus: status,
        code: code,
        requestId: rid,
        retryAfterMs: retryAfter,
      );
    }
    String category;
    String? retryable;
    String? text;
    String? nextAction;
    if (entry != null) {
      category = entry['category'] is String
          ? entry['category']! as String
          : categoryForStatus(status, decoded.json);
      retryable = entry['retryable'] as String?;
      text = entry['text'] as String?;
      nextAction = entry['next_action'] as String?;
    } else if (nonJson != null) {
      category = _str(nonJson['category']);
      retryable = nonJson['retryable'] as String?;
      text = nonJson['text'] as String?;
    } else {
      category = categoryForStatus(status, decoded.json);
    }
    if (category == 'OUTCOME_UNKNOWN' && !op.isMutation) {
      category = 'UPSTREAM_UNAVAILABLE';
      if (retryable == null || retryable == 'same_key_only') {
        retryable = 'after_delay';
      }
    }
    if (text == null || text.isEmpty) {
      text = genericRemediation(category);
      final said = decoded.json ? serverMessage(decoded.value) : null;
      if (said != null) text += ' Server message: ${_Util.jsString(said)}';
    }
    return base(_diag(op.id, category))
        .code(code)
        .remedy(text)
        .retrying(retryable)
        .next(nextAction)
        .build();
  }
}
