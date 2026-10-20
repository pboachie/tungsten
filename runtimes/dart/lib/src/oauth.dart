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

  /// The code verifier, passed to [OAuthFlow.lowLevelExchangeCode].
  final String verifier;

  /// The code challenge, passed to [OAuthFlow.lowLevelAuthorizationUrl].
  final String challenge;

  @override
  String toString() => 'Pkce($redacted)';
}

/// What [OAuthFlow.lowLevelExchangeCode] and [OAuthFlow.refresh] report: no
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
  String toString() => 'TokenInfo($redacted, refreshable: $refreshable)';
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

  /// Explicit low-level PKCE primitive; managed authorization generates its own.
  Pkce pkce() => Pkce.generate();

  /// Begin a managed S256/state attempt bound to the owning browser/listener.
  Result<AuthorizationAttempt> authorizationUrl({
    required String sessionBinding,
    required IssuerDefense issuerDefense,
    required bool s256Supported,
    String? redirectUri,
    List<String>? scopes,
    String? resource,
    String? audience,
    String? accountContext,
    Map<String, String> extra = const {},
    OAuthCancellation? cancellation,
  }) =>
      _core._beginAuthorization(
        scheme,
        sessionBinding: sessionBinding,
        issuerDefense: issuerDefense,
        s256Supported: s256Supported,
        redirectUri: redirectUri,
        scopes: scopes,
        resource: resource,
        audience: audience,
        accountContext: accountContext,
        extra: extra,
        cancellation: cancellation,
      );

  /// Validate and consume one full callback. A claimed code is never retried.
  Future<Result<TokenInfo>> exchangeCode({
    required String attemptHandle,
    required String callbackUrl,
    required String sessionBinding,
    OAuthCancellation? cancellation,
  }) =>
      _core._completeAuthorization(
        scheme,
        attemptHandle,
        callbackUrl,
        sessionBinding,
        cancellation,
      );

  /// Invalidate an attempt; claimed work keeps admission until settlement.
  Result<void> cancelAuthorization(
    String attemptHandle,
    String sessionBinding,
  ) =>
      _core._cancelAuthorization(scheme, attemptHandle, sessionBinding);

  /// Synchronously invalidate this scheme's pending authorization and token
  /// work and dispose its attempt timers. Claimed work retains admission until
  /// settlement. Stored credentials are not deleted; use [logout] for that.
  void cancelAllAuthorization() => _core._cancelAllOAuth(scheme);

  /// Invalidate local attempts/credentials before attempting store removal.
  /// This does not revoke remote provider tokens.
  Future<Result<void>> logout() => _core._logoutOAuth(scheme);

  /// Explicit compatibility primitive: caller owns PKCE, state and issuer checks.
  Result<String> lowLevelAuthorizationUrl({
    String? redirectUri,
    List<String>? scopes,
    String? state,
    String? codeChallenge,
    Map<String, String> extra = const {},
  }) =>
      _core._lowLevelAuthorizationUrl(
        scheme,
        redirectUri: redirectUri,
        scopes: scopes,
        state: state,
        codeChallenge: codeChallenge,
        extra: extra,
      );

  /// Explicit unbound exchange; it bypasses managed credential epochs.
  Future<Result<TokenInfo>> lowLevelExchangeCode({
    required String code,
    String? redirectUri,
    String? codeVerifier,
  }) =>
      _core._lowLevelExchange(scheme, code, redirectUri, codeVerifier);

  /// Refresh the managed stored token now, with generation/epoch fencing.
  Future<Result<TokenInfo>> refresh() => _core._managedRefresh(scheme);
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

  static Err<T> _oauthError<T>(String operation, String remediation) =>
      _diag(operation, 'AUTH_FAILED').remedy(remediation).err<T>();

  static const _TokenFailure _storeFailed = _TokenFailure(
    'The token store failed (it threw an error); fix or replace ClientOptions.tokenStore.',
    false,
  );

  int get _tokenTimeoutMs => math.max(_timeoutMs, 1);

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
    if (_insecureHttp(sat.tokenUrl))
      return (null, ClientCore._insecureHttpAuth);
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
      maxResponseBytes: 1024 * 1024,
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
    final decoded = _Classify.decodeBody(
      out.body!,
      out.headers,
      'application/json',
    );
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

  Result<String> _lowLevelAuthorizationUrl(
    String scheme, {
    String? redirectUri,
    List<String>? scopes,
    String? state,
    String? codeChallenge,
    Map<String, String> extra = const {},
  }) {
    final owner = _reserveOAuth(scheme);
    if (owner == null) return _oauthFail('lowLevelAuthorizationUrl');
    try {
      final budget = _OAuthBudget();
      final config = _captureOAuth(scheme, budget, managed: false);
      final redirect = budget.take(redirectUri ?? config.client.redirect);
      _oauthEndpoint(redirect, redirect: true);
      final requested = scopes == null ? config.scopes : budget.strings(scopes);
      budget.take(state ?? '');
      budget.take(codeChallenge ?? '');
      final fields = <(String, String)>[
        ('response_type', 'code'),
        ('client_id', config.client.id),
        ('redirect_uri', redirect),
        if (requested.isNotEmpty) ('scope', requested.join(' ')),
        if (state != null && state.isNotEmpty) ('state', state),
        if (codeChallenge != null && codeChallenge.isNotEmpty) ...[
          ('code_challenge', codeChallenge),
          ('code_challenge_method', 'S256'),
        ],
      ];
      var count = 0;
      final ownedExtra = <String, String>{};
      for (final e in extra.entries) {
        if (++count > 64) throw const _OAuthInvalid();
        budget.take(e.key);
        budget.take(e.value);
        if (_oauthReserved.contains(e.key)) throw const _OAuthInvalid();
        ownedExtra[e.key] = e.value;
      }
      final keys = ownedExtra.keys.toList()..sort();
      for (final k in keys) {
        fields.add((k, ownedExtra[k]!));
      }
      if (!owner.live || !_sameOAuth(config) || !owner.live)
        throw const _OAuthInvalid();
      final base = config.authorization;
      final separator = !base.contains('?')
          ? '?'
          : (base.endsWith('?') || base.endsWith('&') ? '' : '&');
      return Ok('$base$separator${_formBody(fields)}',
          meta: ResponseMeta.local);
    } catch (_) {
      return _oauthFail(
          'lowLevelAuthorizationUrl', _OAuthSession._invalidConfig);
    } finally {
      _oauthOwners.remove(owner);
    }
  }

  static TokenInfo _tokenInfo(String scheme, StoredToken token) => TokenInfo(
        scheme: scheme,
        expiresAt: token.expiresAt,
        scope: token.scope,
        refreshable: (token.refreshToken ?? '').isNotEmpty,
      );
}
