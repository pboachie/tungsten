# SPDX-License-Identifier: Apache-2.0
"""The MCP server: the official MCP Python SDK's low-level ``Server`` with a
``tools/list`` and a ``tools/call`` handler, served over stdio. Protocol
messages go to standard output only; diagnostics go to standard error."""

from __future__ import annotations

import json
import sys
from collections.abc import Mapping, Sequence
from importlib import resources
from typing import Any

import anyio
import mcp_types as types
from mcp.server.lowlevel import Server
from mcp.server.stdio import stdio_server

from ._catalog import Catalog, ManifestError, build_catalog, list_tools
from ._options import ServerOptions
from ._session import Session

#: The disclosure modes; this runtime serves ``discrete`` only.
MODES = ("discrete", "progressive")

_USAGE = "usage: <server> [--stdio]  (the default; Streamable HTTP is not served by this runtime yet)"


class ServeError(Exception):
    """The server cannot start; the message says why and what to change."""


def load_manifest(package: str, name: str = "manifest.json") -> dict[str, Any]:
    """The manifest packaged as ``name`` inside ``package``."""
    text = resources.files(package).joinpath(name).read_text(encoding="utf-8")
    manifest = json.loads(text)
    if not isinstance(manifest, dict):
        raise ServeError(f"{package}/{name} is not a JSON object")
    return manifest


def check_mode(manifest: Mapping[str, Any], override: str | None, variable: str) -> None:
    """Refuse a disclosure mode this runtime does not serve. ``override`` is
    the mode forced by the environment variable ``variable``, if any."""
    if override is not None and override not in MODES:
        raise ServeError(f'{variable} must be "discrete" or "progressive", not {override!r}')
    mode = override or manifest.get("mode") or "discrete"
    if mode != "discrete":
        source = variable if override else "the manifest (agent.yml disclosure.mode, or auto for a large API)"
        raise ServeError(
            f"progressive disclosure is selected by {source}, and this experimental Python MCP runtime serves "
            f"discrete mode only. Set {variable}=discrete to list every tool, or use the TypeScript MCP server "
            "(the `mcp` target) for progressive disclosure."
        )


def create_server(options: ServerOptions) -> tuple[Server[Any], Session]:
    """The MCP server of ``options`` and its session (one per process over
    stdio). Raises ``ServeError`` for a manifest this runtime cannot serve."""
    try:
        catalog: Catalog = build_catalog(options.manifest)
    except ManifestError as error:
        raise ServeError(f"invalid manifest: {error}") from error
    session = Session(catalog, options)
    listed = types.ListToolsResult(tools=[types.Tool.model_validate(tool) for tool in list_tools(catalog)])

    async def on_list_tools(ctx: Any, params: types.PaginatedRequestParams | None) -> types.ListToolsResult:
        return listed

    async def on_call_tool(ctx: Any, params: types.CallToolRequestParams) -> types.CallToolResult:
        result = await session.call(params.name, params.arguments)
        return types.CallToolResult.model_validate(result)

    server: Server[Any] = Server(
        options.name,
        version=options.version,
        instructions=catalog.instructions or None,
        on_list_tools=on_list_tools,
        on_call_tool=on_call_tool,
    )
    return server, session


async def serve_stdio(options: ServerOptions) -> None:
    """Serve ``options`` over stdio until the client closes standard input."""
    server, session = create_server(options)
    try:
        async with stdio_server() as (read_stream, write_stream):
            await server.run(read_stream, write_stream, server.create_initialization_options())
    finally:
        await session.aclose()


def run(options: ServerOptions, argv: Sequence[str] | None = None) -> int:
    """Run the server from a command line; the exit status. Errors that stop
    the server go to standard error, never standard output."""
    args = list(sys.argv[1:] if argv is None else argv)
    if args not in ([], ["--stdio"]):
        print(_USAGE, file=sys.stderr)
        return 2
    try:
        anyio.run(serve_stdio, options)
    except ServeError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130
    return 0
