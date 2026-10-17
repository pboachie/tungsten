#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Stage and verify the source-only JSR runtime distribution for release CI."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

NAME = "@elysra/tungsten-runtime"
EXPORTS = {".": "./src/index.ts", "./node": "./src/node.ts"}
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z")
IMPORT = re.compile(r"(?P<prefix>\bfrom\s*|\bimport\s*\(\s*)(?P<quote>[\"'])(?P<path>\.{1,2}/[^\"'\r\n]+)\.js(?P=quote)")


def encoded(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def eligible(version: str) -> bool:
    match = VERSION.fullmatch(version)
    if not match:
        raise ValueError("Invalid package version")
    return tuple(map(int, match.group(1, 2, 3))) >= (0, 2, 0)


def regular(path: Path) -> bytes:
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"Expected a regular distribution source file: {path.name}")
    return path.read_bytes()


def package_files(source: Path, expected_version: str | None = None) -> tuple[str, dict[str, bytes]]:
    runtime = source / "runtimes/ts"
    for directory in (source / "runtimes", runtime, runtime / "src"):
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError("Runtime input directories must be regular directories")
    manifest = json.loads(regular(runtime / "package.json"))
    version = manifest["version"]
    eligible(version)  # Validate syntax even during pre-0.2.0 local rehearsals.
    if manifest["name"] != NAME or manifest["license"] != "Apache-2.0":
        raise ValueError("Unexpected runtime name or license")
    if expected_version is not None and version != expected_version:
        raise ValueError("Runtime version does not match the release tag")
    node_types = manifest["devDependencies"]["@types/node"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", node_types):
        raise ValueError("Node types must be pinned exactly")
    if (runtime / "src").is_symlink():
        raise ValueError("Source directory must not be a symlink")
    files = {"LICENSE": regular(runtime / "LICENSE")}
    intro = f"""# {NAME} on JSR

The TypeScript runtime for Tungsten-generated API clients. This source distribution
contains the same runtime as the matching npm version, with relative module
extensions adapted for JSR. The Node type reference uses the release's pinned
dependency. The compiler and generated service clients are installed separately.

## Install version {version}

```sh
deno add jsr:{NAME}@{version}
npx jsr add {NAME}@{version}
```

The root export uses Web APIs; `/node` provides filesystem-backed idempotency
storage for Node.js 22+. Keep `/node` out of browser bundles. Deno filesystem and
network permissions belong to the consuming application. Runtime support badges
require consumer qualification; source publication alone does not prove parity.

"""
    files["README.md"] = intro.encode() + regular(runtime / "README.md")
    for path in sorted((runtime / "src").iterdir()):
        if path.suffix != ".ts":
            raise ValueError("Unexpected entry in the flat runtime source directory")
        text = regular(path).decode()
        text = IMPORT.sub(lambda m: m["prefix"] + m["quote"] + m["path"] + ".ts" + m["quote"], text)
        text = text.replace('/// <reference types="node" />', f'/// <reference types="npm:@types/node@{node_types}" />')
        files[f"src/{path.name}"] = text.encode()
    for entry in ("src/index.ts", "src/node.ts", "src/version.ts"):
        if entry not in files:
            raise ValueError("Required runtime entry point is missing")
    if not re.search(r'export const RUNTIME_VERSION = "' + re.escape(version) + r'";', files["src/version.ts"].decode()):
        raise ValueError("Exported runtime version differs from package metadata")
    files["jsr.json"] = encoded({"name": NAME, "version": version, "license": "Apache-2.0", "exports": EXPORTS,
        "publish": {"include": ["jsr.json", "README.md", "LICENSE", "src/**/*.ts"]}})
    return version, files


def receipt(version: str, sha: str, files: dict[str, bytes]) -> dict:
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("An exact public source commit is required")
    return {"schema": 1, "package": NAME, "version": version, "source_sha": sha,
        "files": {name: {"sha256": hashlib.sha256(data).hexdigest(), "size": len(data)} for name, data in sorted(files.items())}}


def assert_source(source: Path, sha: str) -> None:
    actual = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    if actual != sha:
        raise ValueError("Checkout does not match the requested source commit")


def verify(directory: Path, expected: dict, files: dict[str, bytes]) -> None:
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("Artifact directory must be a regular directory")
    actual_names = set()
    for path in directory.rglob("*"):
        if path.is_symlink():
            raise ValueError("Artifact contains a symlink")
        if path.is_file():
            actual_names.add(path.relative_to(directory).as_posix())
    expected_names = {"receipt.json"} | {"package/" + name for name in files}
    if actual_names != expected_names:
        raise ValueError("Artifact file inventory differs from the source-derived allowlist")
    if json.loads(regular(directory / "receipt.json")) != expected:
        raise ValueError("Artifact receipt does not match the release source")
    for name, data in files.items():
        if regular(directory / "package" / name) != data:
            raise ValueError(f"Artifact differs from the source-derived package: {name}")


def registry_metadata(version: str) -> dict | None:
    request = Request(f"https://jsr.io/{NAME}/{version}_meta.json",
                      headers={"User-Agent": "Deno/2.6.7", "Accept": "application/json"})
    try:
        with urlopen(request, timeout=30) as response:
            return json.load(response)
    except HTTPError as error:
        if error.code == 404:
            return None
        raise


def verify_registry(metadata: dict, expected: dict) -> None:
    manifest = metadata.get("manifest", {})
    normalized = {name.lstrip("/"): entry for name, entry in manifest.items()}
    if set(normalized) != set(expected["files"]):
        raise ValueError("Registry file inventory differs from the checked artifact")
    for name, entry in expected["files"].items():
        if normalized[name].get("checksum") != "sha256-" + entry["sha256"] or normalized[name].get("size") != entry["size"]:
            raise ValueError(f"Registry file differs from the checked artifact: {name}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["eligible", "stage", "verify", "preflight", "registry"])
    parser.add_argument("--source", type=Path, default=Path.cwd())
    parser.add_argument("--directory", type=Path, default=Path("jsr-dist"))
    parser.add_argument("--source-sha")
    parser.add_argument("--version")
    args = parser.parse_args()
    if args.operation == "eligible":
        version = json.loads(regular(args.source / "runtimes/ts/package.json"))["version"]
        print("eligible=" + str(eligible(version)).lower())
        return
    version, files = package_files(args.source, args.version)
    expected = receipt(version, args.source_sha or "", files)
    assert_source(args.source, expected["source_sha"])
    if args.operation == "stage":
        if args.directory.exists():
            raise ValueError("Refusing to overwrite a staged artifact")
        (args.directory / "package").mkdir(parents=True)
        for name, data in files.items():
            path = args.directory / "package" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (args.directory / "receipt.json").write_bytes(encoded(expected))
    verify(args.directory, expected, files)
    if args.operation in ("preflight", "registry"):
        if not eligible(version):
            raise ValueError("JSR CI publication starts at 0.2.0")
        metadata = registry_metadata(version)
        if args.operation == "preflight":
            if metadata is not None:
                verify_registry(metadata, expected)
                raise ValueError("This immutable version already exists and matches; review the original publication/provenance before recovery. No automatic republish.")
        else:
            for _ in range(11):
                if metadata is not None:
                    break
                time.sleep(5)
                metadata = registry_metadata(version)
            if metadata is None:
                raise ValueError("Published version is not visible in the registry")
            verify_registry(metadata, expected)
    print(json.dumps({"package": NAME, "version": version, "source_sha": expected["source_sha"], "operation": args.operation, "files": len(files), "ok": True}))


if __name__ == "__main__":
    main()
