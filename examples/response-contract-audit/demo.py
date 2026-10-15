# SPDX-License-Identifier: AGPL-3.0-only
"""Call both wait modes of a local illustrative API and narrow their union."""

from __future__ import annotations

import json
from http.server import BaseHTTPRequestHandler, HTTPServer
from threading import Thread
from urllib.parse import parse_qs, urlsplit

from runs_api_sdk import RunsApiClient
from runs_api_sdk.models.runs import Job, Run
from tungsten_runtime import Err


class Handler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        query = parse_qs(urlsplit(self.path).query)
        payload: dict[str, str]
        if query.get("wait") == ["true"]:
            payload = {"id": "run-1", "status": "completed", "result": "hello"}
        else:
            payload = {"id": "job-1", "status": "queued", "run_id": "run-1"}
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        pass


def main() -> None:
    server = HTTPServer(("127.0.0.1", 0), Handler)
    worker = Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        with RunsApiClient(
            base_url=f"http://127.0.0.1:{server.server_port}",
            validate_responses="strict",
        ) as client:
            for wait in (True, False):
                result = client.runs.create(input="hello", wait=wait)
                if isinstance(result, Err):
                    raise RuntimeError(str(result.error))
                # Both requests return Run | Job. Inspect the validated result,
                # rather than assuming its variant from the request parameter.
                value = result.value
                if isinstance(value, Run):
                    print(f"wait={wait}: run {value.id}: {value.result}")
                elif isinstance(value, Job):
                    print(f"wait={wait}: job {value.id}: run {value.run_id}")
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


if __name__ == "__main__":
    main()
