# SPDX-License-Identifier: AGPL-3.0-only
"""A loopback record store for the telemetry example.

Record 13 answers ``503`` with ``Retry-After: 0`` the first time it is read,
record 404 does not exist, every other record is served. ``POST /__reset``
makes record 13 unavailable once more. Standard library only; never listens
beyond 127.0.0.1.
"""

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

_lock = threading.Lock()
_unavailable = {13}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format: str, *args: object) -> None:
        pass  # keep the example's output readable

    def _json(self, status: int, body: object, headers: dict[str, str] | None = None) -> None:
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self) -> None:
        if self.path != "/__reset":
            self._json(404, {"error": "not found"})
            return
        with _lock:
            _unavailable.clear()
            _unavailable.add(13)
        self._json(200, {"reset": True})

    def do_GET(self) -> None:
        prefix = "/records/"
        if not self.path.startswith(prefix) or not self.path[len(prefix) :].isdigit():
            self._json(404, {"error": "not found"})
            return
        record_id = int(self.path[len(prefix) :])
        with _lock:
            unavailable = record_id in _unavailable
            _unavailable.discard(record_id)
        if unavailable:
            self._json(503, {"error": "briefly unavailable"}, {"Retry-After": "0"})
        elif record_id == 404:
            self._json(404, {"error": f"no record {record_id}"})
        else:
            self._json(200, {"id": record_id, "value": f"record {record_id}"})


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 0), Handler)
    server.daemon_threads = True
    # The runner reads the chosen port from this line.
    print(f"listening on http://127.0.0.1:{server.server_address[1]}", flush=True)
    server.serve_forever()
