# SPDX-License-Identifier: AGPL-3.0-only
"""Write an offline, reviewed streaming contract into a fresh workspace."""

from __future__ import annotations

import copy
import json
import sys
from pathlib import Path


def prepare(destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=False)
    spec = {
        "openapi": "3.1.0",
        "info": {"title": "Run Events", "version": "1.0.0"},
        "servers": [{"url": "http://127.0.0.1:8098"}],
        "security": [{"ApiKey": []}],
        "components": {
            "securitySchemes": {
                "ApiKey": {"type": "apiKey", "in": "header", "name": "X-API-Key"}
            },
            "schemas": {
                "StepEvent": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["index", "step"],
                    "properties": {
                        "index": {"type": "integer"},
                        "step": {"type": "string"},
                    },
                },
                "DoneEvent": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["run_id", "steps"],
                    "properties": {
                        "run_id": {"type": "string"},
                        "steps": {"type": "integer"},
                    },
                },
            },
        },
        "paths": {
            "/v1/runs/{run_id}/events": {
                "get": {
                    "operationId": "watchRun",
                    "tags": ["runs"],
                    "parameters": [
                        {
                            "name": "run_id",
                            "in": "path",
                            "required": True,
                            "schema": {"type": "string"},
                        }
                    ],
                    "responses": {
                        "200": {"description": "An underspecified streaming response"}
                    },
                }
            },
        },
    }
    media = {
        "text/event-stream": {
            "schema": {
                "oneOf": [
                    {"$ref": "#/components/schemas/StepEvent"},
                    {"$ref": "#/components/schemas/DoneEvent"},
                ]
            }
        }
    }
    typed = copy.deepcopy(spec)
    typed["paths"]["/v1/runs/{run_id}/events"]["get"]["responses"]["200"]["content"] = (
        media
    )
    missing = copy.deepcopy(spec)
    missing["paths"]["/v1/runs/{run_id}/events"]["get"]["responses"]["200"][
        "content"
    ] = {"text/event-stream": {}}
    overlay = {
        "overlay": "1.0.0",
        "info": {"title": "Reviewed run event response", "version": "1.0.0"},
        "actions": [
            {
                "target": "$['paths']['/v1/runs/{run_id}/events']['get']['responses']['200']",
                "update": {"content": media},
            }
        ],
    }
    for name, value in (
        ("before.json", spec),
        ("typed.json", typed),
        ("missing-schema.json", missing),
        ("events.overlay.json", overlay),
    ):
        (destination / name).write_text(
            json.dumps(value, indent=2) + "\n", encoding="utf-8"
        )
    (destination / "tungsten.yml").write_text(
        "tungsten: 1\napi: {name: run_events, title: Run Events}\n"
        "inputs:\n  - namespace: api\n    spec: before.json\n    overlays: [events.overlay.json]\n"
        "targets:\n  python: {package: run-events-sdk, module: run_events_sdk, out: generated/python}\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    prepare(Path(sys.argv[1]))
