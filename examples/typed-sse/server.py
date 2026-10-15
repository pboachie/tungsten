# SPDX-License-Identifier: AGPL-3.0-only
"""Small local FastAPI application with an explicit SSE response contract."""

import json
import os
from collections.abc import Iterator
from pathlib import Path

from fastapi import FastAPI, HTTPException, Security
from fastapi.responses import StreamingResponse
from fastapi.security import APIKeyHeader

app = FastAPI(title="Run Events")
api_key = APIKeyHeader(name="X-API-Key", scheme_name="ApiKey")


@app.get(
    "/v1/runs/{run_id}/events",
    operation_id="watchRun",
    tags=["runs"],
    response_class=StreamingResponse,
    responses={
        200: {
            "description": "Named step events followed by one named done event",
            "content": {
                "text/event-stream": {
                    "schema": {
                        "oneOf": [
                            {
                                "type": "object",
                                "additionalProperties": False,
                                "required": ["index", "step"],
                                "properties": {
                                    "index": {"type": "integer"},
                                    "step": {"type": "string"},
                                },
                            },
                            {
                                "type": "object",
                                "additionalProperties": False,
                                "required": ["run_id", "steps"],
                                "properties": {
                                    "run_id": {"type": "string"},
                                    "steps": {"type": "integer"},
                                },
                            },
                        ],
                    }
                }
            },
        }
    },
)
def events(run_id: str, key: str = Security(api_key)) -> StreamingResponse:
    if key != os.environ["SSE_API_KEY"]:
        raise HTTPException(status_code=401, detail="Invalid API key")

    def frames() -> Iterator[str]:
        yield 'event: step\ndata: {"index":0,"step":"started"}\n\n'
        yield (
            "event: done\ndata: " + json.dumps({"run_id": run_id, "steps": 1}) + "\n\n"
        )

    return StreamingResponse(frames(), media_type="text/event-stream")


if __name__ == "__main__":
    # Export the preferred upstream declaration without starting a server.
    import sys

    Path(sys.argv[1]).write_text(
        json.dumps(app.openapi(), indent=2) + "\n", encoding="utf-8"
    )
