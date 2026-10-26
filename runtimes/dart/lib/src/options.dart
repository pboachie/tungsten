// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// Thrown when a client cannot be built (a base URL that is not an absolute
/// http(s) URL without credentials). Calls never throw; they return [Err].
final class ClientConfigException implements Exception {
  /// A client configuration problem.
  const ClientConfigException(this.message);

  /// What is wrong.
  final String message;

  @override
  String toString() => 'ClientConfigException: $message';
}

/// How responses are checked against the API description.
enum ValidateResponses {
  /// Not checked.
  off,

  /// Reported through [ClientOptions.onDiagnostic]; the call succeeds (the
  /// default).
  warn,

  /// A body that does not match fails the call with UNEXPECTED_RESPONSE.
  strict,
}

/// Which redirects a client follows. Mutations never follow one.
enum RedirectPolicy {
  /// Reads follow redirects that stay on the request's origin, by hand (at
  /// most five), with the credentials re-applied (the default).
  sameOriginRead,

  /// No redirect is followed: a 3xx the operation does not declare fails
  /// the call with UNEXPECTED_RESPONSE.
  none,
}

/// The randomization of retry delays.
enum Jitter {
  /// The full backoff ceiling.
  none,

  /// A random delay between zero and the ceiling (the default).
  full,

  /// Half the ceiling plus a random half.
  equal,
}

/// Retry settings over the API's per-tier defaults; null fields are unset.
final class RetryOptions {
  /// Retry settings.
  const RetryOptions({
    this.max,
    this.base,
    this.maxDelay,
    this.jitter,
    this.honorRetryAfter,
  });

  /// Attempts after the first.
  final int? max;

  /// The first backoff ceiling.
  final Duration? base;

  /// The largest backoff ceiling (and the largest Retry-After honoured).
  final Duration? maxDelay;

  /// How delays are randomized.
  final Jitter? jitter;

  /// Whether a server's Retry-After sets the delay.
  final bool? honorRetryAfter;
}

/// The credential of one auth scheme ([ClientOptions.auth]).
sealed class Credential {
  const Credential();

  /// A bearer token, API key, or `user:password` for HTTP Basic.
  const factory Credential.secret(String value) = _SecretCredential;

  /// The parts of a composite profile, by cookie name or config key.
  const factory Credential.parts(Map<String, String> parts) = _PartsCredential;

  /// OAuth2 client credentials for the scheme's token URL.
  const factory Credential.clientCredentials({
    required String clientId,
    required String clientSecret,
  }) = _ClientCredentials;

  /// The OAuth2 authorization-code flow of the scheme ([OAuthFlow]).
  const factory Credential.authorizationCode({
    required String clientId,
    String? clientSecret,
    String? redirectUri,
  }) = _AuthorizationCodeCredential;

  /// The credential as the auth resolution reads it: a string or a map.
  Object _form();

  @override
  String toString() => 'Credential($redacted)';
}

final class _SecretCredential extends Credential {
  const _SecretCredential(this.value);

  final String value;

  @override
  Object _form() => value;
}

final class _PartsCredential extends Credential {
  const _PartsCredential(this.parts);

  final Map<String, String> parts;

  @override
  Object _form() => <String, String?>{...parts};
}

final class _ClientCredentials extends Credential {
  const _ClientCredentials({
    required this.clientId,
    required this.clientSecret,
  });

  final String clientId;
  final String clientSecret;

  @override
  Object _form() => <String, String?>{
        'client_id': clientId,
        'client_secret': clientSecret,
      };
}

final class _AuthorizationCodeCredential extends Credential {
  const _AuthorizationCodeCredential({
    required this.clientId,
    this.clientSecret,
    this.redirectUri,
  });

  final String clientId;
  final String? clientSecret;
  final String? redirectUri;

  @override
  Object _form() => <String, String?>{
        'flow': 'authorizationCode',
        'client_id': clientId,
        'client_secret': clientSecret,
        'redirect_uri': redirectUri,
      };
}

/// What middleware sees of one attempt; secrets are redacted. [body] is the
/// body as JSON when it is JSON, a text rendering otherwise; [hasBody] is
/// false without one.
final class RequestContext {
  /// One attempt.
  const RequestContext({
    required this.operation,
    required this.attempt,
    required this.method,
    required this.url,
    required this.headers,
    required this.hasBody,
    this.body,
  });

  /// The operation id.
  final String operation;

  /// The attempt, from 1.
  final int attempt;

  /// The HTTP method.
  final String method;

  /// The URL, sensitive query values redacted.
  final String url;

  /// The headers, secrets redacted.
  final Map<String, String> headers;

  /// Whether the request has a body.
  final bool hasBody;

  /// The redacted body.
  final Object? body;

  RequestContext _with({String? url, String? method}) => RequestContext(
        operation: operation,
        attempt: attempt,
        method: method ?? this.method,
        url: url ?? this.url,
        headers: headers,
        hasBody: hasBody,
        body: body,
      );
}

/// What middleware sees of an answer.
final class ResponseContext {
  /// An answer.
  const ResponseContext({required this.status, required this.headers});

  /// The HTTP status.
  final int status;

  /// The headers by lower-cased name.
  final Map<String, String> headers;
}

/// Observers of every attempt. A failing observer never changes the call.
final class Middleware {
  /// Hooks, each optional.
  const Middleware({this.onRequest, this.onResponse, this.onRetry});

  /// Before each attempt is sent.
  final void Function(RequestContext ctx)? onRequest;

  /// When an answer arrives.
  final void Function(RequestContext ctx, ResponseContext response)? onResponse;

  /// Before a retry, with the failure that caused it.
  final void Function(RequestContext ctx, Diagnostic reason)? onRetry;
}

/// Client options, every one optional.
final class ClientOptions {
  /// Client options.
  const ClientOptions({
    this.allowInsecureHttp = false,
    this.baseUrl,
    this.redirects,
    this.auth = const {},
    this.tokenStore,
    this.maxOAuthTokenResponseBytes,
    this.timeout,
    this.deadline,
    this.retries,
    this.idempotencyStore,
    this.middleware = const [],
    this.validateResponses = ValidateResponses.warn,
    this.onDiagnostic,
    this.headers = const {},
    this.maxEventBytes,
    this.maxResponseBytes,
    this.maxCollectBytes,
    this.maxCollect,
    this.maxReconnects,
    this.reconnectMax,
    this.idleTimeout,
    this.confirmationKey,
    this.now,
    this.random,
  });

  /// The API's base URL; defaults to its first server.
  final String? baseUrl;

  /// Explicitly permits credentials over non-loopback HTTP, including OAuth.
  /// Default false; localhost, 127/8 and ::1 remain usable locally.
  final bool allowInsecureHttp;

  /// Which redirects are followed; the API's own policy applies when null,
  /// and either one set to [RedirectPolicy.none] disables them.
  final RedirectPolicy? redirects;

  /// Credentials by auth scheme name.
  final Map<String, Credential> auth;

  /// Where authorization-code tokens live (memory by default).
  final TokenStore? tokenStore;

  /// Authorization-code/refresh response bytes (64 KiB by default). Must be a
  /// positive integer no larger than 1 MiB; independent of ordinary responses.
  final int? maxOAuthTokenResponseBytes;

  /// The per-attempt timeout (30 s).
  final Duration? timeout;

  /// The total deadline of one logical call, see [CallOptions.deadline].
  /// Default: none (only [timeout] per attempt).
  final Duration? deadline;

  /// Retry settings over the API's per-tier defaults.
  final RetryOptions? retries;

  /// Where automatic idempotency keys live (memory by default).
  final IdempotencyStore? idempotencyStore;

  /// Observers of every attempt.
  final List<Middleware> middleware;

  /// How responses are checked (warn).
  final ValidateResponses validateResponses;

  /// Called with the envelopes of warnings.
  final void Function(Diagnostic diagnostic)? onDiagnostic;

  /// Sent on every request.
  final Map<String, String> headers;

  /// The largest event of a stream, in UTF-8 bytes (1 MiB).
  final int? maxEventBytes;

  /// Maximum ordinary response bytes; null inherits the 4 MiB default.
  /// Must be a positive safe integer. SSE uses [maxEventBytes] instead.
  final int? maxResponseBytes;

  /// The most event JSON [EventStream.collect] keeps (4 MiB).
  final int? maxCollectBytes;

  /// The longest [EventStream.collect] runs (60 s).
  final Duration? maxCollect;

  /// How often a dropped stream is reconnected (3; 0 turns it off).
  final int? maxReconnects;

  /// The cap of the wait before a reconnect (30 s).
  final Duration? reconnectMax;

  /// The silence after which a stream is idle (none).
  final Duration? idleTimeout;

  /// Signs confirmation tokens with at least 32 cryptographically random bytes,
  /// copied at construction. Shorter keys fail construction; null generates a key.
  final List<int>? confirmationKey;

  /// The clock, in epoch milliseconds (tests).
  final int Function()? now;

  /// Random numbers in [0, 1) (tests).
  final double Function()? random;
}

/// The confirmation of a destructive or irreversible call.
final class Confirm {
  const Confirm._(this.token);

  /// Confirms a destructive operation without a preview.
  static const Confirm yes = Confirm._(null);

  /// The confirmation token of a preview, bound to the exact arguments.
  factory Confirm.token(String token) => Confirm._(token);

  /// The token, or null for [yes].
  final String? token;

  /// Whether this is [yes].
  bool get isYes => token == null;

  @override
  String toString() => isYes ? 'Confirm.yes' : 'Confirm.token($redacted)';
}

/// Options of one call.
final class CallOptions {
  /// Call options.
  const CallOptions({
    this.idempotencyKey,
    this.confirm,
    this.refuseConfirmYes = false,
    this.timeout,
    this.deadline,
    this.verify = false,
    this.headers = const {},
    this.maxResponseBytes,
  }) : _callDeadline = null;

  const CallOptions._full({
    required this.idempotencyKey,
    required this.confirm,
    required this.refuseConfirmYes,
    required this.timeout,
    required this.deadline,
    required this.verify,
    required this.headers,
    required this.maxResponseBytes,
    required int? callDeadline,
  }) : _callDeadline = callDeadline;

  /// Required for caller-owned operations that persist a key: generate it
  /// once per intent and reuse it on every retry.
  final String? idempotencyKey;

  /// [Confirm.yes] (a destructive operation only) or the confirmation
  /// token of a preview.
  final Confirm? confirm;

  /// [Confirm.yes] confirms nothing and remediation never offers it.
  final bool refuseConfirmYes;

  /// The per-attempt timeout of this call.
  final Duration? timeout;

  /// The total deadline of this call, over the client's (default none).
  /// Measured on a monotonic clock from the call, it bounds authentication,
  /// idempotency-key reservation, every attempt (each one's [timeout] is cut
  /// to the time left), retry waits, response bodies and verification; for
  /// pages, polls and macros the whole traversal, poll or macro. No attempt
  /// starts after it expires (code `DEADLINE_EXCEEDED`); a mutation already
  /// sent reports `OUTCOME_UNKNOWN`, never a blind replay. For a stream it
  /// bounds connecting and each reconnect, never an open stream. A shared
  /// token refresh is not cancelled, only no longer waited for.
  final Duration? deadline;

  /// The absolute monotonic deadline (ms) fixed when a public entry point
  /// starts the call; nested work inherits it.
  final int? _callDeadline;

  /// Run the operation's verification hook after a success.
  final bool verify;

  /// Overrides the client's buffered response limit for this call.
  final int? maxResponseBytes;

  /// Sent with this call.
  final Map<String, String> headers;

  bool get _hasKey => idempotencyKey != null && idempotencyKey!.isNotEmpty;

  bool get _confirmYes => confirm != null && confirm!.isYes;

  String? get _token {
    final t = confirm?.token;
    return t == null || t.isEmpty ? null : t;
  }

  CallOptions _with({
    bool clearKey = false,
    bool clearConfirm = false,
    bool? verify,
    Map<String, String>? headers,
    int? callDeadline,
  }) =>
      CallOptions._full(
        idempotencyKey: clearKey ? null : idempotencyKey,
        confirm: clearConfirm ? null : confirm,
        refuseConfirmYes: refuseConfirmYes,
        timeout: timeout,
        deadline: deadline,
        verify: verify ?? this.verify,
        maxResponseBytes: maxResponseBytes,
        headers: headers ?? this.headers,
        callDeadline: callDeadline ?? _callDeadline,
      );

  @override
  String toString() =>
      'CallOptions(idempotencyKey: ${_hasKey ? redacted : null}, confirm: $confirm, '
      'timeout: $timeout, deadline: $deadline, verify: $verify, headers: ${headers.keys.toList()})';
}
