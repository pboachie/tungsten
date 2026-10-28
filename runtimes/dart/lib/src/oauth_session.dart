// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

extension _OAuthSession on ClientCore {
  static const _TokenFailure _unavailable = _TokenFailure(
    'OAuth work is unavailable, canceled, expired or superseded. Begin a new authorization attempt if needed.',
    false,
  );
  static const _TokenFailure _missingToken = _TokenFailure(
    'No OAuth2 token is stored; begin with authorizationUrl(), then call exchangeCode() with its handle, full callback and session binding.',
    false,
  );
  static const _TokenFailure _invalidConfig = _TokenFailure(
    'OAuth requires valid bounded configuration and a FencedTokenStore. Check the authorization-code credential and endpoints. Use HTTPS, or explicitly set ClientOptions.allowInsecureHttp for intentional insecure transport.',
    false,
  );
  static const _TokenFailure _endpointFailure = _TokenFailure(
    'The OAuth token request did not complete within its deadline and byte limit. The code may have been consumed; do not retry it.',
    false,
  );
  static const Set<String> _safeErrors = {
    'invalid_request',
    'invalid_client',
    'invalid_grant',
    'unauthorized_client',
    'unsupported_grant_type',
    'invalid_scope',
  };

  Err<T> _oauthFail<T>(String helper, [_TokenFailure failure = _unavailable]) =>
      _OAuth._oauthError<T>('oauth.$helper', failure.message);

  void _checkOAuthLimit() {
    if (_maxOAuthTokenResponseBytes <= 0 ||
        _maxOAuthTokenResponseBytes > 1048576) {
      throw const _OAuthInvalid();
    }
  }

  _OAuthConfig _captureOAuth(
    String scheme,
    _OAuthBudget budget, {
    bool managed = true,
  }) {
    budget.take(scheme);
    final (flow, _) = _codeFlow(scheme);
    if (flow == null || (managed && _tokenStore is! FencedTokenStore)) {
      throw const _OAuthInvalid();
    }
    _checkOAuthLimit();
    String text(Object? value) {
      if (value is! String) throw const _OAuthInvalid();
      return budget.take(value);
    }

    // Bound caller-owned credential maps before copying them.
    final credential = _auth[scheme];
    final _CodeClient ownedClient;
    if (credential is _AuthorizationCodeCredential) {
      ownedClient = _CodeClient(
          budget.take(credential.clientId),
          budget.take(credential.clientSecret ?? ''),
          budget.take(credential.redirectUri ?? ''));
    } else if (credential is _PartsCredential) {
      final parts = <String, String>{};
      var count = 0;
      for (final entry in credential.parts.entries) {
        if (++count > 64) throw const _OAuthInvalid();
        parts[budget.take(entry.key)] = budget.take(entry.value);
      }
      if (parts['flow'] != 'authorizationCode') throw const _OAuthInvalid();
      ownedClient = _CodeClient(parts['client_id'] ?? '',
          parts['client_secret'] ?? '', parts['redirect_uri'] ?? '');
    } else {
      throw const _OAuthInvalid();
    }
    if (ownedClient.id.isEmpty) throw const _OAuthInvalid();
    final authorization = text(flow['authorization_url']);
    final token = text(flow['token_url']);
    final rawRefresh = flow['refresh_url'];
    final refresh = rawRefresh == null ? null : text(rawRefresh);
    final rawScopes = flow['scopes'];
    if (rawScopes is! List<Object?>) throw const _OAuthInvalid();
    final scopes = budget.strings(rawScopes);
    _oauthEndpoint(authorization);
    _oauthEndpoint(token);
    if (refresh != null) _oauthEndpoint(refresh);
    if (ownedClient.redirect.isNotEmpty)
      _oauthEndpoint(ownedClient.redirect, redirect: true);
    if (_insecureHttp(authorization) ||
        _insecureHttp(token) ||
        (refresh != null && _insecureHttp(refresh)))
      throw const _OAuthInvalid();
    return _OAuthConfig(
      scheme,
      ownedClient,
      authorization,
      token,
      refresh,
      scopes,
      managed,
    );
  }

  bool _sameOAuth(_OAuthConfig config) {
    try {
      return config.same(
        _captureOAuth(config.scheme, _OAuthBudget(), managed: config.managed),
      );
    } catch (_) {
      return false;
    }
  }

  _OAuthVersion _oauthVersion(String scheme) {
    final existing = _oauthVersions[scheme];
    if (existing != null) return existing;
    // Bound all retained scheme identities, not just each individual map.
    if (_oauthVersions.length >= 32) throw const _OAuthInvalid();
    return _oauthVersions[scheme] = _OAuthVersion();
  }

  _OAuthOwner? _reserveOAuth(String scheme, {int? deadline}) {
    if (_oauthOwners.length >= 32) return null;
    try {
      _oauthBytes(scheme, maxAuthorizationAttemptBytes);
    } catch (_) {
      return null;
    }
    final started = _Transport.monotonicMs();
    final until = deadline == null
        ? started + _tokenTimeoutMs
        : math.min(started + _tokenTimeoutMs, deadline);
    if (until <= started) return null;
    final version = _oauthVersions[scheme];
    final owner = _OAuthOwner(
      scheme,
      version?.generation ?? 0,
      version?.epoch ?? 0,
      until,
    );
    _oauthOwners.add(owner);
    return owner;
  }

  bool _ownerCurrent(
    _OAuthOwner owner, {
    bool epoch = true,
    bool login = false,
  }) {
    final version = _oauthVersions[owner.scheme];
    return owner.live &&
        version != null &&
        version.loggingOut == 0 &&
        (login || !version.loggedOut) &&
        version.generation == owner.generation &&
        (!epoch || version.epoch == (owner.committedEpoch ?? owner.epoch));
  }

  bool _pendingCurrent(_OAuthPending pending) {
    final v = _oauthVersions[pending.config.scheme];
    return !pending.canceled &&
        v != null &&
        v.loggingOut == 0 &&
        v.generation == pending.generation &&
        _Transport.monotonicMs() - pending.started <
            authorizationAttemptLifetimeMs;
  }

  void _removeOAuth(String handle, _OAuthPending pending) {
    pending.verifier = null;
    pending.expiry?.cancel();
    pending.detach?.call();
    pending.detach = null;
    if (identical(_oauthAttempts[handle], pending))
      _oauthAttempts.remove(handle);
  }

  void _cancelOAuth(String handle, _OAuthPending pending) {
    pending.canceled = true;
    pending.verifier = null;
    pending.cancellation.cancel();
    pending.expiry?.cancel();
    pending.detach?.call();
    pending.detach = null;
    if (!pending.claimed) _removeOAuth(handle, pending);
  }

  void _sweepOAuth() {
    for (final entry in _oauthAttempts.entries.toList()) {
      if (!_pendingCurrent(entry.value)) _cancelOAuth(entry.key, entry.value);
    }
  }

  Result<AuthorizationAttempt> _beginAuthorization(
    String scheme, {
    required String sessionBinding,
    required IssuerDefense issuerDefense,
    required bool s256Supported,
    String? redirectUri,
    List<String>? scopes,
    String? resource,
    String? audience,
    String? accountContext,
    required Map<String, String> extra,
    OAuthCancellation? cancellation,
  }) {
    _sweepOAuth();
    // Reserve before caller collections/descriptor hooks can reenter us.
    final reservation = _reserveOAuth(scheme);
    if (reservation == null ||
        _oauthAttempts.length >= maxAuthorizationAttempts) {
      if (reservation != null) _oauthOwners.remove(reservation);
      return _oauthFail('authorizationUrl');
    }
    String? handle;
    _OAuthPending? pending;
    try {
      final budget = _OAuthBudget();
      final config = _captureOAuth(scheme, budget);
      final version = _oauthVersion(scheme);
      if (!s256Supported ||
          sessionBinding.isEmpty ||
          issuerDefense._identity.isEmpty ||
          cancellation?.isCanceled == true ||
          version.loggingOut != 0 ||
          reservation.generation != version.generation ||
          !reservation.live) {
        throw const _OAuthInvalid();
      }
      budget.take(sessionBinding);
      budget.take(issuerDefense._identity);
      final redirect = budget.take(redirectUri ?? config.client.redirect);
      _oauthEndpoint(redirect, redirect: true);
      final requested = scopes == null ? config.scopes : budget.strings(scopes);
      final ownedResource = resource == null ? null : budget.take(resource);
      final ownedAudience = audience == null ? null : budget.take(audience);
      final ownedContext = budget.take(accountContext ?? '');
      final fields = <(String, String)>[];
      var count = 0;
      for (final entry in extra.entries) {
        if (++count > 64) throw const _OAuthInvalid();
        final key = budget.take(entry.key);
        final value = budget.take(entry.value);
        if (_oauthReserved.contains(key)) throw const _OAuthInvalid();
        fields.add((key, value));
      }
      final pair = Pkce.generate();
      final state = _Util.base64url(_Util.randomBytes(32));
      handle = _Util.base64url(_Util.randomBytes(32));
      pending = _OAuthPending(
        config,
        version.generation,
        sessionBinding,
        redirect,
        issuerDefense,
        state,
        pair.verifier,
        _Transport.monotonicMs(),
        requested,
        ownedResource,
        ownedAudience,
        ownedContext,
        List.unmodifiable(fields),
      );
      // Captures can call user iterators. Recheck before installing any attempt.
      if (!_sameOAuth(config) ||
          !_pendingCurrent(pending) ||
          !reservation.live ||
          _oauthAttempts.length >= maxAuthorizationAttempts ||
          cancellation?.isCanceled == true ||
          _oauthAttempts.containsKey(handle)) {
        throw const _OAuthInvalid();
      }
      _oauthAttempts[handle] = pending;
      final record = pending;
      final identity = handle;
      pending.expiry = Timer(
        Duration(
            milliseconds: math.max(
                0,
                authorizationAttemptLifetimeMs -
                    (_Transport.monotonicMs() - pending.started))),
        () => _cancelOAuth(identity, record),
      );
      pending.detach = cancellation?._listen(
        () => _cancelOAuth(identity, record),
      );
      final query = _OAuth._formBody([
        ('response_type', 'code'),
        ('client_id', config.client.id),
        ('redirect_uri', redirect),
        if (requested.isNotEmpty) ('scope', requested.join(' ')),
        ('state', state),
        ('code_challenge', pair.challenge),
        ('code_challenge_method', 'S256'),
        if (ownedResource != null) ('resource', ownedResource),
        if (ownedAudience != null) ('audience', ownedAudience),
        ...fields,
      ]);
      final base = config.authorization;
      final separator = !base.contains('?')
          ? '?'
          : (base.endsWith('?') || base.endsWith('&') ? '' : '&');
      return Ok(
        AuthorizationAttempt._(
          handle,
          '$base$separator$query',
          math.max(
            0,
            authorizationAttemptLifetimeMs -
                (_Transport.monotonicMs() - pending.started),
          ),
        ),
        meta: ResponseMeta.local,
      );
    } catch (_) {
      if (handle != null && pending != null) _removeOAuth(handle, pending);
      return _oauthFail('authorizationUrl', _invalidConfig);
    } finally {
      _oauthOwners.remove(reservation);
    }
  }

  Result<void> _cancelAuthorization(
    String scheme,
    String handle,
    String binding,
  ) {
    if (handle.length != 43 || binding.length > maxAuthorizationAttemptBytes)
      return _oauthFail('cancelAuthorization');
    final pending = _oauthAttempts[handle];
    if (pending == null ||
        pending.config.scheme != scheme ||
        pending.binding != binding) {
      return _oauthFail('cancelAuthorization');
    }
    _cancelOAuth(handle, pending);
    return const Ok(null, meta: ResponseMeta.local);
  }

  void _cancelAllOAuth(String scheme) {
    if (scheme.length > maxAuthorizationAttemptBytes) return;
    final version = _oauthVersions[scheme];
    if (version != null) {
      version.generation++;
      version.epoch++;
    }
    for (final entry in _oauthAttempts.entries.toList()) {
      if (entry.value.config.scheme == scheme)
        _cancelOAuth(entry.key, entry.value);
    }
    for (final owner in _oauthOwners.toList()) {
      if (owner.scheme == scheme) owner.cancellation.cancel();
    }
  }

  Future<Result<TokenInfo>> _completeAuthorization(
    String scheme,
    String handle,
    String callback,
    String binding,
    OAuthCancellation? cancellation,
  ) async {
    if (handle.length != 43 || binding.length > maxAuthorizationAttemptBytes)
      return _oauthFail('exchangeCode');
    _sweepOAuth();
    final pending = _oauthAttempts[handle];
    if (pending == null ||
        pending.claimed ||
        pending.config.scheme != scheme ||
        binding != pending.binding ||
        !_pendingCurrent(pending) ||
        cancellation?.isCanceled == true) return _oauthFail('exchangeCode');
    String? code;
    try {
      _oauthBytes(callback, maxOAuthCallbackBytes);
      final question = callback.indexOf('?');
      if (question < 0 ||
          callback.contains('#') ||
          callback.substring(0, question) != pending.redirect)
        throw const _OAuthInvalid();
      final fields = _oauthFields(callback.substring(question + 1));
      if (fields['state'] != pending.state ||
          (pending.defense is _IssuerResponse &&
              fields['iss'] != pending.defense._identity)) {
        throw const _OAuthInvalid();
      }
      code = fields['code'];
      final error = fields['error'];
      if ((code == null) == (error == null) ||
          code == '' ||
          error == '' ||
          (code != null &&
              (fields.containsKey('error_description') ||
                  fields.containsKey('error_uri')))) {
        throw const _OAuthInvalid();
      }
      if (!_sameOAuth(pending.config) ||
          !_pendingCurrent(pending) ||
          pending.claimed ||
          cancellation?.isCanceled == true) throw const _OAuthInvalid();
      if (error != null) {
        pending.claimed = true;
        _removeOAuth(handle, pending);
        return _oauthFail(
          'exchangeCode',
          const _TokenFailure(
            'Authorization was denied. Begin a new authorization attempt.',
            false,
          ),
        );
      }
    } catch (_) {
      return _oauthFail('exchangeCode');
    }
    final owner = _reserveOAuth(scheme);
    if (owner == null) return _oauthFail('exchangeCode');
    pending.claimed = true; // No await separates validation and one-shot claim.
    final verifier = pending.verifier;
    pending.verifier = null;
    final detachPending = pending.cancellation._listen(
      owner.cancellation.cancel,
    );
    final detachCaller = cancellation?._listen(
      () => _cancelOAuth(handle, pending),
    );
    try {
      if (verifier == null ||
          !_ownerCurrent(owner, epoch: false, login: true) ||
          !_pendingCurrent(pending)) return _oauthFail('exchangeCode');
      final (stored, failure) = await _requestOAuthToken(
        owner,
        pending.config,
        pending.config.token,
        [
          ('grant_type', 'authorization_code'),
          ('code', code!),
          ('redirect_uri', pending.redirect),
          ('code_verifier', verifier),
        ],
        '',
      );
      if (failure != null || stored == null)
        return _oauthFail('exchangeCode', _endpointFailure);
      var calls = 0;
      final committed = await (_tokenStore as FencedTokenStore).setIfCurrent(
        scheme,
        stored,
        (_) {
          if (++calls != 1 ||
              !_sameOAuth(pending.config) ||
              !_pendingCurrent(pending) ||
              !_ownerCurrent(owner, epoch: false, login: true)) return false;
          final version = _oauthVersions[scheme]!;
          owner.committedEpoch = ++version.epoch;
          return true;
        },
      );
      final info = _OAuth._tokenInfo(scheme, stored);
      if (!committed ||
          calls != 1 ||
          !_sameOAuth(pending.config) ||
          !_pendingCurrent(pending) ||
          !_ownerCurrent(owner, login: true)) return _oauthFail('exchangeCode');
      // A failed commit must not reopen a previously logged-out credential.
      _oauthVersions[scheme]!.loggedOut = false;
      return Ok(info, meta: ResponseMeta.local);
    } catch (_) {
      return _oauthFail('exchangeCode');
    } finally {
      detachPending();
      detachCaller?.call();
      _removeOAuth(handle, pending);
      _oauthOwners.remove(owner);
    }
  }

  Future<Result<void>> _logoutOAuth(String scheme) async {
    if (scheme.length > maxAuthorizationAttemptBytes)
      return _oauthFail('logout');
    // An existing identity is fenced before admission or any user/store work.
    var version = _oauthVersions[scheme];
    if (version == null) {
      final preflight = _reserveOAuth(scheme);
      if (preflight == null) return _oauthFail('logout');
      try {
        _captureOAuth(scheme, _OAuthBudget());
        version = _oauthVersion(scheme);
      } catch (_) {
        return _oauthFail('logout', _invalidConfig);
      } finally {
        _oauthOwners.remove(preflight);
      }
    }
    final activeVersion = version;
    activeVersion.generation++;
    activeVersion.epoch++;
    activeVersion.loggedOut = true;
    for (final entry in _oauthAttempts.entries.toList()) {
      if (entry.value.config.scheme == scheme)
        _cancelOAuth(entry.key, entry.value);
    }
    for (final owner in _oauthOwners.toList()) {
      if (owner.scheme == scheme) owner.cancellation.cancel();
    }
    final owner = _reserveOAuth(scheme);
    if (owner == null) return _oauthFail('logout');
    activeVersion.loggingOut++;
    try {
      if (_tokenStore is! FencedTokenStore)
        return _oauthFail('logout', _invalidConfig);
      var calls = 0;
      final committed = await _tokenStore.deleteIfCurrent(
        scheme,
        (_) {
          if (++calls != 1 ||
              !owner.live ||
              activeVersion.generation != owner.generation) return false;
          owner.committedEpoch = ++activeVersion.epoch;
          return true;
        },
      );
      if (!committed ||
          calls != 1 ||
          !owner.live ||
          activeVersion.generation != owner.generation) {
        return _oauthFail('logout');
      }
      return const Ok(null, meta: ResponseMeta.local);
    } catch (_) {
      return _oauthFail('logout', _OAuth._storeFailed);
    } finally {
      activeVersion.loggingOut--;
      _oauthOwners.remove(owner);
    }
  }

  Future<(String?, String?)> _storedToken(String scheme, bool force) async {
    final (stored, failure) = await _managedStored(scheme, force);
    return (stored?.accessToken, failure?.message);
  }

  Future<String?> _refreshAfterRejection(String scheme, String rejected) async {
    final (stored, failure) = await _managedStored(
      scheme,
      true,
      rejected: rejected,
    );
    return failure == null ? stored?.accessToken : null;
  }

  Future<Result<TokenInfo>> _managedRefresh(String scheme) async {
    final (stored, failure) = await _managedStored(scheme, true);
    if (failure != null || stored == null)
      return _oauthFail('refresh', failure ?? _unavailable);
    return Ok(_OAuth._tokenInfo(scheme, stored), meta: ResponseMeta.local);
  }

  Future<(StoredToken?, _TokenFailure?)> _managedStored(
    String scheme,
    bool force, {
    String? rejected,
  }) async {
    if (scheme.length > maxAuthorizationAttemptBytes)
      return (null, _unavailable);
    if (_oauthWaiters >= 32) return (null, _unavailable);
    _oauthWaiters++;
    final deadline = _Transport.monotonicMs() + _tokenTimeoutMs;
    try {
      while (true) {
        if (_Transport.monotonicMs() >= deadline) return (null, _unavailable);
        // A version-keyed owner includes the first custom Get and keeps admission
        // through its final store return, even when every follower has left.
        final known = _oauthVersions[scheme];
        final key = (scheme, known?.generation ?? 0, known?.epoch ?? 0);
        var flight = _oauthRefreshes[key];
        if (flight == null) {
          final owner = _reserveOAuth(scheme, deadline: deadline);
          if (owner == null) return (null, _unavailable);
          flight = _OAuthRefresh(owner, force);
          _oauthRefreshes[key] = flight;
          unawaited(_runOAuthRefresh(key, flight, rejected));
        } else if (force) {
          flight.force = true;
        }
        final remaining = math.max(0, deadline - _Transport.monotonicMs());
        final (stored, failure) = await flight.result.future.timeout(
          Duration(milliseconds: remaining),
          onTimeout: () => (null, _unavailable),
        );
        if (failure != null || stored == null)
          return (null, failure ?? _unavailable);
        if (!_ownerCurrent(flight.owner) ||
            _Transport.monotonicMs() >= deadline) return (null, _unavailable);
        // A force join racing a completed fresh-read decision must not accept the
        // rejected credential without doing a fresh version-keyed refresh.
        if (force &&
            !flight.refreshed &&
            (rejected == null || stored.accessToken == rejected)) {
          if (identical(_oauthRefreshes[key], flight))
            _oauthRefreshes.remove(key);
          continue;
        }
        return (stored, null);
      }
    } catch (_) {
      return (null, _unavailable);
    } finally {
      _oauthWaiters--;
    }
  }

  Future<void> _runOAuthRefresh(
    (String, int, int) key,
    _OAuthRefresh flight,
    String? rejected,
  ) async {
    (StoredToken?, _TokenFailure?) result = (null, _unavailable);
    final owner = flight.owner;
    try {
      final config = _captureOAuth(owner.scheme, _OAuthBudget());
      _oauthVersion(owner.scheme);
      if (!_ownerCurrent(owner)) return;
      final stored = await _tokenStore.get(owner.scheme);
      if (!_validStoredOAuth(stored) ||
          !_sameOAuth(config) ||
          !_ownerCurrent(owner)) return;
      if (stored == null || stored.accessToken.isEmpty) {
        result = (null, _missingToken);
        return;
      }
      final currentTime = now; // Custom clock is inside the admitted owner.
      if (!_ownerCurrent(owner)) return;
      if ((rejected != null && stored.accessToken != rejected) ||
          (!flight.force &&
              (stored.expiresAt == null ||
                  stored.expiresAt! > currentTime + _expirySkewMs))) {
        result = (stored, null);
        return;
      }
      if ((stored.refreshToken ?? '').isEmpty) {
        result = (null, _missingToken);
        return;
      }
      flight.refreshed = true;
      final (renewed, failure) = await _requestOAuthToken(
        owner,
        config,
        config.refresh ?? config.token,
        [
          ('grant_type', 'refresh_token'),
          ('refresh_token', stored.refreshToken!),
        ],
        stored.refreshToken!,
      );
      if (!_sameOAuth(config) || !_ownerCurrent(owner)) return;
      final fenced = _tokenStore as FencedTokenStore;
      var calls = 0;
      bool current(StoredToken? currentToken) {
        if (++calls != 1 ||
            !_sameStoredOAuth(currentToken, stored) ||
            !_sameOAuth(config) ||
            !_ownerCurrent(owner)) return false;
        owner.committedEpoch = ++_oauthVersions[owner.scheme]!.epoch;
        return true;
      }

      if (failure != null) {
        if (failure.rejected)
          await fenced.deleteIfCurrent(owner.scheme, current);
        result = (null, failure);
        return;
      }
      if (renewed == null) return;
      final committed = await fenced.setIfCurrent(
        owner.scheme,
        renewed,
        current,
      );
      if (committed && calls == 1 && _sameOAuth(config) && _ownerCurrent(owner))
        result = (renewed, null);
    } catch (_) {
      result = (null, _OAuth._storeFailed);
    } finally {
      if (!flight.result.isCompleted) flight.result.complete(result);
      if (identical(_oauthRefreshes[key], flight)) _oauthRefreshes.remove(key);
      _oauthOwners.remove(owner);
    }
  }

  bool _sameStoredOAuth(StoredToken? current, StoredToken expected) =>
      current != null &&
      current.accessToken == expected.accessToken &&
      current.refreshToken == expected.refreshToken &&
      current.expiresAt == expected.expiresAt &&
      current.tokenType == expected.tokenType &&
      current.scope == expected.scope;

  bool _validStoredOAuth(StoredToken? token) {
    if (token == null) return true;
    for (final value in [
      token.accessToken,
      token.refreshToken ?? '',
      token.scope ?? '',
      token.tokenType ?? '',
    ]) {
      _oauthBytes(value, _maxOAuthTokenResponseBytes);
    }
    return true;
  }

  Future<(StoredToken?, _TokenFailure?)> _requestOAuthToken(
    _OAuthOwner owner,
    _OAuthConfig config,
    String url,
    List<(String, String)> fields,
    String previous,
  ) async {
    try {
      _oauthEndpoint(url);
      _checkOAuthLimit();
      if (_insecureHttp(url) ||
          !owner.live ||
          !_sameOAuth(config) ||
          !owner.live) {
        return (null, _unavailable);
      }
      final headers = <(String, String)>[
        ('Content-Type', 'application/x-www-form-urlencoded'),
        ('Accept', 'application/json'),
      ];
      final all = [...fields];
      if (config.client.secret.isEmpty) {
        all.add(('client_id', config.client.id));
      } else {
        final basic = base64Encode(
          utf8.encode(
            '${_Serialize.percentEncode(config.client.id)}:${_Serialize.percentEncode(config.client.secret)}',
          ),
        );
        headers.add(('Authorization', 'Basic $basic'));
      }
      final body = _Encoded(
        empty: false,
        bytes: utf8.encode(_OAuth._formBody(all)),
      );
      if (!owner.live) return (null, _unavailable);
      final out = await _Transport.attempt(
        url: url,
        method: 'POST',
        headers: headers,
        body: body,
        timeoutMs: owner.remaining,
        maxResponseBytes: _maxOAuthTokenResponseBytes,
        oauthCancellation: owner.cancellation,
        oauthDeadline: owner.deadline,
      );
      if (!owner.live || out.kind != _Kind.response || out.body == null)
        return (null, _endpointFailure);
      final decoded = jsonDecode(utf8.decode(out.body!));
      if (!owner.live || decoded is! Map<String, Object?>)
        return (null, _endpointFailure);
      if (out.status < 200 || out.status > 299) {
        final error = decoded['error'];
        final label =
            error is String && _safeErrors.contains(error) ? ' ($error)' : '';
        return (
          null,
          _TokenFailure(
            'The OAuth token endpoint rejected the request$label. Check credentials and authorize again.',
            out.status >= 400 && out.status <= 499,
          ),
        );
      }
      if (decoded.containsKey('error') ||
          decoded.containsKey('error_description') ||
          decoded.containsKey('error_uri')) {
        return (null, _endpointFailure);
      }
      String text(String field) {
        final value = decoded[field];
        if (value == null) return '';
        if (value is! String) throw const _OAuthInvalid();
        _oauthBytes(value, _maxOAuthTokenResponseBytes);
        return value;
      }

      final access = text('access_token');
      if (access.isEmpty) return (null, _endpointFailure);
      final refresh = text('refresh_token');
      final type = text('token_type');
      final scope = text('scope');
      final expiresIn = decoded['expires_in'];
      final epochNow = now;
      if (!owner.live) return (null, _endpointFailure);
      final expires = expiresIn is num &&
              expiresIn.isFinite &&
              expiresIn >= 0 &&
              expiresIn <= 9007199254740
          ? epochNow + (expiresIn * 1000).truncate()
          : null;
      final stored = StoredToken(
        accessToken: access,
        refreshToken:
            refresh.isEmpty ? (previous.isEmpty ? null : previous) : refresh,
        expiresAt: expires,
        tokenType: type.isEmpty ? null : type,
        scope: scope.isEmpty ? null : scope,
      );
      return owner.live ? (stored, null) : (null, _endpointFailure);
    } catch (_) {
      return (null, _endpointFailure);
    }
  }

  Future<Result<TokenInfo>> _lowLevelExchange(
    String scheme,
    String code,
    String? redirectUri,
    String? verifier,
  ) async {
    final owner = _reserveOAuth(scheme);
    if (owner == null) return _oauthFail('lowLevelExchangeCode');
    try {
      // The explicit escape can use a legacy store, but still has bounded input,
      // network and admission. It deliberately bypasses managed mutation epochs.
      final budget = _OAuthBudget();
      final config = _captureOAuth(scheme, budget, managed: false);
      final redirect = budget.take(redirectUri ?? config.client.redirect);
      _oauthEndpoint(redirect, redirect: true);
      budget.take(code);
      budget.take(verifier ?? '');
      if (code.isEmpty) return _oauthFail('lowLevelExchangeCode');
      final (stored, failure) = await _requestOAuthToken(
        owner,
        config,
        config.token,
        [
          ('grant_type', 'authorization_code'),
          ('code', code),
          ('redirect_uri', redirect),
          if (verifier != null && verifier.isNotEmpty)
            ('code_verifier', verifier),
        ],
        '',
      );
      if (failure != null || stored == null)
        return _oauthFail('lowLevelExchangeCode', failure ?? _endpointFailure);
      await _tokenStore.set(scheme, stored);
      if (!owner.live) return _oauthFail('lowLevelExchangeCode');
      return Ok(_OAuth._tokenInfo(scheme, stored), meta: ResponseMeta.local);
    } catch (_) {
      return _oauthFail('lowLevelExchangeCode', _invalidConfig);
    } finally {
      _oauthOwners.remove(owner);
    }
  }
}
