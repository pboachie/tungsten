# SPDX-License-Identifier: Apache-2.0
"""Experimental Python dispatch for browser WebMCP tools.

Browser registration stays JavaScript (``registerRemoteWebMcp`` of
``@elysra/tungsten-webmcp``). This module is the application-owned server end
it calls: a framework-neutral ASGI app, mounted by the application under a
prefix of its own origin, that lists the tools a signed-in principal may use
and runs each call through the generated Python SDK and its runtime policy.
It is application integration, not a WebMCP protocol.

Contract (``webmcpDispatchVersion`` 1), relative to the mount point:

- ``GET  .../tools``: ``{"webmcpDispatchVersion": 1, "tools": [...]}``, each a
  WebMCP tool (``name``, ``description``, ``inputSchema``, ``annotations``)
  with ``call: {"tool", "preview"}`` to send back.
- ``POST .../call`` with ``{"tool", "preview", "arguments"}``: ``{"ok": true,
  "value"}`` or ``{"ok": false, "error": <diagnostic envelope>}``.

Every request needs the header ``X-Tungsten-WebMCP: 1`` and an authenticated
principal (``WebMcpOptions.principal``); a ``POST`` also needs a JSON body and
an ``Origin`` equal to the request's own origin (or in ``allowed_origins``).
No CORS headers are ever sent. Exposure defaults to read-only operations;
writes and macros need an explicit allowlist, checked here on every call.
"""

from __future__ import annotations

import inspect
import json
from collections import OrderedDict
from collections.abc import Awaitable, Callable, Mapping, MutableMapping, Sequence
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import urlsplit

from tungsten_runtime import Ok

from ._catalog import Catalog, CatalogTool, build_catalog
from ._options import ServerOptions
from ._render import envelope, to_json
from ._session import Session, _Outcome

#: Version of the dispatch contract.
DISPATCH_VERSION = 1
#: The header every request must carry (CSRF: a cross-site form or a simple
#: request cannot set it, and no CORS preflight is ever granted).
CSRF_HEADER = "x-tungsten-webmcp"
DEFAULT_MAX_BYTES = 1024 * 1024
DEFAULT_MAX_PRINCIPALS = 1000

Scope = MutableMapping[str, Any]
Receive = Callable[[], Awaitable[MutableMapping[str, Any]]]
Send = Callable[[MutableMapping[str, Any]], Awaitable[None]]
#: Resolves the signed-in principal (user, tenant or conversation) of a request
#: from its ASGI scope, or None when the request is not authenticated.
PrincipalResolver = Callable[[Scope], "str | Awaitable[str | None] | None"]
#: Whether a principal may see and call a tool.
Authorizer = Callable[[str, str], "bool | Awaitable[bool]"]


@dataclass
class WebMcpOptions:
    """Options of the dispatch app."""

    #: The application's authentication: the principal of a request, or None
    #: (answered 401). Required: tools never run for an anonymous caller.
    principal: PrincipalResolver
    #: Manifest tool names to expose. Default: the read-only operations.
    #: Mutations and macros are exposed only when listed here.
    allowlist: Sequence[str] | None = None
    #: Per-principal authorization of each tool, checked when listing and on
    #: every call. Default: every exposed tool for every principal.
    authorize: Authorizer | None = None
    #: Origins accepted besides the request's own (for example the page's
    #: origin behind a proxy that rewrites Host).
    allowed_origins: Sequence[str] = field(default_factory=tuple)
    #: UTF-8 bytes of one request body and of one result; no silent clipping.
    max_bytes: int = DEFAULT_MAX_BYTES
    #: Principals with live state (API client, confirmation tokens); the
    #: least recently used one is dropped past it.
    max_principals: int = DEFAULT_MAX_PRINCIPALS

    def __post_init__(self) -> None:
        if not callable(self.principal):
            raise ValueError("WebMcpOptions.principal must be callable")
        if self.authorize is not None and not callable(self.authorize):
            raise ValueError("WebMcpOptions.authorize must be callable")
        if not isinstance(self.max_bytes, int) or self.max_bytes < 1024:
            raise ValueError("WebMcpOptions.max_bytes must be an integer of at least 1024")
        if not isinstance(self.max_principals, int) or self.max_principals < 1:
            raise ValueError("WebMcpOptions.max_principals must be a positive integer")


def _binary(media: str | None) -> bool:
    if not media:
        return False
    value = media.lower().split(";")[0].strip()
    return not (
        value.startswith("text/")
        or value.endswith(("json", "+json", "xml", "+xml"))
        or value in ("application/x-ndjson", "application/jsonl", "application/jsonlines")
    )


def _failure(operation: str, code: str, remediation: str, category: str = "VALIDATION_FAILED") -> dict[str, Any]:
    error = envelope(operation, category, remediation=remediation)
    error["code"] = code
    return {"ok": False, "error": error}


async def _maybe(value: Any) -> Any:
    return await value if inspect.isawaitable(value) else value


class WebMcpDispatch:
    """The ASGI app. ``forget(principal)`` drops a principal's state (call it
    at logout); ``aclose()`` releases every client."""

    def __init__(self, options: ServerOptions, web: WebMcpOptions) -> None:
        self._options = options
        self._web = web
        catalog: Catalog = build_catalog(options.manifest, "discrete")
        self._catalog = catalog
        names = (
            list(web.allowlist)
            if web.allowlist is not None
            else [t.name for t in catalog.tools if t.kind == "operation" and t.safety == "read_only"]
        )
        if len(set(names)) != len(names):
            raise ValueError("allowlist must contain unique manifest names")
        unknown = [n for n in names if n not in catalog.by_name]
        if unknown:
            raise ValueError(f"unknown allowlist tool(s): {', '.join(unknown)}")
        self._exposed = {n: catalog.by_name[n] for n in names}
        self._listing: list[dict[str, Any]] = []
        for tool in self._exposed.values():
            self._listing.append(self._tool(tool, preview=False))
            if tool.confirmation_token is not None:
                self._listing.append(self._tool(tool, preview=True))
        listed = [t["name"] for t in self._listing]
        if len(set(listed)) != len(listed):
            raise ValueError("generated preview tool names collide with manifest tool names")
        self._sessions: OrderedDict[str, Session] = OrderedDict()

    # ---------------------------------------------------------- the tools

    @staticmethod
    def _tool(tool: CatalogTool, preview: bool) -> dict[str, Any]:
        schema = dict(tool.input_schema)
        token = tool.confirmation_token
        if preview and token is not None:
            if isinstance(schema.get("required"), list):
                schema["required"] = [n for n in schema["required"] if n != token]
            if isinstance(schema.get("properties"), dict):
                schema["properties"] = {k: v for k, v in schema["properties"].items() if k != token}
        description = (
            f"Preview {tool.name} and obtain a confirmation_token bound to these exact arguments. Review the "
            f"effects before executing. {tool.description}"
            if preview
            else tool.description
        )
        return {
            "name": f"{tool.name}__preview" if preview else tool.name,
            "description": description,
            "inputSchema": schema,
            "annotations": {
                "readOnlyHint": preview or tool.safety == "read_only",
                "untrustedContentHint": True,
                "consequentialHint": not preview and tool.safety != "read_only",
                "debugging": False,
            },
            "call": {"tool": tool.name, "preview": preview},
        }

    async def _allowed(self, principal: str, name: str) -> bool:
        if self._web.authorize is None:
            return True
        try:
            return (await _maybe(self._web.authorize(principal, name))) is True
        except Exception:
            return False

    async def _session(self, principal: str) -> Session:
        session = self._sessions.get(principal)
        if session is None:
            session = Session(self._catalog, self._options)
            self._sessions[principal] = session
            while len(self._sessions) > self._web.max_principals:
                _, old = self._sessions.popitem(last=False)
                await old.aclose()
        else:
            self._sessions.move_to_end(principal)
        return session

    async def forget(self, principal: str) -> None:
        """Drop ``principal``'s client and confirmation state (logout)."""
        session = self._sessions.pop(principal, None)
        if session is not None:
            await session.aclose()

    async def aclose(self) -> None:
        sessions = list(self._sessions.values())
        self._sessions.clear()
        for session in sessions:
            await session.aclose()

    async def call(self, principal: str, tool_name: Any, preview: Any, arguments: Any) -> dict[str, Any]:
        """Run one call for ``principal``; never raises."""
        tool = self._exposed.get(tool_name) if isinstance(tool_name, str) else None
        if tool is None or not isinstance(preview, bool) or (preview and tool.confirmation_token is None):
            return _failure(
                str(tool_name)[:100], "UNKNOWN_TOOL", "Call a tool listed by this page's WebMCP registration."
            )
        if not await self._allowed(principal, tool.name):
            return _failure(
                tool.target, "FORBIDDEN", "This tool is not available to the signed-in user.", "AUTH_FAILED"
            )
        try:
            return await self._run(await self._session(principal), tool, preview, arguments)
        except Exception:
            return _failure(
                tool.target,
                "EXECUTION_FAILED",
                "The server failed unexpectedly. Inspect API state before retrying a mutation.",
                "TRANSPORT_FAILED",
            )

    async def _run(self, session: Session, tool: CatalogTool, preview: bool, raw: Any) -> dict[str, Any]:
        if not isinstance(raw, Mapping):
            return _failure(tool.target, "INVALID_ARGUMENTS", "Pass a JSON object matching the tool input schema.")
        prepared = session._prepare(tool, dict(raw))
        if prepared.invalid is not None:
            return {"ok": False, "error": prepared.invalid}
        if tool.confirmation_token and raw.get(tool.confirmation_token) is not None and prepared.confirm is None:
            return _failure(
                tool.target,
                "INVALID_RESERVED_FIELD",
                f"Reserved field {tool.confirmation_token} must be a nonempty string; use the preview token for "
                "confirmation.",
            )
        target = session._resolve(tool)
        if isinstance(target, _Outcome):
            return {"ok": False, "error": target.error}
        core = session._core
        assert core is not None
        kind, descriptor = target
        ops = [descriptor] if kind == "operation" else [core.operation(s["operation"]) for s in descriptor["steps"]]
        if not preview and any(op is not None and op.get("stream") is not None for op in ops):
            return _failure(
                tool.target, "STREAMING_UNSUPPORTED", "Streamed results are not exposed to the browser; use the SDK."
            )
        if not preview and any(
            op is not None and any(r.get("kind") == "success" and _binary(r.get("media_type")) for r in op["responses"])
            for op in ops
        ):
            return _failure(
                tool.target, "BINARY_UNSUPPORTED", "Binary output is not exposed to the browser; use the SDK."
            )
        secret = (
            tool.shown_once
            or bool(tool.sensitive_response_fields)
            or any(
                op is not None and (op["agent"].get("shown_once") or op["agent"].get("sensitive_response_fields"))
                for op in ops
            )
        )
        if secret and (not preview or (kind == "operation" and descriptor["agent"]["preview"].get("mode") == "header")):
            return _failure(
                tool.target,
                "SENSITIVE_RESULT_UNSUPPORTED",
                "This tool can return credentials or values shown only once. Use the SDK directly in trusted "
                "application code; this WebMCP dispatch does not expose secret-bearing results.",
            )
        opts = session._call_options(prepared, not preview)
        opts["max_response_bytes"] = self._web.max_bytes
        if not preview and kind == "operation" and tool.safety == "irreversible":
            opts["verify"] = True
        args = prepared.args
        if kind == "macro":
            result = await (core.preview_macro if preview else core.run_macro)(descriptor, args, opts)  # type: ignore[arg-type]
        else:
            result = await (core.preview if preview else core.call)(descriptor, args, opts)  # type: ignore[arg-type]
        if isinstance(result, Ok):
            out: dict[str, Any] = {"ok": True, "value": to_json(result.value)}
            if result.verification is not None:
                out["verification"] = to_json(result.verification)
        else:
            out = {"ok": False, "error": to_json(result.error)}
            partial = getattr(result, "partial", None)
            if partial is not None:
                out["partial"] = to_json(partial)
        if len(json.dumps(out, ensure_ascii=False, separators=(",", ":")).encode()) > self._web.max_bytes:
            return _failure(
                tool.target,
                "RESULT_TOO_LARGE",
                "The API result is larger than the dispatch limit. The call may already have taken effect; "
                "inspect API state before retrying.",
                "UNEXPECTED_RESPONSE",
            )
        return out

    # ---------------------------------------------------------- the HTTP side

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] == "lifespan":
            while True:
                message = await receive()
                if message["type"] == "lifespan.startup":
                    await send({"type": "lifespan.startup.complete"})
                elif message["type"] == "lifespan.shutdown":
                    await self.aclose()
                    await send({"type": "lifespan.shutdown.complete"})
                    return
        if scope["type"] != "http":
            return
        path = scope.get("path", "").rstrip("/")
        method = scope.get("method", "GET")
        route = "tools" if path.endswith("/tools") else "call" if path.endswith("/call") else None
        if route is None:
            await _respond(send, 404, {"error": "not found"})
            return
        if (route, method) not in (("tools", "GET"), ("call", "POST")):
            await _respond(send, 405, {"error": "method not allowed"}, {"allow": "GET" if route == "tools" else "POST"})
            return
        headers: dict[str, str] = {}
        for key, value in scope.get("headers") or []:
            headers.setdefault(key.decode("latin-1").lower(), value.decode("latin-1"))
        if headers.get(CSRF_HEADER) != "1":
            await _respond(send, 403, {"error": f"missing {CSRF_HEADER}: 1"})
            return
        origin = headers.get("origin")
        if (origin is not None or method == "POST") and not self._same_origin(scope, headers, origin):
            await _respond(send, 403, {"error": "origin not allowed"})
            return
        try:
            principal = await _maybe(self._web.principal(scope))
        except Exception:
            principal = None
        if not isinstance(principal, str) or principal == "":
            await _respond(send, 401, {"error": "not authenticated"})
            return
        if route == "tools":
            tools = [t for t in self._listing if await self._allowed(principal, t["call"]["tool"])]
            await _respond(send, 200, {"webmcpDispatchVersion": DISPATCH_VERSION, "tools": tools})
            return
        if not headers.get("content-type", "").lower().startswith("application/json"):
            await _respond(send, 415, {"error": "expected application/json"})
            return
        body = await _read(receive, self._web.max_bytes)
        if body is None:
            await _respond(send, 413, {"error": "request body too large"})
            return
        try:
            request = json.loads(body)
        except ValueError:
            await _respond(send, 400, {"error": "the body is not JSON"})
            return
        if not isinstance(request, dict):
            await _respond(send, 400, {"error": "the body is not a JSON object"})
            return
        result = await self.call(
            principal, request.get("tool"), request.get("preview", False), request.get("arguments", {})
        )
        await _respond(send, 200, result)

    def _same_origin(self, scope: Scope, headers: Mapping[str, str], origin: str | None) -> bool:
        if origin is None:
            return False
        if origin in self._web.allowed_origins:
            return True
        host = headers.get("host")
        scheme = scope.get("scheme", "http")
        try:
            parsed = urlsplit(origin)
        except ValueError:
            return False
        return host is not None and parsed.scheme == scheme and parsed.netloc.lower() == host.lower()


async def _read(receive: Receive, limit: int) -> bytes | None:
    chunks: list[bytes] = []
    size = 0
    while True:
        message = await receive()
        if message["type"] != "http.request":
            return b"".join(chunks)
        chunk = message.get("body", b"")
        size += len(chunk)
        if size > limit:
            return None
        chunks.append(chunk)
        if not message.get("more_body"):
            return b"".join(chunks)


async def _respond(send: Send, status: int, payload: Any, extra: Mapping[str, str] | None = None) -> None:
    body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode()
    headers = [
        (b"content-type", b"application/json"),
        (b"content-length", str(len(body)).encode()),
        (b"cache-control", b"no-store"),
        (b"x-content-type-options", b"nosniff"),
    ]
    headers += [(k.encode(), v.encode()) for k, v in (extra or {}).items()]
    await send({"type": "http.response.start", "status": status, "headers": headers})
    await send({"type": "http.response.body", "body": body})


def webmcp_app(options: ServerOptions, web: WebMcpOptions) -> WebMcpDispatch:
    """The dispatch app of ``options`` (the manifest and client factory a
    generated server builds) for ``web``. Raises ``ValueError`` for an invalid
    allowlist."""
    return WebMcpDispatch(options, web)
