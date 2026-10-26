# SPDX-License-Identifier: AGPL-3.0-only
"""The behaviour scenario of check-python-consumers.py: an API description, a
local stdlib server and a strictly typed consumer. The consumer exercises
bearer auth, cursor pagination, nullable and union models, a safe retry after
503, client-side validation, JSON bodies, an operation deadline, async
cancellation, an unknown mutation outcome and server-sent events, sync and
async. The server records each request so the checker can prove that invalid
calls sent nothing and that the unknown outcome was not replayed.
"""
from __future__ import annotations

import contextlib
import json
import threading
import time
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

MODULE = "qualification_sdk"
TOKEN = "qualification-token"
ITEMS = [{"id": i, "label": f"item {i}", "value": v} for i, v in
         [("a", "one"), ("b", 2), ("c", "three"), ("d", 4), ("e", "five")]]

_ITEM = {"type": "object", "additionalProperties": False, "required": ["id", "label", "value"],
         "properties": {"id": {"type": "string"}, "label": {"type": ["string", "null"]},
                        "value": {"oneOf": [{"type": "string"}, {"type": "integer"}]}}}
_ITEM_REF = {"$ref": "#/components/schemas/Item"}


def _json(schema: dict, description: str) -> dict:
    return {"description": description, "content": {"application/json": {"schema": schema}}}


SPEC = {
    "openapi": "3.1.0",
    "info": {"title": "Qualification API", "version": "1.0.0"},
    "servers": [{"url": "http://127.0.0.1:1"}],
    "security": [{"bearer": []}],
    "components": {"securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}},
                   "schemas": {"Item": _ITEM}},
    "paths": {
        "/items": {
            "get": {
                "operationId": "listItems",
                "parameters": [
                    {"name": "cursor", "in": "query", "schema": {"type": "string"}},
                    {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 10}},
                ],
                "responses": {"200": _json({
                    "type": "object", "additionalProperties": False, "required": ["items", "next_cursor"],
                    "properties": {"items": {"type": "array", "items": _ITEM_REF},
                                   "next_cursor": {"type": ["string", "null"]}},
                }, "A page")},
            },
            "post": {
                "operationId": "createItem",
                "requestBody": {"required": True, "content": {"application/json": {"schema": {
                    "type": "object", "additionalProperties": False, "required": ["label"],
                    "properties": {"label": {"type": "string", "minLength": 1}},
                }}}},
                "responses": {"201": _json(_ITEM_REF, "Created")},
            },
        },
        "/items/{item_id}": {"get": {
            "operationId": "getItem",
            "parameters": [{"name": "item_id", "in": "path", "required": True, "schema": {"type": "string"}}],
            "responses": {"200": _json(_ITEM_REF, "One item")},
        }},
        "/events": {"get": {
            "operationId": "listEvents",
            "responses": {"200": {"description": "Progress events", "content": {"text/event-stream": {"schema": {
                "type": "object", "additionalProperties": False, "required": ["index"],
                "properties": {"index": {"type": "integer"}},
            }}}}},
        }},
    },
}

CONSUMER = '''from __future__ import annotations

import asyncio
import sys

from qualification_sdk import AsyncQualificationClient, ClientOptions, Err, Ok, QualificationClient
from tungsten_runtime import RetryOptions

TOKEN = "qualification-token"


def options(base: str, token: str = TOKEN) -> ClientOptions:
    return ClientOptions(
        base_url=base,
        auth={"bearer": token},
        validate_responses="strict",
        retries=RetryOptions(max=2, base_ms=10, max_ms=50),
    )


def sync_calls(base: str) -> None:
    with QualificationClient(options(base, "wrong-token")) as client:
        denied = client.items.get(item_id="a")
        assert isinstance(denied, Err) and denied.error["category"] == "AUTH_FAILED", denied
    with QualificationClient(options(base)) as client:
        ids: list[str] = []
        values: list[str | int] = []
        for page in client.items.list_pages(limit=2):
            assert isinstance(page, Ok), page
            ids += [item.id for item in page.value.items]
            values += [item.value for item in page.value.items]
        assert ids == ["a", "b", "c", "d", "e"], ids
        assert values == ["one", 2, "three", 4, "five"], values
        flaky = client.items.get(item_id="flaky")
        assert isinstance(flaky, Ok) and flaky.value.label is None, flaky
        invalid = client.items.create(label="")
        assert isinstance(invalid, Err) and invalid.error["category"] == "VALIDATION_FAILED", invalid
        created = client.items.create(label="new")
        assert isinstance(created, Ok) and created.value.id == "new", created
        dropped = client.items.create(label="drop")
        assert isinstance(dropped, Err) and dropped.error["category"] == "OUTCOME_UNKNOWN", dropped
        late = client.items.get(item_id="slow", opts={"deadline_ms": 300})
        assert isinstance(late, Err) and late.error["code"] == "DEADLINE_EXCEEDED", late
        indexes: list[int] = []
        stream = client.events.list_stream()
        try:
            for event in stream:
                assert not isinstance(event, Err), event
                indexes.append(event.value.index)
        finally:
            stream.close()
        assert indexes == [0, 1, 2], indexes


async def async_calls(base: str) -> None:
    async with AsyncQualificationClient(options(base)) as client:
        ids: list[str] = []
        async for page in client.items.list_pages(limit=2):
            assert isinstance(page, Ok), page
            ids += [item.id for item in page.value.items]
        assert ids == ["a", "b", "c", "d", "e"], ids
        try:
            await asyncio.wait_for(client.items.get(item_id="slow"), timeout=0.3)
        except asyncio.TimeoutError:
            pass
        else:
            raise AssertionError("cancellation did not interrupt the call")
        after = await client.items.get(item_id="a")
        assert isinstance(after, Ok) and after.value.value == "one", after
        indexes: list[int] = []
        stream = client.events.list_stream()
        try:
            async for event in stream:
                assert not isinstance(event, Err), event
                indexes.append(event.value.index)
        finally:
            await stream.aclose()
        assert indexes == [0, 1, 2], indexes


def main() -> None:
    base = sys.argv[1]
    sync_calls(base)
    asyncio.run(async_calls(base))
    print("auth, pagination, retries, validation, deadlines, cancellation, unknown outcome and streams behaved")


if __name__ == "__main__":
    main()
'''


class Recorder:
    """The requests one server received, by method and path."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.calls: list[tuple[str, str]] = []
        self.flaky = 0

    def record(self, method: str, path: str) -> None:
        with self.lock:
            self.calls.append((method, path))

    def count(self, method: str, path: str) -> int:
        with self.lock:
            return sum(1 for call in self.calls if call == (method, path))


def handler(recorder: Recorder) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, format: str, *args: object) -> None:
            pass

        def send_json(self, status: int, body: object, headers: tuple[tuple[str, str], ...] = ()) -> None:
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            for name, value in headers:
                self.send_header(name, value)
            self.end_headers()
            self.wfile.write(data)

        def authorized(self) -> bool:
            if self.headers.get("Authorization") == f"Bearer {TOKEN}":
                return True
            self.send_json(401, {"error": "unauthorized"})
            return False

        def do_GET(self) -> None:
            url = urlsplit(self.path)
            recorder.record("GET", url.path)
            if not self.authorized():
                return
            if url.path == "/items":
                query = parse_qs(url.query)
                start = int(query.get("cursor", ["0"])[0])
                limit = int(query.get("limit", ["10"])[0])
                end = start + limit
                self.send_json(200, {"items": ITEMS[start:end],
                                     "next_cursor": str(end) if end < len(ITEMS) else None})
            elif url.path == "/items/flaky":
                # The first attempt fails retryably; the safe read is retried.
                with recorder.lock:
                    recorder.flaky += 1
                    first = recorder.flaky == 1
                if first:
                    self.send_json(503, {"error": "busy"}, (("Retry-After", "0"),))
                else:
                    self.send_json(200, {"id": "flaky", "label": None, "value": 7})
            elif url.path == "/items/slow":
                # Longer than the consumer's deadline and cancellation budgets.
                time.sleep(2)
                # The client has given up by now; the late reply is expected to fail.
                with contextlib.suppress(OSError):
                    self.send_json(200, {"id": "slow", "label": None, "value": 0})
            elif url.path.startswith("/items/"):
                item = next((i for i in ITEMS if i["id"] == url.path.rsplit("/", 1)[1]), None)
                self.send_json(200 if item else 404, item or {"error": "not found"})
            elif url.path == "/events":
                frames = "".join(f"id: {i}\ndata: {json.dumps({'index': i})}\n\n" for i in range(3)).encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(frames)))
                self.end_headers()
                self.wfile.write(frames)
            else:
                self.send_json(404, {"error": "not found"})

        def do_POST(self) -> None:
            url = urlsplit(self.path)
            recorder.record("POST", url.path)
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"{}")
            if not self.authorized():
                return
            if body.get("label") == "drop":
                # The request was received; the response is lost.
                self.close_connection = True
                self.connection.shutdown(2)
                return
            self.send_json(201, {"id": body.get("label"), "label": body.get("label"), "value": 1})

    return Handler


@contextlib.contextmanager
def serve() -> Iterator[tuple[str, Recorder]]:
    """A loopback server for one consumer run: yields its base URL and recorder."""
    recorder = Recorder()
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler(recorder))
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}", recorder
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=10)


def check(recorder: Recorder) -> None:
    """Prove what the consumer's calls sent: one valid and one dropped create
    (the invalid one sent nothing, the unknown outcome was not replayed) and
    exactly one retry of the safe read."""
    posts = recorder.count("POST", "/items")
    assert posts == 2, f"expected 2 POST /items (invalid input unsent, unknown outcome not replayed), saw {posts}"
    flaky = recorder.count("GET", "/items/flaky")
    assert flaky == 2, f"expected one retry of the safe read after 503, saw {flaky} attempts"
