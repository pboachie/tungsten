# SPDX-License-Identifier: AGPL-3.0-only
"""Prepare attributable public inputs; never invent a response or send a write."""
from pathlib import Path
import datetime
import hashlib
import json
import time
import urllib.request


def prepare(destination, source_repo):
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    sources = []

    def fetch(name, url):
        start = time.perf_counter()
        request = urllib.request.Request(
            url,
            headers={
                "User-Agent": "Tungsten-Release-Video/1.0",
                "Accept": "application/json",
            },
        )
        with urllib.request.urlopen(request, timeout=45) as response:
            body = response.read(8 * 1024 * 1024 + 1)
            status = response.status
        if len(body) > 8 * 1024 * 1024:
            raise ValueError("Public example exceeds limit")
        path = destination / name
        path.write_bytes(body)
        sources.append(
            dict(
                file=name,
                url=url,
                status=status,
                sha256=hashlib.sha256(body).hexdigest(),
                acquired_at_utc=datetime.datetime.now(
                    datetime.timezone.utc
                ).isoformat(),
            )
        )
        return body, status, (time.perf_counter() - start) * 1000

    # Exact public standards examples, retained unchanged.
    fetch(
        "swagger2.json",
        "https://raw.githubusercontent.com/OAI/OpenAPI-Specification/3.0.3/examples/v2.0/json/petstore-minimal.json",
    )
    fetch(
        "openapi30.yaml",
        "https://raw.githubusercontent.com/OAI/OpenAPI-Specification/3.0.3/examples/v3.0/petstore.yaml",
    )
    body, status, elapsed = fetch("httpbin-response.json", "https://httpbin.org/json")
    response = json.loads(body)
    assert set(response) == {
        "slideshow"
    }, "Unexpected public JSON payload; inspect before retaining"
    # HAR is an actual controlled GET capture. No cookies, user/IP headers or
    # invented responses are persisted. The original public body is unchanged.
    har = {
        "log": {
            "version": "1.2",
            "creator": {
                "name": "Tungsten release-video public GET capture",
                "version": "1",
            },
            "entries": [
                {
                    "startedDateTime": sources[-1]["acquired_at_utc"],
                    "time": elapsed,
                    "request": {
                        "method": "GET",
                        "url": "https://httpbin.org/json",
                        "httpVersion": "HTTP/1.1",
                        "headers": [],
                        "queryString": [],
                        "cookies": [],
                        "headersSize": -1,
                        "bodySize": 0,
                    },
                    "response": {
                        "status": status,
                        "statusText": "OK",
                        "httpVersion": "HTTP/1.1",
                        "headers": [
                            {"name": "Content-Type", "value": "application/json"}
                        ],
                        "cookies": [],
                        "content": {
                            "size": len(body),
                            "mimeType": "application/json",
                            "text": body.decode(),
                        },
                        "redirectURL": "",
                        "headersSize": -1,
                        "bodySize": len(body),
                    },
                    "cache": {},
                    "timings": {"send": 0, "wait": elapsed, "receive": 0},
                }
            ],
        }
    }
    (destination / "public-get.har").write_text(
        json.dumps(har, indent=2), encoding="utf-8"
    )
    (destination / "public-get.curl").write_text(
        "curl --request GET --url 'https://httpbin.org/json' --header 'Accept: application/json'\n",
        encoding="utf-8",
    )
    (destination / "public-get.http").write_text(
        "GET https://httpbin.org/json HTTP/1.1\nAccept: application/json\n",
        encoding="utf-8",
    )
    (destination / "public-get.rest").write_text(
        "GET https://httpbin.org/json HTTP/1.1\nAccept: application/json\n",
        encoding="utf-8",
    )
    (destination / "har-paths.json").write_text(
        json.dumps({"/json": "/json"}), encoding="utf-8"
    )
    # A Postman 2.1 collection exported from one REAL ZROtext contract operation.
    # No response is invented and no request or auth script is executed.
    contract = Path(source_repo) / "protocol/v1/openapi/public-v1.json"
    spec = json.loads(contract.read_text(encoding="utf-8"))
    route = "/v1/owner/messages"
    operation = spec["paths"][route]["get"]
    servers = spec.get("servers", [])
    origin = servers[0]["url"] if servers else "http://localhost:8080"
    collection = {
        "info": {
            "name": "ZROtext public contract: " + operation["operationId"],
            "description": "Derived Postman export of the committed public GET route. Response contract/auth remain review items; no saved response is fabricated.",
            "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json",
        },
        "item": [
            {
                "name": operation["operationId"],
                "request": {
                    "method": "GET",
                    "url": origin.rstrip("/") + route,
                    "description": operation.get("summary", ""),
                },
            }
        ],
    }
    (destination / "zrotext.postman_collection.json").write_text(
        json.dumps(collection, indent=2), encoding="utf-8"
    )
    sources.append(
        dict(
            file="zrotext.postman_collection.json",
            source="pboachie/zrotext/protocol/v1/openapi/public-v1.json#/paths/~1v1~1owner~1messages/get",
            derivation="Postman v2.1 export of the real declared route; no response examples invented",
            source_sha256=hashlib.sha256(contract.read_bytes()).hexdigest(),
        )
    )
    # Explicit endpoint metadata and permissive observed shape from that actual
    # httpbin response. This is an illustrative declared manifest, not discovery.
    schema = {
        "type": "object",
        "additionalProperties": True,
        "properties": {"slideshow": {"type": "object", "additionalProperties": True}},
    }
    manifest = {
        "endpoint_manifest": 1,
        "title": "Public httpbin JSON GET",
        "version": "1.0.0",
        "base_url": "https://httpbin.org",
        "models": {"Response": schema},
        "endpoints": [
            {
                "id": "getPublicJson",
                "method": "get",
                "path": "/json",
                "responses": {
                    "200": {
                        "description": "Observed public JSON response",
                        "content": {"application/json": {"$ref": "#/models/Response"}},
                    }
                },
            }
        ],
    }
    (destination / "endpoints.json").write_text(
        json.dumps(manifest, indent=2), encoding="utf-8"
    )
    sources.append(
        dict(
            file="endpoints.json",
            derivation="Explicit GET /json routing plus deliberately permissive declared schema, based on actual public JSON capture; not a complete service contract",
        )
    )
    (destination / "provenance.json").write_text(
        json.dumps(sources, indent=2), encoding="utf-8"
    )
    return sources


if __name__ == "__main__":
    import sys

    result = prepare(sys.argv[1], sys.argv[2])
    print(json.dumps(result, indent=2))
