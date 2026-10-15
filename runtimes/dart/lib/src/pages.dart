// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Where a pagination is: the arguments of the next page, the URL of a
/// Link header page, the last cursor seen, and an error to report next.
final class _PageState {
  _PageState(this.args);

  /// The arguments of one pagination as JSON (a copy, updated page by
  /// page); arguments that are not JSON stay as given, so the call refuses
  /// them.
  factory _PageState.of(Map<String, Object?> raw) {
    Object? converted;
    try {
      converted = Json.from(Models.dump(raw));
    } on ArgumentError {
      converted = null;
    }
    return _PageState(
      converted is Map<String, Object?> ? {...converted} : {...raw},
    );
  }

  final Map<String, Object?> args;
  String? overrideUrl;
  Object? previousCursor = unset;
  bool done = false;
  Err<Page<Object?>>? pendingError;

  Object? arg(String key) => args.containsKey(key) ? args[key] : unset;
}

/// The next-page URL of a `Link` header (`rel="next"`), or null.
String? _nextLink(String? header) {
  if (header == null || header.isEmpty) return null;
  final rel = RegExp(
    r'(;\s*rel\s*=\s*"?([^";]*\s)?next(\s[^";]*)?"?\s*(;|$))',
    caseSensitive: false,
  );
  for (final part in header.split(RegExp(r',(?=\s*<)'))) {
    final m = RegExp(r'^\s*<([^>]*)>(.*)$', dotAll: true).firstMatch(part);
    if (m == null) continue;
    if (rel.hasMatch(m[2]!)) return m[1];
  }
  return null;
}

/// Pagination: cursor, offset, page number or `Link` header, page by page.
extension _Pages on ClientCore {
  /// The pages of a paginated operation as a stream: each item is an [Ok]
  /// whose value is a [Page] or, last, an [Err]. A page is requested when
  /// the stream is read past the one before it.
  Stream<Result<Page<Object?>>> _pagesWith(
    OperationDescriptor op,
    Map<String, Object?> args,
    CallOptions opts,
  ) async* {
    final state = _PageState.of(args);
    while (true) {
      Result<Page<Object?>>? page;
      try {
        page = await _pageStep(op, state, opts, null);
      } catch (_) {
        state.done = true;
        page = _bug(op.id, 'paginating');
      }
      if (page == null) return;
      yield page;
      if (page is Err<Page<Object?>>) return;
    }
  }

  /// The envelope of a failure inside the runtime (a bug).
  Err<T> _bug<T>(String id, String during) => _diag(id, 'UNEXPECTED_RESPONSE')
      .retrying('never')
      .remedy(
        'The SDK failed while $during. This is a bug in the generated SDK or the runtime; report it. '
        'If the call may have been sent, check its effect before repeating it.',
      )
      .err<T>();

  /// The next page ([Ok] or [Err]), or null after the last one.
  Future<Result<Page<Object?>>?> _pageStep(
    OperationDescriptor op,
    _PageState state,
    CallOptions opts,
    _Claim? claim,
  ) async {
    final pending = state.pendingError;
    if (pending != null) {
      state
        ..pendingError = null
        ..done = true;
      return pending;
    }
    if (state.done) return null;
    final response = await _guard(
      op.id,
      () => _callWith(op, state.args, opts, state.overrideUrl, claim),
    );
    if (response is Err<Object?>) {
      state.done = true;
      return response.cast<Page<Object?>>();
    }
    final ok = response as Ok<Object?>;
    final body = ok.raw ?? unset;
    final pg = op.pagination;
    final itemsField = pg == null ? '' : _str(pg['items_field']);
    final found =
        itemsField.isEmpty ? body : _Util.getPathStr(body, itemsField);
    final items = found is List ? [...found] : <Object?>[];
    Object? next;
    switch (pg?['style']) {
      case 'cursor':
        bool usable(Object? v) => !identical(v, unset) && v != null && v != '';
        Object? cursor = unset;
        final responseField = _str(pg!['response_field']);
        if (responseField.isNotEmpty) {
          final v = _Util.getPathStr(body, responseField);
          if (usable(v)) cursor = v;
        }
        final itemField = _str(pg['cursor_item_field']);
        if (identical(cursor, unset) &&
            itemField.isNotEmpty &&
            items.isNotEmpty) {
          final v = _Util.getPathStr(items.last, itemField);
          if (usable(v)) cursor = v;
        }
        var finished = false;
        final moreField = _str(pg['has_more_field']);
        if (moreField.isNotEmpty) {
          finished = _Util.getPathStr(body, moreField) == false;
        }
        final repeated = !identical(state.previousCursor, unset) &&
            !identical(cursor, unset) &&
            Json.canonical(state.previousCursor) == Json.canonical(cursor);
        if (!identical(cursor, unset) && !finished && !repeated) {
          next = cursor;
          state
            ..previousCursor = cursor
            ..args[op.argName(_str(pg['request_param']))] = cursor;
        }
      case 'offset':
        final offsetKey = op.argName(_str(pg!['offset_param']));
        final limit = state.arg(op.argName(_str(pg['limit_param'])));
        final offset =
            _Util.bounded(state.arg(offsetKey), 0, 0, _Util.maxSafeInteger);
        final short = limit is num && items.length < limit;
        if (items.isNotEmpty && !short) {
          next = Json.number(offset + items.length);
          state.args[offsetKey] = next;
        }
      case 'page':
        final pageKey = op.argName(_str(pg!['page_param']));
        final size = state.arg(op.argName(_str(pg['size_param'])));
        final page =
            _Util.bounded(state.arg(pageKey), 1, 0, _Util.maxSafeInteger);
        final short = size is num && items.length < size;
        if (items.isNotEmpty && !short) {
          next = Json.number(page + 1);
          state.args[pageKey] = next;
        }
      case 'link_header':
        final link = _nextLink(ok.meta.headers['link']);
        if (link != null) {
          final target = _linkTarget(state.overrideUrl ?? baseUrl, link);
          if (target != null) {
            state.overrideUrl = target;
            next = target;
          } else {
            state.pendingError = _diag(op.id, 'UNEXPECTED_RESPONSE')
                .status(ok.meta.status)
                .requestId(ok.meta.requestId)
                .remedy(
                  "The next-page Link header points to another origin; it is not followed so credentials never leave the API's host. "
                  'Stop paginating here.',
                )
                .attempts(ok.meta.attempts)
                .err<Page<Object?>>();
          }
        }
    }
    final bad = _checkItems(op, itemsField, items, ok.meta);
    if (bad != null) {
      state.done = true;
      return bad;
    }
    if (next == null) state.done = true;
    return Ok<Page<Object?>>(
      Page<Object?>(
        items: items,
        rawItems: items,
        body: identical(body, unset) ? null : body,
        next: next,
      ),
      meta: ok.meta,
      raw: ok.raw,
      verification: ok.verification,
    );
  }

  /// The absolute URL of a next-page link on the origin of [base], or null
  /// when it leaves that origin.
  static String? _linkTarget(String base, String link) {
    final origin = Uri.tryParse(base);
    if (origin == null || origin.scheme.isEmpty || origin.host.isEmpty) {
      return null;
    }
    final Uri resolved;
    try {
      resolved = origin.resolve(link).removeFragment();
    } on FormatException {
      return null;
    }
    if (!ClientCore._sameOrigin(origin, resolved) ||
        resolved.userInfo.isNotEmpty) {
      return null;
    }
    return resolved.toString();
  }

  /// Page items checked with the operation's page item validator: a
  /// mismatch is a warning, or with strict validation the failure that
  /// ends the pagination.
  Err<Page<Object?>>? _checkItems(
    OperationDescriptor op,
    String itemsField,
    List<Object?> items,
    ResponseMeta meta,
  ) {
    final schema = op.pageItem;
    if (schema == null || _validateResponses == ValidateResponses.off) {
      return null;
    }
    for (var index = 0; index < items.length; index++) {
      final issues = ClientCore._judge(schema, items[index]);
      if (issues.isEmpty) continue;
      final issue = issues.first;
      final field = itemsField.isEmpty ? '' : '.$itemsField';
      final path = 'response$field[$index]${Schema.issuePath(issue.path)}';
      final d = _diag(op.id, 'UNEXPECTED_RESPONSE')
          .status(meta.status)
          .requestId(meta.requestId)
          .param(path)
          .expected(issue.message)
          .remedy(
            'A page item does not match the API description at $path (${issue.message}).',
          )
          .attempts(meta.attempts)
          .build();
      if (_validateResponses == ValidateResponses.strict) {
        return Err<Page<Object?>>(d);
      }
      _emit(d);
      return null;
    }
    return null;
  }
}
