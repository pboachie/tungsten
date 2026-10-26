// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// A pre-flight failure inside the pipeline: the call ends with [err] and
/// nothing is sent. It never leaves the runtime.
final class _Stop implements Exception {
  const _Stop(this.err);

  final Err<Object?> err;
}

/// Why a call is being prepared.
enum _Purpose { call, preview, serverPreview, macroPreview }

/// A claim on the confirmation token of a macro: the first step about to
/// send claims it ([ClientCore] calls [claim] once per send).
abstract interface class _Claim {
  /// Null when the step may be sent, else why not.
  Err<Object?>? claim(ClientCore core);
}

/// A request ready to send, with its redacted rendering.
final class _Prepared {
  _Prepared({
    required this.op,
    required this.args,
    required this.method,
    required this.url,
    required this.displayUrl,
    required this.headers,
    required this.body,
    required this.key,
    required this.keyHeader,
    required this.authQuery,
    required this.hiddenQuery,
    required this.secrets,
    required this.oauth,
  });

  final OperationDescriptor op;
  final Map<String, Object?> args;
  final String method;
  final String url;
  final String displayUrl;
  final _HeaderBag headers;
  final _Encoded body;
  final String? key;
  final String keyHeader;
  final List<(String, String)> authQuery;
  final List<String> hiddenQuery;
  final _Secrets secrets;
  String? oauth;
  _PageState? pageState;
}

/// An event stream answer handed back unread by the sending loop.
final class _Streamed {
  const _Streamed(this.meta, this.body);

  final ResponseMeta meta;
  final _EventBody body;
}

/// What a poll ends with: the last body ([unset] without one) and whether
/// the budget ran out before the condition held.
final class PollResult {
  /// A poll result.
  const PollResult({required this.body, required this.timedOut});

  /// The last body, or [unset].
  final Object? body;

  /// Whether the budget ran out first.
  final bool timedOut;

  /// The result as JSON.
  Map<String, Object?> toJson() => {
        'body': identical(body, unset) ? null : body,
        'timed_out': timedOut,
      };
}

/// The engine behind every generated client. Each call runs one pipeline:
/// pre-flight validation of the arguments (nothing is sent on failure),
/// auth, idempotency keys, the confirmation rules of destructive and
/// irreversible operations, sending with tier-aware retries, and the
/// classification of the answer (or of a lost request) into [Ok] or [Err]
/// with the diagnostic envelope. On top of it: previews with confirmation
/// tokens, verification hooks and polling. Nothing throws for an API or
/// transport failure; only invalid client options do
/// ([ClientConfigException]).
final class ClientCore {
  /// A client over an SDK descriptor document. Throws
  /// [ClientConfigException] for a base URL that is not an absolute
  /// http(s) URL without credentials, or an unknown redirect policy.
  ClientCore(this.descriptors, [ClientOptions options = const ClientOptions()])
      : _api = descriptors.api,
        _allowInsecureHttp = options.allowInsecureHttp,
        _auth = {...options.auth},
        _headers = {...options.headers},
        _retries = options.retries,
        _store = options.idempotencyStore ?? MemoryIdempotencyStore(),
        _middleware = [...options.middleware],
        _validateResponses = options.validateResponses,
        _onDiagnostic = options.onDiagnostic,
        _confirmationKey = _configuredConfirmationKey(options.confirmationKey),
        _nowFn = options.now,
        _randomFn = options.random,
        _tokenStore = options.tokenStore ?? MemoryTokenStore(),
        _maxOAuthTokenResponseBytes = options.maxOAuthTokenResponseBytes ??
            defaultMaxOAuthTokenResponseBytes,
        _timeoutMs = _positiveMs(options.timeout) ?? defaultTimeoutMs,
        maxResponseBytes = options.maxResponseBytes ?? 4 * 1024 * 1024,
        maxEventBytes = _positive(options.maxEventBytes) ?? 1024 * 1024,
        maxCollectBytes = _positive(options.maxCollectBytes) ?? 4 * 1024 * 1024,
        maxCollectMs = _positiveMs(options.maxCollect) ?? 60000,
        maxReconnects = (options.maxReconnects ?? 3).clamp(0, 1000),
        reconnectMaxMs = _positiveMs(options.reconnectMax) ?? 30000,
        idleTimeoutMs = _positiveMs(options.idleTimeout) {
    final policy = _api['redirects'];
    if (policy != null && policy != 'same_origin_read' && policy != 'none') {
      throw const ClientConfigException(
        'redirects must be same_origin_read or none',
      );
    }
    _redirectsDisabled =
        policy == 'none' || options.redirects == RedirectPolicy.none;
    var base = options.baseUrl ?? '';
    if (base.isEmpty) {
      final servers = _list(_api['servers']);
      base = servers.isEmpty ? '' : _str(servers.first);
    }
    if (base.isNotEmpty && !_validBaseUrl(base)) {
      throw const ClientConfigException(
        'the base URL is not a valid absolute http(s) URL without credentials',
      );
    }
    baseUrl = base;
    _registry.addAll(descriptors.operations);
  }

  /// Generated clients call this when the API forbids redirects, so they
  /// build only against a runtime that honours the policy.
  static void requireRedirectPolicyV1() {}

  static List<int> _configuredConfirmationKey(List<int>? key) {
    if (key == null) return _Util.randomBytes(32);
    if (key.length < 32 || key.any((byte) => byte < 0 || byte > 255)) {
      throw const ClientConfigException(
        'confirmationKey must contain at least 32 bytes; omit it to generate a random key',
      );
    }
    return [...key];
  }

  /// Credential-bearing HTTP requests require an explicit opt-in off loopback.
  static const _insecureHttpAuth =
      'Refusing to send credentials over non-loopback HTTP. Use HTTPS, or set ClientOptions.allowInsecureHttp = true for intentional insecure transport. Nothing was sent.';
  final bool _allowInsecureHttp;

  bool _credentialHeader(String name, [bool preservedIdempotency = false]) {
    if ((!preservedIdempotency && _Util.looksSensitive(name)) ||
        ['authorization', 'proxy-authorization', 'cookie']
            .contains(name.toLowerCase())) return true;
    final schemes = _api['auth'];
    return schemes is List &&
        schemes.whereType<Map<String, Object?>>().any((s) =>
            (s['kind'] == 'api_key' &&
                s['in'] == 'header' &&
                _str(s['wire']).toLowerCase() == name.toLowerCase()) ||
            (s['kind'] == 'composite' &&
                s['parts'] is List &&
                (s['parts'] as List).whereType<Map<String, Object?>>().any(
                    (p) =>
                        p['kind'] == 'header' &&
                        p['from_config'] == null &&
                        _str(p['name']).toLowerCase() == name.toLowerCase())));
  }

  bool _insecureHttp(String target) {
    if (_allowInsecureHttp) return false;
    final url = Uri.tryParse(target);
    if (url == null || url.scheme.toLowerCase() != 'http') return false;
    final host = url.host.toLowerCase();
    if (host == 'localhost' || host == 'localhost.') return false;
    final ip = InternetAddress.tryParse(host);
    return ip == null ||
        (ip.type == InternetAddressType.IPv4
            ? ip.rawAddress.first != 127
            : !ip.rawAddress.take(15).every((b) => b == 0) ||
                ip.rawAddress.last != 1);
  }

  /// Same-origin redirects one read follows.
  static const int maxRedirects = 5;

  /// The per-attempt timeout when none is configured, in milliseconds.
  static const int defaultTimeoutMs = 30000;

  /// The interval of polls without one, in milliseconds.
  static const int defaultPollIntervalMs = 1000;

  /// The budget of verification polls without one, in milliseconds.
  static const int defaultVerifyBudgetMs = 30000;

  static const List<String> _retryableCategories = [
    'RATE_LIMITED',
    'UPSTREAM_UNAVAILABLE',
    'TRANSPORT_FAILED',
    'OUTCOME_UNKNOWN',
  ];

  /// The SDK descriptor document.
  final SdkDescriptors descriptors;

  /// The base URL requests go to (empty when none is configured).
  late final String baseUrl;

  /// Maximum ordinary response bytes; omitted options inherit 4 MiB.
  final int? maxResponseBytes;

  /// The largest event of a stream, in UTF-8 bytes.
  final int maxEventBytes;

  /// The most event JSON a collect keeps, in UTF-8 bytes.
  final int maxCollectBytes;

  /// The longest a collect runs, in milliseconds.
  final int maxCollectMs;

  /// How often a dropped stream is reconnected.
  final int maxReconnects;

  /// The cap of the wait before a reconnect, in milliseconds.
  final int reconnectMaxMs;

  /// The silence after which a stream is idle, in milliseconds (none when
  /// null).
  final int? idleTimeoutMs;

  final Map<String, Object?> _api;
  final Map<String, Credential> _auth;
  final Map<String, String> _headers;
  final RetryOptions? _retries;
  final IdempotencyStore _store;
  final List<Middleware> _middleware;
  final ValidateResponses _validateResponses;
  final void Function(Diagnostic diagnostic)? _onDiagnostic;
  final List<int> _confirmationKey;
  final int Function()? _nowFn;
  final double Function()? _randomFn;
  final TokenStore _tokenStore;
  final int _maxOAuthTokenResponseBytes;
  final int _timeoutMs;
  late final bool _redirectsDisabled;
  final Map<String, OperationDescriptor> _registry = {};
  final _Lock _keyLock = _Lock();
  final Map<String, (String?, int)> _usedTokens = {};
  final Map<String, (String, String, int)> _ccTokens = {};
  final Map<String, Future<(String?, String?)>> _ccFlights = {};
  final Map<String, _OAuthPending> _oauthAttempts = {};
  final Map<String, _OAuthVersion> _oauthVersions = {};
  final Set<_OAuthOwner> _oauthOwners = {};
  final Map<(String, int, int), _OAuthRefresh> _oauthRefreshes = {};
  int _oauthWaiters = 0;
  static final math.Random _jitter = math.Random();

  static int? _positive(int? value) =>
      value != null && value > 0 ? value : null;

  static int? _positiveMs(Duration? value) =>
      value != null && value.inMilliseconds > 0 ? value.inMilliseconds : null;

  /// The API descriptor (the document's `api`).
  Map<String, Object?> get api => _api;

  /// The per-attempt timeout, in milliseconds.
  int get timeoutMs => _timeoutMs;

  /// The store of authorization-code tokens.
  TokenStore get tokenStore => _tokenStore;

  /// A registered operation by id, or null.
  OperationDescriptor? operation(String id) => _registry[id];

  /// Makes operations resolvable by id (verification hooks, endpoint
  /// previews, macro steps).
  void register(Iterable<OperationDescriptor> ops) {
    for (final op in ops) {
      if (op.id.isNotEmpty) _registry.putIfAbsent(op.id, () => op);
    }
  }

  /// The authorization-code helpers of an OAuth2 scheme.
  OAuthFlow oauth(String scheme) => OAuthFlow._(this, scheme);

  /// Milliseconds since the epoch (the clock of [ClientOptions.now]).
  int get now => _nowFn?.call() ?? DateTime.now().millisecondsSinceEpoch;

  /// Calls an operation ([operation] an id or an [OperationDescriptor])
  /// with arguments keyed by the SDK's argument names. The value of an
  /// [Ok] is what [decode] makes of the wire value ([Ok.raw]); without a
  /// decoder it is the wire value itself.
  Future<Result<T>> call<T>(
    Object operation,
    Map<String, Object?> args, {
    CallOptions options = const CallOptions(),
    T Function(Object? raw)? decode,
  }) async {
    final op = _resolve(operation);
    if (op is Err<Object?>) return op.cast<T>();
    final d = op as OperationDescriptor;
    final result = await _guard(
      d.id,
      () => _callWith(d, args, options, null, null),
    );
    return _typed(result, d.id, decode);
  }

  /// Renders the request without sending it (or runs the operation's
  /// header or endpoint preview). A mutating operation gets a confirmation
  /// token bound to these exact arguments for five minutes.
  Future<Result<PreviewResult>> preview(
    Object operation,
    Map<String, Object?> args, {
    CallOptions options = const CallOptions(),
  }) async {
    final op = _resolve(operation);
    if (op is Err<Object?>) return op.cast<PreviewResult>();
    final d = op as OperationDescriptor;
    final result = await _guard(d.id, () => _previewWith(d, args, options));
    return switch (result) {
      Err<Object?>() => result.cast<PreviewResult>(),
      Ok<Object?>() => result.withValue(result.value! as PreviewResult),
    };
  }

  /// Calls a read operation until [until] holds or [budget] runs out.
  Future<Result<PollResult>> poll(
    Object operation,
    Map<String, Object?> args, {
    Object? until,
    Duration interval = const Duration(milliseconds: defaultPollIntervalMs),
    Duration budget = const Duration(milliseconds: defaultVerifyBudgetMs),
    CallOptions options = const CallOptions(),
  }) async {
    final op = _resolve(operation);
    if (op is Err<Object?>) return op.cast<PollResult>();
    final d = op as OperationDescriptor;
    final result = await _guard(
      d.id,
      () => _pollWith(
        d,
        args,
        until,
        interval.inMilliseconds,
        budget.inMilliseconds,
        options,
        null,
      ),
    );
    return switch (result) {
      Err<Object?>() => result.cast<PollResult>(),
      Ok<Object?>() => result.withValue(result.value! as PollResult),
    };
  }

  // ------------------------------------------------------------ helpers

  Object _resolve(Object operation) {
    if (operation is OperationDescriptor) return operation;
    final id = '$operation';
    final d = _registry[id];
    if (d != null) return d;
    final shown = id.isEmpty ? '<unknown operation>' : id;
    return _diag(shown, 'VALIDATION_FAILED')
        .param('operation')
        .expected('an operation id of this SDK')
        .remedy(
          'This SDK has no operation $shown; check the id. Nothing was sent.',
        )
        .err<Object?>();
  }

  /// Runs a pipeline: a pre-flight stop is its Err; an unexpected failure
  /// (a bug) is UNEXPECTED_RESPONSE instead of an exception.
  Future<Result<Object?>> _guard(
    String id,
    Future<Result<Object?>> Function() body,
  ) async {
    try {
      return await body();
    } on _Stop catch (s) {
      return s.err;
    } catch (_) {
      return _diag(id, 'UNEXPECTED_RESPONSE')
          .retrying('never')
          .remedy(
            'The SDK failed while building or sending the request. This is a bug in the generated SDK or the runtime; report it. '
            'If the call may have been sent, check its effect before repeating it.',
          )
          .err<Object?>();
    }
  }

  Result<T> _typed<T>(
    Result<Object?> result,
    String id,
    T Function(Object? raw)? decode,
  ) {
    if (decode != null) return Typed.result(result, decode, operation: id);
    return switch (result) {
      Err<Object?>() => result.cast<T>(),
      Ok<Object?>() => result.value is T || (result.value == null && null is T)
          ? Ok<T>(
              result.value as T,
              meta: result.meta,
              raw: result.raw,
              verification: result.verification,
            )
          : Typed.result(result, (raw) => raw as T, operation: id),
    };
  }

  static bool _validBaseUrl(String base) {
    final u = Uri.tryParse(base);
    return u != null &&
        (u.scheme == 'http' || u.scheme == 'https') &&
        u.host.isNotEmpty &&
        u.userInfo.isEmpty;
  }

  double _random() {
    if (_randomFn == null) return _jitter.nextDouble();
    return _Util.bounded(_randomFn(), 0.5, 0, 1).toDouble();
  }

  /// The retry policy of one operation: the runtime defaults, then the
  /// API's defaults for the operation's tier, then [ClientOptions.retries],
  /// field by field.
  ({int max, double baseMs, double maxMs, String jitter, bool honor})
      _retryOptions(OperationDescriptor op) {
    var max = 3;
    var baseMs = 200.0;
    var maxMs = 5000.0;
    var jitter = 'full';
    var honor = true;
    final tiers = _api['retries'];
    if (tiers is Map) {
      final l = tiers[op.isMutation ? 'mutating' : 'read_only'];
      if (l is Map) {
        max = _Util.bounded(l['max'], max, 0, 100).floor();
        baseMs = _Util.bounded(
          l['base_ms'],
          baseMs,
          0,
          _Util.maxSafeInteger,
        ).toDouble();
        maxMs = _Util.bounded(
          l['max_ms'],
          maxMs,
          0,
          _Util.maxSafeInteger,
        ).toDouble();
        final j = l['jitter'];
        if (j is String && j.isNotEmpty) jitter = j;
        final h = l['honor_retry_after'];
        if (h is bool) honor = h;
      }
    }
    final r = _retries;
    if (r != null) {
      if (r.max != null) max = r.max!.clamp(0, 100);
      if (r.base != null) baseMs = r.base!.inMicroseconds / 1000;
      if (r.maxDelay != null) maxMs = r.maxDelay!.inMicroseconds / 1000;
      if (r.jitter != null) jitter = r.jitter!.name;
      if (r.honorRetryAfter != null) honor = r.honorRetryAfter!;
    }
    return (
      max: max,
      baseMs: baseMs,
      maxMs: maxMs,
      jitter: jitter,
      honor: honor,
    );
  }

  /// The per-attempt timeout (at least one millisecond).
  int _attemptTimeout(CallOptions opts) {
    final t = opts.timeout;
    final ms =
        t != null && t.inMilliseconds > 0 ? t.inMilliseconds : _timeoutMs;
    return math.max(ms, 1);
  }

  /// Reports a warning; a failing observer never changes the call.
  void _emit(Diagnostic diagnostic) {
    try {
      _onDiagnostic?.call(diagnostic);
    } catch (_) {
      // Observers cannot change the call.
    }
  }

  void _onRequest(RequestContext ctx) {
    for (final m in _middleware) {
      try {
        m.onRequest?.call(ctx);
      } catch (_) {
        // Observers cannot change the call.
      }
    }
  }

  void _onResponse(RequestContext ctx, ResponseContext response) {
    for (final m in _middleware) {
      try {
        m.onResponse?.call(ctx, response);
      } catch (_) {
        // Observers cannot change the call.
      }
    }
  }

  void _onRetry(RequestContext ctx, Diagnostic reason) {
    for (final m in _middleware) {
      try {
        m.onRetry?.call(ctx, reason);
      } catch (_) {
        // Observers cannot change the call.
      }
    }
  }

  // ----------------------------------------------------------- pipeline

  /// The pipeline shared by calls, pages, polls and macro steps.
  Future<Result<Object?>> _callWith(
    OperationDescriptor op,
    Map<String, Object?> args,
    CallOptions opts,
    String? urlOverride,
    _Claim? claim, [
    _PageState? pageState,
  ]) async {
    final p = await _prepare(op, args, opts, _Purpose.call, urlOverride, claim);
    p.pageState = pageState;
    if (pageState?.seenUrls.contains(_PageState.urlKey(p.url)) ?? false) {
      return _PageState.repeated(op.id);
    }
    var result = await _sendRequest(p, opts);
    if (result is Err<Object?> &&
        result.error.httpStatus == 401 &&
        p.oauth != null) {
      result = await _resendRejected(p, result, opts);
    }
    if (result is Err<Object?>) return result;
    final ok = result as Ok<Object?>;
    if (opts.verify && op.agent['verify'] is Map) {
      final v = await _verify(op, p.args, ok.raw ?? unset, opts);
      return ok.withVerification(v);
    }
    return ok;
  }

  /// Sends a request whose stored OAuth2 token was rejected (401) once more
  /// with the refreshed token.
  Future<Result<Object?>> _resendRejected(
    _Prepared p,
    Err<Object?> rejected,
    CallOptions opts,
  ) async {
    final scheme = p.oauth!;
    p.oauth = null;
    final current = p.headers.get('Authorization');
    if (current == null || !current.startsWith('Bearer ')) return rejected;
    final token = await _refreshAfterRejection(scheme, current.substring(7));
    if (token == null) return rejected;
    p.headers.set('Authorization', 'Bearer $token', true);
    p.secrets.add(token);
    return _sendRequest(p, opts);
  }

  _Stop _validation(
    OperationDescriptor op,
    List<Object> path,
    Object? value,
    String expected,
    String remediation,
  ) {
    final shown =
        identical(value, unset) ? null : _Helpers.redactBelow(op, path, value);
    final received = _Util.envelopeValue(
      shown,
      _Helpers.sensitiveArg(op, path),
    );
    return _Stop(
      _diag(op.id, 'VALIDATION_FAILED')
          .param(_Helpers.argumentPath(op, path))
          .received(received)
          .expected(expected)
          .remedy(remediation)
          .err<Object?>(),
    );
  }

  void _validateArgs(
    OperationDescriptor op,
    Map<String, Object?> args,
    bool placeholders,
  ) {
    final params = op.argumentParams;
    for (final p in params) {
      if (p.required && args[p.name] == null) {
        throw _validation(
          op,
          [p.name],
          args.containsKey(p.name) ? args[p.name] : unset,
          'a value for the required ${p.location} parameter ${p.wire}',
          'Pass ${p.name}; ${op.id} cannot be called without it.',
        );
      }
    }
    final body = op.body;
    if (body != null &&
        body.required &&
        body.arg != null &&
        !args.containsKey(body.arg)) {
      throw _validation(
        op,
        [body.arg!],
        unset,
        'a request body',
        'Pass the request body as ${body.arg}.',
      );
    }
    final request = op.request;
    if (request != null) {
      final List<Issue> issues;
      final notes = <UncheckedPattern>[];
      try {
        issues = request.validate(args, onUnchecked: notes.add);
      } catch (_) {
        throw _Stop(
          _diag(op.id, 'UNEXPECTED_RESPONSE')
              .retrying('never')
              .remedy(
                'The SDK failed while building or sending the request (the request validator failed). '
                'This is a bug in the generated SDK or the runtime; report it. Nothing was sent.',
              )
              .err<Object?>(),
        );
      }
      _reportUnchecked(op.id, 'args', notes, 'VALIDATION_FAILED');
      if (issues.isEmpty) return;
      Issue? issue;
      if (placeholders) {
        for (final i in issues) {
          var pending = false;
          for (var n = 1; n <= i.path.length; n++) {
            if (_Helpers.isPlaceholder(
              _Util.getPath(args, i.path.sublist(0, n)),
            )) {
              pending = true;
              break;
            }
          }
          if (!pending) {
            issue = i;
            break;
          }
        }
      } else {
        issue = issues.first;
      }
      if (issue == null) return;
      final shown = _Helpers.argumentPath(op, issue.path);
      throw _validation(
        op,
        issue.path,
        _Util.getPath(args, issue.path),
        issue.message,
        'Fix $shown (${issue.message}) and call again.',
      );
    }
    if (op.rpc != null && body == null) return;
    final allowed = [for (final p in params) p.name];
    if (body != null) {
      if (body.arg != null) {
        allowed.add(body.arg!);
      } else {
        allowed.addAll([for (final (name, _) in body.fields) name]);
      }
    }
    for (final k in args.keys) {
      if (allowed.contains(k)) continue;
      final names = (allowed.toList()..sort()).take(20).toList();
      final list = names.isEmpty ? 'no arguments' : names.join(', ');
      throw _validation(
        op,
        [k],
        args[k],
        'one of: $list',
        'Remove $k; ${op.id} does not accept it.',
      );
    }
  }

  void _checkKey(OperationDescriptor op, CallOptions opts, _Purpose purpose) {
    final policy = op._idempotency;
    final note = _str(policy['note']).isEmpty ? '' : ' ${policy['note']}';
    if (!opts._hasKey) {
      if (purpose == _Purpose.call &&
          policy['policy'] == 'caller_owned' &&
          policy['persist_required'] == true) {
        final fresh = _str(policy['format']).toLowerCase().contains('uuid')
            ? 'UUIDv4'
            : 'unique key';
        throw _Stop(
          _diag(op.id, 'VALIDATION_FAILED')
              .param('idempotencyKey')
              .expected(_Keys.formatDescription(policy['format']))
              .remedy(
                'Generate a random $fresh, persist it with the intent of this ${op.id} call, and pass it as idempotencyKey. '
                'Reuse the exact same key on every retry; a new key can apply the effect twice.$note',
              )
              .err<Object?>(),
        );
      }
      return;
    }
    if (policy['policy'] == 'none' &&
        !op.params.any((p) => p.role == 'idempotency_key')) {
      return;
    }
    if (policy['policy'] == 'content_identity' ||
        policy['policy'] == 'content_hash') {
      return;
    }
    final format = policy['policy'] == 'caller_owned' ? policy['format'] : null;
    final expected = _Keys.checkFormat(opts.idempotencyKey, format);
    if (expected == null) return;
    throw _Stop(
      _diag(op.id, 'VALIDATION_FAILED')
          .param('idempotencyKey')
          .received(_Util.envelopeValue(opts.idempotencyKey, false))
          .expected(expected)
          .remedy(
            'Pass idempotencyKey as $expected. Generate it once, persist it with the intent of this call, and reuse it on every retry.$note',
          )
          .err<Object?>(),
    );
  }

  /// The confirmation rule of a tier: destructive accepts [Confirm.yes]
  /// (unless refused) or a token, irreversible only a token issued for the
  /// subject and these exact arguments. Null when confirmed.
  Err<Object?>? _confirmed(
    String id,
    String subject,
    String previewCall,
    String safety,
    Object? args,
    CallOptions opts,
  ) {
    if (safety != 'destructive' && safety != 'irreversible') return null;
    final allowYes = !opts.refuseConfirmYes;
    final how =
        'call $previewCall with the same arguments and pass its confirmation_token as confirm';
    Err<Object?> required(String remediation) =>
        _diag(id, 'CONFIRMATION_REQUIRED')
            .param('confirm')
            .expected('a confirmation_token from preview()')
            .remedy(remediation)
            .err<Object?>();
    if (opts._confirmYes) {
      if (safety == 'destructive' && allowYes) return null;
      return required('$id is $safety, so Confirm.yes is not accepted: $how.');
    }
    final token = opts._token;
    if (token != null) {
      switch (_Keys.checkToken(_confirmationKey, token, subject, args, now)) {
        case 'valid':
          return null;
        case 'expired':
          return required(
            'The confirmation token expired (tokens last ${_Keys.ttlMs ~/ 60000} minutes): $how.',
          );
        default:
          return required(
            'The confirmation token was not issued by this client for $id with exactly these arguments: $how.',
          );
      }
    }
    final orYes =
        safety == 'destructive' && allowYes ? ' (or pass Confirm.yes)' : '';
    return required('$id is $safety: $how$orYes.');
  }

  /// Spends a valid token on the call about to be sent: its first use binds
  /// it to that call's replay protection ([bind], null for none), and a
  /// later use is accepted only as a retry under the same protection.
  Err<Object?>? _claimToken(
    String id,
    String token,
    String? bind,
    String previewCall,
  ) {
    final t = now;
    _usedTokens.removeWhere((_, u) => u.$2 <= t);
    final previous = _usedTokens[token];
    if (previous != null) {
      if (previous.$1 != null && bind != null && previous.$1 == bind) {
        return null;
      }
      final why = previous.$1 == null
          ? 'This call has no idempotency key, so a repeat can apply the effect twice'
          : bind == null
              ? 'It was used with an idempotency key; only a retry with that same key may reuse it'
              : 'It was used with another idempotency key; only a retry with that same key may reuse it';
      return _diag(id, 'CONFIRMATION_REQUIRED')
          .param('confirm')
          .expected('a confirmation_token from preview()')
          .remedy(
            'This confirmation token was already used for one $id call. $why. Check whether that call took effect; '
            'to send again, call $previewCall and confirm with its new token.',
          )
          .err<Object?>();
    }
    final parsed = _Keys.tokenExpiry(token);
    _usedTokens[token] = (bind, parsed != null ? parsed.$1 : t + _Keys.ttlMs);
    return null;
  }

  /// Arguments as a JSON object.
  Map<String, Object?> _normalizeArgs(
    OperationDescriptor op,
    Map<String, Object?> raw,
  ) {
    Object? value;
    try {
      value = Json.from(Models.dump(raw));
    } on ArgumentError {
      value = unset;
    }
    if (value is Map<String, Object?>) return value;
    final names = [for (final p in op.argumentParams.take(3)) '${p.name}: ...'];
    throw _Stop(
      _diag(op.id, 'VALIDATION_FAILED')
          .param('args')
          .received(
            _Util.envelopeValue(identical(value, unset) ? null : value, false),
          )
          .expected('an object of named arguments')
          .remedy(
            'Pass the arguments of ${op.id} as one object of JSON values, e.g. {${names.join(', ')}}.',
          )
          .err<Object?>(),
    );
  }

  void _checkDescriptor(OperationDescriptor op) {
    final problem = _Helpers.descriptorProblem(op);
    if (problem == null) return;
    final id = op.id.isEmpty ? '<unknown operation>' : op.id;
    throw _Stop(
      _diag(id, 'VALIDATION_FAILED')
          .param('operation')
          .expected('a valid operation descriptor')
          .remedy(
            'The operation descriptor is invalid: $problem. Regenerate the SDK or fix the descriptor; nothing was sent.',
          )
          .err<Object?>(),
    );
  }

  /// Validates, authenticates and renders a request; throws [_Stop].
  Future<_Prepared> _prepare(
    OperationDescriptor op,
    Map<String, Object?> rawArgs,
    CallOptions opts,
    _Purpose purpose,
    String? urlOverride,
    _Claim? claim,
  ) async {
    _checkDescriptor(op);
    final limit = opts.maxResponseBytes ?? maxResponseBytes;
    if (limit != null && (limit <= 0 || limit > 9007199254740991)) {
      throw _Stop(
        _diag(op.id, 'VALIDATION_FAILED')
            .param('maxResponseBytes')
            .remedy(
              'maxResponseBytes must be a positive safe integer; nothing was sent.',
            )
            .err<Object?>(),
      );
    }
    register([op]);
    final args = _normalizeArgs(op, rawArgs);
    if (_Util.nestsDeeper(args, _Util.maxArgDepth)) {
      throw _validation(
        op,
        const [],
        '<deeply nested value>',
        'a JSON value nested at most ${_Util.maxArgDepth} levels',
        'Pass arguments without such deep nesting.',
      );
    }
    _validateArgs(op, args, purpose == _Purpose.macroPreview);
    _checkKey(op, opts, purpose);
    if (purpose == _Purpose.call && claim == null) {
      final err = _confirmed(
        op.id,
        op.id,
        'preview(...)',
        op.safety,
        args,
        opts,
      );
      if (err != null) throw _Stop(err);
    }
    final method = op.method;
    final auth = await _resolveAuth(op, method,
        _insecureHttp(urlOverride ?? baseUrl) ? _insecureHttpAuth : null);
    if (auth.failed != null) {
      throw _Stop(
        _diag(op.id, 'AUTH_FAILED').remedy(auth.failed).err<Object?>(),
      );
    }
    final secrets = auth.secrets;
    for (final path in _Helpers.sensitiveRequestPaths(op)) {
      final found = <String>[];
      _Helpers.valuesAt(args, _Util.splitPath(path), found);
      found.forEach(secrets.add);
    }
    final (fullUrl, displayUrl) = _buildUrl(op, args, auth.plan, urlOverride);
    final headers = _HeaderBag();
    _buildHeaders(op, args, opts, auth.plan, headers);
    if (_insecureHttp(fullUrl) &&
        (auth.plan.headers.isNotEmpty ||
            auth.plan.cookies.isNotEmpty ||
            auth.plan.query.isNotEmpty ||
            headers.entries
                .any((h) => h.secret || _credentialHeader(h.name)))) {
      throw _Stop(
          _diag(op.id, 'AUTH_FAILED').remedy(_insecureHttpAuth).err<Object?>());
    }
    final paramNames = [for (final p in op.argumentParams) p.name];
    final bodyPaths = _Helpers.sensitiveBodyPaths(op);
    Object? value = unset;
    if (!_Auth.safeMethod(method) || method == 'OPTIONS' || method == 'QUERY') {
      value = _Serialize.bodyValue(op, args, paramNames);
    }
    Object? redact(Object? display) {
      if (bodyPaths.contains('')) return redacted;
      return _Util.redactSensitiveKeys(_Util.redactPaths(display, bodyPaths));
    }

    final encodedOr = _Serialize.encodeBody(op.body, value, redact);
    if (encodedOr is _SerializationError) {
      final path = op.body?.arg != null && encodedOr.parameter == 'body'
          ? <Object>[op.body!.arg!]
          : const <Object>[];
      throw _validation(
        op,
        path,
        encodedOr.value,
        encodedOr.expected,
        'Pass ${encodedOr.parameter} as ${encodedOr.expected}.',
      );
    }
    final encoded = encodedOr as _Encoded;
    if (encoded.contentType != null) {
      headers.set('Content-Type', encoded.contentType!, false);
    }
    final header = _Keys.header(op);
    final key = await _idempotencyKey(op, args, opts, purpose, encoded);
    final policy = op._idempotency['policy'];
    if (key != null) {
      headers.set(header, key, true);
      secrets.add(key);
    } else if (purpose != _Purpose.call &&
        (policy == 'auto' || policy == 'caller_owned')) {
      headers.set(header, '<set at call time>', false);
    }
    for (final (name, v, secret) in auth.plan.headers) {
      headers.set(name, v, secret);
    }
    final preview = op._preview;
    if (purpose == _Purpose.serverPreview && preview['mode'] == 'header') {
      headers.set(_str(preview['header']), _str(preview['value']), false);
    }
    if (_insecureHttp(fullUrl) &&
        headers.entries.any((h) {
          final preserved = key != null &&
              h.name.toLowerCase() == header.toLowerCase() &&
              h.value == key;
          return (h.secret && !preserved) ||
              _credentialHeader(h.name, preserved);
        })) {
      throw _Stop(
          _diag(op.id, 'AUTH_FAILED').remedy(_insecureHttpAuth).err<Object?>());
    }
    for (final e in headers.entries) {
      if (_Serialize.validHeaderName(e.name) &&
          _Serialize.validHeaderValue(e.value)) {
        continue;
      }
      final lower = e.name.toLowerCase();
      if (auth.plan.headers.any((h) => h.$1.toLowerCase() == lower)) {
        throw _Stop(
          _diag(op.id, 'AUTH_FAILED')
              .remedy(
                'The credential for header ${e.name} contains characters not allowed in an HTTP header; check ClientOptions.auth.',
              )
              .err<Object?>(),
        );
      }
      ParamDescriptor? param;
      for (final x in op.params) {
        if (x.wire.toLowerCase() == lower) {
          param = x;
          break;
        }
      }
      throw _validation(
        op,
        param != null ? [param.name] : const [],
        e.secret ? redacted : e.value,
        'a header value of visible ASCII characters (no line breaks)',
        'Header ${e.name} cannot carry this value; remove line breaks and non-Latin-1 characters.',
      );
    }
    if (purpose == _Purpose.call) {
      if (claim != null) {
        final err = claim.claim(this);
        if (err != null) throw _Stop(err);
      } else {
        final token = opts._token;
        if (token != null &&
            (op.safety == 'destructive' || op.safety == 'irreversible')) {
          final bind =
              key ?? (policy == 'content_identity' ? 'content-identity' : null);
          final err = _claimToken(op.id, token, bind, 'preview(...)');
          if (err != null) throw _Stop(err);
        }
      }
    }
    headers.secrets.forEach(secrets.add);
    final hidden = [for (final (k, _) in auth.plan.query) k];
    for (final p in op.argumentParams) {
      if (p.location != 'query' || !args.containsKey(p.name)) continue;
      if (Json.sameOpt(_displayValue(op, p, args[p.name]), args[p.name])) {
        continue;
      }
      for (final part in _Serialize.queryParam(p, args[p.name])) {
        final pairs = _queryPairs(part);
        if (pairs.isNotEmpty) hidden.add(pairs.first.$1);
      }
    }
    return _Prepared(
      op: op,
      args: args,
      method: method,
      url: fullUrl,
      displayUrl: displayUrl,
      headers: headers,
      body: encoded,
      key: key,
      keyHeader: header,
      authQuery: auth.plan.query,
      hiddenQuery: hidden,
      secrets: secrets,
      oauth: auth.oauth,
    );
  }

  _Stop _badBase(OperationDescriptor op) => _Stop(
        _diag(op.id, 'TRANSPORT_FAILED')
            .retrying('never')
            .remedy(
              'The base URL is not a valid absolute http(s) URL without credentials: fix ClientOptions.baseUrl. Nothing was sent.',
            )
            .err<Object?>(),
      );

  static bool _dotSegment(String segment) {
    var rest = segment.toLowerCase();
    var count = 0;
    while (count < 3) {
      if (rest.startsWith('%2e')) {
        rest = rest.substring(3);
      } else if (rest.startsWith('.')) {
        rest = rest.substring(1);
      } else {
        break;
      }
      count++;
    }
    return segment.isEmpty || (rest.isEmpty && count >= 1 && count <= 2);
  }

  Object? _displayValue(
    OperationDescriptor op,
    ParamDescriptor p,
    Object? value,
  ) {
    if (_Helpers.sensitiveArg(op, [p.name]) || _Util.looksSensitive(p.wire)) {
      return redacted;
    }
    return _Util.redactSensitiveKeys(_Helpers.redactBelow(op, [p.name], value));
  }

  /// Redirects and pagination can supply paths other than the template:
  /// the path is hidden conservatively when it may carry sensitive
  /// arguments.
  String _displayOverrideUrl(
    OperationDescriptor op,
    Map<String, Object?> args,
    String url,
  ) {
    var uri = Uri.tryParse(url);
    if (uri == null) return url;
    final hidden = <String>[];
    for (final p in op.argumentParams) {
      if (!args.containsKey(p.name)) continue;
      if (Json.sameOpt(_displayValue(op, p, args[p.name]), args[p.name])) {
        continue;
      }
      if (p.location == 'path') {
        uri = uri!.replace(path: '/${_Serialize.encodeComponent(redacted)}');
      } else if (p.location == 'query') {
        for (final part in _Serialize.queryParam(p, args[p.name])) {
          hidden.addAll([for (final (k, _) in _queryPairs(part)) k]);
        }
      }
    }
    return _withQuery(uri.toString(), const [], hidden, replace: true);
  }

  /// (url, display url); throws [_Stop].
  (String, String) _buildUrl(
    OperationDescriptor op,
    Map<String, Object?> args,
    _AuthPlan plan,
    String? urlOverride,
  ) {
    if (baseUrl.isEmpty && urlOverride == null) {
      throw _Stop(
        _diag(op.id, 'TRANSPORT_FAILED')
            .retrying('never')
            .remedy(
              'No base URL is configured: set ClientOptions.baseUrl. Nothing was sent.',
            )
            .err<Object?>(),
      );
    }
    final params = op.argumentParams;
    String full;
    String display;
    if (urlOverride != null) {
      full = urlOverride;
      display = _displayOverrideUrl(op, args, urlOverride);
      if (plan.query.isNotEmpty) {
        if (Uri.tryParse(full) == null) throw _badBase(op);
        full = _withQuery(full, plan.query, const [], replace: false);
        display = _withQuery(
          display,
          [for (final (k, _) in plan.query) (k, redacted)],
          const [],
          replace: true,
        );
      }
    } else {
      String? unknown;
      ParamDescriptor? missing;
      final displayed = <String, String>{};
      final placeholder = RegExp(r'\{([^{}]+)\}');
      ParamDescriptor? pathParam(String inner) {
        for (final x in params) {
          if (x.location == 'path' && x.wire == inner) return x;
        }
        for (final x in params) {
          if (x.location == 'path' && x.name == inner) return x;
        }
        return null;
      }

      final built = op.path.replaceAllMapped(placeholder, (m) {
        final inner = m[1]!;
        final p = pathParam(inner);
        if (p == null) {
          unknown ??= inner;
          return '{$inner}';
        }
        if (args[p.name] == null) {
          missing ??= p;
          return '{$inner}';
        }
        displayed[inner] = _Serialize.pathParam(
          p,
          _displayValue(op, p, args[p.name]),
        );
        return _Serialize.pathParam(p, args[p.name]);
      });
      if (unknown != null) {
        throw _Stop(
          _diag(op.id, 'VALIDATION_FAILED')
              .param('operation')
              .expected('a valid operation descriptor')
              .remedy(
                'The path template has a placeholder {$unknown} with no path parameter; regenerate the SDK.',
              )
              .err<Object?>(),
        );
      }
      if (missing != null) {
        throw _validation(
          op,
          [missing!.name],
          unset,
          'a value for path parameter ${missing!.wire}',
          'Pass ${missing!.name}.',
        );
      }
      final template = op.path.split('/');
      final segments = built.split('/');
      if (template.length == segments.length) {
        for (var i = 0; i < segments.length; i++) {
          final tmpl = template[i];
          if (!tmpl.contains('{') || !_dotSegment(segments[i])) continue;
          final wire = RegExp(r'\{([^}]*)\}').firstMatch(tmpl)?[1] ?? '';
          final p = pathParam(wire);
          throw _validation(
            op,
            p != null ? [p.name] : const [],
            p != null ? args[p.name] : segments[i],
            'a non-empty path segment other than . and ..',
            'Pass ${p != null ? p.name : 'the path parameter'} as the identifier of one resource; "${segments[i]}" would change the request path.',
          );
        }
      }
      final parts = <String>[];
      final shown = <String>[];
      for (final p in params) {
        if (p.location != 'query' || !args.containsKey(p.name)) continue;
        parts.addAll(_Serialize.queryParam(p, args[p.name]));
        shown.addAll(
          _Serialize.queryParam(p, _displayValue(op, p, args[p.name])),
        );
      }
      for (final (k, v) in plan.query) {
        parts.add(
          '${_Serialize.encodeComponent(k)}=${_Serialize.encodeComponent(v)}',
        );
        shown.add('${_Serialize.encodeComponent(k)}=$redacted');
      }
      final root = baseUrl.replaceFirst(RegExp(r'/+$'), '');
      String join(List<String> list) => list.isEmpty
          ? ''
          : (built.contains('?') ? '&' : '?') + list.join('&');
      full = root + built + join(parts);
      final displayedPath = op.path.replaceAllMapped(
        placeholder,
        (m) => displayed[m[1]!] ?? m[0]!,
      );
      display = root + displayedPath + join(shown);
    }
    if (!_validBaseUrl(full)) throw _badBase(op);
    return (full, display);
  }

  static List<(String, String)> _queryPairs(String? raw) {
    if (raw == null || raw.isEmpty) return const [];
    final out = <(String, String)>[];
    for (final part in raw.split('&')) {
      if (part.isEmpty) continue;
      final eq = part.indexOf('=');
      final k = eq < 0 ? part : part.substring(0, eq);
      final v = eq < 0 ? '' : part.substring(eq + 1);
      try {
        out.add((Uri.decodeQueryComponent(k), Uri.decodeQueryComponent(v)));
      } on ArgumentError {
        out.add((k, v));
      } on FormatException {
        out.add((k, v));
      }
    }
    return out;
  }

  /// A URL with the pairs set (replaced when [replace], else only when
  /// absent) and the [hidden] parameters shown redacted.
  static String _withQuery(
    String url,
    List<(String, String)> pairs,
    List<String> hidden, {
    required bool replace,
  }) {
    final hash = url.indexOf('#');
    final noFragment = hash < 0 ? url : url.substring(0, hash);
    final fragment = hash < 0 ? '' : url.substring(hash);
    final q = noFragment.indexOf('?');
    final head = q < 0 ? noFragment : noFragment.substring(0, q);
    var list = _queryPairs(q < 0 ? null : noFragment.substring(q + 1));
    for (final (k, v) in pairs) {
      final index = list.indexWhere((e) => e.$1 == k);
      if (index < 0) {
        list = [...list, (k, v)];
      } else if (replace) {
        list = [
          for (var i = 0; i < list.length; i++)
            if (i == index) (k, v) else if (list[i].$1 != k) list[i],
        ];
      }
    }
    for (final name in hidden) {
      list = [for (final (k, v) in list) k == name ? (k, redacted) : (k, v)];
    }
    if (list.isEmpty) return head + fragment;
    final query = [
      for (final (k, v) in list)
        '${_Serialize.encodeFormComponent(k)}=${_Serialize.encodeFormComponent(v)}',
    ].join('&');
    return '$head?$query$fragment';
  }

  void _buildHeaders(
    OperationDescriptor op,
    Map<String, Object?> args,
    CallOptions opts,
    _AuthPlan plan,
    _HeaderBag headers,
  ) {
    final accept = <String>[];
    for (final r in op.responses) {
      final m = r.mediaType;
      if (r.kind == 'success' &&
          m != null &&
          m.isNotEmpty &&
          !accept.contains(m)) {
        accept.add(m);
      }
    }
    if (accept.isNotEmpty) headers.set('Accept', accept.join(', '), false);
    final apiName = _str(_api['name']).isEmpty ? 'api' : _str(_api['name']);
    final apiVersion =
        _str(_api['version']).isEmpty ? '0' : _str(_api['version']);
    headers.set(
      'X-Tungsten-Runtime',
      'tungsten-dart/$runtimeVersion $apiName-sdk/$apiVersion',
      false,
    );
    headers.set('X-Tungsten-Operation', op.id, false);
    final tungsten = _str(_api['tungsten_version']).isEmpty
        ? runtimeVersion
        : _str(_api['tungsten_version']);
    headers.set(
      'User-Agent',
      '$apiName-sdk/$apiVersion tungsten/$tungsten (dart)',
      false,
    );
    for (final p in op.params) {
      if (p.role == 'constant' &&
          p.location == 'header' &&
          p.constant != null) {
        headers.set(p.wire, _Serialize.scalar(p.constant), false);
      }
    }
    for (final extra in [_headers, opts.headers]) {
      final names = extra.keys.toList()..sort();
      for (final n in names) {
        headers.set(n, extra[n]!, _Util.looksSensitive(n));
      }
    }
    final cookies = <String>[];
    final existing = headers.get('Cookie');
    if (existing != null) cookies.add(existing);
    for (final p in op.argumentParams) {
      final v = args[p.name];
      if (v == null) continue;
      if (p.location == 'header') {
        final secret = !Json.sameOpt(_displayValue(op, p, v), v);
        headers.set(p.wire, _Serialize.headerParam(p, v), secret);
      } else if (p.location == 'cookie') {
        cookies.add(_Serialize.cookieParam(p, v));
      }
    }
    for (final (k, v) in plan.cookies) {
      cookies.add('$k=$v');
    }
    // Cookies are session material: the whole header is always redacted.
    if (cookies.isNotEmpty) headers.set('Cookie', cookies.join('; '), true);
  }

  /// The key to send (null for none); throws [_Stop].
  Future<String?> _idempotencyKey(
    OperationDescriptor op,
    Map<String, Object?> args,
    CallOptions opts,
    _Purpose purpose,
    _Encoded encoded,
  ) async {
    final supplied = opts._hasKey ? opts.idempotencyKey : null;
    switch (op._idempotency['policy']) {
      case 'content_identity':
        return null;
      case 'content_hash':
        final hashed = encoded.hashed;
        return hashed == null ? null : _Util.sha256Hex(hashed);
      case 'caller_owned':
        return supplied;
      case 'auto':
        if (supplied != null) return supplied;
        if (purpose != _Purpose.call) return null;
        final logical = _Util.sha256Hex(utf8.encode(Json.canonical(args)));
        Err<Object?> failed(String why) => _diag(op.id, 'TRANSPORT_FAILED')
            .retrying('never')
            .remedy(
              'The idempotency store failed ($why), so the call was not sent rather than risk a second key '
              'for the same intent. Fix or replace ClientOptions.idempotencyStore.',
            )
            .err<Object?>();
        final (key, err) = await _keyLock.run(() async {
          try {
            final key = await _store.reserve(op.id, logical, _Util.uuidV4());
            if (key.isNotEmpty) return (key, null);
            return (null, failed('it returned no key'));
          } catch (_) {
            return (null, failed('it threw an error'));
          }
        });
        if (err != null) throw _Stop(err);
        return key;
      default:
        return op.params.any((p) => p.role == 'idempotency_key')
            ? supplied
            : null;
    }
  }

  // ------------------------------------------------------------ sending

  RequestContext _requestContext(_Prepared p, int attempt) => RequestContext(
        operation: p.op.id,
        attempt: attempt,
        method: p.method,
        url: p.displayUrl,
        headers: p.headers.toMap(true),
        hasBody: !p.body.empty,
        body: p.body.empty ? null : p.body.display,
      );

  Future<Result<Object?>> _sendRequest(_Prepared p, CallOptions opts) async {
    final (result, streamed) = await _sendWith(p, opts, false);
    if (streamed == null) return result!;
    streamed.body.close();
    return _diag(p.op.id, 'UNEXPECTED_RESPONSE')
        .retrying('never')
        .remedy(
          'The SDK received an event stream it did not ask for. This is a bug in the runtime; report it. '
          'If the call may have been sent, check its effect before repeating it.',
        )
        .err<Object?>();
  }

  static bool _redirect(int status) =>
      const [301, 302, 303, 307, 308].contains(status);

  static bool _sameOrigin(Uri a, Uri b) =>
      a.scheme.toLowerCase() == b.scheme.toLowerCase() &&
      a.host.toLowerCase() == b.host.toLowerCase() &&
      a.port == b.port;

  /// (url, shown url) of a read's redirect that stays on the request's
  /// origin, with the auth query re-applied; null otherwise.
  (String, String)? _redirectTarget(
    _Prepared p,
    String from,
    String? location,
  ) {
    if (location == null || location.isEmpty) return null;
    try {
      final base = Uri.parse(from);
      final target = base.resolve(location);
      if (!_sameOrigin(base, target) || target.userInfo.isNotEmpty) {
        return null;
      }
      final sent = _withQuery(
        target.toString(),
        p.authQuery,
        const [],
        replace: false,
      );
      final display = _displayOverrideUrl(p.op, p.args, sent);
      return (
        sent,
        _withQuery(display, const [], p.hiddenQuery, replace: true),
      );
    } on FormatException {
      return null;
    }
  }

  /// Sends with retries. With [stream], a 2xx text/event-stream answer is
  /// handed back unread as a [_Streamed]; otherwise the result.
  Future<(Result<Object?>?, _Streamed?)> _sendWith(
    _Prepared p,
    CallOptions opts,
    bool stream,
  ) async {
    final op = p.op;
    final retries = _retryOptions(op);
    final mutation = op.isMutation;
    final protected = _Classify.replayProtection(op, p.key != null);
    final check = mutation && !protected ? _outcomeCheck(op, p.args) : null;
    final timeout = _attemptTimeout(opts);
    var attempts = 0;
    while (true) {
      attempts++;
      var rctx = _requestContext(p, attempts);
      _onRequest(rctx);
      var target = p.url;
      var method = p.method;
      var body = p.body;
      var headers = [for (final e in p.headers.entries) (e.name, e.value)];
      Future<_Outcome> sendOnce() => _Transport.attempt(
            url: target,
            method: method,
            headers: headers,
            body: body,
            timeoutMs: timeout,
            stream: stream,
            idleMs: stream ? idleTimeoutMs : null,
            maxResponseBytes: opts.maxResponseBytes ?? maxResponseBytes,
          );
      var outcome = await sendOnce();
      final pageUrls = <String>{_PageState.urlKey(target)};
      // Redirects are followed by hand, and only for reads: a mutation
      // never follows one.
      if (!mutation && !_redirectsDisabled) {
        for (var i = 0; i < maxRedirects; i++) {
          if (outcome.kind != _Kind.response ||
              outcome.bodyFailure == 'oversized' ||
              !_redirect(outcome.status)) {
            break;
          }
          final next = _redirectTarget(p, target, outcome.headers['location']);
          if (next == null) break;
          final key = _PageState.urlKey(next.$1);
          if (p.pageState != null &&
              (p.pageState!.seenUrls.contains(key) || !pageUrls.add(key))) {
            return (_PageState.repeated<Object?>(op.id), null);
          }
          target = next.$1;
          if (outcome.status != 307 &&
              outcome.status != 308 &&
              method != 'GET' &&
              method != 'HEAD') {
            method = 'GET';
            body = _Encoded.none;
            headers = [
              for (final h in headers)
                if (h.$1.toLowerCase() != 'content-type') h,
            ];
          }
          rctx = rctx._with(url: next.$2, method: method);
          outcome = await sendOnce();
        }
      }
      final ctx = _CallContext(
        api: _api,
        op: op,
        key: p.key,
        keyHeader: p.keyHeader,
        attempts: attempts,
        check: check,
      );
      final (result, streamed) = _classifyResponse(
        ctx,
        p,
        outcome,
        rctx,
        timeout,
      );
      if (streamed != null) return (null, streamed);
      if (result is Ok<Object?>) {
        p.pageState
          ?..effectiveUrl = target
          ..seenUrls.addAll(pageUrls);
        return (result, null);
      }
      final failed = result! as Err<Object?>;
      final d = _Util.scrubDiagnostic(failed.error, p.secrets);
      final done = Err<Object?>(d, partial: failed.partial);
      if (attempts > retries.max ||
          !_retryableCategories.contains(d.category)) {
        return (done, null);
      }
      if (d.retryable == 'never' ||
          d.retryable == 'after_remediation' ||
          (mutation && !protected)) {
        return (done, null);
      }
      double delay;
      final after = d.retryAfterMs;
      if (after != null && retries.honor) {
        if (after > retries.maxMs) return (done, null);
        delay = after.toDouble();
      } else {
        final ceiling = math.min(
          retries.maxMs,
          retries.baseMs * math.pow(2, attempts - 1),
        );
        final r = _random();
        delay = switch (retries.jitter) {
          'none' => ceiling,
          'equal' => ceiling / 2 + r * ceiling / 2,
          _ => r * ceiling,
        };
      }
      _onRetry(rctx, d);
      await Future<void>.delayed(
        Duration(microseconds: (math.max(delay, 0) * 1000).round()),
      );
    }
  }

  (Result<Object?>?, _Streamed?) _classifyResponse(
    _CallContext ctx,
    _Prepared p,
    _Outcome outcome,
    RequestContext rctx,
    int timeout,
  ) {
    final op = p.op;
    final mutation = op.isMutation;
    final attempts = ctx.attempts;
    switch (outcome.kind) {
      case _Kind.notSent:
        return (
          _diag(op.id, 'TRANSPORT_FAILED')
              .remedy(
                'The request could not be delivered (${outcome.detail}), so the server did not receive it. '
                'Check baseUrl and network access, then call again.',
              )
              .attempts(attempts)
              .err<Object?>(),
          null,
        );
      case _Kind.lost:
        final cause =
            'The connection failed before a response arrived (${outcome.detail}).';
        if (mutation) {
          return (Err<Object?>(_Classify.outcomeUnknown(ctx, cause)), null);
        }
        return (
          _diag(op.id, 'TRANSPORT_FAILED')
              .remedy('$cause This read has no side effects; call again.')
              .attempts(attempts)
              .err<Object?>(),
          null,
        );
      case _Kind.timeout:
        if (mutation) {
          return (
            Err<Object?>(
              _Classify.outcomeUnknown(
                ctx,
                'No response arrived within $timeout ms.',
              ),
            ),
            null,
          );
        }
        return (
          _diag(op.id, 'UPSTREAM_UNAVAILABLE')
              .remedy(
                'No response arrived within $timeout ms. This read has no side effects; call again later or with a larger `timeout`.',
              )
              .attempts(attempts)
              .err<Object?>(),
          null,
        );
      case _Kind.streaming:
        _onResponse(
          rctx,
          ResponseContext(status: outcome.status, headers: outcome.headers),
        );
        final meta = ResponseMeta(
          status: outcome.status,
          headers: outcome.headers,
          requestId: _Classify.requestId(outcome.headers),
          attempts: attempts,
        );
        return (null, _Streamed(meta, outcome.stream!));
      case _Kind.response:
        break;
    }
    final status = outcome.status;
    final headers = outcome.headers;
    _onResponse(rctx, ResponseContext(status: status, headers: headers));
    final rid = _Classify.requestId(headers);
    final retryAfter = _Transport.parseRetryAfter(headers['retry-after'], now);
    final declared = _Classify.matchResponse(op.responses, status);
    // A status the descriptor declares (exactly or by class) is classified
    // as declared. With redirects disabled, a 3xx it does not declare is a
    // refused redirect, never a success.
    final refusedRedirect = _redirectsDisabled &&
        status >= 300 &&
        status <= 399 &&
        (declared == null || declared.status == 'default');
    final success = !refusedRedirect &&
        ((status >= 200 && status <= 299) ||
            (declared?.kind == 'success' && status >= 100 && status <= 399));
    final verifyOp = op.agent['verify'];
    final hint = verifyOp is Map
        ? 'Call ${verifyOp['operation']} to read the current state.'
        : null;
    if (outcome.bodyFailure == 'oversized') {
      return (
        _diag(op.id, 'UNEXPECTED_RESPONSE')
            .code('RESPONSE_TOO_LARGE')
            .status(status)
            .requestId(rid)
            .retrying('never')
            .next(hint)
            .attempts(attempts)
            .remedy(
              'Response exceeded maxResponseBytes and was abandoned. '
              '${mutation ? "The call may have taken effect; inspect API state before retrying." : "The read is incomplete."}',
            )
            .err<Object?>(),
        null,
      );
    }
    if (success && outcome.body == null) {
      final failure = outcome.bodyFailure == 'timeout' ? 'timeout' : 'broken';
      if (!mutation) {
        return (
          _diag(op.id, 'UPSTREAM_UNAVAILABLE')
              .status(status)
              .requestId(rid)
              .retrying('after_delay')
              .remedy(
                'The response body was cut off ($failure). This read has no side effects; call again.',
              )
              .attempts(attempts)
              .err<Object?>(),
          null,
        );
      }
      return (
        _diag(op.id, 'UNEXPECTED_RESPONSE')
            .status(status)
            .requestId(rid)
            .retrying('never')
            .remedy(
              'The server accepted ${op.id} (HTTP $status) but the response body was cut off ($failure). The call took effect; do not repeat it.',
            )
            .next(hint)
            .attempts(attempts)
            .err<Object?>(),
        null,
      );
    }
    final data = outcome.body ?? const <int>[];
    final declaredMedia = declared?.mediaType ?? '';
    if (!success) {
      if (status >= 300 && status <= 399) {
        final location = headers['location'];
        final to =
            location != null ? ' to ${Json.takeUnits(location, 200)}' : '';
        final String remediation;
        if (mutation) {
          remediation =
              'The server answered with a redirect$to. Redirects are never followed on mutations; check baseUrl (scheme, host, trailing slash). '
              'Whether the call took effect is unknown only if the server applied it before redirecting.';
        } else if (_redirectsDisabled) {
          remediation =
              'The server answered with a redirect$to. Redirects are disabled; check redirects and baseUrl.';
        } else {
          remediation =
              "The server answered with a redirect$to that leaves the API's origin (or too many redirects). "
              "It is not followed, so credentials never leave the API's host; check baseUrl.";
        }
        return (
          _diag(op.id, 'UNEXPECTED_RESPONSE')
              .status(status)
              .requestId(rid)
              .remedy(remediation)
              .next(mutation ? hint : null)
              .attempts(attempts)
              .err<Object?>(),
          null,
        );
      }
      final decoded = _Classify.decodeBody(data, headers, declaredMedia);
      return (
        Err<Object?>(
          _Classify.classifyError(ctx, status, headers, decoded, retryAfter),
        ),
        null,
      );
    }
    final decoded = _Classify.decodeBody(data, headers, declaredMedia);
    final mode = _validateResponses;
    final afterEffect =
        mutation ? ' The call took effect; do not repeat it.' : '';
    Diagnostic? problem;
    final response = op.response;
    if (decoded.invalidJson) {
      final expected = decoded.jsonl ? 'a JSON Lines body' : 'a JSON body';
      final remediation = decoded.jsonl
          ? 'The success response announced JSON Lines but line ${decoded.badLine} did not parse.$afterEffect'
          : 'The success response announced JSON but did not parse.$afterEffect';
      problem = _diag(op.id, 'UNEXPECTED_RESPONSE')
          .status(status)
          .requestId(rid)
          .param('response')
          .expected(expected)
          .remedy(remediation)
          .attempts(attempts)
          .build();
    } else if (mode != ValidateResponses.off &&
        response != null &&
        decoded.present) {
      final issues = _judge(
        op.id,
        'response',
        response,
        decoded.value,
      );
      if (issues.isNotEmpty) {
        final issue = issues.first;
        final pathText = Schema.issuePath(issue.path);
        final kept = mutation && mode == ValidateResponses.strict
            ? " The decoded body is in the result's partial; store any value shown only once from it before anything else."
            : '';
        final shown = _Util.redactPaths(
          decoded.value,
          op.sensitiveResponseFields,
        );
        final received = _Util.getPath(shown, issue.path);
        final sensitive = issue.path.any(
          (s) => s is String && _Util.looksSensitive(s),
        );
        problem = _diag(op.id, 'UNEXPECTED_RESPONSE')
            .status(status)
            .requestId(rid)
            .param('response$pathText')
            .received(_Util.envelopeValue(received, sensitive))
            .expected(issue.message)
            .remedy(
              'The success response does not match the API description at response$pathText (${issue.message}).$afterEffect$kept',
            )
            .attempts(attempts)
            .build();
      }
    }
    if (problem != null) {
      final scrubbed = _Util.scrubDiagnostic(problem, p.secrets);
      if (mode == ValidateResponses.strict) {
        // The effect of a mutation happened: its body is never dropped,
        // since it can hold the only copy of a value.
        if (mutation && !decoded.invalidJson && decoded.present) {
          return (Err<Object?>(scrubbed, partial: decoded.value), null);
        }
        return (Err<Object?>(scrubbed), null);
      }
      _emit(scrubbed);
    }
    final value = decoded.present ? decoded.value : null;
    return (
      Ok<Object?>(
        value,
        raw: value,
        meta: ResponseMeta(
          status: status,
          headers: headers,
          requestId: rid,
          attempts: attempts,
        ),
      ),
      null,
    );
  }

  /// A validator that only judges (responses, page items, events): a
  /// failure of the validator itself is an issue at the root. Patterns it
  /// could not check are reported under [root] (once per path and pattern in
  /// [seen]).
  List<Issue> _judge(
    String operation,
    String root,
    Schema validator,
    Object? value, [
    Set<String>? seen,
  ]) {
    final notes = <UncheckedPattern>[];
    try {
      final issues = validator.validate(value, onUnchecked: notes.add);
      _reportUnchecked(operation, root, notes, 'UNEXPECTED_RESPONSE', seen);
      return issues;
    } catch (_) {
      return const [
        Issue([], 'the response schema failed (the validator threw an error)'),
      ];
    }
  }

  /// A schema pattern the runtime cannot compile accepts the value but is
  /// reported through `onDiagnostic` as `PATTERN_UNCHECKED`, once per path and
  /// pattern (at most 16 a call). The value is never included.
  void _reportUnchecked(
    String operation,
    String root,
    List<UncheckedPattern> notes,
    String category, [
    Set<String>? seen,
  ]) {
    final done = seen ?? <String>{};
    var sent = 0;
    for (final note in notes) {
      if (sent >= 16) break;
      final param = '$root${Schema.issuePath(note.path)}';
      if (!done.add('$param\n${note.pattern}')) continue;
      sent++;
      _emit(
        _diag(operation, category)
            .code('PATTERN_UNCHECKED')
            .retrying('never')
            .param(param)
            .expected('a value matching the pattern /${note.pattern}/')
            .remedy(
              'The pattern /${note.pattern}/ at $param was not checked because ${note.reason}; '
              'the value was accepted without it. The server still validates it.',
            )
            .build(),
      );
    }
  }

  /// How to find out whether a mutation without replay protection took
  /// effect after its answer was lost.
  _OutcomeCheck? _outcomeCheck(
    OperationDescriptor op,
    Map<String, Object?> args,
  ) {
    final sentArgs = _Helpers.sentArguments(op, args);
    final hook = op.agent['verify'];
    if (hook is Map<String, Object?> &&
        _str(hook['operation']).isNotEmpty &&
        _Classify.verifyCallable(hook)) {
      final scope = <String, Object?>{'args': _Helpers.withWireNames(op, args)};
      final evaluated = Expr.evaluate(
        hook['args'] ?? <String, Object?>{},
        scope,
      );
      final call = _str(hook['operation']) + _Util.withArguments(evaluated);
      final lost = <String>[];
      final expect = hook['expect'] is Map
          ? _resolveLost(hook['expect'], scope, lost)
          : <String, Object?>{};
      final fields =
          expect is Map ? [for (final k in expect.keys) '$k'] : <String>[];
      final String shows;
      if (fields.isEmpty) {
        shows = 'whether it shows ${_Classify.changeOf(op)}';
      } else if (lost.isNotEmpty) {
        shows =
            'whether ${fields.join(' and ')} holds what ${op.id} creates, matching the arguments you sent$sentArgs '
            '(its ${lost.toSet().join(', ')} was in the lost response)';
      } else {
        shows = 'whether ${Expr.describe(expect)}';
      }
      return _OutcomeCheck(call, shows);
    }
    final read = _resourceRead(op, args);
    if (read == null) return null;
    return _OutcomeCheck(
      read.$1 + _Util.withArguments(read.$2),
      'whether it shows ${_Classify.changeOf(op)}',
    );
  }

  static bool _rootSegment(String s) =>
      s.isEmpty || s == 'api' || RegExp(r'^[vV]\d+(\.\d+)*$').hasMatch(s);

  /// A registered read of the resource [op] changes: a GET whose path is
  /// the longest prefix of op's path (never the API root) whose required
  /// parameters are path parameters op was called with.
  (String, Map<String, Object?>)? _resourceRead(
    OperationDescriptor op,
    Map<String, Object?> args,
  ) {
    if (op.rpc != null) return null;
    final known = <String, Object?>{};
    for (final p in op.params) {
      if (p.location == 'path' && args[p.name] != null) {
        known[p.wire] = args[p.name];
      }
    }
    final segments = op.path.split('/');
    final ids = _registry.keys.toList()..sort();
    final reads = [
      for (final id in ids)
        if (_registry[id]!.id != op.id &&
            _registry[id]!.method == 'GET' &&
            !_registry[id]!.isMutation &&
            _registry[id]!.rpc == null)
          _registry[id]!,
    ];
    for (var n = segments.length; n >= 1; n--) {
      final prefix = segments.sublist(0, n);
      if (prefix.every(_rootSegment)) break;
      final path = prefix.join('/');
      for (final read in reads) {
        if (read.path != path) continue;
        final params = read.argumentParams;
        final fits = params.every(
          (p) =>
              !p.required ||
              (p.location == 'path' && known.containsKey(p.wire)),
        );
        if (!fits) continue;
        final readArgs = <String, Object?>{
          for (final p in params)
            if (p.location == 'path' && known.containsKey(p.wire))
              p.wire: known[p.wire],
        };
        return (read.id, readArgs);
      }
    }
    return null;
  }

  Object? _resolveLost(
    Object? node,
    Map<String, Object?> scope,
    List<String> lost, [
    int depth = 0,
  ]) {
    if (depth > 32) return null;
    if (node is String) {
      if (node.startsWith(r'$response')) {
        var rest = node.substring(9);
        if (rest.startsWith('.')) rest = rest.substring(1);
        lost.add(rest.isEmpty ? 'body' : rest);
        return '<${node.substring(1)}>';
      }
      if (!node.startsWith(r'$')) return node;
      final v = Expr.resolveRef(node, scope);
      return identical(v, unset) ? '<${node.substring(1)}>' : v;
    }
    if (node is List) {
      return [for (final n in node) _resolveLost(n, scope, lost, depth + 1)];
    }
    if (node is Map) {
      return <String, Object?>{
        for (final e in node.entries)
          '${e.key}': _resolveLost(e.value, scope, lost, depth + 1),
      };
    }
    return node;
  }

  // ------------------------------------------------- previews and polls

  Future<Result<Object?>> _previewWith(
    OperationDescriptor op,
    Map<String, Object?> rawArgs,
    CallOptions opts,
  ) async {
    final p = await _prepare(op, rawArgs, opts, _Purpose.preview, null, null);
    final conf = op.agent['confirmation'];
    final effects = <String>[];
    if (conf is Map && _str(conf['message']).isNotEmpty) {
      effects.add(_Helpers.interpolate(conf['message'], p.args, op));
    }
    final note = _str(op.agent['remediation_note']);
    if (note.isNotEmpty) effects.add(note);
    if (conf is Map && _list(conf['summary_fields']).isNotEmpty) {
      final fields = [for (final f in _list(conf['summary_fields'])) '$f={$f}'];
      effects.add(
        _Helpers.interpolate('Arguments: ${fields.join(', ')}.', p.args, op),
      );
    }
    if (op.summary.isNotEmpty) effects.add(op.summary);
    final mutating = op.isMutation;
    final result = PreviewResult(
      operation: op.id,
      safety: op.safety,
      request: RenderedRequest(
        method: p.method,
        url: p.displayUrl,
        headers: p.headers.toMap(true),
        body: p.body.empty ? null : p.body.display,
      ),
      effects: effects,
      confirmationToken: mutating
          ? _Keys.issueToken(_confirmationKey, op.id, p.args, now)
          : null,
      expiresInMs: mutating ? _Keys.ttlMs : null,
    );
    switch (op._preview['mode']) {
      case 'header':
        final dry = await _prepare(
          op,
          rawArgs,
          opts,
          _Purpose.serverPreview,
          null,
          null,
        );
        final answer = await _sendRequest(dry, opts);
        if (answer is Err<Object?>) return answer;
        final ok = answer as Ok<Object?>;
        final shown = ok.raw == null
            ? unset
            : _Util.redactPaths(ok.raw, op.sensitiveResponseFields);
        return Ok<Object?>(result._withServerPreview(shown), meta: ok.meta);
      case 'endpoint':
        final name = _str(op._preview['operation']);
        final target = operation(name);
        if (target == null) {
          return _diag(op.id, 'VALIDATION_FAILED')
              .param('operation')
              .expected(
                'the preview operation $name registered with the client',
              )
              .remedy(
                'Register $name (ClientCore.register) so preview() can call it; nothing was sent.',
              )
              .err<Object?>();
        }
        final answer = await _callWith(
          target,
          rawArgs,
          opts._with(clearConfirm: true, verify: false),
          null,
          null,
        );
        if (answer is Err<Object?>) return answer;
        final ok = answer as Ok<Object?>;
        final fields = {
          ...op.sensitiveResponseFields,
          ...target.sensitiveResponseFields,
        }.toList();
        final shown =
            ok.raw == null ? unset : _Util.redactPaths(ok.raw, fields);
        return Ok<Object?>(result._withServerPreview(shown), meta: ok.meta);
      default:
        return Ok<Object?>(result, meta: ResponseMeta.local);
    }
  }

  /// Polls until [until] holds or the budget runs out: an [Ok] whose value
  /// is a [PollResult].
  Future<Result<Object?>> _pollWith(
    OperationDescriptor op,
    Map<String, Object?> args,
    Object? until,
    int intervalMs,
    int budgetMs,
    CallOptions opts,
    _Claim? claim,
  ) async {
    final started = _Transport.monotonicMs();
    final once = opts._with(verify: false);
    while (true) {
      final response = await _callWith(op, args, once, null, claim);
      if (response is Err<Object?>) return response;
      final ok = response as Ok<Object?>;
      final body = ok.raw ?? unset;
      final done = Expr.predicate(until, body);
      final outOfTime =
          !done && (_Transport.monotonicMs() - started) + intervalMs > budgetMs;
      if (done || outOfTime) {
        return Ok<Object?>(
          PollResult(body: body, timedOut: outOfTime),
          raw: ok.raw,
          meta: ok.meta,
        );
      }
      await Future<void>.delayed(
        Duration(milliseconds: math.max(intervalMs, 0)),
      );
    }
  }

  Future<Verification> _verify(
    OperationDescriptor op,
    Map<String, Object?> args,
    Object? body,
    CallOptions opts,
  ) async {
    final hook = op.agent['verify'];
    if (hook is! Map<String, Object?>) return const Verification.unchecked();
    final target = operation(_str(hook['operation']));
    if (target == null) return const Verification.unchecked();
    // The hook is written against the API: argument keys and $args
    // references use wire names.
    final scope = <String, Object?>{'args': _Helpers.withWireNames(op, args)};
    if (!identical(body, unset)) scope['response'] = body;
    final evaluated = Expr.evaluate(hook['args'] ?? <String, Object?>{}, scope);
    if (evaluated is! Map) return const Verification.unchecked();
    final mapped = <String, Object?>{
      for (final e in evaluated.entries) target.argName('${e.key}'): e.value,
    };
    Object? resolve(Object? predicate) =>
        predicate is Map && predicate.isNotEmpty
            ? _resolveRefs(predicate, scope)
            : null;
    final terminal = resolve(hook['terminal']);
    final expect = resolve(hook['expect']) ?? <String, Object?>{};
    final interval = _Util.bounded(
      hook['poll_interval_ms'],
      defaultPollIntervalMs,
      0,
      _Util.maxSafeInteger,
    ).floor();
    final budget = _Util.bounded(
      hook['poll_budget_ms'],
      terminal != null ? defaultVerifyBudgetMs : 0,
      0,
      _Util.maxSafeInteger,
    ).floor();
    final rest = opts._with(clearConfirm: true, clearKey: true, verify: false);
    final polled = await _pollWith(
      target,
      mapped,
      terminal ?? expect,
      interval,
      budget,
      rest,
      null,
    );
    if (polled is Err<Object?>) return Verification.unchecked(polled.error);
    final poll = (polled as Ok<Object?>).value! as PollResult;
    final last = poll.body;
    return Verification(
      checked: true,
      passed: Expr.predicate(expect, last),
      observed: identical(last, unset)
          ? unset
          : _Util.redactPaths(last, target.sensitiveResponseFields),
      timedOut: budget != 0 && poll.timedOut,
    );
  }

  static Map<String, Object?> _unresolved() => <String, Object?>{
        r'$tungsten_unresolved': true,
      };

  /// References resolved; one that does not resolve equals nothing a
  /// response can hold.
  static Object? _resolveRefs(
    Object? node,
    Map<String, Object?> scope, [
    int depth = 0,
  ]) {
    if (depth > 64) return _unresolved();
    if (node is String) {
      if (!node.startsWith(r'$')) return node;
      final v = Expr.resolveRef(node, scope);
      return identical(v, unset) || v == null ? _unresolved() : v;
    }
    if (node is List) {
      return [for (final n in node) _resolveRefs(n, scope, depth + 1)];
    }
    if (node is Map) {
      return <String, Object?>{
        for (final e in node.entries)
          '${e.key}': _resolveRefs(e.value, scope, depth + 1),
      };
    }
    return node;
  }
}
