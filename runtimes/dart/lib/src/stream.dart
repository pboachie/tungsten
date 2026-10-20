// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// The wait before a reconnect when the server sent no `retry`, in
/// milliseconds.
const int _defaultReconnectMs = 250;

/// What the consuming helpers of an [EventStream] give: the result, or
/// when the stream failed, what was gathered before the failure as [value]
/// with the failure as [error]; [reconnects] is the reconnect count of the
/// last event delivered.
final class StreamResult<R> {
  /// A helper's result.
  const StreamResult(this.value, {this.error, required this.reconnects});

  /// The result, or what was gathered before [error].
  final R value;

  /// The failure that ended the stream, or null when it ended cleanly.
  final Diagnostic? error;

  /// The reconnects made up to the last event delivered.
  final int reconnects;

  /// Whether the stream ended without a failure.
  bool get isOk => error == null;
}

/// A handler of [EventStream.on]: the decoded data and the event.
typedef EventHandler<T> = FutureOr<void> Function(
  T value,
  StreamEvent<T> event,
);

/// The server-sent events of an operation, a single-subscription [Stream]
/// of [Result]s: one [Ok] per event (its value a [StreamEvent]), then the
/// end of the stream or, last, the [Err] that ended it. Reading never
/// throws for an API or transport failure.
///
/// The request is sent when the stream is first listened to. A dropped
/// stream is reconnected (with `Last-Event-ID`, after the server's `retry`
/// wait) when the call is safe to repeat, and an event whose id was
/// delivered is never delivered twice; with [ClientOptions.idleTimeout] a
/// silent connection is idle (`STREAM_IDLE`). Cancelling the subscription,
/// or [cancel], closes the connection.
///
/// The helpers consume the stream (it is read once) and close the
/// connection when they return: [on], [collect], [reduceEvents] and
/// [firstEvent] (the `reduce` and `first` of the other runtimes; the
/// names of [Stream.reduce] and [Stream.first] keep their [Stream]
/// meaning, over the items).
final class EventStream<T> extends Stream<Result<StreamEvent<T>>> {
  EventStream._(
    this._core,
    this._op,
    this._args,
    this._options,
    this._decode,
  ) : _failure = null;

  EventStream._failed(Err<Object?> failure)
      : _core = null,
        _op = null,
        _args = const {},
        _options = const CallOptions(),
        _decode = null,
        _failure = failure;

  final ClientCore? _core;
  final OperationDescriptor? _op;
  final Map<String, Object?> _args;
  final CallOptions _options;
  final T Function(Object? raw)? _decode;
  final Err<Object?>? _failure;
  bool _listened = false;
  bool _cancelled = false;
  bool _stopped = false;
  _StreamOpen? _open;
  Completer<void>? _wake;
  final Completer<void> _stopSignal = Completer<void>();

  /// The streaming operation, or null when the stream failed before it
  /// started.
  OperationDescriptor? get operation => _op;

  /// Whether [cancel] was called.
  bool get cancelled => _cancelled;

  /// Closes the connection and ends the stream for good: a pending or
  /// later read ends without a failure item, and nothing reconnects. Safe
  /// to call more than once, before or while the stream is read.
  void cancel() {
    _cancelled = true;
    _stop();
  }

  void _stop() {
    if (_stopped) return;
    _stopped = true;
    _open?.body.close();
    final wake = _wake;
    _wake = null;
    if (wake != null && !wake.isCompleted) wake.complete();
    if (!_stopSignal.isCompleted) _stopSignal.complete();
  }

  @override
  bool get isBroadcast => false;

  @override
  StreamSubscription<Result<StreamEvent<T>>> listen(
    void Function(Result<StreamEvent<T>> event)? onData, {
    Function? onError,
    void Function()? onDone,
    bool? cancelOnError,
  }) {
    if (_listened) throw StateError('An EventStream can be read only once.');
    _listened = true;
    StreamSubscription<Result<StreamEvent<T>>>? source;
    final controller = StreamController<Result<StreamEvent<T>>>();
    controller
      ..onListen = () {
        source = _items().listen(
          controller.add,
          onError: controller.addError,
          onDone: () => unawaited(controller.close()),
        );
      }
      ..onPause = () {
        source?.pause();
      }
      ..onResume = () {
        source?.resume();
      }
      ..onCancel = () {
        _stop();
        return source?.cancel();
      };
    return controller.stream.listen(
      onData,
      onError: onError,
      onDone: onDone,
      cancelOnError: cancelOnError,
    );
  }

  Stream<Result<StreamEvent<T>>> _items() async* {
    final failure = _failure;
    if (failure != null) {
      yield failure.cast<StreamEvent<T>>();
      return;
    }
    final core = _core!;
    final op = _op!;
    try {
      while (true) {
        final Result<StreamEvent<T>>? item;
        try {
          item = await _next(core, op);
        } on _Stop catch (s) {
          yield s.err.cast<StreamEvent<T>>();
          return;
        } catch (_) {
          yield core._bug<StreamEvent<T>>(op.id, 'streaming');
          return;
        }
        if (item == null) return;
        yield item;
        if (item is Err<StreamEvent<T>>) return;
      }
    } finally {
      _open?.body.close();
      _open = null;
    }
  }

  /// Waits [ms] milliseconds, or less when the stream is stopped.
  /// The result of an open that is in flight, or null once the stream is
  /// stopped first (a cancel or a collection deadline interrupts the wait for
  /// the response; a connection that opens later is closed).
  Future<(Err<Object?>?, _StreamOpen?)?> _opening(
    Future<(Err<Object?>?, _StreamOpen?)> opening,
  ) async {
    if (_stopped) {
      unawaited(_discard(opening));
      return null;
    }
    final stopped = _stopSignal.future.then<(Err<Object?>?, _StreamOpen?)?>(
      (_) => null,
    );
    final result = await Future.any([opening, stopped]);
    if (result == null) unawaited(_discard(opening));
    return result;
  }

  static Future<void> _discard(
    Future<(Err<Object?>?, _StreamOpen?)> opening,
  ) async {
    try {
      (await opening).$2?.body.close();
    } catch (_) {
      // The abandoned open failed; nothing is left to close.
    }
  }

  Future<void> _pause(double ms) {
    final wake = Completer<void>();
    _wake = wake;
    final timer = Timer(
      Duration(microseconds: (math.max(ms, 0) * 1000).round()),
      () {
        if (!wake.isCompleted) wake.complete();
      },
    );
    return wake.future.whenComplete(timer.cancel);
  }

  /// The next item, or null at the end.
  Future<Result<StreamEvent<T>>?> _next(
    ClientCore core,
    OperationDescriptor op,
  ) async {
    while (true) {
      if (_stopped) return null;
      var o = _open;
      if (o == null) {
        final (err, opened) =
            await _opening(core._openStream(op, _args, _options, null, null)) ??
                (null, null);
        if (_stopped) {
          opened?.body.close();
          return null;
        }
        if (err != null) return err.cast<StreamEvent<T>>();
        _open = o = opened!;
      }
      if (o.queue.isNotEmpty) {
        final ev = o.queue.removeFirst();
        final done = op.stream!.done;
        if (done != null && ev.data == done) return null;
        final item = core._acceptEvent<T>(op, o, ev, _decode);
        if (item == null) continue;
        return item;
      }
      if (o.oversize) return core._oversize(op, o).cast<StreamEvent<T>>();
      String why;
      if (o.ended) {
        // A stream that names its terminal event has not finished without it.
        if (op.stream!.done == null) return null;
        why = 'early_end';
      } else {
        final (kind, chunk) = await o.body.read();
        if (_stopped) return null;
        switch (kind) {
          case 'chunk':
            o.queue.addAll(o.parser.push(o.decoder.decode(chunk!)));
            o.oversize = o.parser.exceeded;
            continue;
          case 'end':
            o.queue
              ..addAll(o.parser.push(o.decoder.finish()))
              ..addAll(o.parser.end());
            o
              ..oversize = o.parser.exceeded
              ..ended = true;
            continue;
          case 'timeout':
            if (core.idleTimeoutMs == null) {
              return core._interrupted(op, o, null).cast<StreamEvent<T>>();
            }
            why = 'idle';
          default:
            why = 'lost';
        }
      }
      if (!core._mayReconnect(o)) {
        // Without reconnects a clean end stays a clean end.
        if (why == 'early_end' && o.reconnects == 0) return null;
        return core._interrupted(op, o, why).cast<StreamEvent<T>>();
      }
      o.body.close();
      final server = o.lastRetry ?? _defaultReconnectMs;
      final ceiling = math.min(server, core.reconnectMaxMs).toDouble();
      await _pause(ceiling - core._random() * 0.25 * ceiling);
      if (_stopped) return null;
      final (err, fresh) =
          await _opening(core._openStream(op, _args, _options, o.lastId, o)) ??
              (null, null);
      if (_stopped) {
        fresh?.body.close();
        return null;
      }
      if (err != null) return err.cast<StreamEvent<T>>();
      _open = fresh;
    }
  }

  /// Reads the stream to its end and calls the handler named like each
  /// event (`handlers['message_delta']` for `event: message_delta`; an
  /// event without a name is `message`), else `handlers['*']`. A handler
  /// that throws ends the stream (the connection is closed) and the error
  /// propagates. The value is the number of events delivered.
  Future<StreamResult<int>> on(Map<String, EventHandler<T>> handlers) async {
    var count = 0;
    var reconnects = 0;
    await for (final item in this) {
      switch (item) {
        case Err<StreamEvent<T>>():
          return StreamResult(count, error: item.error, reconnects: reconnects);
        case Ok<StreamEvent<T>>():
          final event = item.value;
          reconnects = event.reconnects;
          count++;
          final handler = handlers[event.event] ?? handlers[anyEvent];
          if (handler != null) await handler(event.value, event);
      }
    }
    return StreamResult(count, reconnects: reconnects);
  }

  /// The key of [on] handlers that receives every event no other handler
  /// took.
  static const String anyEvent = '*';

  /// Reads the stream to its end and folds the events into one value
  /// (concatenating text deltas, say); after a failure the value is what
  /// was folded before it.
  Future<StreamResult<A>> reduceEvents<A>(
    A initial,
    A Function(A accumulator, T value, StreamEvent<T> event) fold,
  ) async {
    var accumulator = initial;
    var reconnects = 0;
    await for (final item in this) {
      switch (item) {
        case Err<StreamEvent<T>>():
          return StreamResult(accumulator,
              error: item.error, reconnects: reconnects);
        case Ok<StreamEvent<T>>():
          final event = item.value;
          reconnects = event.reconnects;
          accumulator = fold(accumulator, event.value, event);
      }
    }
    return StreamResult(accumulator, reconnects: reconnects);
  }

  /// Reads the stream to its end into the list of its events. The
  /// collection is bounded: it ends with UNEXPECTED_RESPONSE once the kept
  /// events hold more than [ClientOptions.maxCollectBytes] of JSON, or it
  /// ran longer than [ClientOptions.maxCollect]; the events collected
  /// before are the value.
  Future<StreamResult<List<StreamEvent<T>>>> collect() async {
    final events = <StreamEvent<T>>[];
    var reconnects = 0;
    final core = _core;
    if (core == null) {
      return StreamResult(events,
          error: _failure?.error, reconnects: reconnects);
    }
    var size = 0;
    var overTime = false;
    final deadline = Timer(Duration(milliseconds: core.maxCollectMs), () {
      overTime = true;
      _stop();
    });
    try {
      await for (final item in this) {
        switch (item) {
          case Err<StreamEvent<T>>():
            return StreamResult(events,
                error: item.error, reconnects: reconnects);
          case Ok<StreamEvent<T>>():
            final event = item.value;
            size += utf8.encode(Json.text(event.raw)).length;
            if (size > core.maxCollectBytes) {
              return StreamResult(
                events,
                error: core._overBudget(_op!, true, events.length),
                reconnects: reconnects,
              );
            }
            reconnects = event.reconnects;
            events.add(event);
        }
      }
    } finally {
      deadline.cancel();
    }
    if (overTime) {
      return StreamResult(
        events,
        error: core._overBudget(_op!, false, events.length),
        reconnects: reconnects,
      );
    }
    return StreamResult(events, reconnects: reconnects);
  }

  /// The first event for which [predicate] holds (any event without one),
  /// then the stream is closed; the value is null when the stream ended
  /// without such an event.
  Future<StreamResult<StreamEvent<T>?>> firstEvent([
    bool Function(T value, StreamEvent<T> event)? predicate,
  ]) async {
    var reconnects = 0;
    await for (final item in this) {
      switch (item) {
        case Err<StreamEvent<T>>():
          return StreamResult(null, error: item.error, reconnects: reconnects);
        case Ok<StreamEvent<T>>():
          final event = item.value;
          reconnects = event.reconnects;
          if (predicate == null || predicate(event.value, event)) {
            return StreamResult(event, reconnects: reconnects);
          }
      }
    }
    return StreamResult(null, reconnects: reconnects);
  }
}

/// The connection of a stream being read and where the stream is.
final class _StreamOpen {
  _StreamOpen({
    required this.body,
    required this.parser,
    required this.meta,
    required this.key,
    required this.keyHeader,
    required this.check,
    required this.secrets,
    required this.timeoutMs,
    required this.repeatable,
  });

  final _EventBody body;
  final _SseParser parser;
  final _Utf8Chunks decoder = _Utf8Chunks();
  final Queue<_SseEvent> queue = Queue();
  bool ended = false;
  bool oversize = false;
  final ResponseMeta meta;
  final String? key;
  final String keyHeader;
  final _OutcomeCheck? check;
  final _Secrets secrets;
  final int timeoutMs;
  final bool repeatable;
  int delivered = 0;
  int reconnects = 0;
  String? lastId;
  int? lastRetry;
  Set<String> deliveredIds = {};
  bool replaying = false;
}

/// The value of an `Accept` header that asks for an event stream: the
/// ranges of [value] (duplicates dropped) with `text/event-stream` first
/// unless one of them names it.
String _mergeAccept(String value) {
  final ranges = <String>[];
  for (final part in value.split(',')) {
    final range = part.trim();
    if (range.isNotEmpty && !ranges.contains(range)) ranges.add(range);
  }
  final names = ranges.any(
    (r) => r.split(';').first.trim().toLowerCase() == 'text/event-stream',
  );
  return (names ? ranges : ['text/event-stream', ...ranges]).join(', ');
}

/// Server-sent event streams: opening, reconnecting, accepting events and
/// the envelopes of a stream that broke.
extension _Streams on ClientCore {
  /// The stream's request flag set to true, in a merged body field or
  /// inside an object body argument.
  static Map<String, Object?> _withStreamFlag(
    OperationDescriptor op,
    Map<String, Object?> args,
  ) {
    final flag = op.stream?.flag;
    final body = op.body;
    if (flag == null || body == null) return args;
    final out = <String, Object?>{...args};
    final arg = body.arg;
    if (arg == null) {
      for (final (name, wire) in body.fields) {
        if (wire == flag || name == flag) {
          out[name] = true;
          break;
        }
      }
    } else {
      final inner = args[arg];
      if (inner is Map) {
        out[arg] = <String, Object?>{
          for (final e in inner.entries) '${e.key}': e.value,
          flag: true,
        };
      }
    }
    return out;
  }

  /// Sends the stream's request (with `Last-Event-ID` [resumeFrom] on a
  /// reconnect): the open connection, or why there is none. A reconnect
  /// carries the state of [previous] over.
  Future<(Err<Object?>?, _StreamOpen?)> _openStream(
    OperationDescriptor op,
    Map<String, Object?> args,
    CallOptions opts,
    String? resumeFrom,
    _StreamOpen? previous,
  ) async {
    final headers = <String, String>{};
    var accept = '';
    for (final (own, layer) in [(false, _headers), (true, opts.headers)]) {
      for (final e in layer.entries) {
        if (e.key.toLowerCase() == 'accept') {
          accept = e.value;
        } else if (own && e.key.toLowerCase() != 'last-event-id') {
          headers[e.key] = e.value;
        }
      }
    }
    headers['Accept'] = _mergeAccept(accept);
    if (resumeFrom != null) headers['Last-Event-ID'] = resumeFrom;
    final streamOpts = opts._with(headers: headers);
    Object? converted;
    try {
      converted = Json.from(Models.dump(args));
    } on ArgumentError {
      converted = null;
    }
    final flagged = converted is Map<String, Object?>
        ? _withStreamFlag(op, converted)
        : args;
    final _Prepared p;
    try {
      p = await _prepare(op, flagged, streamOpts, _Purpose.call, null, null);
    } on _Stop catch (s) {
      return (s.err, null);
    }
    final (result, streamed) = await _sendWith(p, streamOpts, true);
    if (streamed == null) {
      final answer = result!;
      if (answer is Err<Object?>) return (answer, null);
      final meta = (answer as Ok<Object?>).meta;
      final type = meta.headers['content-type'];
      final after =
          op.isMutation ? ' The call took effect; do not repeat it.' : '';
      return (
        _diag(op.id, 'UNEXPECTED_RESPONSE')
            .status(meta.status)
            .requestId(meta.requestId)
            .param('response')
            .received(type == null ? null : _Util.envelopeValue(type, false))
            .expected('a text/event-stream body')
            .remedy(
              'The success response is not an event stream (Content-Type ${type ?? 'missing'}).$after',
            )
            .retrying('never')
            .attempts(meta.attempts)
            .err<Object?>(),
        null,
      );
    }
    final protected = _Classify.replayProtection(op, p.key != null);
    final open = _StreamOpen(
      body: streamed.body,
      parser: _SseParser(maxEventBytes),
      meta: streamed.meta,
      key: p.key,
      keyHeader: p.keyHeader,
      check: op.isMutation && !protected ? _outcomeCheck(op, p.args) : null,
      secrets: p.secrets.copy(),
      timeoutMs: _attemptTimeout(streamOpts),
      repeatable: !op.isMutation || protected,
    );
    if (previous != null) {
      open
        ..delivered = previous.delivered
        ..reconnects = previous.reconnects + 1
        ..lastId = previous.lastId
        ..lastRetry = previous.lastRetry
        ..deliveredIds = previous.deliveredIds
        ..replaying = true;
    }
    return (null, open);
  }

  /// A dropped stream may be reconnected when the call is safe to repeat,
  /// reconnects are left, and an event id to resume from is known (or no
  /// event was delivered yet).
  bool _mayReconnect(_StreamOpen o) =>
      o.repeatable &&
      o.reconnects < maxReconnects &&
      (o.delivered == 0 ||
          (o.lastId != null && _Serialize.validHeaderValue(o.lastId!)));

  static String _plural(int n, String one, String many) => n == 1 ? one : many;

  /// The final failure of a stream that broke after the events delivered;
  /// [why] is `lost`, `idle`, `early_end`, or null for a silence past the
  /// attempt timeout.
  Err<Object?> _interrupted(
      OperationDescriptor op, _StreamOpen o, String? why) {
    final after =
        'after ${o.delivered} ${_plural(o.delivered, 'event', 'events')}';
    final retried = switch (o.reconnects) {
      0 => '',
      1 => ' (1 reconnect made)',
      _ => ' (${o.reconnects} reconnects made)',
    };
    final idle = why == 'idle';
    final status = o.meta.status;
    final ctx = _CallContext(
      api: _api,
      op: op,
      key: o.key,
      keyHeader: o.keyHeader,
      attempts: o.meta.attempts,
      check: o.check,
    );
    final Diagnostic d;
    if (why == null || idle) {
      final silence = idle
          ? 'No bytes arrived within $idleTimeoutMs ms $after$retried'
          : 'No event arrived within ${o.timeoutMs} ms $after';
      final option = idle ? 'idleTimeout' : 'timeout';
      final cause = '$silence; the stream was abandoned.';
      final code = idle ? 'STREAM_IDLE' : null;
      d = op.isMutation
          ? _Classify.outcomeUnknown(
              ctx,
              cause,
              httpStatus: status,
              code: code,
              requestId: o.meta.requestId,
            )
          : _diag(op.id, 'UPSTREAM_UNAVAILABLE')
              .status(status)
              .code(code)
              .requestId(o.meta.requestId)
              .remedy(
                '$cause This read has no side effects; call again later or with a larger $option.',
              )
              .attempts(o.meta.attempts)
              .build();
    } else {
      final cause =
          'The event stream was cut off $after$retried: the connection failed before it ended.';
      d = op.isMutation
          ? _Classify.outcomeUnknown(
              ctx,
              cause,
              httpStatus: status,
              requestId: o.meta.requestId,
            )
          : _diag(op.id, 'TRANSPORT_FAILED')
              .status(status)
              .requestId(o.meta.requestId)
              .remedy('$cause This read has no side effects; call again.')
              .attempts(o.meta.attempts)
              .build();
    }
    return Err<Object?>(_Util.scrubDiagnostic(d, o.secrets));
  }

  /// The failure of a collect that kept too many bytes of events ([bytes])
  /// or ran too long.
  Diagnostic _overBudget(OperationDescriptor op, bool bytes, int kept) {
    final what = bytes
        ? 'at most $maxCollectBytes bytes of events'
        : 'at most $maxCollectMs ms to collect the events';
    final after =
        op.isMutation ? ' The call took effect; do not repeat it.' : '';
    return _diag(op.id, 'UNEXPECTED_RESPONSE')
        .param('events')
        .expected(what)
        .remedy(
          'The stream was abandoned after $kept ${_plural(kept, 'event', 'events')} '
          '($what; maxCollectBytes and maxCollect).$after The events collected so far precede this error.',
        )
        .retrying('never')
        .build();
  }

  /// The failure of an event larger than [maxEventBytes].
  Err<Object?> _oversize(OperationDescriptor op, _StreamOpen o) {
    final limit = maxEventBytes;
    final after =
        op.isMutation ? ' The call took effect; do not repeat it.' : '';
    final d = _diag(op.id, 'UNEXPECTED_RESPONSE')
        .status(o.meta.status)
        .requestId(o.meta.requestId)
        .param('events[${o.delivered}]')
        .expected('an event of at most $limit bytes')
        .remedy(
          'Event ${o.delivered} of the stream is larger than the limit of $limit bytes (maxEventBytes); the stream was abandoned.$after '
          'Events before it were delivered. Raise maxEventBytes if the server sends events this large on purpose.',
        )
        .retrying('never')
        .attempts(o.meta.attempts)
        .build();
    return Err<Object?>(_Util.scrubDiagnostic(d, o.secrets));
  }

  /// Decodes, checks and numbers one event: its [Ok], null for a replayed
  /// event, or the [Err] that ends the stream.
  Result<StreamEvent<T>>? _acceptEvent<T>(
    OperationDescriptor op,
    _StreamOpen o,
    _SseEvent ev,
    T Function(Object? raw)? decode,
  ) {
    if (o.replaying) {
      if (ev.id != null && o.deliveredIds.contains(ev.id)) return null;
      o.replaying = false;
    }
    final index = o.delivered;
    final afterEffect =
        op.isMutation ? ' The call took effect; do not repeat it.' : '';
    Err<StreamEvent<T>> failure(_Diag b) => Err<StreamEvent<T>>(
          _Util.scrubDiagnostic(
            b
                .status(o.meta.status)
                .requestId(o.meta.requestId)
                .retrying('never')
                .attempts(o.meta.attempts)
                .build(),
            o.secrets,
          ),
        );
    final value = Json.tryParse(ev.data);
    if (identical(value, unset)) {
      return failure(
        _diag(op.id, 'UNEXPECTED_RESPONSE')
            .param('events[$index]')
            .received(_Util.envelopeValue(ev.data, false))
            .expected('JSON in the data of every event')
            .remedy(
              'Event $index of the stream (event "${ev.event}") does not carry JSON in its data.$afterEffect '
              'Events before it were delivered.',
            ),
      );
    }
    final schema = op.stream!.event;
    if (_validateResponses != ValidateResponses.off) {
      final issues = _judge(op.id, 'events[$index]', schema, value);
      if (issues.isNotEmpty) {
        final issue = issues.first;
        final pathText = Schema.issuePath(issue.path);
        final shown = _Util.redactPaths(value, op.sensitiveResponseFields);
        final received = _Util.getPath(shown, issue.path);
        final sensitive =
            issue.path.any((s) => s is String && _Util.looksSensitive(s));
        final problem = failure(
          _diag(op.id, 'UNEXPECTED_RESPONSE')
              .param('events[$index]$pathText')
              .received(_Util.envelopeValue(received, sensitive))
              .expected(issue.message)
              .remedy(
                'Event $index of the stream (event "${ev.event}") does not match the API description at '
                'events[$index]$pathText (${issue.message}).$afterEffect',
              ),
        );
        if (_validateResponses == ValidateResponses.strict) return problem;
        _emit(problem.error);
      }
    }
    final T typed;
    try {
      typed = decode != null ? decode(value) : value as T;
    } catch (e) {
      return failure(
        _diag(op.id, 'UNEXPECTED_RESPONSE')
            .param('events[$index]')
            .expected('an event the SDK can decode')
            .remedy(
              'The SDK could not build its typed value from event $index of the stream (event "${ev.event}", '
              '${e.runtimeType}).$afterEffect Events before it were delivered.',
            ),
      );
    }
    o.delivered++;
    final id = ev.id;
    if (id != null) {
      o.deliveredIds.add(id);
      o.lastId = id;
    }
    if (ev.retry != null) o.lastRetry = ev.retry;
    return Ok<StreamEvent<T>>(
      StreamEvent<T>(
        value: typed,
        raw: value,
        event: ev.event,
        id: o.lastId,
        retry: o.lastRetry,
        meta: o.meta,
        reconnects: o.reconnects,
      ),
      meta: o.meta,
      raw: value,
    );
  }
}
