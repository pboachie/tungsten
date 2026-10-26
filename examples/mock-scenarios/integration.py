# SPDX-License-Identifier: AGPL-3.0-only
"""A tiny stdlib client with one behavior per saved scenario.

`tungsten scenario run` starts it with TUNGSTEN_MOCK_BASE_URL set to a fresh
loopback mock. Pass `--broken` to apply the deliberately wrong retry policy
the gallery runner expects the scenario assertions to reject.
"""

from __future__ import annotations

import http.client
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid

BASE = os.environ["TUNGSTEN_MOCK_BASE_URL"].rstrip("/")
TIMEOUT = 5
MALFORMED = 3


def request(method: str, path: str, body: object = None, headers: dict[str, str] | None = None):
    data = None if body is None else json.dumps(body).encode()
    all_headers = {"Accept": "application/json", **(headers or {})}
    if data is not None:
        all_headers["Content-Type"] = "application/json"
    req = urllib.request.Request(BASE + path, data=data, method=method, headers=all_headers)
    return urllib.request.urlopen(req, timeout=TIMEOUT)


def rate_limit(broken: bool) -> int:
    for attempt in range(3):
        try:
            with request("GET", "/pets") as response:
                pets = json.load(response)
                return 0 if pets else 1
        except urllib.error.HTTPError as error:
            if error.code != 429 or broken or attempt == 2:
                return 1
            time.sleep(min(float(error.headers.get("Retry-After") or 0), 1.0))
    return 1


def stream_interrupted(broken: bool) -> int:
    """Expect two events; treat end of stream as a disconnect and resume."""
    seen: list[str] = []
    last_id = None
    for _ in range(3):
        headers = {"Accept": "text/event-stream"}
        if last_id is not None and not broken:
            headers["Last-Event-ID"] = last_id
        event_id, data = None, None
        try:
            with request("GET", "/pets/events", headers=headers) as response:
                for raw in response:
                    line = raw.decode()
                    if not line.endswith("\n"):
                        break  # a partial line: the connection was cut
                    line = line.rstrip("\r\n")
                    if line.startswith("id:"):
                        event_id = line[3:].strip()
                    elif line.startswith("data:"):
                        data = line[5:]
                    elif not line and data is not None:
                        # Dispatch only complete events, then remember them.
                        seen.append(json.loads(data)["id"])
                        last_id, event_id, data = event_id, None, None
        except (http.client.IncompleteRead, ConnectionError):
            pass
        if len(seen) >= 2 or broken:
            break
    return 0 if seen == ["pet-1", "pet-2"] else 1


def malformed(broken: bool) -> int:
    with request("GET", "/pets") as response:
        try:
            json.load(response)
        except json.JSONDecodeError:
            # A correct client reports the bad payload once instead of
            # retrying a request that would return the same document.
            if broken:
                with request("GET", "/pets"):
                    pass
            return MALFORMED
    return 0


def lost_write(broken: bool) -> int:
    key = str(uuid.uuid4())
    for _ in range(3):
        try:
            with request("POST", "/pets", {"name": "Rex"}, {"Idempotency-Key": key}) as response:
                return 0 if json.load(response).get("id") else 1
        except (http.client.RemoteDisconnected, ConnectionError, urllib.error.URLError):
            if broken:
                key = str(uuid.uuid4())
    return 1


BEHAVIORS = {
    "rate-limit": rate_limit,
    "stream-interrupted": stream_interrupted,
    "malformed": malformed,
    "lost-write": lost_write,
}


if __name__ == "__main__":
    name = sys.argv[1]
    sys.exit(BEHAVIORS[name]("--broken" in sys.argv[2:]))
