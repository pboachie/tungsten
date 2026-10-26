# SPDX-License-Identifier: Apache-2.0
"""Streamable HTTP transport: one MCP session, with its own client core, per
``Mcp-Session-Id``. Mirrors ``http.ts`` of the TypeScript adapter: loopback by
default, an optional bearer token checked in constant time before anything
else, Host and Origin checks against DNS rebinding, and bounded sessions,
request bodies and idle time. The session handling itself is the official MCP
SDK's ``StreamableHTTPSessionManager``."""

from __future__ import annotations

import hashlib
import hmac
import json
import re
import sys
from collections.abc import Awaitable, Callable, MutableMapping, Sequence
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import urlsplit

LOOPBACK = frozenset({"localhost", "127.0.0.1", "[::1]", "::1"})
DEFAULT_MAX_SESSIONS = 64
DEFAULT_MAX_BODY_BYTES = 4 * 1024 * 1024
DEFAULT_IDLE_TIMEOUT_MS = 30 * 60 * 1000

_BEARER = re.compile(r"^bearer[ \t]+(\S+)[ \t]*$", re.IGNORECASE)

Scope = MutableMapping[str, Any]
Receive = Callable[[], Awaitable[MutableMapping[str, Any]]]
Send = Callable[[MutableMapping[str, Any]], Awaitable[None]]
Asgi = Callable[[Scope, Receive, Send], Awaitable[None]]


@dataclass
class HttpOptions:
    """Options of the Streamable HTTP endpoint."""

    #: Interface to bind.
    host: str = "127.0.0.1"
    #: Port to bind; 0 picks a free one.
    port: int = 0
    #: Endpoint path.
    path: str = "/mcp"
    #: Host header values accepted besides the loopback names (DNS rebinding
    #: protection). Entries are ``host`` or ``host:port``.
    allowed_hosts: Sequence[str] = field(default_factory=tuple)
    #: Origin header values accepted besides loopback origins.
    allowed_origins: Sequence[str] = field(default_factory=tuple)
    #: Require ``Authorization: Bearer <token>`` on every request; others get
    #: 401 before any session is created or looked up. Compared in constant time.
    bearer_token: str | None = None
    #: Concurrent sessions; further ones are refused.
    max_sessions: int = DEFAULT_MAX_SESSIONS
    #: Largest request body in bytes.
    max_body_bytes: int = DEFAULT_MAX_BODY_BYTES
    #: A session idle this long is closed (clients that vanish without DELETE).
    idle_timeout_ms: int = DEFAULT_IDLE_TIMEOUT_MS

    def __post_init__(self) -> None:
        if self.bearer_token is not None and (not isinstance(self.bearer_token, str) or self.bearer_token == ""):
            raise ValueError("HttpOptions.bearer_token must be a non-empty string")
        if not self.path.startswith("/"):
            raise ValueError("HttpOptions.path must start with /")
        for name in ("max_sessions", "max_body_bytes", "idle_timeout_ms"):
            value = getattr(self, name)
            if not isinstance(value, int) or value < 1:
                raise ValueError(f"HttpOptions.{name} must be a positive integer")


def _hostname(value: str) -> str | None:
    try:
        return (urlsplit(f"http://{value}").hostname or "").lower() or None
    except ValueError:
        return None


def bearer_matches(header: str | None, token: str) -> bool:
    """Constant-time check of an ``Authorization: Bearer`` header against ``token``."""
    match = _BEARER.match(header) if isinstance(header, str) else None
    given = hashlib.sha256((match.group(1) if match else "").encode()).digest()
    wanted = hashlib.sha256(token.encode()).digest()
    return hmac.compare_digest(given, wanted) and match is not None


class Guard:
    """The checks every request passes before the MCP session manager sees
    it: authentication, the endpoint path, then Host and Origin."""

    def __init__(self, app: Asgi, options: HttpOptions) -> None:
        self.app = app
        self.options = options
        self.loopback = options.host.lower() in LOOPBACK
        self.allowed_hosts = {h.lower() for h in options.allowed_hosts}
        self.allowed_origins = set(options.allowed_origins)

    def host_allowed(self, value: str | None) -> bool:
        if not self.loopback and not self.allowed_hosts:
            return True
        if value is None:
            return False
        name = _hostname(value)
        return (
            (name is not None and name in LOOPBACK) or value.lower() in self.allowed_hosts or name in self.allowed_hosts
        )

    def origin_allowed(self, value: str | None) -> bool:
        if value is None or value in self.allowed_origins:
            return True
        try:
            return (urlsplit(value).hostname or "").lower() in LOOPBACK
        except ValueError:
            return False

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return
        headers: dict[str, str] = {}
        for key, value in scope.get("headers") or []:
            headers.setdefault(key.decode("latin-1").lower(), value.decode("latin-1"))
        token = self.options.bearer_token
        if token is not None and not bearer_matches(headers.get("authorization"), token):
            await _error(send, 401, -32000, "Unauthorized", {"www-authenticate": "Bearer"})
            return
        if scope.get("path") != self.options.path:
            await _error(send, 404, -32000, "Not found")
            return
        if not self.host_allowed(headers.get("host")):
            await _error(send, 403, -32000, "Host not allowed")
            return
        if not self.origin_allowed(headers.get("origin")):
            await _error(send, 403, -32000, "Origin not allowed")
            return
        await self.app(scope, receive, send)


async def _error(send: Send, status: int, code: int, message: str, extra: dict[str, str] | None = None) -> None:
    body = json.dumps({"jsonrpc": "2.0", "error": {"code": code, "message": message}, "id": None}).encode()
    headers = [(b"content-type", b"application/json"), (b"content-length", str(len(body)).encode())]
    headers += [(k.encode(), v.encode()) for k, v in (extra or {}).items()]
    await send({"type": "http.response.start", "status": status, "headers": headers})
    await send({"type": "http.response.body", "body": body})


def warn_if_open(options: HttpOptions) -> None:
    """Warn (standard error) about an endpoint anyone who can reach it may use."""
    if options.host.lower() not in LOOPBACK and options.bearer_token is None and not options.allowed_hosts:
        print(
            f"warning: Streamable HTTP is bound to {options.host} with no authentication (a bearer token) and no "
            "allowed hosts: any client that can reach it can use this server. Configure authentication or restrict "
            "access at the network.",
            file=sys.stderr,
        )
