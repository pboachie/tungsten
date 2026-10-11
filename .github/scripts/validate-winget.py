#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Validate generated JSON/YAML against pinned official winget schemas."""
import argparse
import json
import urllib.request
from pathlib import Path

import jsonschema

SCHEMA_COMMIT = "3d9083c062c1e7a7086e05becb414d4e005e4ab6"
BASE = f"https://raw.githubusercontent.com/microsoft/winget-cli/{SCHEMA_COMMIT}/schemas/JSON/manifests/v1.10.0"

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    schemas = {}
    files = sorted(args.directory.rglob("*.yaml"))
    if not files:
        raise SystemExit("no winget manifests found")
    for path in files:
        document = json.loads(path.read_text())
        kind = document["ManifestType"]
        if kind not in ("version", "installer", "defaultLocale"):
            raise SystemExit("unexpected manifest type")
        if kind not in schemas:
            with urllib.request.urlopen(f"{BASE}/manifest.{kind}.1.10.0.json", timeout=30) as response:
                schemas[kind] = json.load(response)
        jsonschema.validate(document, schemas[kind], format_checker=jsonschema.FormatChecker())
        print(f"Validated {path.name} against the official winget schema")
