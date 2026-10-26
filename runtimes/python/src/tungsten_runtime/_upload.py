# SPDX-License-Identifier: Apache-2.0
"""Streamed upload sources: a request body read chunk by chunk from the
caller instead of held in memory, with an explicit replay rule. The
semantics of ``UploadSource`` in ``runtimes/ts/src/upload.ts``.

A one-shot source is sent at most once: once it was read, the request is
never repeated (no retry, no redirect). A recreatable source is opened again
for every attempt. A declared length or SHA-256 is checked against the bytes
read; the last chunk is held back until both match, so a source that differs
never completes its request."""

from __future__ import annotations

import hashlib
import re
from collections.abc import AsyncIterable, AsyncIterator, Callable, Iterable, Iterator
from dataclasses import dataclass
from typing import Literal, cast

UploadReplay = Literal["one_shot", "recreatable"]
Chunks = Iterable[bytes] | AsyncIterable[bytes]

_HEX64 = re.compile(r"[0-9a-fA-F]{64}")
_MAX_SAFE = 9007199254740991


@dataclass(frozen=True, slots=True)
class UploadFailure:
    """Why a source ended an attempt: ``length`` (``read`` bytes, not the
    declared length), ``sha256``, or ``source`` (it raised; ``detail`` names
    the exception type)."""

    kind: Literal["length", "sha256", "source"]
    read: int = 0
    detail: str = ""


class _Rejected(Exception):
    """Ends the body the transport is reading."""


class UploadSource:
    """A request body streamed from the caller. Pass it where the operation
    takes binary data for its whole body (not inside a multipart field).
    Build it with ``UploadSource.one_shot`` or ``UploadSource.recreatable``."""

    __slots__ = ("_open", "_opened", "length", "replay", "sha256")

    def __init__(
        self,
        replay: UploadReplay,
        open: Callable[[], Chunks],
        *,
        length: int | None = None,
        sha256: str | None = None,
    ) -> None:
        if length is not None and (
            isinstance(length, bool) or not isinstance(length, int) or not 0 <= length <= _MAX_SAFE  # pyright: ignore[reportUnnecessaryIsInstance]
        ):
            raise ValueError("UploadSource length must be a non-negative safe integer")
        if sha256 is not None and _HEX64.fullmatch(sha256) is None:
            raise ValueError("UploadSource sha256 must be 64 hexadecimal digits")
        #: ``one_shot`` or ``recreatable``.
        self.replay: UploadReplay = replay
        #: The declared length, or None; sent as ``Content-Length``.
        self.length = length
        #: The declared SHA-256 (lower-case hex), or None. It is the
        #: ``content_hash`` idempotency key and binds confirmation tokens to the
        #: bytes; operations whose key or confirmation depends on the body
        #: require it.
        self.sha256 = sha256.lower() if sha256 is not None else None
        self._open = open
        self._opened = False

    @classmethod
    def one_shot(
        cls, chunks: Chunks, *, length: int | None = None, sha256: str | None = None
    ) -> UploadSource:
        """A source that can be read once (an iterator or async iterator of
        bytes): a request whose body was read is never sent again."""
        return cls("one_shot", lambda: chunks, length=length, sha256=sha256)

    @classmethod
    def recreatable(
        cls, open: Callable[[], Chunks], *, length: int | None = None, sha256: str | None = None
    ) -> UploadSource:
        """A source that ``open`` recreates from the start for every attempt
        (a file opened again, an object re-fetched); every attempt must
        produce the same bytes."""
        return cls("recreatable", open, length=length, sha256=sha256)

    @property
    def opened(self) -> bool:
        """Whether an attempt has started reading the source."""
        return self._opened

    @property
    def sendable(self) -> bool:
        """Whether another attempt may send this source."""
        return self.replay == "recreatable" or not self._opened

    def attempt(self) -> UploadAttempt:
        """The body of one attempt."""
        return UploadAttempt(self)

    def __repr__(self) -> str:
        return f"UploadSource({self.replay!r}, length={self.length!r}, sha256={self.sha256!r})"


class UploadAttempt:
    """One attempt's body: the transport iterates ``sync_chunks()`` or
    ``async_chunks()``; ``sent`` and ``failure`` say how it went."""

    __slots__ = ("_hash", "_pending", "_read", "_source", "failure", "sent")

    def __init__(self, source: UploadSource) -> None:
        self._source = source
        self._hash = hashlib.sha256() if source.sha256 is not None else None
        self._pending: bytes | None = None
        self._read = 0
        #: Bytes handed to the transport so far.
        self.sent = 0
        #: Why the source ended the attempt, or None.
        self.failure: UploadFailure | None = None

    def _stop(self, why: UploadFailure) -> _Rejected:
        self.failure = why
        return _Rejected("upload source rejected")

    def _take(self, chunk: object) -> list[bytes]:
        """The bytes to send now for one chunk read from the source."""
        source = self._source
        if not isinstance(chunk, bytes | bytearray | memoryview):
            raise self._stop(UploadFailure("source", detail="a chunk that is not bytes"))
        data = bytes(cast(bytes, chunk))
        if len(data) == 0:
            return []
        self._read += len(data)
        if self._hash is not None:
            self._hash.update(data)
        if source.length is not None and self._read > source.length:
            raise self._stop(UploadFailure("length", read=self._read))
        if source.length is None and source.sha256 is None:
            return self._emit(data)
        # The last chunk waits until the declared length and digest match.
        ready, self._pending = self._pending, data
        return [] if ready is None else self._emit(ready)

    def _finish(self) -> list[bytes]:
        source = self._source
        if source.length is not None and self._read != source.length:
            raise self._stop(UploadFailure("length", read=self._read))
        if self._hash is not None and self._hash.hexdigest() != source.sha256:
            raise self._stop(UploadFailure("sha256"))
        ready, self._pending = self._pending, None
        return [] if ready is None else self._emit(ready)

    def _emit(self, data: bytes) -> list[bytes]:
        self.sent += len(data)
        return [data]

    def _source_failed(self, error: Exception) -> _Rejected:
        if isinstance(error, _Rejected):
            return error
        return self._stop(UploadFailure("source", detail=type(error).__name__))

    def sync_chunks(self) -> Iterator[bytes]:
        """The body for a synchronous transport."""
        self._source._opened = True  # pyright: ignore[reportPrivateUsage]
        try:
            chunks = self._source._open()  # pyright: ignore[reportPrivateUsage]
            if not isinstance(chunks, Iterable):
                raise TypeError("an async source needs AsyncClientCore")
            for chunk in chunks:
                yield from self._take(chunk)
            yield from self._finish()
        except Exception as error:
            raise self._source_failed(error) from None

    async def async_chunks(self) -> AsyncIterator[bytes]:
        """The body for an asynchronous transport (a synchronous source is
        read on the event loop)."""
        self._source._opened = True  # pyright: ignore[reportPrivateUsage]
        try:
            chunks = self._source._open()  # pyright: ignore[reportPrivateUsage]
            if isinstance(chunks, AsyncIterable):
                async for chunk in chunks:
                    for data in self._take(chunk):
                        yield data
            else:
                for chunk in chunks:
                    for data in self._take(chunk):
                        yield data
            for data in self._finish():
                yield data
        except Exception as error:
            raise self._source_failed(error) from None


def upload_marker(source: UploadSource) -> str:
    """The canonical JSON standing for a source (it is never read for it)."""
    length = "null" if source.length is None else str(source.length)
    digest = "null" if source.sha256 is None else f'"{source.sha256}"'
    return f'{{"$upload":{{"length":{length},"replay":"{source.replay}","sha256":{digest}}}}}'
