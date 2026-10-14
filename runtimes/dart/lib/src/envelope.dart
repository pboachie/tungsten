// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// The closed set of error categories shared by every tungsten runtime.
const List<String> categories = [
  'VALIDATION_FAILED',
  'MALFORMED_REQUEST',
  'REQUEST_TOO_LARGE',
  'AUTH_FAILED',
  'NOT_FOUND',
  'CONFLICT',
  'PRECONDITION_FAILED',
  'RATE_LIMITED',
  'UPSTREAM_UNAVAILABLE',
  'OUTCOME_UNKNOWN',
  'TRANSPORT_FAILED',
  'CONFIRMATION_REQUIRED',
  'GATE_DISABLED',
  'UNEXPECTED_RESPONSE',
];

/// Whether and how a failed call may be repeated.
const List<String> retryableValues = [
  'never',
  'after_delay',
  'same_key_only',
  'after_remediation',
];

/// Written wherever a secret or sensitive value would appear.
const String redacted = '<redacted>';

/// The error envelope (planning/06): [toJson] is exactly the fourteen
/// members of `specs/envelope.schema.json` in order, every member present
/// (null when it has no value), identical across the runtimes.
final class Diagnostic {
  /// An envelope; use the fields of another one with [copyWith].
  const Diagnostic({
    required this.category,
    required this.operation,
    this.httpStatus,
    this.code,
    this.failedParameter,
    this.receivedValue,
    this.expected,
    required this.remediation,
    required this.retryable,
    this.retryAfterMs,
    this.nextAction,
    this.requestId,
    this.attempts = 0,
  });

  /// One of [categories].
  final String category;

  /// The operation id (or macro name) of the call.
  final String operation;

  /// The HTTP status of the answer, null when none arrived.
  final int? httpStatus;

  /// The API's error code.
  final String? code;

  /// The argument, option or response path the failure is about.
  final String? failedParameter;

  /// The offending value, redacted and cut to 200 UTF-16 code units.
  final Object? receivedValue;

  /// What was expected instead.
  final String? expected;

  /// What to do about it.
  final String remediation;

  /// One of [retryableValues].
  final String retryable;

  /// How long to wait before calling again, when the server said.
  final int? retryAfterMs;

  /// The call to make next, in words.
  final String? nextAction;

  /// The request id the server gave.
  final String? requestId;

  /// The attempts made.
  final int attempts;

  /// The envelope with some fields replaced.
  Diagnostic copyWith({
    String? operation,
    String? remediation,
    String? retryable,
    String? nextAction,
    bool clearNextAction = false,
  }) =>
      Diagnostic(
        category: category,
        operation: operation ?? this.operation,
        httpStatus: httpStatus,
        code: code,
        failedParameter: failedParameter,
        receivedValue: receivedValue,
        expected: expected,
        remediation: remediation ?? this.remediation,
        retryable: retryable ?? this.retryable,
        retryAfterMs: retryAfterMs,
        nextAction: clearNextAction ? null : (nextAction ?? this.nextAction),
        requestId: requestId,
        attempts: attempts,
      );

  /// The envelope as JSON: its fourteen members in order.
  Map<String, Object?> toJson() => {
        'status': 'error',
        'category': category,
        'operation': operation,
        'http_status': httpStatus,
        'code': code,
        'failed_parameter': failedParameter,
        'received_value': receivedValue,
        'expected': expected,
        'remediation': remediation,
        'retryable': retryable,
        'retry_after_ms': retryAfterMs,
        'next_action': nextAction,
        'request_id': requestId,
        'trace': {'attempts': attempts},
      };

  /// "<category> in <operation>: <remediation>".
  @override
  String toString() => '$category in $operation: $remediation';
}

/// The default `retryable` of a category.
String defaultRetryable(String category) => switch (category) {
      'RATE_LIMITED' ||
      'UPSTREAM_UNAVAILABLE' ||
      'TRANSPORT_FAILED' =>
        'after_delay',
      'OUTCOME_UNKNOWN' => 'same_key_only',
      _ => 'never',
    };

const Map<String, String> _genericRemediation = {
  'VALIDATION_FAILED':
      'The server rejected the arguments. Fix the parameter the API error names and call again; the same request fails the same way.',
  'MALFORMED_REQUEST':
      'The server could not parse the request. Check the format of path and query parameters (for example canonical UUIDs); do not retry unchanged.',
  'REQUEST_TOO_LARGE':
      'The request body is larger than the server accepts. Send a smaller body; do not retry unchanged.',
  'AUTH_FAILED':
      'The credential is missing, expired, revoked or lacks permission. Do not retry; fix the credential configured in ClientOptions.auth.',
  'NOT_FOUND':
      'The resource does not exist or is not visible to this credential. Check the identifiers; do not retry unchanged.',
  'CONFLICT':
      "The request conflicts with the resource's current state. Read the current state before deciding what to do; do not retry unchanged.",
  'PRECONDITION_FAILED':
      'A precondition of this operation is not met (account, billing or resource state). Resolve it first; do not retry unchanged.',
  'RATE_LIMITED':
      'Rate limited. Wait retry_after_ms (or a few seconds when it is null) before calling again.',
  'UPSTREAM_UNAVAILABLE':
      'The service is temporarily unavailable. Wait, then call again.',
  'OUTCOME_UNKNOWN':
      'The server may or may not have applied this call; do not retry it blindly. Check whether it took effect before doing anything else.',
  'TRANSPORT_FAILED':
      'The request could not be delivered (DNS, TLS or connection failure), so the server did not receive it. Check baseUrl and network access, then call again.',
  'CONFIRMATION_REQUIRED':
      'This operation needs confirmation: call preview(...) and pass its confirmation_token.',
  'GATE_DISABLED':
      'This operation is disabled on this deployment. It is a deployment setting; do not retry.',
  'UNEXPECTED_RESPONSE':
      "The server's response did not match the API description. Do not retry blindly; report it.",
};

/// The remediation used when no manifest entry applies.
String genericRemediation(String category) =>
    _genericRemediation[category] ?? 'The call failed.';

/// The category of an HTTP error status without a manifest entry.
String categoryForStatus(int status, bool jsonBody) {
  switch (status) {
    case 401 || 403 || 407:
      return 'AUTH_FAILED';
    case 404 || 410:
      return 'NOT_FOUND';
    case 409:
      return 'CONFLICT';
    case 402 || 412 || 428:
      return 'PRECONDITION_FAILED';
    case 413:
      return 'REQUEST_TOO_LARGE';
    case 422:
      return 'VALIDATION_FAILED';
    case 429:
      return 'RATE_LIMITED';
    case 408 || 502 || 503 || 504:
      return 'UPSTREAM_UNAVAILABLE';
  }
  if (status >= 500 && status <= 599) return 'UPSTREAM_UNAVAILABLE';
  if (status >= 400 && status <= 499) {
    return jsonBody ? 'VALIDATION_FAILED' : 'MALFORMED_REQUEST';
  }
  return 'UNEXPECTED_RESPONSE';
}

/// Builds an envelope member by member; [build] fills the remediation and
/// `retryable` a category implies when they were not set.
final class _Diag {
  _Diag(this._operation, this._category);

  final String _operation;
  final String _category;
  int? _status;
  String? _code;
  String? _param;
  Object? _received;
  String? _expected;
  String? _remedy;
  String? _retryable;
  int? _retryAfter;
  String? _next;
  String? _requestId;
  int _attempts = 0;

  _Diag status(int? v) => this.._status = v;
  _Diag code(String? v) => this.._code = v;
  _Diag param(String? v) => this.._param = v;
  _Diag received(Object? v) => this.._received = v;
  _Diag expected(String? v) => this.._expected = v;
  _Diag remedy(String? v) => this.._remedy = v;
  _Diag retrying(String? v) => this.._retryable = v ?? _retryable;
  _Diag retryAfter(int? v) => this.._retryAfter = v;
  _Diag next(String? v) => this.._next = v;
  _Diag requestId(String? v) => this.._requestId = v;
  _Diag attempts(int v) => this.._attempts = v;

  Diagnostic build() => Diagnostic(
        category: _category,
        operation: _operation,
        httpStatus: _status,
        code: _code,
        failedParameter: _param,
        receivedValue: _received,
        expected: _expected,
        remediation: _remedy ?? genericRemediation(_category),
        retryable: _retryable ?? defaultRetryable(_category),
        retryAfterMs: _retryAfter,
        nextAction: _next,
        requestId: _requestId,
        attempts: _attempts,
      );

  Err<T> err<T>() => Err<T>(build());
}

_Diag _diag(String operation, String category) => _Diag(operation, category);

/// What the HTTP answer of a call said: the status, the headers (names
/// lower-cased; repeated values joined with ", ", set-cookie keeps its
/// last), the request id the server gave and the attempts made.
final class ResponseMeta {
  /// The answer of a call.
  const ResponseMeta({
    required this.status,
    required this.headers,
    this.requestId,
    required this.attempts,
  });

  /// The metadata of a result made without a request (previews).
  static const ResponseMeta local =
      ResponseMeta(status: 0, headers: {}, attempts: 0);

  /// The HTTP status.
  final int status;

  /// The headers by lower-cased name.
  final Map<String, String> headers;

  /// The request id the server gave.
  final String? requestId;

  /// The attempts made.
  final int attempts;
}

/// The result of an operation's verification hook: [checked] false when it
/// could not run (then [error] says why); [passed] when its expectation
/// held; [observed] the read's body (sensitive fields redacted, [unset]
/// without one); [timedOut] when polling for its terminal state ran out of
/// budget.
final class Verification {
  /// A verification result.
  const Verification({
    required this.checked,
    required this.passed,
    this.observed = unset,
    this.timedOut = false,
    this.error,
  });

  /// A verification that could not run.
  const Verification.unchecked([this.error])
      : checked = false,
        passed = false,
        observed = unset,
        timedOut = false;

  /// Whether the hook ran.
  final bool checked;

  /// Whether its expectation held.
  final bool passed;

  /// The body the read returned, or [unset].
  final Object? observed;

  /// Polling ran out of budget.
  final bool timedOut;

  /// Why it could not run.
  final Diagnostic? error;

  /// The verification as the TypeScript runtime shapes it.
  Map<String, Object?> toJson() => {
        'checked': checked,
        'passed': passed,
        'observed': identical(observed, unset) ? null : observed,
        if (timedOut) 'timed_out': true,
        if (error != null) 'error': error!.toJson(),
      };
}

/// A request rendered without sending it; secrets show as `<redacted>`.
final class RenderedRequest {
  /// A rendered request.
  const RenderedRequest({
    required this.method,
    required this.url,
    required this.headers,
    this.body,
  });

  /// The HTTP method.
  final String method;

  /// The URL, sensitive query values redacted.
  final String url;

  /// The headers, secrets redacted.
  final Map<String, String> headers;

  /// The body as JSON when it is JSON, else a text rendering; null without.
  final Object? body;

  /// The request as JSON.
  Map<String, Object?> toJson() =>
      {'method': method, 'url': url, 'headers': headers, 'body': body};
}

/// One step of a macro preview: [request] is null when the step's
/// arguments (or a bytes or multipart body) come from an earlier result.
final class MacroStepPreview {
  /// A step of a macro preview.
  const MacroStepPreview({
    required this.step,
    required this.kind,
    required this.operation,
    this.as,
    required this.safety,
    this.request,
    required this.effects,
  });

  /// The 1-based step number.
  final int step;

  /// `call`, `poll` or `paginate`.
  final String kind;

  /// The operation id.
  final String operation;

  /// The name the step's result is bound to.
  final String? as;

  /// The operation's safety tier.
  final String safety;

  /// The rendered request.
  final RenderedRequest? request;

  /// The step's effects, in words.
  final List<String> effects;

  /// The step as JSON.
  Map<String, Object?> toJson() => {
        'step': step,
        'kind': kind,
        'operation': operation,
        'as': as,
        'safety': safety,
        'request': request?.toJson(),
        'effects': effects,
      };
}

/// What a preview returns: the request (for a macro, its first step's), the
/// side effects in words, and for a mutating operation a confirmation token
/// bound to the exact arguments for [expiresInMs].
final class PreviewResult {
  /// A preview.
  const PreviewResult({
    required this.operation,
    required this.safety,
    required this.request,
    required this.effects,
    this.confirmationToken,
    this.expiresInMs,
    this.serverPreview = unset,
    this.steps,
  });

  /// The operation id or macro name.
  final String operation;

  /// Its safety tier.
  final String safety;

  /// The request it would send.
  final RenderedRequest request;

  /// Its effects, in words.
  final List<String> effects;

  /// Pass as `CallOptions(confirm: Confirm.token(...))` to confirm.
  final String? confirmationToken;

  /// How long the token is valid.
  final int? expiresInMs;

  /// What the server's own preview answered, or [unset].
  final Object? serverPreview;

  /// A macro's steps.
  final List<MacroStepPreview>? steps;

  PreviewResult _withServerPreview(Object? value) => PreviewResult(
        operation: operation,
        safety: safety,
        request: request,
        effects: effects,
        confirmationToken: confirmationToken,
        expiresInMs: expiresInMs,
        serverPreview: value,
        steps: steps,
      );

  /// The preview as JSON.
  Map<String, Object?> toJson() => {
        'operation': operation,
        'safety': safety,
        'request': request.toJson(),
        'effects': effects,
        'confirmation_token': confirmationToken,
        'expires_in_ms': expiresInMs,
        if (!identical(serverPreview, unset)) 'server_preview': serverPreview,
        if (steps != null) 'steps': [for (final s in steps!) s.toJson()],
      };
}

/// One page of a paginated operation: its items, the whole body and the
/// cursor, offset, page number or URL of the next page (null at the end).
final class Page<T> {
  /// A page.
  const Page({
    required this.items,
    required this.rawItems,
    this.body,
    this.next,
  });

  /// The items, typed.
  final List<T> items;

  /// The items as they arrived.
  final List<Object?> rawItems;

  /// The whole body.
  final Object? body;

  /// The cursor, offset, page number or URL of the next page.
  final Object? next;
}

/// One event of a stream: its decoded data ([value]; [raw] is the JSON it
/// came from), the event name (`message` when it has none), the last event
/// id and retry value the stream has set so far, the answer of the
/// connection it arrived on and the reconnects made up to it.
final class StreamEvent<T> {
  /// An event.
  const StreamEvent({
    required this.value,
    required this.raw,
    required this.event,
    this.id,
    this.retry,
    required this.meta,
    required this.reconnects,
  });

  /// The typed data.
  final T value;

  /// The data as decoded JSON.
  final Object? raw;

  /// The event name.
  final String event;

  /// The last event id so far.
  final String? id;

  /// The last retry value so far, in milliseconds.
  final int? retry;

  /// The answer of the connection.
  final ResponseMeta meta;

  /// The reconnects made up to this event.
  final int reconnects;
}
