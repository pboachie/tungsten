# SPDX-License-Identifier: Apache-2.0
"""A server-sent events parser (WHATWG HTML 9.2 "Server-sent events"):
lines end with CRLF, LF or CR; a line is a comment (``:``), a field
(``event``, ``data``, ``id``, ``retry``; the value loses one leading
space) or the blank line that dispatches the event. Several ``data`` lines
are joined with LF; an event without ``data`` lines is not dispatched; an
event still open when the stream ends is discarded. Same behaviour as
``runtimes/ts/src/sse.ts``.

An event may hold at most ``max_event_bytes`` (default
``DEFAULT_MAX_EVENT_BYTES``, 1 MiB): the UTF-8 bytes of its field lines (each
counted with a one-byte terminator; a finished comment line is not kept and
does not count) plus the line being read, whatever it turns out to be, so a
comment longer than the limit with no line end also stops the parser. A parser that sees more stops:
``exceeded`` becomes true, the events completed before the oversize one are
still returned, and later input is ignored, so nothing is buffered without
bound."""

from __future__ import annotations

from dataclasses import dataclass

#: The default size limit of one event, in UTF-8 bytes.
DEFAULT_MAX_EVENT_BYTES = 1024 * 1024


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
    __slots__ = (
        "_buffer",
        "_buffer_bytes",
        "_data",
        "_event",
        "_event_bytes",
        "_exceeded",
        "_has_data",
        "_id",
        "_max",
        "_retry",
        "_started",
        "_pending_cr",
    )

    def __init__(self, max_event_bytes: int = DEFAULT_MAX_EVENT_BYTES) -> None:
        self._max = max_event_bytes
        self._event_bytes = 0
        self._exceeded = False
        self._buffer: list[str] = []
        self._buffer_bytes = 0
        self._started = False
        self._pending_cr = False
        self._event = ""
        self._data: list[str] = []
        self._has_data = False
        self._id: str | None = None
        self._retry: int | None = None

    @property
    def exceeded(self) -> bool:
        """The event being read is larger than the limit: the parser has stopped."""
        return self._exceeded

    def push(self, chunk: str) -> list[SseEvent]:
        """Feed decoded text; returns the events it completes. A CR at the
        end of the text is held back until the next text shows whether an LF
        follows."""
        if self._exceeded:
            return []
        if not self._started and chunk:
            self._started = True
            chunk = chunk.removeprefix("\ufeff")
        out: list[SseEvent] = []
        if self._pending_cr:
            if not chunk:
                return out
            self._pending_cr = False
            self._complete(out)
            chunk = chunk.removeprefix("\n")
        start = 0
        i = 0
        while i < len(chunk) and not self._exceeded:
            c = chunk[i]
            if c not in "\r\n":
                i += 1
                continue
            self._buffer.append(chunk[start:i])
            if c == "\r" and i + 1 == len(chunk):
                self._buffer_bytes += len(chunk[start:i].encode("utf-8")) + 1
                self._pending_cr = True
                self._check_limit()
                return out
            self._complete(out)
            if c == "\r" and chunk[i + 1] == "\n":
                i += 1
            i += 1
            start = i
        if not self._exceeded:
            tail = chunk[start:]
            self._buffer_bytes += len(tail.encode("utf-8"))
            if self._check_limit() and tail:
                self._buffer.append(tail)
        return out

    def _check_limit(self) -> bool:
        if self._event_bytes + self._buffer_bytes > self._max:
            self._exceeded = True
        if self._exceeded:
            self._buffer.clear()
            self._buffer_bytes = 0
            self._data = []
            self._event = ""
            self._id = None
            self._has_data = False
        return not self._exceeded

    def _complete(self, out: list[SseEvent]) -> None:
        line = "".join(self._buffer)
        self._buffer.clear()
        self._buffer_bytes = 0
        self._line(line, out)
        self._check_limit()

    def end(self) -> list[SseEvent]:
        """A held CR ends its line; any open event is discarded."""
        out: list[SseEvent] = []
        if not self._exceeded and self._pending_cr:
            self._complete(out)
        self._pending_cr = False
        self._buffer.clear()
        self._buffer_bytes = 0
        self._event = ""
        self._data = []
        self._has_data = False
        self._event_bytes = 0
        return out

    def _line(self, line: str, out: list[SseEvent]) -> None:
        if line == "":
            if self._has_data:
                out.append(
                    SseEvent(
                        self._event if self._event != "" else "message",
                        "\n".join(self._data),
                        self._id,
                        self._retry,
                    )
                )
            self._event = ""
            self._data = []
            self._has_data = False
            self._event_bytes = 0
            return
        if line.startswith(":"):
            return
        self._event_bytes += len(line.encode("utf-8")) + 1
        if self._event_bytes > self._max:
            self._exceeded = True
            return
        colon = line.find(":")
        name = line if colon < 0 else line[:colon]
        value = "" if colon < 0 else line[colon + 1 :]
        if value.startswith(" "):
            value = value[1:]
        if name == "event":
            self._event = value
        elif name == "data":
            self._data.append(value)
            self._has_data = True
        elif name == "id":
            if "\0" not in value:
                self._id = value
        elif name == "retry" and value.isascii() and value.isdigit():
            digits = value.lstrip("0") or "0"
            if len(digits) < 16 or (len(digits) == 16 and digits <= "9007199254740991"):
                self._retry = int(digits)
