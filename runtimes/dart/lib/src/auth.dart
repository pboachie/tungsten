// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// What a [TokenStore] keeps for one authorization-code scheme. Its
/// [toString] shows no secret.
final class StoredToken {
  /// A stored token; [expiresAt] is in epoch milliseconds.
  const StoredToken({
    required this.accessToken,
    this.refreshToken,
    this.expiresAt,
    this.tokenType,
    this.scope,
  });

  /// The access token sent as a bearer token.
  final String accessToken;

  /// The refresh token, when the server gave one.
  final String? refreshToken;

  /// When the access token expires, in epoch milliseconds (null: unknown).
  final int? expiresAt;

  /// The token type the server named.
  final String? tokenType;

  /// The scopes the server granted.
  final String? scope;

  @override
  String toString() => 'StoredToken($redacted)';
}

/// Where the tokens of the authorization-code schemes live
/// ([ClientOptions.tokenStore]); a failing store fails the call with
/// AUTH_FAILED, never with an exception.
abstract interface class TokenStore {
  /// The token of a scheme, or null.
  FutureOr<StoredToken?> get(String scheme);

  /// Stores the token of a scheme.
  FutureOr<void> set(String scheme, StoredToken token);

  /// Forgets the token of a scheme.
  FutureOr<void> delete(String scheme);
}

/// The default [TokenStore]: in memory, for the life of the client.
final class MemoryTokenStore implements TokenStore {
  final Map<String, StoredToken> _tokens = {};

  @override
  StoredToken? get(String scheme) => _tokens[scheme];

  @override
  void set(String scheme, StoredToken token) {
    _tokens[scheme] = token;
  }

  @override
  void delete(String scheme) {
    _tokens.remove(scheme);
  }
}

/// The headers (name, value, secret), cookies and query parameters that
/// credentials add.
final class _AuthPlan {
  final List<(String, String, bool)> headers = [];
  final List<(String, String)> cookies = [];
  final List<(String, String)> query = [];
}

/// The plan, the secrets it carries and the authorization-code scheme
/// whose stored token it sends; or the remediation of an AUTH_FAILED
/// envelope ([failed]).
final class _AuthResult {
  _AuthResult(this.plan, this.secrets) : failed = null;

  _AuthResult.failure(String this.failed)
      : plan = _AuthPlan(),
        secrets = _Secrets();

  final _AuthPlan plan;
  final _Secrets secrets;
  final String? failed;
  String? oauth;
}

enum _SatKind { composite, secret, oauthCode, oauth }

/// How one scheme name is satisfied by the configured credentials.
final class _Satisfier {
  _Satisfier.composite(this.name, this.parts, this.config, this.mutation)
      : kind = _SatKind.composite,
        scheme = const {},
        secret = '',
        clientId = '',
        clientSecret = '',
        tokenUrl = '',
        scopes = const [];

  _Satisfier.secret(this.scheme, this.secret)
      : kind = _SatKind.secret,
        name = _str(scheme['name']),
        parts = const [],
        config = const {},
        mutation = false,
        clientId = '',
        clientSecret = '',
        tokenUrl = '',
        scopes = const [];

  _Satisfier.oauthCode(this.name, this.clientSecret)
      : kind = _SatKind.oauthCode,
        scheme = const {},
        parts = const [],
        config = const {},
        mutation = false,
        secret = '',
        clientId = '',
        tokenUrl = '',
        scopes = const [];

  _Satisfier.oauth(
    this.name,
    this.tokenUrl,
    this.scopes,
    this.clientId,
    this.clientSecret,
  )   : kind = _SatKind.oauth,
        scheme = const {},
        parts = const [],
        config = const {},
        mutation = false,
        secret = '';

  final _SatKind kind;
  final String name;
  final List<Map<String, Object?>> parts;
  final Map<String, String?> config;
  final bool mutation;
  final Map<String, Object?> scheme;
  final String secret;
  final String clientId;
  final String clientSecret;
  final String tokenUrl;
  final List<String> scopes;
}

/// (key, satisfier) or the missing credentials, in words.
typedef _Try = (String?, _Satisfier?, List<String>);

/// Auth resolution: an operation's security is an OR of AND-sets of
/// scheme names; the first alternative the configured credentials fully
/// satisfy is applied. A composite profile satisfies the names in its
/// `satisfies` list when every part it needs for this request is
/// configured.
extension _Auth on ClientCore {
  static bool safeMethod(String method) =>
      const ['GET', 'HEAD', 'OPTIONS', 'TRACE'].contains(method);

  static String _compositeKey(Map<String, Object?> part) =>
      switch (part['kind']) {
        'cookie' => _str(part['name']),
        'bearer' => 'bearer',
        _ => _str(part['equals_cookie'] ?? part['from_config'] ?? part['name']),
      };

  static String? _checkPrefix(String token, Object? prefix, String what) {
    if (prefix is! String || prefix.isEmpty || token.startsWith(prefix)) {
      return null;
    }
    return 'The $what credential must start with "$prefix"; check that the right kind of token is configured.';
  }

  static bool _skips(Map<String, Object?> part, bool mutation) =>
      part['kind'] == 'header' && part['mutation_only'] == true && !mutation;

  /// The configured credential of a scheme: a String, a map, or [unset].
  Object? _credential(String name) {
    final c = _auth[name];
    if (c == null) return unset;
    final form = c._form();
    if (form is Map) {
      return <String, String?>{
        for (final e in form.entries) '${e.key}': e.value?.toString(),
      };
    }
    return '$form';
  }

  List<Map<String, Object?>> get _schemes =>
      [for (final s in _list(_api['auth'])) _map(s)];

  _Try _tryComposite(
    String name,
    List<Map<String, Object?>> parts,
    Object? cred,
    String method,
  ) {
    if (cred is! Map<String, String?>) {
      return (
        null,
        null,
        ['auth.$name (an object for the composite profile $name)'],
      );
    }
    final mutation = !safeMethod(method);
    final missing = <String>[];
    for (final p in parts) {
      if (_skips(p, mutation)) continue;
      final key = _compositeKey(p);
      if ((cred[key] ?? '').isNotEmpty) continue;
      final role = switch (p['kind']) {
        'cookie' => 'cookie ${p['name']}',
        'bearer' => 'bearer token',
        _ =>
          'header ${p['name']}${p['equals_cookie'] != null ? ' (equal to cookie ${p['equals_cookie']})' : ''}',
      };
      missing.add(
        'auth.$name[${_Util.jsString(key)}] ($role of the composite profile $name)',
      );
    }
    if (missing.isNotEmpty) return (null, null, missing);
    return (
      'composite:$name',
      _Satisfier.composite(name, parts, cred, mutation),
      const [],
    );
  }

  _Try _tryDirect(Map<String, Object?> scheme, Object? cred) {
    final name = _str(scheme['name']);
    if (scheme['kind'] == 'oauth2' && cred is Map<String, String?>) {
      if (cred['flow'] == 'authorizationCode') {
        if (scheme['authorization_code'] is! Map) {
          return (
            null,
            null,
            [
              'auth.$name (sets flow "authorizationCode", but the API descriptor has no authorizationCode flow for $name)',
            ],
          );
        }
        if ((cred['client_id'] ?? '').isEmpty) {
          return (
            null,
            null,
            ['auth.$name (a client_id for the authorizationCode flow)'],
          );
        }
        return (
          'scheme:$name',
          _Satisfier.oauthCode(name, cred['client_secret'] ?? ''),
          const [],
        );
      }
      final id = cred['client_id'];
      final secret = cred['client_secret'];
      final tokenUrl = scheme['token_url'];
      if (id != null &&
          secret != null &&
          tokenUrl is String &&
          tokenUrl.isNotEmpty) {
        return (
          'scheme:$name',
          _Satisfier.oauth(
            name,
            tokenUrl,
            [for (final s in _list(scheme['scopes'])) '$s'],
            id,
            secret,
          ),
          const [],
        );
      }
      final code = scheme['authorization_code'] is Map
          ? ', or {flow: "authorizationCode", client_id, client_secret?, redirect_uri?}'
          : '';
      return (
        null,
        null,
        [
          'auth.$name (an access token, or {client_id, client_secret} for the token URL$code)',
        ],
      );
    }
    if (cred is String && cred.isNotEmpty) {
      return ('scheme:$name', _Satisfier.secret(scheme, cred), const []);
    }
    final what = switch (scheme['kind']) {
      'http_basic' => '"user:password" for HTTP Basic scheme $name',
      'api_key' =>
        'API key for scheme $name (${scheme['in']} ${scheme['wire']})',
      _ => 'bearer token for scheme $name',
    };
    return (null, null, ['auth.$name ($what)']);
  }

  _Try _tryScheme(String name, String method) {
    final missing = <String>[];
    for (final s in _schemes) {
      if (s['kind'] != 'composite' || !_list(s['satisfies']).contains(name)) {
        continue;
      }
      final profile = _str(s['name']);
      final cred = _credential(profile);
      if (identical(cred, unset)) {
        missing.add('auth.$profile (composite profile $profile)');
        continue;
      }
      final parts = [for (final p in _list(s['parts'])) _map(p)];
      final (key, sat, more) = _tryComposite(profile, parts, cred, method);
      if (sat != null) return (key, sat, const []);
      missing.addAll(more);
    }
    Map<String, Object?>? direct;
    for (final s in _schemes) {
      if (s['name'] == name && s['kind'] != 'composite') {
        direct = s;
        break;
      }
    }
    if (direct != null) {
      final (key, sat, more) = _tryDirect(direct, _credential(name));
      if (sat != null) return (key, sat, const []);
      if (missing.isEmpty) missing.addAll(more);
    } else if (missing.isEmpty) {
      missing
          .add('a scheme named $name (the API descriptor does not define it)');
    }
    return (null, null, missing);
  }

  bool _configured(String name) {
    if (_auth.containsKey(name)) return true;
    return _schemes.any(
      (s) =>
          s['kind'] == 'composite' &&
          _list(s['satisfies']).contains(name) &&
          _auth.containsKey(_str(s['name'])),
    );
  }

  static void _bearer(_AuthPlan plan, String token) =>
      plan.headers.add(('Authorization', 'Bearer $token', true));

  /// Applies one satisfier; a remediation when it cannot be applied.
  Future<String?> _apply(_Satisfier sat, _AuthResult result) async {
    final plan = result.plan;
    switch (sat.kind) {
      case _SatKind.composite:
        for (final p in sat.parts) {
          if (_skips(p, sat.mutation)) continue;
          final value = sat.config[_compositeKey(p)] ?? '';
          switch (p['kind']) {
            case 'cookie':
              plan.cookies.add((_str(p['name']), value));
              result.secrets.add(value);
            case 'bearer':
              final problem =
                  _checkPrefix(value, p['prefix'], '${sat.name} bearer');
              if (problem != null) return problem;
              _bearer(plan, value);
              result.secrets.add(value);
            default:
              final secret = p['from_config'] == null;
              plan.headers.add((_str(p['name']), value, secret));
              if (secret) result.secrets.add(value);
          }
        }
      case _SatKind.secret:
        final scheme = sat.scheme;
        result.secrets.add(sat.secret);
        switch (scheme['kind']) {
          case 'api_key':
            final wire = _str(scheme['wire']);
            switch (scheme['in']) {
              case 'query':
                plan.query.add((wire, sat.secret));
              case 'cookie':
                plan.cookies.add((wire, sat.secret));
              default:
                plan.headers.add((wire, sat.secret, true));
            }
          case 'http_bearer':
            final problem = _checkPrefix(
                sat.secret, scheme['prefix'], _str(scheme['name']));
            if (problem != null) return problem;
            _bearer(plan, sat.secret);
          case 'http_basic':
            final encoded = base64Encode(utf8.encode(sat.secret));
            result.secrets.add(encoded);
            plan.headers.add(('Authorization', 'Basic $encoded', true));
          default:
            _bearer(plan, sat.secret);
        }
      case _SatKind.oauthCode:
        if (sat.clientSecret.isNotEmpty) result.secrets.add(sat.clientSecret);
        final (token, problem) = await _storedToken(sat.name, false);
        if (problem != null) return problem;
        result.secrets.add(token);
        _bearer(plan, token!);
        result.oauth ??= sat.name;
      case _SatKind.oauth:
        result.secrets.add(sat.clientSecret);
        final (token, problem) = await _clientCredentialsToken(sat);
        if (problem != null) return problem;
        result.secrets.add(token);
        _bearer(plan, token!);
    }
    return null;
  }

  /// The credentials of an operation.
  Future<_AuthResult> _resolveAuth(
      OperationDescriptor op, String method) async {
    if (op.security.isEmpty) return _AuthResult(_AuthPlan(), _Secrets());
    List<String>? best;
    var bestTouched = false;
    final ordered = [
      for (final a in op.security)
        if (a.isNotEmpty) a,
      for (final a in op.security)
        if (a.isEmpty) a,
    ];
    for (final alternative in ordered) {
      final sats = <(String, _Satisfier)>[];
      final missing = <String>[];
      for (final name in alternative) {
        final (key, sat, more) = _tryScheme(name, method);
        if (sat == null) {
          missing.addAll(more);
          continue;
        }
        final at = sats.indexWhere((s) => s.$1 == key);
        if (at >= 0) {
          sats[at] = (key!, sat);
        } else {
          sats.add((key!, sat));
        }
      }
      if (missing.isEmpty) {
        final out = _AuthResult(_AuthPlan(), _Secrets());
        for (final (_, sat) in sats) {
          final problem = await _apply(sat, out);
          if (problem != null) {
            return _AuthResult.failure(
              '$problem Operation ${op.id} was not sent.',
            );
          }
        }
        return out;
      }
      final unique = missing.toSet().toList();
      final touched = alternative.any(_configured);
      final better = best == null ||
          (touched && !bestTouched) ||
          (touched == bestTouched && unique.length < best.length);
      if (better) {
        best = unique;
        bestTouched = touched;
      }
    }
    final alternatives = [
      for (final alt in op.security)
        alt.isEmpty ? 'no credentials' : alt.join(' AND '),
    ];
    final missingText = best == null ? 'credentials' : best.join('; ');
    return _AuthResult.failure(
      'Operation ${op.id} needs ${alternatives.join(' OR ')}. Missing: $missingText. '
      'Configure it in ClientOptions.auth and call again; the request was not sent.',
    );
  }
}
