// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Managed browser authorization lasts at most ten monotonic minutes.
const int authorizationAttemptLifetimeMs = 600000;

/// Maximum admitted authorization attempts per client, including claimed work.
const int maxAuthorizationAttempts = 32;

/// Maximum captured configuration bytes per managed authorization attempt.
const int maxAuthorizationAttemptBytes = 32768;

/// Maximum raw and decoded callback bytes, and number of query parameters.
const int maxOAuthCallbackBytes = 16384;
const int maxOAuthCallbackParameters = 64;

/// Default buffered authorization-code and refresh response limit.
const int defaultMaxOAuthTokenResponseBytes = 65536;

/// Pin one mix-up defense before opening the browser; there is no fallback.
sealed class IssuerDefense {
  const IssuerDefense();

  /// Require the callback's decoded `iss` to equal this exact issuer.
  const factory IssuerDefense.issuerResponse(String issuer) = _IssuerResponse;

  /// Caller qualification that this redirect is distinct for this issuer.
  const factory IssuerDefense.distinctRedirect(String qualification) =
      _DistinctRedirect;

  String get _identity;

  @override
  String toString() => 'IssuerDefense($redacted)';
}

final class _IssuerResponse extends IssuerDefense {
  const _IssuerResponse(this._identity);
  @override
  final String _identity;
}

final class _DistinctRedirect extends IssuerDefense {
  const _DistinctRedirect(this._identity);
  @override
  final String _identity;
}

/// A one-way cancellation signal. Cancellation invalidates results but owned
/// HTTP/store work retains admission until it actually settles.
final class OAuthCancellation {
  bool _canceled = false;
  final Set<void Function()> _listeners = {};

  /// Whether cancellation has been requested.
  bool get isCanceled => _canceled;

  /// Idempotently cancel the operation(s) using this signal.
  void cancel() {
    if (_canceled) return;
    _canceled = true;
    final listeners = _listeners.toList();
    _listeners.clear();
    for (final listener in listeners) {
      listener();
    }
  }

  void Function() _listen(void Function() listener) {
    if (_canceled) {
      listener();
      return () {};
    }
    _listeners.add(listener);
    return () => _listeners.remove(listener);
  }

  @override
  String toString() => 'OAuthCancellation(canceled: $_canceled)';
}

/// A managed attempt. Treat its URL and opaque handle as sensitive values.
final class AuthorizationAttempt {
  const AuthorizationAttempt._(this.attemptHandle, this.url, this.expiresInMs);

  /// Opaque, client-owned identity; never derive it from the scheme or state.
  final String attemptHandle;

  /// Open this URL in the owning browser session.
  final String url;

  /// Remaining monotonic lifetime when returned.
  final int expiresInMs;

  @override
  String toString() => 'AuthorizationAttempt($redacted)';
}

final class _OAuthInvalid implements Exception {
  const _OAuthInvalid();
}

/// Checks UTF-16 and counts strict UTF-8 without allocating an encoded copy.
int _oauthBytes(String value, int maximum) {
  if (value.length > maximum) throw const _OAuthInvalid();
  var size = 0;
  for (var i = 0; i < value.length; i++) {
    final c = value.codeUnitAt(i);
    if (c >= 0xd800 && c <= 0xdbff) {
      if (++i >= value.length) throw const _OAuthInvalid();
      final low = value.codeUnitAt(i);
      if (low < 0xdc00 || low > 0xdfff) throw const _OAuthInvalid();
      size += 4;
    } else if (c >= 0xdc00 && c <= 0xdfff) {
      throw const _OAuthInvalid();
    } else {
      size += c <= 0x7f ? 1 : (c <= 0x7ff ? 2 : 3);
    }
    if (size > maximum) throw const _OAuthInvalid();
  }
  return size;
}

final class _OAuthBudget {
  int size = 0;
  String take(String value) {
    size += _oauthBytes(value, maxAuthorizationAttemptBytes);
    if (size > maxAuthorizationAttemptBytes) throw const _OAuthInvalid();
    return value;
  }

  List<String> strings(Iterable<Object?> values) {
    final out = <String>[];
    for (final value in values) {
      if (out.length >= 64 || value is! String) throw const _OAuthInvalid();
      out.add(take(value));
    }
    return List.unmodifiable(out);
  }
}

const Set<String> _oauthReserved = {
  'response_type',
  'client_id',
  'client_secret',
  'redirect_uri',
  'scope',
  'resource',
  'audience',
  'state',
  'code_challenge',
  'code_challenge_method',
  'code_verifier',
  'grant_type',
  'code',
  'iss',
  'error',
  'error_description',
  'error_uri',
  'request',
  'request_uri',
  'response_mode',
};

Map<String, String> _oauthFields(String raw) {
  _oauthBytes(raw, maxOAuthCallbackBytes);
  var count = raw.isEmpty ? 0 : 1;
  for (var i = 0; i < raw.length; i++) {
    if (raw.codeUnitAt(i) == 38 && ++count > maxOAuthCallbackParameters) {
      throw const _OAuthInvalid();
    }
  }
  final out = <String, String>{};
  var size = 0;
  for (final pair in raw.isEmpty ? <String>[] : raw.split('&')) {
    final at = pair.indexOf('=');
    if (at < 1) throw const _OAuthInvalid();
    // Uri.decodeQueryComponent rejects malformed percent/UTF-8 sequences.
    final key = Uri.decodeQueryComponent(pair.substring(0, at));
    final value = Uri.decodeQueryComponent(pair.substring(at + 1));
    size += _oauthBytes(key, maxOAuthCallbackBytes) +
        _oauthBytes(value, maxOAuthCallbackBytes);
    if (size > maxOAuthCallbackBytes || out.containsKey(key)) {
      throw const _OAuthInvalid();
    }
    out[key] = value;
  }
  return out;
}

void _oauthEndpoint(String value, {bool redirect = false}) {
  _oauthBytes(value, maxAuthorizationAttemptBytes);
  if (value.isEmpty || RegExp(r'[\x00-\x20\x7f\\]').hasMatch(value)) {
    throw const _OAuthInvalid();
  }
  // Parse the original query before Uri can normalize malformed escapes.
  final question = value.indexOf('?');
  if (question >= 0) {
    if (redirect) throw const _OAuthInvalid();
    for (final key in _oauthFields(value.substring(question + 1)).keys) {
      if (_oauthReserved.contains(key)) throw const _OAuthInvalid();
    }
  }
  final uri = Uri.parse(value);
  if (!uri.isAbsolute || uri.userInfo.isNotEmpty || value.contains('#')) {
    throw const _OAuthInvalid();
  }
  if (uri.scheme == 'http' || uri.scheme == 'https') {
    if (uri.host.isEmpty) throw const _OAuthInvalid();
  } else if (!redirect) {
    throw const _OAuthInvalid();
  }
}

final class _OAuthConfig {
  const _OAuthConfig(
    this.scheme,
    this.client,
    this.authorization,
    this.token,
    this.refresh,
    this.scopes,
    this.managed,
  );
  final String scheme;
  final _CodeClient client;
  final String authorization;
  final String token;
  final String? refresh;
  final List<String> scopes;
  final bool managed;

  bool same(_OAuthConfig other) =>
      scheme == other.scheme &&
      client.id == other.client.id &&
      client.secret == other.client.secret &&
      client.redirect == other.client.redirect &&
      authorization == other.authorization &&
      token == other.token &&
      refresh == other.refresh &&
      Json.canonical(scopes) == Json.canonical(other.scopes);
}

final class _OAuthPending {
  _OAuthPending(
    this.config,
    this.generation,
    this.binding,
    this.redirect,
    this.defense,
    this.state,
    this.verifier,
    this.started,
    this.scopes,
    this.resource,
    this.audience,
    this.accountContext,
    this.extra,
  );
  final _OAuthConfig config;
  final int generation;
  final String binding;
  final String redirect;
  final IssuerDefense defense;
  final String state;
  String? verifier;
  final int started;
  final List<String> scopes;
  final String? resource;
  final String? audience;
  final String accountContext;
  final List<(String, String)> extra;
  bool claimed = false;
  bool canceled = false;
  Timer? expiry;
  void Function()? detach;
  final OAuthCancellation cancellation = OAuthCancellation();
}

final class _OAuthOwner {
  _OAuthOwner(this.scheme, this.generation, this.epoch, this.deadline);
  final String scheme;
  final int generation;
  final int epoch;
  final int deadline;
  final OAuthCancellation cancellation = OAuthCancellation();
  int? committedEpoch;
  bool get live =>
      !cancellation.isCanceled && _Transport.monotonicMs() < deadline;
  int get remaining => math.max(0, deadline - _Transport.monotonicMs());
}

final class _OAuthRefresh {
  _OAuthRefresh(this.owner, this.force);
  final _OAuthOwner owner;
  bool force;
  bool refreshed = false;
  final Completer<(StoredToken?, _TokenFailure?)> result = Completer();
}

final class _OAuthVersion {
  int generation = 0;
  int epoch = 0;
  int loggingOut = 0;
  bool loggedOut = false;
}
