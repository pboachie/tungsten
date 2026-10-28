// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// The [ClientCore] entry points of pagination, server-sent event streams
/// and macros. Like the calls of [ClientCore], none of them throws for an
/// API or transport failure: a failure is an [Err] (the last item of a
/// stream).
extension ClientCoreStreams on ClientCore {
  /// The pages of a paginated operation ([operation] an id or an
  /// [OperationDescriptor]): cursor, offset, page number or `Link` header.
  /// Each item is an [Ok] whose value is a [Page] with its items decoded by
  /// [decode] (the wire values without one), or, last, the [Err] that ends
  /// the pagination. The next page is requested when the stream is read
  /// past the one before it; cancelling the subscription stops it.
  Stream<Result<Page<T>>> pages<T>(
    Object operation,
    Map<String, Object?> args, {
    CallOptions options = const CallOptions(),
    T Function(Object? raw)? decode,
  }) async* {
    final resolved = _resolve(operation);
    if (resolved is Err<Object?>) {
      yield resolved.cast<Page<T>>();
      return;
    }
    final op = resolved as OperationDescriptor;
    await for (final page in _pagesWith(op, args, _withCallDeadline(options))) {
      switch (page) {
        case Err<Page<Object?>>():
          yield page.cast<Page<T>>();
          return;
        case Ok<Page<Object?>>():
          final typed = _typedPage(op, page, decode);
          yield typed;
          if (typed is Err<Page<T>>) return;
      }
    }
  }

  /// A page with its items decoded; a decoder that throws makes it
  /// UNEXPECTED_RESPONSE, with the page body as the partial.
  Result<Page<T>> _typedPage<T>(
    OperationDescriptor op,
    Ok<Page<Object?>> page,
    T Function(Object? raw)? decode,
  ) {
    final p = page.value;
    final items = <T>[];
    for (var i = 0; i < p.rawItems.length; i++) {
      final raw = p.rawItems[i];
      try {
        items.add(decode != null ? decode(raw) : raw as T);
      } catch (e) {
        final mutation =
            op.isMutation ? ' Do not repeat the call, it took effect.' : '';
        return Err<Page<T>>(
          _diag(op.id, 'UNEXPECTED_RESPONSE')
              .status(page.meta.status == 0 ? null : page.meta.status)
              .requestId(page.meta.requestId)
              .param('items[$i]')
              .expected('a page item the SDK can decode')
              .remedy(
                'The SDK could not build its typed value from item $i of the page (${e.runtimeType}). '
                'The page body is in the partial.$mutation',
              )
              .retrying('never')
              .attempts(page.meta.attempts)
              .build(),
          partial: p.body,
        );
      }
    }
    return Ok<Page<T>>(
      Page<T>(items: items, rawItems: p.rawItems, body: p.body, next: p.next),
      meta: page.meta,
      raw: page.raw,
      verification: page.verification,
    );
  }

  /// The server-sent events of a streaming operation ([operation] an id or
  /// an [OperationDescriptor]), each event's JSON data decoded by [decode]
  /// (the JSON value without one). The request is sent when the stream is
  /// first listened to; see [EventStream].
  EventStream<T> stream<T>(
    Object operation,
    Map<String, Object?> args, {
    CallOptions options = const CallOptions(),
    T Function(Object? raw)? decode,
  }) {
    final resolved = _resolve(operation);
    if (resolved is Err<Object?>) return EventStream<T>._failed(resolved);
    final op = resolved as OperationDescriptor;
    if (op.stream == null) {
      final id = op.id.isEmpty ? '<unknown operation>' : op.id;
      return EventStream<T>._failed(
        _diag(id, 'VALIDATION_FAILED')
            .param('operation')
            .expected('an operation with an event stream')
            .remedy(
              '$id has no event stream (its success response is not text/event-stream); use call instead. Nothing was sent.',
            )
            .err<Object?>(),
      );
    }
    register([op]);
    return EventStream<T>._(this, op, args, _withCallDeadline(options), decode);
  }

  /// Runs a macro ([macro] a name or a [MacroDescriptor]) with its input
  /// keyed by the SDK's names: its steps in order, each step's result bound
  /// to its `as` name. The value of the [Ok] is what [decode] makes of the
  /// macro's evaluated output (the output itself without one); an [Err]
  /// carries the failing step's envelope under the macro's name (its
  /// remediation names the steps already completed) with their results by
  /// `as` name as the partial. A destructive or irreversible macro needs
  /// `confirm`: [Confirm.yes] (destructive only) or the token of
  /// [previewMacro].
  Future<Result<T>> runMacro<T>(
    Object macro,
    Map<String, Object?> input, {
    CallOptions options = const CallOptions(),
    T Function(Object? raw)? decode,
  }) async {
    final resolved = _resolveMacro(macro);
    if (resolved is Err<Object?>) return resolved.cast<T>();
    final m = resolved as MacroDescriptor;
    final name = m.name.isEmpty ? '<unknown macro>' : m.name;
    final result = await _guard(
        name, () => _runMacroWith(m, input, _withCallDeadline(options)));
    return _typed(result, name, decode);
  }

  /// Previews a macro ([macro] a name or a [MacroDescriptor]); nothing is
  /// sent. The first step is validated and rendered (as are later steps
  /// whose arguments are known; the results of earlier steps show as
  /// `<from step NAME: path>`), every step's effects are listed in
  /// [PreviewResult.steps], and a macro that is not read-only gets a
  /// confirmation token bound to the macro and this exact input.
  Future<Result<PreviewResult>> previewMacro(
    Object macro,
    Map<String, Object?> input, {
    CallOptions options = const CallOptions(),
  }) async {
    final resolved = _resolveMacro(macro);
    if (resolved is Err<Object?>) return resolved.cast<PreviewResult>();
    final m = resolved as MacroDescriptor;
    final name = m.name.isEmpty ? '<unknown macro>' : m.name;
    final result = await _guard(
        name, () => _previewMacroWith(m, input, _withCallDeadline(options)));
    return switch (result) {
      Err<Object?>() => result.cast<PreviewResult>(),
      Ok<Object?>() => result.withValue(result.value! as PreviewResult),
    };
  }
}
