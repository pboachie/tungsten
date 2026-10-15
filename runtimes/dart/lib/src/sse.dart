// SPDX-License-Identifier: Apache-2.0
part of '../tungsten_runtime.dart';

/// One dispatched server-sent event: its `event` field (`message` when it
/// has none), its data, and the last id and `retry` value the stream has
/// set so far (null before any).
final class _SseEvent {
  const _SseEvent(this.event, this.data, this.id, this.retry);

  final String event;
  final String data;
  final String? id;
  final int? retry;
}

/// A server-sent events parser (WHATWG HTML 9.2 "Server-sent events"):
/// lines end with CRLF, LF or CR; a line is a comment (`:`), a field
/// (`event`, `data`, `id`, `retry`; the value loses one leading space) or
/// the blank line that dispatches the event. Several `data` lines are
/// joined with LF; an event without `data` lines is not dispatched; an
/// event still open when the stream ends is discarded.
///
/// An event may hold at most [_max] UTF-8 bytes: its field lines (each
/// counted with a one-byte terminator; a finished comment line is not kept
/// and does not count) plus the line being read, whatever it turns out to
/// be, so a comment longer than the limit with no line end also stops the
/// parser. A parser that sees more stops: [exceeded] becomes true, the
/// events completed before the oversize one are still returned, and later
/// input is ignored, so nothing is buffered without bound.
final class _SseParser {
  _SseParser(this._max);

  final int _max;
  final List<String> _buffer = [];
  bool _started = false;
  bool _pendingCR = false;
  String _event = '';
  final List<String> _data = [];
  bool _hasData = false;
  String? _id;
  int? _retry;
  int _eventBytes = 0;
  int _bufferBytes = 0;
  bool _exceeded = false;

  /// The event being read is larger than the limit: the parser stopped.
  bool get exceeded => _exceeded;

  /// The number of bytes of [text] in UTF-8 (a lone surrogate counts as
  /// the three bytes of U+FFFD).
  static int utf8Length(String text) {
    var bytes = 0;
    for (var i = 0; i < text.length; i++) {
      final c = text.codeUnitAt(i);
      if (c < 0x80) {
        bytes += 1;
      } else if (c < 0x800) {
        bytes += 2;
      } else if (c >= 0xd800 &&
          c <= 0xdbff &&
          i + 1 < text.length &&
          text.codeUnitAt(i + 1) >= 0xdc00 &&
          text.codeUnitAt(i + 1) <= 0xdfff) {
        bytes += 4;
        i++;
      } else {
        bytes += 3;
      }
    }
    return bytes;
  }

  /// Feeds decoded text and returns the events it completes. A CR at the
  /// end of the text is held back until the next text shows whether an LF
  /// follows.
  List<_SseEvent> push(String text) {
    final out = <_SseEvent>[];
    if (_exceeded) return out;
    var chunk = text;
    if (!_started && chunk.isNotEmpty) {
      _started = true;
      if (chunk.codeUnitAt(0) == 0xfeff) chunk = chunk.substring(1);
    }
    if (_pendingCR) {
      if (chunk.isEmpty) return out;
      _pendingCR = false;
      _complete(out);
      if (chunk.startsWith('\n')) chunk = chunk.substring(1);
    }
    var start = 0;
    for (var i = 0; i < chunk.length && !_exceeded; i++) {
      final c = chunk.codeUnitAt(i);
      if (c != 10 && c != 13) continue;
      _append(chunk.substring(start, i));
      if (c == 13 && i + 1 == chunk.length) {
        _bufferBytes += 1;
        _pendingCR = true;
        _checkLimit();
        return out;
      }
      _complete(out);
      if (c == 13 && chunk.codeUnitAt(i + 1) == 10) i++;
      start = i + 1;
    }
    if (!_exceeded) {
      _append(chunk.substring(start));
      _checkLimit();
    }
    return out;
  }

  /// The stream ended: a held CR ends its line; any open event is
  /// discarded.
  List<_SseEvent> end() {
    final out = <_SseEvent>[];
    if (!_exceeded && _pendingCR) _complete(out);
    _pendingCR = false;
    _buffer.clear();
    _bufferBytes = 0;
    _event = '';
    _data.clear();
    _hasData = false;
    _eventBytes = 0;
    return out;
  }

  // Fragments are scanned and counted once, then joined only at a line end.
  void _append(String text) {
    if (text.isEmpty) return;
    final previous = _buffer.isEmpty ? '' : _buffer.last;
    final last =
        previous.isEmpty ? 0 : previous.codeUnitAt(previous.length - 1);
    final first = text.codeUnitAt(0);
    _bufferBytes += utf8Length(text);
    // A surrogate pair split across fragments was counted as two lone
    // surrogates (3 + 3 bytes) instead of one 4-byte sequence.
    if (last >= 0xd800 &&
        last <= 0xdbff &&
        first >= 0xdc00 &&
        first <= 0xdfff) {
      _bufferBytes -= 2;
    }
    _buffer.add(text);
  }

  void _checkLimit() {
    if (_eventBytes + _bufferBytes > _max) _exceeded = true;
    if (_exceeded) {
      _buffer.clear();
      _bufferBytes = 0;
      _data.clear();
      _event = '';
      _id = null;
      _hasData = false;
    }
  }

  void _complete(List<_SseEvent> out) {
    final line = _buffer.join();
    _buffer.clear();
    _bufferBytes = 0;
    _line(line, out);
    _checkLimit();
  }

  static final RegExp _digits = RegExp(r'^[0-9]+$');

  void _line(String line, List<_SseEvent> out) {
    if (line.isEmpty) {
      if (_hasData) {
        out.add(
          _SseEvent(
            _event.isEmpty ? 'message' : _event,
            _data.join('\n'),
            _id,
            _retry,
          ),
        );
      }
      _event = '';
      _data.clear();
      _hasData = false;
      _eventBytes = 0;
      return;
    }
    if (line.startsWith(':')) return;
    _eventBytes += utf8Length(line) + 1;
    if (_eventBytes > _max) {
      _exceeded = true;
      return;
    }
    final colon = line.indexOf(':');
    final field = colon < 0 ? line : line.substring(0, colon);
    var value = colon < 0 ? '' : line.substring(colon + 1);
    if (value.startsWith(' ')) value = value.substring(1);
    switch (field) {
      case 'event':
        _event = value;
      case 'data':
        _data.add(value);
        _hasData = true;
      case 'id':
        if (!value.contains('\u0000')) _id = value;
      case 'retry':
        if (_digits.hasMatch(value)) {
          final ms = int.tryParse(value);
          if (ms != null && ms <= _Util.maxSafeInteger) _retry = ms;
        }
    }
  }
}

/// Decodes UTF-8 chunk by chunk: a sequence split across chunks is joined,
/// invalid bytes become U+FFFD.
final class _Utf8Chunks {
  _Utf8Chunks() {
    _sink = const Utf8Decoder(allowMalformed: true)
        .startChunkedConversion(StringConversionSink.fromStringSink(_out));
  }

  final StringBuffer _out = StringBuffer();
  late final ByteConversionSink _sink;
  bool _closed = false;

  String _take() {
    final text = _out.toString();
    _out.clear();
    return text;
  }

  /// The text of [bytes] (a trailing partial sequence is held back).
  String decode(List<int> bytes) {
    if (_closed) return '';
    _sink.add(bytes);
    return _take();
  }

  /// The end of the input: a held partial sequence becomes U+FFFD.
  String finish() {
    if (_closed) return '';
    _closed = true;
    _sink.close();
    return _take();
  }
}
