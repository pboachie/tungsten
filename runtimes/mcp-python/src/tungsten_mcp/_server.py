# SPDX-License-Identifier: Apache-2.0
"""The MCP server: the official MCP Python SDK's low-level ``Server`` with a
``tools/list`` and a ``tools/call`` handler, served over stdio or Streamable
HTTP. Protocol messages go to standard output (stdio) or the HTTP response
only; diagnostics go to standard error."""

from __future__ import annotations

import json
import os
import socket
import sys
from collections.abc import Callable, Collection, Mapping, Sequence
from dataclasses import dataclass
from importlib import resources
from typing import Any

import anyio
import mcp_types as types
from mcp.server.lowlevel import Server
from mcp.server.stdio import stdio_server

from ._catalog import MODES, Catalog, ManifestError, build_catalog, list_tools
from ._http import Guard, HttpOptions, warn_if_open
from ._options import ServerOptions
from ._session import Session

_USAGE = """usage: <server> [--stdio | --http <port> [--host <host>] [--token-env <NAME>]]

  --stdio             serve one MCP session over stdin/stdout (default)
  --http <port>       serve Streamable HTTP at http://<host>:<port>/mcp (0 picks a free port)
  --host <host>       interface to bind with --http (default 127.0.0.1; other hosts accept
                      any Host header: use --token-env or your own access control)
  --token-env <NAME>  with --http, require `Authorization: Bearer <token>`, the token read
                      from the environment variable NAME (never pass it as an argument)
  --bearer-token-env <NAME>  same as --token-env
  --help              print this text
"""


class ServeError(Exception):
    """The server cannot start; the message says why and what to change."""


def load_manifest(package: str, name: str = "manifest.json") -> dict[str, Any]:
    """The manifest packaged as ``name`` inside ``package``."""
    text = resources.files(package).joinpath(name).read_text(encoding="utf-8")
    manifest = json.loads(text)
    if not isinstance(manifest, dict):
        raise ServeError(f"{package}/{name} is not a JSON object")
    return manifest


def check_mode(manifest: Mapping[str, Any], override: str | None, variable: str) -> str | None:
    """The disclosure mode forced by the environment variable ``variable``
    (``override``), if any; ``ServeError`` for any other value."""
    del manifest  # every mode of the manifest is served
    if override is not None and override not in MODES:
        raise ServeError(f'{variable} must be "discrete" or "progressive", not {override!r}')
    return override


class Sessions:
    """The tool session of each MCP session: its own client core, so
    confirmation tokens and shown-once state never cross sessions. Over stdio
    there is one; over HTTP, one per ``Mcp-Session-Id``, closed once the SDK's
    session manager has dropped that session (DELETE, idle timeout)."""

    def __init__(self, catalog: Catalog, options: ServerOptions, shared: bool) -> None:
        self._catalog = catalog
        self._options = options
        self._shared = Session(catalog, options) if shared else None
        self._by_id: dict[str, Session] = {}
        #: The session ids the transport still serves, when it can tell.
        self.live: Callable[[], Collection[str]] | None = None

    async def get(self, ctx: Any) -> Session:
        if self._shared is not None:
            return self._shared
        await self._prune()
        request = getattr(ctx, "request", None)
        headers = getattr(request, "headers", None)
        session_id = headers.get("mcp-session-id") if headers is not None else None
        if not session_id:
            # No MCP session to attach state to: a fresh core per request, so a
            # confirmation token never outlives the request that made it.
            return Session(self._catalog, self._options)
        session = self._by_id.get(session_id)
        if session is None:
            session = Session(self._catalog, self._options)
            self._by_id[session_id] = session
        return session

    async def _prune(self) -> None:
        if self.live is None:
            return
        live = set(self.live())
        for session_id in [i for i in self._by_id if i not in live]:
            await self._by_id.pop(session_id).aclose()

    async def aclose(self) -> None:
        sessions = [self._shared] if self._shared is not None else list(self._by_id.values())
        self._by_id.clear()
        for session in sessions:
            await session.aclose()


def create_server(options: ServerOptions, shared: bool = True) -> tuple[Server[Any], Sessions]:
    """The MCP server of ``options`` and its sessions: one shared session
    (stdio) or one per MCP session (``shared=False``, HTTP). Raises
    ``ServeError`` for a manifest this runtime cannot serve."""
    try:
        catalog: Catalog = build_catalog(options.manifest, options.mode)
    except ManifestError as error:
        raise ServeError(f"invalid manifest: {error}") from error
    sessions = Sessions(catalog, options, shared)
    listed = types.ListToolsResult(tools=[types.Tool.model_validate(tool) for tool in list_tools(catalog)])

    async def on_list_tools(ctx: Any, params: types.PaginatedRequestParams | None) -> types.ListToolsResult:
        return listed

    async def on_call_tool(ctx: Any, params: types.CallToolRequestParams) -> types.CallToolResult:
        result = await (await sessions.get(ctx)).call(params.name, params.arguments)
        return types.CallToolResult.model_validate(result)

    server: Server[Any] = Server(
        options.name,
        version=options.version,
        instructions=catalog.instructions or None,
        on_list_tools=on_list_tools,
        on_call_tool=on_call_tool,
    )
    return server, sessions


async def serve_stdio(options: ServerOptions) -> None:
    """Serve ``options`` over stdio until the client closes standard input."""
    server, sessions = create_server(options)
    try:
        async with stdio_server() as (read_stream, write_stream):
            await server.run(read_stream, write_stream, server.create_initialization_options())
    finally:
        await sessions.aclose()


async def serve_http(options: ServerOptions, http: HttpOptions, ready: Callable[[str], None] | None = None) -> None:
    """Serve ``options`` over Streamable HTTP until cancelled. ``ready`` gets
    the endpoint URL once the socket listens (default: printed to standard
    error)."""
    import uvicorn
    from mcp.server.transport_security import TransportSecuritySettings

    server, sessions = create_server(options, shared=False)
    app = server.streamable_http_app(
        streamable_http_path=http.path,
        max_request_body_size=http.max_body_bytes,
        session_idle_timeout=http.idle_timeout_ms / 1000,
        max_sessions=http.max_sessions,
        # Host and Origin are checked by `Guard`, with the TypeScript server's rules.
        transport_security=TransportSecuritySettings(enable_dns_rebinding_protection=False),
        host=http.host,
    )
    # The manager's live sessions (`mcp` is pinned exactly, so this private
    # attribute is stable); without it sessions are closed only at shutdown.
    manager = server.session_manager
    sessions.live = lambda: getattr(manager, "_server_instances", {}).keys()
    warn_if_open(http)
    infos = socket.getaddrinfo(http.host, http.port, type=socket.SOCK_STREAM, flags=socket.AI_PASSIVE)
    family, kind, proto, _, address = infos[0]
    sock = socket.socket(family, kind, proto)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(address)
    port = sock.getsockname()[1]
    shown = f"[{http.host}]" if ":" in http.host and not http.host.startswith("[") else http.host
    url = f"http://{shown}:{port}{http.path}"
    config = uvicorn.Config(Guard(app, http), log_level="warning", access_log=False, lifespan="on")
    runner = uvicorn.Server(config)
    if ready is not None:
        ready(url)
    else:
        print(f"serving Streamable HTTP at {url}", file=sys.stderr, flush=True)
    try:
        await runner.serve(sockets=[sock])
    finally:
        await sessions.aclose()


@dataclass
class _Args:
    http: int | None = None
    host: str = "127.0.0.1"
    token_env: str | None = None
    help: bool = False


def _parse(argv: Sequence[str]) -> _Args:
    """``--stdio``, ``--http <port>``, ``--host <host>``, ``--token-env <NAME>``
    (alias ``--bearer-token-env``), ``--help``; ``ServeError`` for anything else."""
    out = _Args()
    host: str | None = None
    stdio = False
    i = 0
    while i < len(argv):
        arg = argv[i]
        flag, eq, inline = arg.partition("=") if arg.startswith("--") else (arg, "", "")
        if flag in ("--help", "-h"):
            out.help = True
            return out
        if flag == "--stdio":
            stdio = True
            i += 1
            continue
        if flag not in ("--http", "--host", "--token-env", "--bearer-token-env"):
            raise ServeError(f"unknown argument {arg!r}")
        if eq:
            text = inline
        elif i + 1 < len(argv) and not argv[i + 1].startswith("--"):
            i += 1
            text = argv[i]
        else:
            raise ServeError(f"{flag} needs a value")
        if flag == "--http":
            if not (text.isdigit() and len(text) <= 5 and int(text) <= 65535):
                raise ServeError(f"--http needs a port from 0 to 65535, not {text!r}")
            out.http = int(text)
        elif flag == "--host":
            host = text
        else:
            if not text:
                raise ServeError(f"{flag} needs an environment variable name")
            out.token_env = text
        i += 1
    if stdio and out.http is not None:
        raise ServeError("--stdio and --http are exclusive")
    if out.http is None and (host is not None or out.token_env is not None):
        raise ServeError("--host and --token-env need --http")
    if host is not None:
        out.host = host
    return out


def run(options: ServerOptions, argv: Sequence[str] | None = None) -> int:
    """Run the server from a command line; the exit status. Errors that stop
    the server go to standard error, never standard output."""
    try:
        args = _parse(list(sys.argv[1:] if argv is None else argv))
        if args.help:
            print(_USAGE, file=sys.stderr)
            return 0
        if args.http is None:
            anyio.run(serve_stdio, options)
            return 0
        token = None
        if args.token_env is not None:
            token = os.environ.get(args.token_env)
            if not token:
                raise ServeError(f"--token-env names {args.token_env}, which is unset or empty")
        http = HttpOptions(host=args.host, port=args.http, bearer_token=token)
        anyio.run(serve_http, options, http)
    except ServeError as error:
        print(f"error: {error}\n\n{_USAGE}", file=sys.stderr)
        return 1
    except (ValueError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130
    return 0
