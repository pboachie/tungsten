# SPDX-License-Identifier: AGPL-3.0-only
"""A loopback file store: ``PUT /files/{name}`` keeps the body (fixed length
or chunked) and answers its size and SHA-256; ``GET /files/{name}`` sends it
back with ``Content-Length`` and a ``Content-Disposition`` file name.
Standard library only; never listens beyond 127.0.0.1.
"""

import hashlib
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

_lock = threading.Lock()
_files: dict[str, bytes] = {}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: object) -> None:
        pass  # keep the example's output readable

    def _name(self) -> str | None:
        prefix = "/files/"
        name = self.path[len(prefix) :] if self.path.startswith(prefix) else ""
        return name if name and "/" not in name else None

    def _json(self, status: int, body: object) -> None:
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _body(self) -> bytes:
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            data = bytearray()
            while True:
                size = int(self.rfile.readline().split(b";")[0].strip(), 16)
                if size == 0:
                    self.rfile.readline()
                    return bytes(data)
                data += self.rfile.read(size)
                self.rfile.readline()
        return self.rfile.read(int(self.headers.get("Content-Length", "0")))

    def do_PUT(self) -> None:
        name = self._name()
        if name is None:
            self._json(404, {"error": "not found"})
            return
        data = self._body()
        with _lock:
            _files[name] = data
        self._json(200, {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})

    def do_GET(self) -> None:
        name = self._name()
        with _lock:
            data = _files.get(name) if name is not None else None
        if data is None:
            self._json(404, {"error": "no such file"})
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Content-Disposition", f'attachment; filename="{name}"')
        self.end_headers()
        # Sent in pieces, as a large object would be.
        for start in range(0, len(data), 1 << 16):
            self.wfile.write(data[start : start + (1 << 16)])


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 0), Handler)
    server.daemon_threads = True
    # The runner reads the chosen port from this line.
    print(f"listening on http://127.0.0.1:{server.server_address[1]}", flush=True)
    server.serve_forever()
