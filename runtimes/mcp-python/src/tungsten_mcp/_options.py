# SPDX-License-Identifier: Apache-2.0
"""Options of a tungsten MCP server."""

from __future__ import annotations

from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any

from tungsten_runtime import AsyncClientCore

#: Default cap, in characters, on the JSON of one tool result.
DEFAULT_MAX_RESULT_CHARS = 50_000
#: Default cap on bytes read from each upstream response (4 MiB).
DEFAULT_MAX_RESPONSE_BYTES = 4 * 1024 * 1024
#: Default cap on the JSON bytes of the events one streamed call collects (4 MiB).
DEFAULT_MAX_STREAM_BYTES = 4 * 1024 * 1024
#: Default cap on the time one streamed call spends collecting events (60 s).
DEFAULT_MAX_STREAM_MS = 60_000


@dataclass
class ServerOptions:
    """What a generated server passes to ``serve``."""

    #: The manifest the MCP emitter wrote (``manifest.json``).
    manifest: Mapping[str, Any]
    #: Builds the client core of the session. Over stdio one process serves
    #: one client, so this is called once; confirmation tokens and shown-once
    #: state live in that core and session.
    create_core: Callable[[], AsyncClientCore]
    #: Server name and version reported to the client.
    name: str
    version: str
    #: Cap, in characters, on the JSON of one tool result; larger results are
    #: cut (arrays to their first items, then long strings) with a note.
    max_result_chars: int = DEFAULT_MAX_RESULT_CHARS
    #: Cap on bytes read from each upstream non-streaming API response before
    #: parsing, independent of result display truncation.
    max_response_bytes: int = DEFAULT_MAX_RESPONSE_BYTES
    #: Total deadline, in milliseconds, of one tool call, passed to the client
    #: runtime as ``CallOptions.deadline_ms``: it bounds authentication,
    #: idempotency-key reservation, queue admission, every attempt, retry
    #: waits, response bodies and verification, and a whole macro. Expiry
    #: gives a failure with ``code`` ``DEADLINE_EXCEEDED`` and nothing more is
    #: sent; a mutation already sent reports ``OUTCOME_UNKNOWN``. None or 0:
    #: none.
    tool_deadline_ms: int | None = None
    #: Cap, in bytes of JSON, on the events one streamed tool call collects;
    #: past it the call fails with an ``UNEXPECTED_RESPONSE`` envelope and the
    #: events so far as ``partial``. A streamed call also stops after 1000
    #: events (``truncated: true``).
    max_stream_bytes: int = DEFAULT_MAX_STREAM_BYTES
    #: Cap, in milliseconds, on the wall-clock time one streamed tool call may
    #: spend collecting events (same failure). The call's deadline bounds
    #: connecting and reconnects; this bounds collecting.
    max_stream_ms: int = DEFAULT_MAX_STREAM_MS
    #: Override the manifest's disclosure mode: ``discrete`` (every tool) or
    #: ``progressive`` (``search_tools``, ``describe_tool``, ``invoke``...).
    mode: str | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.max_result_chars, int) or self.max_result_chars < 200:
            raise ValueError("max_result_chars must be an integer of at least 200")
        if not isinstance(self.max_response_bytes, int) or self.max_response_bytes < 1:
            raise ValueError("max_response_bytes must be a positive integer")
        if self.tool_deadline_ms is not None and (
            not isinstance(self.tool_deadline_ms, int) or self.tool_deadline_ms < 0
        ):
            raise ValueError("tool_deadline_ms must be a non-negative integer of milliseconds")
        for name in ("max_stream_bytes", "max_stream_ms"):
            value = getattr(self, name)
            if not isinstance(value, int) or value < 1:
                raise ValueError(f"{name} must be a positive integer")
        if self.mode is not None and self.mode not in ("discrete", "progressive"):
            raise ValueError('mode must be "discrete" or "progressive"')
