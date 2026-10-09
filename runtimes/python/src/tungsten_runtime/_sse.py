# SPDX-License-Identifier: Apache-2.0
"""A server-sent events parser (WHATWG HTML 9.2 "Server-sent events"):
lines end with CRLF, LF or CR; a line is a comment (``:``), a field
(``event``, ``data``, ``id``, ``retry``; the value loses one leading
space) or the blank line that dispatches the event. Several ``data`` lines
are joined with LF; an event without ``data`` lines is not dispatched; an
event still open when the stream ends is discarded. Same behaviour as
``runtimes/ts/src/sse.ts``."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class SseEvent:
    #: The ``event`` field, ``message`` when the event has none.
    event: str
    data: str
    #: The last event id seen so far (it survives across events), or None.
    id: str | None
    #: The last valid ``retry`` value seen so far, in milliseconds, or None.
    retry: int | None


class SseParser:
    __slots__ = ("_buffer", "_data", "_event", "_has_data", "_id", "_retry", "_started")

    def __init__(self) -> None:
        self._buffer = ""
        self._started = False
        self._event = ""
        self._data = ""
        self._has_data = False
        self._id: str | None = None
        self._retry: int | None = None

    def push(self, chunk: str) -> list[SseEvent]:
        """Feed decoded text; returns the events it completes. A CR at the
        end of the text is held back until the next text shows whether an LF
        follows."""
        text = self._buffer + chunk
        self._buffer = ""
        if not self._started and len(text) > 0:
            self._started = True
            if text[0] == "﻿":
                text = text[1:]
        out: list[SseEvent] = []
        start = 0
        i = 0
        size = len(text)
        while i < size:
            c = text[i]
            if c != "\n" and c != "\r":
                i += 1
                continue
            if c == "\r" and i + 1 == size:
                self._buffer = text[start:]
                return out
            line = text[start:i]
            if c == "\r" and text[i + 1] == "\n":
                i += 1
            start = i + 1
            i += 1
            self._line(line, out)
        self._buffer = text[start:]
        return out

    def end(self) -> list[SseEvent]:
        """The stream ended: a CR held back ends its line. Whatever is still
        open is discarded."""
        out: list[SseEvent] = []
        if self._buffer.endswith("\r"):
            self._line(self._buffer[:-1], out)
        self._buffer = ""
        self._event = ""
        self._data = ""
        self._has_data = False
        return out

    def _line(self, line: str, out: list[SseEvent]) -> None:
        if line == "":
            if self._has_data:
                out.append(
                    SseEvent(
                        self._event if self._event != "" else "message",
                        self._data[:-1],
                        self._id,
                        self._retry,
                    )
                )
            self._event = ""
            self._data = ""
            self._has_data = False
            return
        if line.startswith(":"):
            return
        colon = line.find(":")
        name = line if colon < 0 else line[:colon]
        value = "" if colon < 0 else line[colon + 1 :]
        if value.startswith(" "):
            value = value[1:]
        if name == "event":
            self._event = value
        elif name == "data":
            self._data += value + "\n"
            self._has_data = True
        elif name == "id":
            if "\0" not in value:
                self._id = value
        elif name == "retry" and value.isascii() and value.isdigit():
            ms = int(value)
            if ms <= 9_007_199_254_740_991:
                self._retry = ms
