# SPDX-License-Identifier: AGPL-3.0-only
"""A loopback record store that allows QUOTA concurrent requests.

A request beyond the quota is refused with 429 and ``Retry-After: 1``, as many
real APIs do. ``GET /__stats`` reports what the server saw; ``POST /__reset``
clears it. Standard library only; never listens beyond 127.0.0.1.
"""

import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

QUOTA = 4
_lock = threading.Lock()
_stats = {"served": 0, "limited": 0, "peak": 0}
_active = 0


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
        global _active
        if self.path != "/__reset":
            self._json(404, {"error": "not found"})
            return
        with _lock:
            _stats.update(served=0, limited=0, peak=0)
        self._json(200, _stats)

    def do_GET(self) -> None:
        global _active
        if self.path == "/__stats":
            with _lock:
                self._json(200, dict(_stats))
            return
        prefix = "/records/"
        if not self.path.startswith(prefix) or not self.path[len(prefix) :].isdigit():
            self._json(404, {"error": "not found"})
            return
        record_id = int(self.path[len(prefix) :])
        with _lock:
            _active += 1
            _stats["peak"] = max(_stats["peak"], _active)
            refused = _active > QUOTA
            if refused:
                _active -= 1
                _stats["limited"] += 1
        if refused:
            self._json(429, {"error": "too many concurrent requests"}, {"Retry-After": "1"})
            return
        try:
            time.sleep(0.02)  # the work behind one read
            self._json(200, {"id": record_id, "value": f"record {record_id}"})
        finally:
            with _lock:
                _active -= 1
                _stats["served"] += 1


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 0), Handler)
    server.daemon_threads = True
    # The runner reads the chosen port from this line.
    print(f"listening on http://127.0.0.1:{server.server_address[1]}", flush=True)
    server.serve_forever()
