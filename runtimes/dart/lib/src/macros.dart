// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// The budget of a macro's poll step without one, in milliseconds.
const int _defaultMacroBudgetMs = 30000;

/// The pages a macro's paginate step reads without a `max_pages`.
const int _macroPageLimit = 100;

const Map<String, int> _safetyRank = {
  'read_only': 0,
  'mutating': 1,
  'destructive': 2,
  'irreversible': 3,
};

/// A macro run's one-time spend of its confirmation token: the first step
/// about to send claims it, so a step refused before anything was sent
/// leaves the token valid for the corrected run.
final class _MacroClaim implements _Claim {
  _MacroClaim(this.name, this.token, this.bind);

  final String name;
  final String? token;
  final String? bind;
  bool _claimed = false;

  @override
  Err<Object?>? claim(ClientCore core) {
    final t = token;
    if (t == null || _claimed) return null;
    _claimed = true;
    return core._claimToken(name, t, bind, "the macro's preview(...)");
  }
}

/// Macros: steps in order, each one's result bound to its `as` name.
extension _Macros on ClientCore {
  /// A macro by descriptor or name, or why there is none.
  Object _resolveMacro(Object macro) {
    if (macro is MacroDescriptor) return macro;
    final m = descriptors.macros['$macro'];
    if (m != null) return m;
    final shown = '$macro'.isEmpty ? '<unknown macro>' : '$macro';
    return _diag(shown, 'VALIDATION_FAILED')
        .param('macro')
        .expected('a macro name of this SDK')
        .remedy(
            'This SDK has no macro $shown; check the name. Nothing was sent.')
        .err<Object?>();
  }

  static bool _usesKey(OperationDescriptor op) {
    final policy = op._idempotency['policy'];
    return policy == 'caller_owned' || policy == 'auto';
  }

  static _Stop _macroInvalid(String name, String remediation) => _Stop(
        _diag(name, 'VALIDATION_FAILED')
            .param('macro')
            .expected('a valid macro descriptor')
            .remedy(remediation)
            .err<Object?>(),
      );

  static _Stop _notAnObject(String name, String remediation) => _Stop(
        _diag(name, 'VALIDATION_FAILED')
            .param('input')
            .expected('an object')
            .remedy(remediation)
            .err<Object?>(),
      );

  /// The input keyed by the arguments layout keys, with the added fields'
  /// defaults; throws [_Stop].
  static Map<String, Object?> _macroInput(
    MacroDescriptor m,
    Map<String, Object?> input,
  ) {
    Object? converted;
    try {
      converted = Json.from(Models.dump(input));
    } on ArgumentError {
      converted = unset;
    }
    if (_Util.nestsDeeper(converted, _Util.maxArgDepth)) {
      throw _Stop(
        _diag(m.name, 'VALIDATION_FAILED')
            .param('input')
            .expected('a JSON value nested at most ${_Util.maxArgDepth} levels')
            .remedy('Pass the input of ${m.name} without such deep nesting.')
            .err<Object?>(),
      );
    }
    if (converted is! Map<String, Object?>) {
      throw _notAnObject(m.name, 'Pass the input of ${m.name} as one object.');
    }
    final keys = {for (final (key, name, _) in m.inputFields) name: key};
    final effective = <String, Object?>{
      for (final e in converted.entries) keys[e.key] ?? e.key: e.value,
    };
    m.added.forEach((key, schema) {
      if (!effective.containsKey(key) &&
          schema is Map &&
          schema.containsKey('default')) {
        effective[key] = schema['default'];
      }
    });
    return effective;
  }

  /// The steps with their operations, and the strictest safety of the
  /// macro and its steps; throws [_Stop].
  (List<(Map<String, Object?>, OperationDescriptor)>, String) _macroPlan(
    MacroDescriptor m,
  ) {
    var safety = _safetyRank.containsKey(m.safety) ? m.safety : 'irreversible';
    final steps = <(Map<String, Object?>, OperationDescriptor)>[];
    for (var i = 0; i < m.steps.length; i++) {
      final step = m.steps[i];
      final op = operation(_str(step['operation']));
      if (op == null) {
        throw _macroInvalid(
          m.name,
          'Step ${i + 1} of ${m.name} names ${step['operation']}, which is not registered with the client.',
        );
      }
      if ((_safetyRank[op.safety] ?? 3) > (_safetyRank[safety] ?? 3)) {
        safety = op.safety;
      }
      steps.add((step, op));
    }
    return (steps, safety);
  }

  /// A step's arguments keyed by the step operation's argument names (a
  /// dry evaluation when [pending] names results not produced yet).
  static Object? _stepArgs(
    MacroDescriptor m,
    Map<String, Object?> step,
    OperationDescriptor op,
    Map<String, Object?> scope,
    Set<String>? pending,
  ) {
    final source = step['args'] ?? <String, Object?>{};
    var evaluated = pending != null
        ? Expr.evaluateDry(source, scope, pending)
        : Expr.evaluate(source, scope);
    if (identical(evaluated, unset) || evaluated == null) {
      evaluated = <String, Object?>{};
    }
    evaluated = Json.copy(evaluated);
    if (evaluated is! Map) return evaluated;
    final added = m.added.keys.toSet();
    final extending = op.id == m.extendsOperation;
    return <String, Object?>{
      for (final e in evaluated.entries)
        if (!extending || !added.contains('${e.key}'))
          op.nameOfKey('${e.key}'): e.value,
    };
  }

  static String _kind(Map<String, Object?> step) {
    final kind = step['kind'];
    return kind == 'poll' || kind == 'paginate' ? kind! as String : 'call';
  }

  static int _pageLimit(Map<String, Object?> step) =>
      _Util.bounded(step['max_pages'], _macroPageLimit, 1, 10000).floor();

  static (num, num) _pollNumbers(
    Map<String, Object?> step,
    Map<String, Object?> scope,
  ) =>
      (
        _Util.bounded(
          Expr.evaluate(step['budget_ms'], scope),
          _defaultMacroBudgetMs,
          0,
          _Util.maxSafeInteger,
        ),
        _Util.bounded(
          step['interval_ms'],
          ClientCore.defaultPollIntervalMs,
          0,
          _Util.maxSafeInteger,
        ),
      );

  /// A URL rendered from a dry evaluation, with the placeholders of
  /// pending results shown as written rather than percent-encoded.
  static String _unescapePlaceholders(String url, Object? args) {
    final found = <String>[];
    _Helpers.placeholdersIn(args, found);
    var out = url;
    for (final text in found.toSet()) {
      out = out.replaceAll(_Serialize.encodeComponent(text), text);
    }
    return out;
  }

  /// Previews a macro without sending anything: validates and renders the
  /// first step, renders every later step whose arguments are known, lists
  /// every step's effects, and issues a confirmation token bound to the
  /// macro and this exact input unless the macro is read-only.
  Future<Result<Object?>> _previewMacroWith(
    MacroDescriptor m,
    Map<String, Object?> input,
    CallOptions opts,
  ) async {
    final name = m.name;
    final (steps, safety) = _macroPlan(m);
    final effective = _macroInput(m, input);
    if (steps.isEmpty) {
      throw _macroInvalid(name, '$name has no steps; regenerate the SDK.');
    }
    final rest = opts._with(clearConfirm: true, verify: false);
    final scope = <String, Object?>{'input': effective};
    final pending = <String>{};
    final previews = <MacroStepPreview>[];
    final effects = <String>[];
    if (m.summary.isNotEmpty) effects.add(m.summary);
    var keyUsed = false;
    for (var index = 0; index < steps.length; index++) {
      final (step, op) = steps[index];
      final where = 'step ${index + 1} of $name: ${op.id}';
      final args = _stepArgs(m, step, op, scope, pending);
      var stepOpts = rest;
      if (!_usesKey(op) || keyUsed) {
        stepOpts = stepOpts._with(clearKey: true);
      } else if (opts._hasKey) {
        keyUsed = true;
      }
      RenderedRequest? request;
      if (args is Map<String, Object?>) {
        final encoding = op.body?.encoding;
        // Bytes and multipart bodies cannot be encoded from a value not
        // known yet.
        final unrenderable = (encoding == 'bytes' || encoding == 'multipart') &&
            _Helpers.isPlaceholder(args);
        if (!unrenderable) {
          final _Prepared p;
          try {
            p = await _prepare(
                op, args, stepOpts, _Purpose.macroPreview, null, null);
          } on _Stop catch (s) {
            final d = s.err.error.copyWith(
              operation: name,
              remediation: '${s.err.error.remediation} ($where)',
            );
            return Err<Object?>(d, partial: s.err.partial);
          }
          request = RenderedRequest(
            method: p.method,
            url: _unescapePlaceholders(p.displayUrl, args),
            headers: p.headers.toMap(true),
            body: p.body.empty ? null : p.body.display,
          );
        }
      } else if (!_Helpers.isPlaceholder(args)) {
        throw _notAnObject(
          name,
          'Step ${index + 1} of $name does not evaluate to an argument object.',
        );
      }
      final kind = _kind(step);
      final own = <String>[];
      final conf = op.agent['confirmation'];
      if (conf is Map && _str(conf['message']).isNotEmpty) {
        own.add(
          _Helpers.interpolate(
            conf['message'],
            args is Map<String, Object?> ? args : const {},
            op,
          ),
        );
      }
      if (kind == 'poll') {
        final until = step['until'] is Map ? Expr.describe(step['until']) : '';
        final (budget, interval) = _pollNumbers(step, scope);
        final untilText = until.isEmpty ? '' : ' until $until';
        own.add(
          'Repeats ${op.id} every ${Json.numberText(interval)} ms$untilText, for at most ${Json.numberText(budget)} ms.',
        );
      } else if (kind == 'paginate') {
        own.add('Reads up to ${_pageLimit(step)} pages of ${op.id}.');
      }
      final note = _str(op.agent['remediation_note']);
      if (note.isNotEmpty) own.add(note);
      effects
        ..add('Step ${index + 1}: $kind ${op.id} (${op.safety}).')
        ..addAll(own);
      final as = _str(step['as']).isEmpty ? null : _str(step['as']);
      previews.add(
        MacroStepPreview(
          step: index + 1,
          kind: kind,
          operation: op.id,
          as: as,
          safety: op.safety,
          request: request,
          effects: own,
        ),
      );
      if (as != null) pending.add(as);
    }
    if (m.shownOnce) {
      effects.add(
        'The result contains values the API shows only once; store them immediately.',
      );
    }
    final first = previews.first.request;
    if (first == null) {
      throw _notAnObject(name, 'Step 1 of $name cannot be rendered.');
    }
    final mutating = safety != 'read_only';
    return Ok<Object?>(
      PreviewResult(
        operation: name,
        safety: safety,
        request: first,
        effects: effects,
        confirmationToken: mutating
            ? _Keys.issueToken(_confirmationKey, 'macro:$name', effective, now)
            : null,
        expiresInMs: mutating ? _Keys.ttlMs : null,
        steps: previews,
      ),
      meta: ResponseMeta.local,
    );
  }

  /// Whether running the step again (by rerunning its macro with the same
  /// input and options) answers the first result instead of applying the
  /// effect twice.
  static bool _rerunProtected(OperationDescriptor op, CallOptions opts) {
    final policy = op._idempotency['policy'];
    if (policy == 'content_identity' ||
        policy == 'content_hash' ||
        policy == 'auto') {
      return true;
    }
    if (!opts._hasKey) return false;
    return policy == 'caller_owned' ||
        op.params.any((p) => p.role == 'idempotency_key');
  }

  /// The replay protection a macro's token is bound to: the run's key when
  /// a step sends it and every mutating step is protected against a rerun;
  /// otherwise none, so the token is used once.
  static String? _macroBind(
    List<(Map<String, Object?>, OperationDescriptor)> steps,
    CallOptions opts,
  ) {
    if (!opts._hasKey) return null;
    var sent = false;
    for (final (_, op) in steps) {
      var stepOpts = opts;
      if (!_usesKey(op) || sent) {
        stepOpts = opts._with(clearKey: true);
      } else {
        sent = true;
      }
      if (op.isMutation && !_rerunProtected(op, stepOpts)) return null;
    }
    return sent ? opts.idempotencyKey : null;
  }

  /// Runs a macro: an [Ok] with its evaluated output, or the failing
  /// step's envelope under the macro's name, with the completed steps'
  /// results by `as` name as the partial.
  Future<Result<Object?>> _runMacroWith(
    MacroDescriptor m,
    Map<String, Object?> input,
    CallOptions opts,
  ) async {
    final name = m.name;
    final (steps, safety) = _macroPlan(m);
    final effective = _macroInput(m, input);
    final refused = _confirmed(
      name,
      'macro:$name',
      "the macro's preview(...)",
      safety,
      effective,
      opts,
    );
    if (refused != null) return refused;
    final claim = _MacroClaim(name, opts._token, _macroBind(steps, opts));
    final scope = <String, Object?>{'input': effective};
    final completed = <String>[];
    final produced = <String, Object?>{};
    final unprotected = <String>[];
    var keyUsed = false;
    var meta = ResponseMeta.local;
    for (var index = 0; index < steps.length; index++) {
      final (step, op) = steps[index];
      final args = _stepArgs(m, step, op, scope, null);
      if (args is! Map<String, Object?>) {
        throw _notAnObject(
          name,
          'Step ${index + 1} of $name does not evaluate to an argument object.',
        );
      }
      var stepOpts = opts._with(clearConfirm: true, verify: false);
      if (!_usesKey(op) || keyUsed) {
        stepOpts = stepOpts._with(clearKey: true);
      } else if (opts._hasKey) {
        keyUsed = true;
      }
      Object? value = unset;
      Err<Object?>? failure;
      switch (_kind(step)) {
        case 'poll':
          final (budget, interval) = _pollNumbers(step, scope);
          final until =
              step['until'] is Map ? step['until'] : <String, Object?>{};
          final polled = await _guard(
            op.id,
            () => _pollWith(
              op,
              args,
              until,
              interval.floor(),
              budget.floor(),
              stepOpts,
              claim,
            ),
          );
          if (polled is Err<Object?>) {
            failure = polled;
          } else {
            final ok = polled as Ok<Object?>;
            final poll = ok.value! as PollResult;
            value = poll.timedOut ? null : poll.body;
            meta = ok.meta;
          }
        case 'paginate':
          final items = <Object?>[];
          final limit = _pageLimit(step);
          var pages = 0;
          final state = _PageState.of(args);
          while (true) {
            final page = await _pageStep(op, state, stepOpts, claim);
            if (page == null) break;
            if (page is Err<Page<Object?>>) {
              failure = page.cast<Object?>();
              break;
            }
            final ok = page as Ok<Page<Object?>>;
            items.addAll(ok.value.rawItems);
            meta = ok.meta;
            pages++;
            if (pages >= limit) break;
          }
          value = items;
        default:
          final response = await _guard(
            op.id,
            () => _callWith(op, args, stepOpts, null, claim),
          );
          if (response is Err<Object?>) {
            failure = response;
          } else {
            final ok = response as Ok<Object?>;
            value = ok.raw ?? unset;
            meta = ok.meta;
          }
      }
      final as = _str(step['as']).isEmpty ? null : _str(step['as']);
      if (failure != null) {
        final d = failure.error;
        if (failure.hasPartial && as != null) produced[as] = failure.partial;
        final done = completed.isEmpty
            ? 'no step completed before it'
            : 'completed before it: ${completed.join(', ')}';
        var remediation =
            '${d.remediation} Macro $name stopped at step ${index + 1} of ${steps.length} (${op.id}); $done.';
        var retryable = d.retryable;
        var nextAction = d.nextAction;
        if (produced.isNotEmpty) {
          final once = m.shownOnce
              ? ', including values the API shows only once: store them now'
              : '';
          remediation +=
              " The result's partial holds the completed steps' results (${produced.keys.join(', ')})$once.";
        }
        if (unprotected.isNotEmpty) {
          // Rerunning the macro would apply these steps again (a second
          // endpoint, a lost one-time secret): finish from here instead.
          remediation +=
              ' Do not run $name again: ${unprotected.join(', ')} already took effect and has no replay protection.';
          if (retryable == 'after_delay' || retryable == 'same_key_only') {
            retryable = 'after_remediation';
          }
          final finish =
              "Finish $name without rerunning it: call ${op.id} yourself with the values from the result's partial.";
          nextAction = nextAction == null ? finish : '$nextAction $finish';
        }
        return Err<Object?>(
          d.copyWith(
            operation: name,
            remediation: remediation,
            retryable: retryable,
            nextAction: nextAction,
          ),
          partial: produced.isEmpty ? unset : {...produced},
        );
      }
      completed.add(op.id);
      if (as != null) {
        if (identical(value, unset)) {
          scope.remove(as);
          produced[as] = null;
        } else {
          scope[as] = value;
          produced[as] = value;
        }
      }
      if (op.isMutation && !_rerunProtected(op, stepOpts)) {
        unprotected.add(op.id);
      }
    }
    var output = Expr.evaluate(m.output, scope);
    if (identical(output, unset)) output = null;
    return Ok<Object?>(output, meta: meta, raw: output);
  }
}
