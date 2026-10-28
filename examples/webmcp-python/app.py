# SPDX-License-Identifier: AGPL-3.0-only
"""A Python web application that backs browser WebMCP tools with the generated
Python SDK (experimental). The runner generates the `petstore_mcp` server with
`webmcp: true`, installs it, and starts this app with the API's base URL in
`PETSTORE_BASE_URL` and a port (0 picks one); it prints `listening on <url>`.

Sign-in is a stand-in: a `demo_session` cookie naming one of two fixed users.
A real application resolves the principal from its own session instead."""

import socket
import sys

import uvicorn
from petstore_mcp.webmcp import app as dispatch_app

SESSIONS = {"alice-session": "alice", "bob-session": "bob"}


def principal(scope):
    """The signed-in user of a request, from the application's session cookie."""
    cookies = dict(scope["headers"]).get(b"cookie", b"").decode("latin-1")
    for part in cookies.split(";"):
        name, _, value = part.strip().partition("=")
        if name == "demo_session":
            return SESSIONS.get(value)
    return None


# Reads are exposed by default; the destructive delete is allowlisted
# explicitly and still needs the preview's confirmation token.
dispatch = dispatch_app(principal, allowlist=["pets_pets_list", "pets_pets_delete"])

PAGE = b"""<!doctype html>
<title>Pets</title>
<p>The pets page. With WebMCP available, its tools are registered by:</p>
<pre>import { registerRemoteWebMcp } from "@elysra/tungsten-webmcp";
const registration = await registerRemoteWebMcp({ endpoint: "/webmcp" });
addEventListener("pagehide", () => void registration.dispose(), { once: true });</pre>
"""


async def app(scope, receive, send):
    if scope["type"] == "lifespan" or scope.get("path", "").startswith("/webmcp/"):
        await dispatch(scope, receive, send)
        return
    if scope["type"] != "http":
        return
    found = scope["path"] == "/"
    await send({
        "type": "http.response.start",
        "status": 200 if found else 404,
        "headers": [(b"content-type", b"text/html; charset=utf-8")],
    })
    await send({"type": "http.response.body", "body": PAGE if found else b""})


if __name__ == "__main__":
    sock = socket.socket()
    sock.bind(("127.0.0.1", int(sys.argv[1])))
    print(f"listening on http://127.0.0.1:{sock.getsockname()[1]}", flush=True)
    config = uvicorn.Config(app, log_level="warning", access_log=False, lifespan="on")
    uvicorn.Server(config).run(sockets=[sock])
