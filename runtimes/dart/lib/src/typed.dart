// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// The result of a call: [Ok] with the typed value or [Err] with the
/// envelope. Calls never throw for API or transport failures.
sealed class Result<T> {
  const Result();

  /// Whether the call succeeded.
  bool get isOk;

  /// The value of an [Ok], else null.
  T? get valueOrNull;

  /// The envelope of an [Err], else null.
  Diagnostic? get errorOrNull;
}

/// A successful call. [value] is the typed value (a model, a page, ...),
/// [raw] the decoded wire value it came from (null for a success without a
/// body), [meta] the HTTP answer, [verification] the verification result
/// when the call asked for one.
final class Ok<T> extends Result<T> {
  /// A success.
  const Ok(this.value, {required this.meta, this.raw, this.verification});

  /// The typed value.
  final T value;

  /// The wire value it came from.
  final Object? raw;

  /// The HTTP answer.
  final ResponseMeta meta;

  /// The verification result of a call made with `verify: true`.
  final Verification? verification;

  @override
  bool get isOk => true;

  @override
  T get valueOrNull => value;

  @override
  Diagnostic? get errorOrNull => null;

  /// The same success with another value.
  Ok<U> withValue<U>(U value) =>
      Ok<U>(value, meta: meta, raw: raw, verification: verification);

  /// The same success with a verification result.
  Ok<T> withVerification(Verification verification) =>
      Ok<T>(value, meta: meta, raw: raw, verification: verification);
}

/// A failed call: [error] is the envelope; [partial] what the failed call
/// or macro already produced when its effect happened (a mutation's body
/// that failed strict validation, a macro's completed step results by name),
/// else [unset]. A partial can hold values the API shows only once: store
/// them before acting on the error.
final class Err<T> extends Result<T> {
  /// A failure.
  const Err(this.error, {this.partial = unset});

  /// The envelope.
  final Diagnostic error;

  /// What the call already produced, or [unset].
  final Object? partial;

  /// Whether [partial] is set.
  bool get hasPartial => !identical(partial, unset);

  @override
  bool get isOk => false;

  @override
  T? get valueOrNull => null;

  @override
  Diagnostic get errorOrNull => error;

  /// The same failure as a result of another type.
  Err<U> cast<U>() => Err<U>(error, partial: partial);
}

/// An optional member that may be null, in three states: absent (not
/// sent), null (sent as JSON null) or a value. Generated models use it for
/// members that are optional and nullable; match on [PatchAbsent],
/// [PatchNull] and [PatchValue].
sealed class Patch<T> {
  const Patch._();

  /// Absent: the member is not sent.
  const factory Patch.absent() = PatchAbsent<T>;

  /// JSON null.
  const factory Patch.nullValue() = PatchNull<T>;

  /// A value.
  const factory Patch.of(T value) = PatchValue<T>;

  /// The patch of a member as decoded JSON holds it: [unset] is absent,
  /// null is JSON null, anything else the value [decode] makes of it.
  static Patch<T> fromJson<T>(Object? value, T Function(Object? raw) decode) {
    if (identical(value, unset)) return Patch<T>.absent();
    if (value == null) return Patch<T>.nullValue();
    return Patch<T>.of(decode(value));
  }

  /// Whether the member is sent (null or a value).
  bool get isPresent;

  /// Whether the member is JSON null.
  bool get isNull;

  /// Whether the member has a value.
  bool get hasValue;

  /// The value; throws [StateError] when there is none.
  T get value;

  /// The value, or null when absent or null.
  T? get valueOrNull;

  /// The member as JSON: [unset] when absent (the member is dropped), null
  /// or the value [encode] makes of it.
  Object? toJson([Object? Function(T value)? encode]);
}

/// An absent member.
final class PatchAbsent<T> extends Patch<T> {
  /// An absent member.
  const PatchAbsent() : super._();

  @override
  bool get isPresent => false;

  @override
  bool get isNull => false;

  @override
  bool get hasValue => false;

  @override
  T get value => throw StateError('the patch has no value');

  @override
  T? get valueOrNull => null;

  @override
  Object? toJson([Object? Function(T value)? encode]) => unset;

  @override
  bool operator ==(Object other) => other is PatchAbsent<T>;

  @override
  int get hashCode => 0;

  @override
  String toString() => 'Patch.absent()';
}

/// A member sent as JSON null.
final class PatchNull<T> extends Patch<T> {
  /// JSON null.
  const PatchNull() : super._();

  @override
  bool get isPresent => true;

  @override
  bool get isNull => true;

  @override
  bool get hasValue => false;

  @override
  T get value => throw StateError('the patch has no value');

  @override
  T? get valueOrNull => null;

  @override
  Object? toJson([Object? Function(T value)? encode]) => null;

  @override
  bool operator ==(Object other) => other is PatchNull<T>;

  @override
  int get hashCode => 1;

  @override
  String toString() => 'Patch.nullValue()';
}

/// A member with a value.
final class PatchValue<T> extends Patch<T> {
  /// A value.
  const PatchValue(this.value) : super._();

  @override
  final T value;

  @override
  bool get isPresent => true;

  @override
  bool get isNull => false;

  @override
  bool get hasValue => true;

  @override
  T? get valueOrNull => value;

  @override
  Object? toJson([Object? Function(T value)? encode]) =>
      encode == null ? Models.dump(value) : encode(value);

  @override
  bool operator ==(Object other) =>
      other is PatchValue<T> &&
      Json.sameOpt(Models.dump(other.value), Models.dump(value));

  @override
  int get hashCode => Object.hash(2, Json.text(Models.dump(value)));

  @override
  String toString() => 'Patch.of($value)';
}

/// The helpers generated models decode and encode with. Each decoder keeps
/// a value it cannot decode (null, [unset], another shape) as it is, so the
/// typed value of a body that does not match the description is still
/// built and response validation reports the mismatch.
abstract final class Models {
  /// Every item of an array, decoded by [item].
  static List<T>? array<T>(Object? value, T Function(Object? raw) item) =>
      value is List ? [for (final v in value) item(v)] : null;

  /// Every member value of an object, decoded by [item].
  static Map<String, T>? map<T>(Object? value, T Function(Object? raw) item) =>
      value is Map
          ? {for (final e in value.entries) '${e.key}': item(e.value)}
          : null;

  /// A base64 string (or a [Binary]) as bytes, or null.
  static Uint8List? bytes(Object? value) {
    if (value is Uint8List) return value;
    if (value is Binary) return value.data;
    if (value is! String) return null;
    try {
      return base64Decode(value);
    } on FormatException {
      return null;
    }
  }

  /// A string member, or null.
  static String? string(Object? value) => value is String ? value : null;

  /// An integer member (an integral number), or null.
  static int? integer(Object? value) {
    if (value is int) return value;
    if (value is double && value == value.truncateToDouble()) {
      return value.isFinite ? value.toInt() : null;
    }
    return null;
  }

  /// A number member as a double, or null.
  static double? number(Object? value) =>
      value is num ? value.toDouble() : null;

  /// A boolean member, or null.
  static bool? boolean(Object? value) => value is bool ? value : null;

  /// An object member, or null.
  static Map<String, Object?>? object(Object? value) =>
      value is Map<String, Object?> ? value : null;

  /// The member [wire] of a decoded object, or [unset] when absent.
  static Object? member(Object? object, String wire) =>
      object is Map && object.containsKey(wire) ? object[wire] : unset;

  /// The index of the variant of the union named [typeId] that [value] is
  /// (by its tag, else the first variant that accepts it), or null.
  static int? variant(
      SdkDescriptors descriptors, String typeId, Object? value) {
    final union = descriptors.defs[typeId];
    if (union is! Map || union['kind'] != 'union') return null;
    final variants = _list(union['variants']);
    final tag = union['tag'];
    if (tag is String) {
      if (value is! Map) return null;
      final index = _list(union['tags']).indexOf(value[tag]);
      return index >= 0 && index < variants.length ? index : null;
    }
    for (var i = 0; i < variants.length; i++) {
      if (Schema(_map(variants[i]), descriptors.defs).accepts(value)) return i;
    }
    return null;
  }

  /// A typed value back to JSON: models ([JsonEncodable]) as their JSON,
  /// arrays and maps converted without [unset] members, [Patch]es as their
  /// state, bytes as [Binary] (base64 in the JSON text), enums with a wire
  /// value as it.
  static Object? dump(Object? value, [int depth = 0]) {
    if (depth > Json.maxDepth) {
      throw ArgumentError('the value is nested too deeply');
    }
    if (value is Patch) return value.toJson();
    if (value is JsonEncodable) return dump(value.toJson(), depth + 1);
    if (value is Uint8List) return Binary(value);
    if (value is List) {
      return [
        for (final v in value)
          if (!identical(v, unset)) dump(v, depth + 1),
      ];
    }
    if (value is Map) {
      final out = <String, Object?>{};
      for (final e in value.entries) {
        final v = dump(e.value, depth + 1);
        if (!identical(v, unset)) out['${e.key}'] = v;
      }
      return out;
    }
    return value;
  }
}

/// Typed results over the dynamic ones of the core.
abstract final class Typed {
  /// The [Ok] of a call with its value decoded by [decode] ([Ok.raw] keeps
  /// the wire value); an [Err] as it is. A success without a body has the
  /// value null when [T] admits it. A decoder that throws makes the call
  /// UNEXPECTED_RESPONSE, with the wire value as the partial.
  static Result<T> result<T>(
    Result<Object?> result,
    T Function(Object? raw) decode, {
    String operation = '<operation>',
  }) {
    switch (result) {
      case Err<Object?>():
        return result.cast<T>();
      case Ok<Object?>():
        final raw = result.raw;
        if (raw == null && null is T) {
          return Ok<T>(
            null as T,
            meta: result.meta,
            raw: raw,
            verification: result.verification,
          );
        }
        try {
          return Ok<T>(
            decode(raw),
            meta: result.meta,
            raw: raw,
            verification: result.verification,
          );
        } catch (e) {
          return Err<T>(
            Diagnostic(
              category: 'UNEXPECTED_RESPONSE',
              operation: operation,
              httpStatus: result.meta.status == 0 ? null : result.meta.status,
              requestId: result.meta.requestId,
              remediation:
                  'The SDK could not build its typed value from the response (${e.runtimeType}). '
                  'The wire value is in the partial; do not repeat a mutation, it took effect.',
              retryable: 'never',
              attempts: result.meta.attempts,
            ),
            partial: raw,
          );
        }
    }
  }

  /// The pages of a stream of page results with their items decoded by
  /// [item]; failures as they are.
  static Stream<Result<Page<T>>> pages<T>(
    Stream<Result<Page<Object?>>> pages,
    T Function(Object? raw) item,
  ) =>
      pages.map((page) {
        switch (page) {
          case Err<Page<Object?>>():
            return page.cast<Page<T>>();
          case Ok<Page<Object?>>():
            final p = page.value;
            return Ok<Page<T>>(
              Page<T>(
                items: [for (final i in p.rawItems) item(i)],
                rawItems: p.rawItems,
                body: p.body,
                next: p.next,
              ),
              meta: page.meta,
              raw: page.raw,
              verification: page.verification,
            );
        }
      });
}
