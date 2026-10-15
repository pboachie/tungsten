// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// One typed method a [Dispatcher] can route to: the argument names it
/// requires, every name it takes, and the call itself.
typedef DispatchEntry<R> = ({
  List<String> required,
  List<String> every,
  R Function(Map<String, Object?> args, CallOptions options) call,
});

/// The base of every generated `Dispatch`: calls by operation id (or macro
/// name) with JSON arguments keyed by the SDK's argument names, for drivers
/// that only know ids and JSON (the contract suite, tool servers).
/// Arguments that fit a typed method (every required name given, no
/// unknown name) run it; any others go to the core as they are, so
/// pre-flight validation reports them. Results are the typed methods'
/// ([Ok.raw] is the wire value).
base class Dispatcher {
  /// A dispatcher over a client's core and its typed methods by id.
  Dispatcher(
    this.core, {
    this.operationEntries = const {},
    this.previewEntries = const {},
    this.pageEntries = const {},
    this.streamEntries = const {},
    this.macroEntries = const {},
  });

  /// The core every untyped call goes to.
  final ClientCore core;

  /// The typed calls by operation id, in IR order.
  final Map<String, DispatchEntry<Future<Result<Object?>>>> operationEntries;

  /// The typed previews by operation id.
  final Map<String, DispatchEntry<Future<Result<PreviewResult>>>>
      previewEntries;

  /// The typed page streams by operation id.
  final Map<String, DispatchEntry<Object>> pageEntries;

  /// The typed event streams by operation id.
  final Map<String, DispatchEntry<Object>> streamEntries;

  /// The typed macro runs by macro name, in IR order.
  final Map<String, DispatchEntry<Object>> macroEntries;

  /// Every callable operation's id, in IR order.
  List<String> get operations => operationEntries.keys.toList();

  /// Every macro's name, in IR order.
  List<String> get macros => macroEntries.keys.toList();

  /// Calls an operation.
  Future<Result<Object?>> invoke(
    String operation, [
    Map<String, Object?> args = const {},
    CallOptions options = const CallOptions(),
  ]) =>
      route(
        operationEntries,
        operation,
        args,
        options,
        () => core.call<Object?>(operation, args, options: options),
      );

  /// Previews an operation.
  Future<Result<PreviewResult>> previewOperation(
    String operation, [
    Map<String, Object?> args = const {},
    CallOptions options = const CallOptions(),
  ]) =>
      route(
        previewEntries,
        operation,
        args,
        options,
        () => core.preview(operation, args, options: options),
      );

  /// Runs the typed entry of [name] in [table] when [args] fit it, else
  /// [fallback].
  R route<R>(
    Map<String, DispatchEntry<R>> table,
    String name,
    Map<String, Object?> args,
    CallOptions options,
    R Function() fallback,
  ) {
    final entry = table[name];
    if (entry == null) return fallback();
    final given = args.keys.toList();
    final fits = given.every(entry.every.contains) &&
        entry.required.every(given.contains);
    return fits ? entry.call(args, options) : fallback();
  }
}
