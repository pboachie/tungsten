// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Stored tokens expiring within this many milliseconds count as expired.
const int _expirySkewMs = 30000;

/// A PKCE pair (RFC 7636, S256); the [verifier] is a secret until the
/// exchange.
final class Pkce {
  /// A pair.
  const Pkce({required this.verifier, required this.challenge});

  /// A fresh pair: a 43 character verifier from 32 bytes of
  /// `Random.secure`.
  factory Pkce.generate() {
    final verifier = _Util.base64url(_Util.randomBytes(32));
    return Pkce(verifier: verifier, challenge: challengeOf(verifier));
  }

  /// base64url(SHA-256(ASCII(verifier))).
  static String challengeOf(String verifier) =>
      _Util.base64url(crypto.sha256.convert(ascii.encode(verifier)).bytes);

  /// The code verifier, passed to [OAuthFlow.exchangeCode].
  final String verifier;

  /// The code challenge, passed to [OAuthFlow.authorizationUrl].
  final String challenge;

  @override
  String toString() => 'Pkce(challenge: $challenge)';
}

/// What [OAuthFlow.exchangeCode] and [OAuthFlow.refresh] report: no
/// secrets.
final class TokenInfo {
  /// Token facts.
  const TokenInfo({
    required this.scheme,
    this.expiresAt,
    this.scope,
    required this.refreshable,
  });

  /// The scheme name.
  final String scheme;

  /// When the access token expires, in epoch milliseconds.
  final int? expiresAt;

  /// The scopes granted.
  final String? scope;

  /// Whether a refresh token is stored.
  final bool refreshable;

  @override
  String toString() =>
      'TokenInfo(scheme: $scheme, expiresAt: $expiresAt, scope: $scope, refreshable: $refreshable)';
}

/// The authorization-code helpers of one OAuth2 scheme
/// ([ClientCore.oauth]). Failures are AUTH_FAILED envelopes in [Err].
final class OAuthFlow {
  OAuthFlow._(this._core, this.scheme);

  final ClientCore _core;

  /// The scheme name.
  final String scheme;

  /// The store the tokens go to.
  TokenStore get tokenStore => _core._tokenStore;

  /// A fresh PKCE pair: pass its challenge to [authorizationUrl] and keep
  /// its verifier for [exchangeCode].
  Pkce pkce() => Pkce.generate();

  /// The URL to send the user to.
  Result<String> authorizationUrl({
    String? redirectUri,
    List<String>? scopes,
    String? state,
    String? codeChallenge,
    Map<String, String> extra = const {},
  }) =>
      _core._authorizationUrl(
        scheme,
        redirectUri: redirectUri,
        scopes: scopes,
        state: state,
        codeChallenge: codeChallenge,
        extra: extra,
      );

  /// Trades the code from the redirect for tokens, stored for later calls.
  Future<Result<TokenInfo>> exchangeCode({
    required String code,
    String? redirectUri,
    String? codeVerifier,
  }) =>
      _core._exchangeCode(
        scheme,
        code: code,
        redirectUri: redirectUri,
        codeVerifier: codeVerifier,
      );

  /// Refreshes the stored token now (calls refresh it on their own when it
  /// expires).
  Future<Result<TokenInfo>> refresh() => _core._refreshNow(scheme);
}

final class _TokenFailure {
  const _TokenFailure(this.message, this.rejected);

  final String message;
  final bool rejected;
}

final class _CodeClient {
  const _CodeClient(this.id, this.secret, this.redirect);

  final String id;
  final String secret;
  final String redirect;
}

typedef _TokenOutcome = (String?, _TokenFailure?);

extension _OAuth on ClientCore {
  static int _realNow() => DateTime.now().millisecondsSinceEpoch;

  static String _formBody(List<(String, String)> fields) => [
        for (final (k, v) in fields)
          '${_Serialize.percentEncode(k)}=${_Serialize.percentEncode(v)}',
      ].join('&');

  (Map<String, Object?>?, String?) _codeFlow(String scheme) {
    for (final s in _schemes) {
      if (s['name'] != scheme) continue;
      final flow = s['authorization_code'];
      if (s['kind'] == 'oauth2' && flow is Map) return (_map(flow), null);
      return (null, 'the scheme $scheme has no authorizationCode flow');
    }
    return (null, 'the API descriptor defines no scheme named $scheme');
  }

  (_CodeClient?, String?) _codeClient(String scheme) {
    final cred = _credential(scheme);
    if (cred is! Map<String, String?> || cred['flow'] != 'authorizationCode') {
      return (
        null,
        'auth.$scheme must be Credential.authorizationCode(clientId: ..., clientSecret: ..., redirectUri: ...)',
      );
    }
    final id = cred['client_id'] ?? '';
    if (id.isEmpty) return (null, 'auth.$scheme needs a client_id');
    return (
      _CodeClient(id, cred['client_secret'] ?? '', cred['redirect_uri'] ?? ''),
      null,
    );
  }

  static Err<T> _oauthError<T>(String operation, String remediation) =>
      _diag(operation, 'AUTH_FAILED').remedy(remediation).err<T>();

  static const _TokenFailure _storeFailed = _TokenFailure(
    'The token store failed (it threw an error); fix or replace ClientOptions.tokenStore.',
    false,
  );

  Future<StoredToken?> _readToken(String scheme) async {
    final t = await _tokenStore.get(scheme);
    return t != null && t.accessToken.isNotEmpty ? t : null;
  }

  _TokenFailure _missing(String scheme) => _TokenFailure(
        'No OAuth2 token is stored for $scheme; send the user to authorizationUrl(), then call exchangeCode() '
        'with the code, or put a token in ClientOptions.tokenStore.',
        false,
      );

  /// (token, null) or (null, remediation).
  Future<(String?, String?)> _storedToken(String scheme, bool force) async {
    final (token, failure) = await _storedTokenInner(scheme, force);
    return failure != null ? (null, failure.message) : (token, null);
  }

  Future<_TokenOutcome> _storedTokenInner(String scheme, bool force) async {
    final StoredToken? stored;
    try {
      stored = await _readToken(scheme);
    } catch (_) {
      return (null, _storeFailed);
    }
    if (stored == null) return (null, _missing(scheme));
    final expires = stored.expiresAt;
    final fresh = expires == null || expires > now + _expirySkewMs;
    if (fresh && !force) return (stored.accessToken, null);
    // One refresh per scheme at a time: callers that find it running wait
    // for its result instead of spending the refresh token again.
    final running = _refreshes[scheme];
    if (running != null) return running;
    final flight = _refreshFlight(scheme, force);
    _refreshes[scheme] = flight;
    try {
      return await flight;
    } finally {
      _refreshes.removeWhere((k, v) => k == scheme && identical(v, flight));
    }
  }

  Future<_TokenOutcome> _refreshFlight(String scheme, bool force) async {
    final StoredToken? current;
    try {
      current = await _readToken(scheme);
    } catch (_) {
      return (null, _storeFailed);
    }
    if (current == null) return (null, _missing(scheme));
    return _refreshStored(scheme, current, force);
  }

  /// The refreshed token after a 401 when the stored token has a refresh
  /// token (or another call already replaced it), else null.
  Future<String?> _refreshAfterRejection(String scheme, String rejected) async {
    final StoredToken? stored;
    try {
      stored = await _readToken(scheme);
    } catch (_) {
      return null;
    }
    if (stored == null) return null;
    if (stored.accessToken != rejected) return stored.accessToken;
    if ((stored.refreshToken ?? '').isEmpty) return null;
    final (token, problem) = await _storedToken(scheme, true);
    return problem == null ? token : null;
  }

  Future<_TokenOutcome> _refreshStored(
    String scheme,
    StoredToken stored,
    bool force,
  ) async {
    final (flow, reason) = _codeFlow(scheme);
    if (flow == null) {
      return (
        null,
        _TokenFailure(
          'Cannot refresh the OAuth2 token for $scheme: $reason.',
          false,
        ),
      );
    }
    final (client, why) = _codeClient(scheme);
    if (client == null) {
      return (
        null,
        _TokenFailure(
            'Cannot refresh the OAuth2 token for $scheme: $why.', false),
      );
    }
    final refresh = stored.refreshToken ?? '';
    if (refresh.isEmpty) {
      final state = '${force ? '' : 'expired and '}has no refresh token';
      return (
        null,
        _TokenFailure(
          'The OAuth2 token for $scheme $state; authorize again with authorizationUrl() and exchangeCode().',
          false,
        ),
      );
    }
    final url = _str(flow['refresh_url'] ?? flow['token_url']);
    final (renewed, failure) = await _tokenRequest(
      scheme,
      url,
      client,
      [('grant_type', 'refresh_token'), ('refresh_token', refresh)],
      refresh,
    );
    try {
      if (failure != null) {
        if (failure.rejected) await _tokenStore.delete(scheme);
        return (null, failure);
      }
      await _tokenStore.set(scheme, renewed!);
    } catch (_) {
      return (null, _storeFailed);
    }
    return (renewed.accessToken, null);
  }

  int get _tokenTimeoutMs => math.max(_timeoutMs, 1);

  Future<(StoredToken?, _TokenFailure?)> _tokenRequest(
    String scheme,
    String url,
    _CodeClient client,
    List<(String, String)> fields,
    String previous,
  ) async {
    final headers = <(String, String)>[
      ('Content-Type', 'application/x-www-form-urlencoded'),
      ('Accept', 'application/json'),
    ];
    final all = [...fields];
    if (client.secret.isEmpty) {
      all.add(('client_id', client.id));
    } else {
      final basic = base64Encode(
        utf8.encode(
          '${_Serialize.percentEncode(client.id)}:${_Serialize.percentEncode(client.secret)}',
        ),
      );
      headers.add(('Authorization', 'Basic $basic'));
    }
    final body = _Encoded(empty: false, bytes: utf8.encode(_formBody(all)));
    final out = await _Transport.attempt(
      url: url,
      method: 'POST',
      headers: headers,
      body: body,
      timeoutMs: _tokenTimeoutMs,
    );
    if (out.kind != _Kind.response || out.body == null) {
      return (
        null,
        _TokenFailure(
          'The OAuth2 token endpoint for $scheme could not be reached; check network access and the token URL.',
          false,
        ),
      );
    }
    final decoded =
        _Classify.decodeBody(out.body!, out.headers, 'application/json');
    if (out.status < 200 || out.status > 299) {
      final code = _Util.getPathStr(decoded.value, 'error');
      final named = code is String && RegExp(r'^[a-z_]{1,40}$').hasMatch(code)
          ? ' ($code)'
          : '';
      return (
        null,
        _TokenFailure(
          'The OAuth2 token endpoint for $scheme answered HTTP ${out.status}$named; check the code or refresh token, client credentials and redirect URI.',
          out.status >= 400 && out.status <= 499,
        ),
      );
    }
    String text(String path) {
      final v = _Util.getPathStr(decoded.value, path);
      return v is String ? v : '';
    }

    final access = text('access_token');
    if (access.isEmpty) {
      return (
        null,
        _TokenFailure(
          'The OAuth2 token endpoint for $scheme returned no access_token.',
          false,
        ),
      );
    }
    var refresh = text('refresh_token');
    if (refresh.isEmpty) refresh = previous;
    final expiresIn = _Util.getPathStr(decoded.value, 'expires_in');
    final expires = expiresIn is num && expiresIn.isFinite
        ? now + (math.max(expiresIn, 0) * 1000).truncate()
        : null;
    final type = text('token_type');
    final scope = text('scope');
    return (
      StoredToken(
        accessToken: access,
        refreshToken: refresh.isEmpty ? null : refresh,
        expiresAt: expires,
        tokenType: type.isEmpty ? null : type,
        scope: scope.isEmpty ? null : scope,
      ),
      null,
    );
  }

  /// An OAuth2 client-credentials token, fetched once per scheme and cached
  /// until 30 s before it expires. Concurrent calls share one request
  /// (single flight through a shared Future).
  Future<(String?, String?)> _clientCredentialsToken(_Satisfier sat) {
    final cached = _ccTokens[sat.name];
    if (cached != null &&
        cached.$1 == sat.clientId &&
        cached.$3 > _realNow() + _expirySkewMs) {
      return Future.value((cached.$2, null));
    }
    final running = _ccFlights[sat.name];
    if (running != null) return running;
    final flight = _fetchClientCredentials(sat);
    _ccFlights[sat.name] = flight;
    return flight.whenComplete(
      () => _ccFlights.removeWhere(
        (k, v) => k == sat.name && identical(v, flight),
      ),
    );
  }

  Future<(String?, String?)> _fetchClientCredentials(_Satisfier sat) async {
    final form = ['grant_type=client_credentials'];
    if (sat.scopes.isNotEmpty) {
      form.add('scope=${_Serialize.encodeFormComponent(sat.scopes.join(' '))}');
    }
    final basic = base64Encode(
      utf8.encode(
        '${_Serialize.encodeComponent(sat.clientId)}:${_Serialize.encodeComponent(sat.clientSecret)}',
      ),
    );
    final out = await _Transport.attempt(
      url: sat.tokenUrl,
      method: 'POST',
      headers: [
        ('Content-Type', 'application/x-www-form-urlencoded'),
        ('Accept', 'application/json'),
        ('Authorization', 'Basic $basic'),
      ],
      body: _Encoded(empty: false, bytes: utf8.encode(form.join('&'))),
      timeoutMs: _tokenTimeoutMs,
    );
    if (out.kind != _Kind.response || out.body == null) {
      return (
        null,
        'The OAuth2 token endpoint for ${sat.name} could not be reached; check network access and the token URL.',
      );
    }
    if (out.status < 200 || out.status > 299) {
      return (
        null,
        'The OAuth2 token endpoint for ${sat.name} answered HTTP ${out.status}; check client_id, client_secret and scopes.',
      );
    }
    final decoded =
        _Classify.decodeBody(out.body!, out.headers, 'application/json');
    final token = _Util.getPathStr(decoded.value, 'access_token');
    if (token is! String || token.isEmpty) {
      return (
        null,
        'The OAuth2 token endpoint for ${sat.name} returned no access_token.',
      );
    }
    var lifetime = 3600000;
    final e = _Util.getPathStr(decoded.value, 'expires_in');
    if (e is num && e.isFinite) lifetime = math.max((e * 1000).truncate(), 0);
    _ccTokens[sat.name] = (sat.clientId, token, _realNow() + lifetime);
    return (token, null);
  }

  static String _oauthOperation(String scheme, String helper) =>
      'oauth.$scheme.$helper';

  Result<String> _authorizationUrl(
    String scheme, {
    String? redirectUri,
    List<String>? scopes,
    String? state,
    String? codeChallenge,
    Map<String, String> extra = const {},
  }) {
    final operation = _oauthOperation(scheme, 'authorizationUrl');
    final (flow, reason) = _codeFlow(scheme);
    if (flow == null) {
      return _oauthError(
        operation,
        'Cannot build an authorization URL: $reason.',
      );
    }
    final (client, why) = _codeClient(scheme);
    if (client == null) {
      return _oauthError(operation, 'Cannot build an authorization URL: $why.');
    }
    final redirect =
        (redirectUri ?? '').isEmpty ? client.redirect : redirectUri!;
    if (redirect.isEmpty) {
      return _oauthError(
        operation,
        'Pass redirectUri, or set redirectUri in auth.$scheme; the authorization URL needs one.',
      );
    }
    final scope =
        (scopes ?? [for (final s in _list(flow['scopes'])) '$s']).join(' ');
    final fields = <(String, String)>[
      ('response_type', 'code'),
      ('client_id', client.id),
      ('redirect_uri', redirect),
      if (scope.isNotEmpty) ('scope', scope),
      if ((state ?? '').isNotEmpty) ('state', state!),
      if ((codeChallenge ?? '').isNotEmpty) ...[
        ('code_challenge', codeChallenge!),
        ('code_challenge_method', 'S256'),
      ],
    ];
    final reserved = [for (final (k, _) in fields) k];
    final keys = extra.keys.toList()..sort();
    for (final k in keys) {
      if (!reserved.contains(k)) fields.add((k, extra[k]!));
    }
    final base = _str(flow['authorization_url']);
    final sep = base.contains('?')
        ? (base.endsWith('?') || base.endsWith('&') ? '' : '&')
        : '?';
    return Ok(base + sep + _formBody(fields), meta: ResponseMeta.local);
  }

  Future<Result<TokenInfo>> _exchangeCode(
    String scheme, {
    required String code,
    String? redirectUri,
    String? codeVerifier,
  }) async {
    final operation = _oauthOperation(scheme, 'exchangeCode');
    final (flow, reason) = _codeFlow(scheme);
    if (flow == null) {
      return _oauthError(operation, 'Cannot exchange the code: $reason.');
    }
    final (client, why) = _codeClient(scheme);
    if (client == null) {
      return _oauthError(operation, 'Cannot exchange the code: $why.');
    }
    if (code.isEmpty) {
      return _oauthError(
        operation,
        'Pass the authorization code the server redirected back with as code.',
      );
    }
    final redirect =
        (redirectUri ?? '').isEmpty ? client.redirect : redirectUri!;
    if (redirect.isEmpty) {
      return _oauthError(
        operation,
        'Pass redirectUri, or set redirectUri in auth.$scheme; it must equal the one used for the authorization URL.',
      );
    }
    final fields = <(String, String)>[
      ('grant_type', 'authorization_code'),
      ('code', code),
      ('redirect_uri', redirect),
      if ((codeVerifier ?? '').isNotEmpty) ('code_verifier', codeVerifier!),
    ];
    final (stored, failure) = await _tokenRequest(
      scheme,
      _str(flow['token_url']),
      client,
      fields,
      '',
    );
    if (failure != null) return _oauthError(operation, failure.message);
    try {
      await _tokenStore.set(scheme, stored!);
    } catch (_) {
      return _oauthError(operation, _storeFailed.message);
    }
    return Ok(_tokenInfo(scheme, stored), meta: ResponseMeta.local);
  }

  Future<Result<TokenInfo>> _refreshNow(String scheme) async {
    final operation = _oauthOperation(scheme, 'refresh');
    final (_, failure) = await _storedTokenInner(scheme, true);
    if (failure != null) return _oauthError(operation, failure.message);
    final StoredToken? stored;
    try {
      stored = await _readToken(scheme);
    } catch (_) {
      return _oauthError(operation, _storeFailed.message);
    }
    if (stored == null) {
      return _oauthError(operation, 'No token is stored for $scheme.');
    }
    return Ok(_tokenInfo(scheme, stored), meta: ResponseMeta.local);
  }

  static TokenInfo _tokenInfo(String scheme, StoredToken token) => TokenInfo(
        scheme: scheme,
        expiresAt: token.expiresAt,
        scope: token.scope,
        refreshable: (token.refreshToken ?? '').isNotEmpty,
      );
}
