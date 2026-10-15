// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.nio.ByteBuffer;
import java.nio.CharBuffer;
import java.nio.charset.CharsetDecoder;
import java.nio.charset.CoderResult;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * A server-sent events parser (WHATWG HTML 9.2): lines end with CRLF, LF or CR; a line is a
 * comment (":"), a field (event, data, id, retry; the value loses one leading space) or the blank
 * line that dispatches the event. data lines are joined with LF; an event without data lines is
 * not dispatched; an event still open when the stream ends is discarded. An event may hold at
 * most max bytes (its field lines, each with a one-byte terminator, plus the line being read);
 * past it the parser stops.
 */
final class Sse {
    /** One dispatched event; id and retry persist across events. */
    record Event(String event, String data, String id, Long retry) {}

    private final int max;
    private final StringBuilder line = new StringBuilder();
    private int lineBytes;
    private boolean pendingCr;
    private boolean started;
    private String event = "";
    private final StringBuilder data = new StringBuilder();
    private boolean hasData;
    private String id;
    private Long retry;
    private int eventBytes;
    boolean exceeded;

    private final CharsetDecoder decoder = StandardCharsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPLACE)
            .onUnmappableCharacter(CodingErrorAction.REPLACE);
    private ByteBuffer pending = ByteBuffer.allocate(0);

    Sse(int max) {
        this.max = max;
    }

    /** UTF-8 bytes as text; a sequence split across chunks is joined. */
    String decode(byte[] chunk) {
        ByteBuffer in = ByteBuffer.allocate(pending.remaining() + chunk.length);
        in.put(pending).put(chunk).flip();
        CharBuffer out = CharBuffer.allocate(in.remaining() + 1);
        CoderResult r = decoder.decode(in, out, false);
        if (r.isOverflow()) {
            throw new IllegalStateException("UTF-8 decoding overflowed");
        }
        pending = ByteBuffer.allocate(in.remaining());
        pending.put(in).flip();
        out.flip();
        return out.toString();
    }

    /** The end of the bytes: an unfinished sequence is U+FFFD. */
    String finish() {
        if (!pending.hasRemaining()) {
            return "";
        }
        pending = ByteBuffer.allocate(0);
        return "�";
    }

    private static int utf8Length(String s) {
        return utf8Length(s, 0, s.length());
    }

    /** The UTF-8 bytes of s[from, to): a surrogate pair is four, an unpaired surrogate one (it encodes as "?"). */
    private static int utf8Length(CharSequence s, int from, int to) {
        int bytes = 0;
        for (int i = from; i < to; i++) {
            char c = s.charAt(i);
            if (c < 0x80) {
                bytes += 1;
            } else if (c < 0x800) {
                bytes += 2;
            } else if (Character.isHighSurrogate(c) && i + 1 < to && Character.isLowSurrogate(s.charAt(i + 1))) {
                bytes += 4;
                i++;
            } else if (Character.isSurrogate(c)) {
                bytes += 1;
            } else {
                bytes += 3;
            }
        }
        return bytes;
    }

    /** Adds chunk[from, to) to the line being read; each fragment is scanned and counted once. */
    private void append(String chunk, int from, int to) {
        if (from >= to) {
            return;
        }
        int n = line.length();
        if (n > 0 && Character.isHighSurrogate(line.charAt(n - 1)) && Character.isLowSurrogate(chunk.charAt(from))) {
            // The pair was counted as two unpaired surrogates (one byte each); it is four bytes.
            lineBytes += 2;
        }
        lineBytes += utf8Length(chunk, from, to);
        line.append(chunk, from, to);
    }

    /** The line read so far ends: it is parsed and the buffer is emptied. */
    private void complete(List<Event> out) {
        String text = line.toString();
        line.setLength(0);
        lineBytes = 0;
        line(text, out);
    }

    /** Feeds text; returns the events it completes. A CR at the end waits for the next text. */
    List<Event> push(String chunk) {
        List<Event> out = new ArrayList<>();
        if (exceeded) {
            return out;
        }
        if (!started && !chunk.isEmpty()) {
            started = true;
            if (chunk.startsWith("\uFEFF")) {
                chunk = chunk.substring(1);
            }
        }
        int start = 0;
        if (pendingCr) {
            if (chunk.isEmpty()) {
                return out;
            }
            pendingCr = false;
            complete(out);
            if (exceeded) {
                return out;
            }
            if (chunk.charAt(0) == '\n') {
                start = 1;
            }
        }
        int length = chunk.length();
        for (int i = start; i < length; i++) {
            char c = chunk.charAt(i);
            if (c != '\n' && c != '\r') {
                continue;
            }
            append(chunk, start, i);
            if (c == '\r' && i + 1 == length) {
                pendingCr = true;
                return out;
            }
            complete(out);
            if (exceeded) {
                return out;
            }
            if (c == '\r' && chunk.charAt(i + 1) == '\n') {
                i++;
            }
            start = i + 1;
        }
        append(chunk, start, length);
        if (eventBytes + lineBytes > max) {
            exceeded = true;
            line.setLength(0);
            lineBytes = 0;
        }
        return out;
    }

    /** The stream ended: a held-back CR ends its line, the rest is discarded. */
    List<Event> end() {
        List<Event> out = new ArrayList<>();
        if (!exceeded && pendingCr) {
            complete(out);
        }
        pendingCr = false;
        line.setLength(0);
        lineBytes = 0;
        event = "";
        data.setLength(0);
        hasData = false;
        return out;
    }

    private void line(String line, List<Event> out) {
        if (line.isEmpty()) {
            if (hasData) {
                String d = data.substring(0, data.length() - 1);
                out.add(new Event(event.isEmpty() ? "message" : event, d, id, retry));
            }
            event = "";
            data.setLength(0);
            hasData = false;
            eventBytes = 0;
            return;
        }
        if (line.startsWith(":")) {
            return;
        }
        eventBytes += utf8Length(line) + 1;
        if (eventBytes > max) {
            exceeded = true;
            return;
        }
        int colon = line.indexOf(':');
        String name = colon < 0 ? line : line.substring(0, colon);
        String value = colon < 0 ? "" : line.substring(colon + 1);
        if (value.startsWith(" ")) {
            value = value.substring(1);
        }
        switch (name) {
            case "event":
                event = value;
                break;
            case "data":
                data.append(value).append('\n');
                hasData = true;
                break;
            case "id":
                if (value.indexOf('\0') < 0) {
                    id = value;
                }
                break;
            case "retry":
                if (Text.allDigits(value) && value.length() <= 16) {
                    long ms = Long.parseLong(value);
                    if (ms <= 9007199254740991L) {
                        retry = ms;
                    }
                }
                break;
            default:
                break;
        }
    }
}
