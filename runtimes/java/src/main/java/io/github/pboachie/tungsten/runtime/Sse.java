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
    private String buffer = "";
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
        return s.getBytes(StandardCharsets.UTF_8).length;
    }

    /** Feeds text; returns the events it completes. A CR at the end waits for the next text. */
    List<Event> push(String chunk) {
        List<Event> out = new ArrayList<>();
        if (exceeded) {
            return out;
        }
        String text = buffer + chunk;
        buffer = "";
        if (!started && !text.isEmpty()) {
            started = true;
            if (text.startsWith("﻿")) {
                text = text.substring(1);
            }
        }
        int start = 0;
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c != '\n' && c != '\r') {
                continue;
            }
            if (c == '\r' && i + 1 == text.length()) {
                buffer = text.substring(start);
                return out;
            }
            String line = text.substring(start, i);
            if (c == '\r' && text.charAt(i + 1) == '\n') {
                i++;
            }
            start = i + 1;
            line(line, out);
            if (exceeded) {
                return out;
            }
        }
        buffer = text.substring(start);
        if (eventBytes + utf8Length(buffer) > max) {
            exceeded = true;
            buffer = "";
        }
        return out;
    }

    /** The stream ended: a held-back CR ends its line, the rest is discarded. */
    List<Event> end() {
        List<Event> out = new ArrayList<>();
        if (!exceeded && buffer.endsWith("\r")) {
            line(buffer.substring(0, buffer.length() - 1), out);
        }
        buffer = "";
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
