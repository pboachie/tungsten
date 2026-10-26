# SPDX-License-Identifier: Apache-2.0
"""Streamed downloads: the body of a 2xx answer handed to the caller chunk by
chunk instead of being held in memory, with a total-byte limit, progress
reports, the idle timeout and the call's deadline, and one release of the
connection (and its scheduler capacity) however the download ends. The
semantics of ``Download`` in ``runtimes/ts/src/download.ts``."""

from __future__ import annotations

import contextlib
import inspect
import re
from collections.abc import AsyncIterator, Awaitable, Callable, Iterator
from dataclasses import dataclass
from types import TracebackType
from typing import Any, Literal, cast

from ._effects import Chunk, ReadOutcome, StreamEnded
from .types import Diagnostic, Err, Ok, ResponseMeta

DownloadFailure = Literal["idle", "deadline", "aborted", "lost", "truncated", "oversized", "sink"]

_DIGITS = re.compile(r"\d+")
_HEX2 = re.compile(r"[0-9A-Fa-f]{2}")
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")


@dataclass(frozen=True, slots=True)
class TransferProgress:
    """How far a download has come."""

    #: Body bytes received so far.
    bytes: int
    #: The declared ``Content-Length``, or None when the server sent none.
    total: int | None


@dataclass(frozen=True, slots=True)
class DownloadStart:
    """What ``Engine.download`` hands the client: the driver's stream (None
    for an answer without a body) and how to describe a failure."""

    stream: object | None
    meta: ResponseMeta
    max_bytes: int | None
    on_progress: Callable[[TransferProgress], object] | None
    failure: Callable[[DownloadFailure, int, str | None], Diagnostic]


def declared_length(value: str | None) -> int | None:
    """A ``Content-Length`` of digits only, else None."""
    if value is None or not _DIGITS.fullmatch(value.strip()):
        return None
    length = int(value.strip())
    return length if length <= 9007199254740991 else None


def _header_params(value: str) -> dict[str, str]:
    """The parameters of a header value such as ``Content-Disposition``,
    after its first ``;``; names lower-cased, quoted strings unescaped."""
    out: dict[str, str] = {}
    i = value.find(";")
    if i < 0:
        return out
    i += 1
    n = len(value)
    while i < n:
        eq = value.find("=", i)
        semi = value.find(";", i)
        if eq < 0 or (0 <= semi < eq):
            if semi < 0:
                break
            i = semi + 1
            continue
        name = value[i:eq].strip().lower()
        j = eq + 1
        while j < n and value[j] in (" ", "\t"):
            j += 1
        param = ""
        if j < n and value[j] == '"':
            j += 1
            while j < n and value[j] != '"':
                if value[j] == "\\" and j + 1 < n:
                    j += 1
                param += value[j]
                j += 1
            nxt = value.find(";", j)
            i = n if nxt < 0 else nxt + 1
        else:
            nxt = value.find(";", j)
            param = value[j : n if nxt < 0 else nxt].strip()
            i = n if nxt < 0 else nxt + 1
        if name != "" and name not in out:
            out[name] = param
    return out


def _extended_value(value: str) -> str | None:
    """An RFC 8187 ``UTF-8''percent-encoded`` value, decoded; None otherwise."""
    quote = value.find("'")
    second = -1 if quote < 0 else value.find("'", quote + 1)
    if second < 0 or value[:quote].lower() != "utf-8":
        return None
    encoded = value[second + 1 :]
    out = bytearray()
    i = 0
    while i < len(encoded):
        c = encoded[i]
        if c == "%":
            pair = encoded[i + 1 : i + 3]
            if not _HEX2.fullmatch(pair):
                return None
            out.append(int(pair, 16))
            i += 3
            continue
        if ord(c) > 0x7E or ord(c) < 0x21:
            return None
        out.append(ord(c))
        i += 1
    try:
        return out.decode("utf-8")
    except UnicodeDecodeError:
        return None


def disposition_filename(value: str | None) -> str | None:
    """The file name a ``Content-Disposition`` value suggests (``filename*``
    before ``filename``), reduced to a base name that cannot leave a
    directory: the part after the last ``/`` or ``\\``, control characters
    removed, spaces trimmed, at most 255 characters; None when nothing usable is
    left (empty, ``.`` or ``..``)."""
    if value is None:
        return None
    params = _header_params(value)
    extended = params.get("filename*")
    raw = _extended_value(extended) if extended is not None else None
    if raw is None:
        raw = params.get("filename")
    if raw is None:
        return None
    base = re.split(r"[/\\]", raw)[-1]
    cleaned = _CONTROL.sub("", base).strip(" ")[:255].strip(" ")
    return None if cleaned in ("", ".", "..") else cleaned


class _Transfer:
    """The state both download handles share."""

    __slots__ = ("_final", "_received", "_start", "content_length", "content_type", "filename", "meta")

    def __init__(self, start: DownloadStart) -> None:
        self._start = start
        #: The answer's status, headers, request id and attempts.
        self.meta = start.meta
        #: The declared ``Content-Length``, or None.
        self.content_length = declared_length(start.meta.headers.get("content-length"))
        #: The ``Content-Type``, or None.
        self.content_type = start.meta.headers.get("content-type")
        #: The ``Content-Disposition`` file name reduced to a plain base name,
        #: or None. It names no destination: callers choose where bytes go.
        self.filename = disposition_filename(start.meta.headers.get("content-disposition"))
        self._received = 0
        self._final: Ok[bytes | None] | Err | None = None

    @property
    def received_bytes(self) -> int:
        """Body bytes received so far."""
        return self._received

    def _failed(self, why: DownloadFailure, detail: str | None = None) -> Err:
        return Err(error=self._start.failure(why, self._received, detail))

    def _settle(self, read: ReadOutcome) -> tuple[Ok[bytes | None] | Err, bool]:
        """The item for one read, and whether the download is over."""
        if isinstance(read, Chunk):
            self._received += len(read.data)
            limit = self._start.max_bytes
            if limit is not None and self._received > limit:
                return self._failed("oversized"), True
            observer = self._start.on_progress
            if observer is not None:
                # Observers never change the transfer.
                with contextlib.suppress(Exception):
                    observer(TransferProgress(self._received, self.content_length))
            return Ok(value=read.data, meta=self.meta), False
        if isinstance(read, StreamEnded):
            if self.content_length is not None and self._received < self.content_length:
                return self._failed("truncated"), True
            return Ok(value=None, meta=self.meta), True
        kind = read.kind
        return self._failed(
            "idle" if kind == "timeout" else "deadline" if kind == "deadline" else "lost"
        ), True


class Download(_Transfer):
    """A streamed response body (``ClientCore.download``). Read it with
    ``read()``, iterate it, or hand it to a sink with ``write_to()``;
    ``close()`` abandons it. The connection is released once the body ended,
    failed or was closed. After the first failure every read returns that
    failure. Usable as a context manager."""

    __slots__ = ()

    def read(self) -> Ok[bytes | None] | Err:
        """The next chunk, ``Ok(None)`` at the end, or the failure that ended
        the download."""
        if self._final is not None:
            return self._final
        stream = self._start.stream
        item, over = self._settle(StreamEnded() if stream is None else cast(Any, stream).read())
        if over:
            self._final = item
            self._release()
        return item

    def __iter__(self) -> Iterator[Ok[bytes] | Err]:
        """Every chunk in order; a failure is the last item. Leaving the loop
        early closes the download."""
        try:
            while True:
                item = self.read()
                if isinstance(item, Ok) and item.value is None:
                    return
                yield cast(Ok[bytes] | Err, item)
                if isinstance(item, Err):
                    return
        finally:
            self.close()

    def write_to(self, sink: Callable[[bytes], object]) -> Ok[int] | Err:
        """Hand every chunk to ``sink`` before the next read (a slow sink slows
        the transfer instead of growing a buffer); ``Ok`` holds the bytes
        delivered. A sink that raises ends the download (``code``
        ``SINK_FAILED``). The download is closed when this returns."""
        try:
            while True:
                item = self.read()
                if isinstance(item, Err):
                    return item
                if item.value is None:
                    return Ok(value=self._received, meta=self.meta)
                try:
                    sink(item.value)
                except Exception as error:
                    self._final = self._failed("sink", type(error).__name__)
                    return self._final
        finally:
            self.close()

    def close(self) -> None:
        """Abandon the download and release the connection. Safe to call more
        than once."""
        if self._final is None:
            self._final = self._failed("aborted")
        self._release()

    def _release(self) -> None:
        stream = self._start.stream
        if stream is not None:
            cast(Any, stream).close()

    def __enter__(self) -> Download:
        return self

    def __exit__(
        self, kind: type[BaseException] | None, error: BaseException | None, traceback: TracebackType | None
    ) -> None:
        self.close()


class AsyncDownload(_Transfer):
    """``Download`` for ``AsyncClientCore``: ``await read()``, ``async for``,
    ``await write_to(sink)`` (an awaitable the sink returns is awaited) and
    ``await aclose()``. Usable as an async context manager."""

    __slots__ = ()

    async def read(self) -> Ok[bytes | None] | Err:
        """See ``Download.read``."""
        if self._final is not None:
            return self._final
        stream = self._start.stream
        read: ReadOutcome = StreamEnded() if stream is None else await cast(Any, stream).read()
        item, over = self._settle(read)
        if over:
            self._final = item
            await self._release()
        return item

    async def __aiter__(self) -> AsyncIterator[Ok[bytes] | Err]:
        """See ``Download.__iter__``."""
        try:
            while True:
                item = await self.read()
                if isinstance(item, Ok) and item.value is None:
                    return
                yield cast(Ok[bytes] | Err, item)
                if isinstance(item, Err):
                    return
        finally:
            await self.aclose()

    async def write_to(self, sink: Callable[[bytes], object]) -> Ok[int] | Err:
        """See ``Download.write_to``."""
        try:
            while True:
                item = await self.read()
                if isinstance(item, Err):
                    return item
                if item.value is None:
                    return Ok(value=self._received, meta=self.meta)
                try:
                    result = sink(item.value)
                    if inspect.isawaitable(result):
                        await cast(Awaitable[object], result)
                except Exception as error:
                    self._final = self._failed("sink", type(error).__name__)
                    return self._final
        finally:
            await self.aclose()

    async def aclose(self) -> None:
        """See ``Download.close``."""
        if self._final is None:
            self._final = self._failed("aborted")
        await self._release()

    async def _release(self) -> None:
        stream = self._start.stream
        if stream is not None:
            await cast(Any, stream).aclose()

    async def __aenter__(self) -> AsyncDownload:
        return self

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, traceback: TracebackType | None
    ) -> None:
        await self.aclose()
