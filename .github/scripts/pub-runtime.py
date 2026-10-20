#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Stage and verify the pub.dev Dart runtime distribution for release CI."""
from __future__ import annotations

import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

NAME = "tungsten_runtime"
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z")
ROOT_FILES = ("LICENSE", "README.md", "pubspec.yaml", "analysis_options.yaml")
REPOSITORY = "https://github.com/pboachie/tungsten"
MAX_ARCHIVE = 8 * 1024 * 1024


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


def field(pubspec: str, key: str) -> str | None:
    match = re.search(r"^" + re.escape(key) + r":[ \t]*(\S.*?)[ \t]*$", pubspec, re.MULTILINE)
    return match.group(1) if match else None


def changelog(version: str) -> bytes:
    # The in-tree guard keeps documentation to a fixed set of paths, so the
    # package changelog is generated: pub requires one, and the release page
    # carries the dated notes.
    return (f"## {version}\n\nSee the [release notes]({REPOSITORY}/releases/tag/v{version}) "
            f"and the [project changelog]({REPOSITORY}/blob/v{version}/CHANGELOG.md).\n").encode()


def package_files(source: Path, expected_version: str | None = None) -> tuple[str, dict[str, bytes]]:
    runtime = source / "runtimes/dart"
    for directory in (source / "runtimes", runtime, runtime / "lib", runtime / "lib/src"):
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError("Runtime input directories must be regular directories")
    pubspec = regular(runtime / "pubspec.yaml").decode()
    version = field(pubspec, "version") or ""
    eligible(version)  # Validate syntax even during pre-0.2.0 local rehearsals.
    if field(pubspec, "name") != NAME:
        raise ValueError("Unexpected runtime name")
    if field(pubspec, "publish_to") is not None:
        raise ValueError("The runtime pubspec must not restrict publication")
    if not re.search(r"^  crypto: \^[0-9]+\.[0-9]+\.[0-9]+$", pubspec, re.MULTILINE):
        raise ValueError("The crypto dependency must use a caret range")
    if expected_version is not None and version != expected_version:
        raise ValueError("Runtime version does not match the release tag")
    files = {name: regular(runtime / name) for name in ROOT_FILES}
    if b"Apache License" not in files["LICENSE"] or b"Version 2.0" not in files["LICENSE"]:
        raise ValueError("Unexpected runtime license text")
    files["CHANGELOG.md"] = changelog(version)
    for path in sorted((runtime / "lib").rglob("*")):
        if path.is_symlink():
            raise ValueError("Runtime sources must not contain symlinks")
        if path.is_dir():
            continue
        if path.suffix != ".dart" or len(path.relative_to(runtime / "lib").parts) > 2:
            raise ValueError("Unexpected entry in the runtime library directory")
        files[path.relative_to(runtime).as_posix()] = regular(path)
    for entry in ("lib/tungsten_runtime.dart",):
        if entry not in files:
            raise ValueError("Required runtime entry point is missing")
    if not re.search(r"runtimeVersion\s*=\s*'" + re.escape(version) + "'", files["lib/tungsten_runtime.dart"].decode()):
        raise ValueError("Exported runtime version differs from package metadata")
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


def assert_outside_repository(directory: Path) -> None:
    # pub lists files through the enclosing git repository's ignore rules; a
    # staged copy under an ignored directory such as target/ would be empty.
    parent = directory.resolve().parent
    parent.mkdir(parents=True, exist_ok=True)
    inside = subprocess.run(["git", "-C", str(parent), "rev-parse", "--is-inside-work-tree"],
        capture_output=True, text=True).stdout.strip()
    if inside == "true":
        raise ValueError("Stage the pub package outside any git work tree")


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


def fetch(url: str, accept: str) -> bytes | None:
    request = Request(url, headers={"User-Agent": "tungsten-release/1", "Accept": accept})
    try:
        with urlopen(request, timeout=30) as response:
            data = response.read(MAX_ARCHIVE + 1)
    except HTTPError as error:
        if error.code == 404:
            return None
        raise
    if len(data) > MAX_ARCHIVE:
        raise ValueError("Registry response is unexpectedly large")
    return data


def registry_package() -> dict | None:
    data = fetch(f"https://pub.dev/api/packages/{NAME}", "application/vnd.pub.v2+json")
    return None if data is None else json.loads(data)


def registry_version(package: dict | None, version: str) -> dict | None:
    for entry in (package or {}).get("versions", []):
        if entry.get("version") == version:
            return entry
    return None


def archive_files(data: bytes) -> dict[str, bytes]:
    files: dict[str, bytes] = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive:
            if member.isdir():
                continue
            name = PurePosixPath(member.name.removeprefix("./"))
            if not member.isreg() or name.is_absolute() or ".." in name.parts or str(name) in files:
                raise ValueError("Registry archive contains an unsafe or duplicate entry")
            files[str(name)] = archive.extractfile(member).read()
    return files


def verify_registry(entry: dict, expected: dict) -> None:
    url = entry.get("archive_url", "")
    if not url.startswith(f"https://pub.dev/") and not url.startswith("https://pub.dartlang.org/"):
        raise ValueError("Registry archive URL is not on pub.dev")
    data = fetch(url, "*/*")
    if data is None:
        raise ValueError("Registry archive is not downloadable")
    if entry.get("archive_sha256") and hashlib.sha256(data).hexdigest() != entry["archive_sha256"]:
        raise ValueError("Registry archive does not match its advertised checksum")
    published = archive_files(data)
    if set(published) != set(expected["files"]):
        raise ValueError("Registry file inventory differs from the checked artifact")
    for name, item in expected["files"].items():
        if hashlib.sha256(published[name]).hexdigest() != item["sha256"] or len(published[name]) != item["size"]:
            raise ValueError(f"Registry file differs from the checked artifact: {name}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["eligible", "stage", "verify", "preflight", "registry"])
    parser.add_argument("--source", type=Path, default=Path.cwd())
    parser.add_argument("--directory", type=Path, required=False)
    parser.add_argument("--source-sha")
    parser.add_argument("--version")
    args = parser.parse_args()
    if args.operation == "eligible":
        version = field(regular(args.source / "runtimes/dart/pubspec.yaml").decode(), "version") or ""
        print("eligible=" + str(eligible(version)).lower())
        return
    if args.directory is None:
        raise ValueError("--directory is required")
    version, files = package_files(args.source, args.version)
    expected = receipt(version, args.source_sha or "", files)
    assert_source(args.source, expected["source_sha"])
    if args.operation == "stage":
        assert_outside_repository(args.directory)
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
            raise ValueError("pub.dev CI publication starts at 0.2.0")
        package = registry_package()
        if args.operation == "preflight":
            if package is None:
                raise ValueError("The package does not exist on pub.dev; its first version must be published "
                    "manually and automated publishing enabled (runbooks/release.md)")
            entry = registry_version(package, version)
            if entry is not None:
                verify_registry(entry, expected)
                raise ValueError("This immutable version already exists and matches; review the original publication first. No automatic republish.")
        else:
            entry = registry_version(package, version)
            for _ in range(11):
                if entry is not None:
                    break
                time.sleep(5)
                entry = registry_version(registry_package(), version)
            if entry is None:
                raise ValueError("Published version is not visible in the registry")
            verify_registry(entry, expected)
    print(json.dumps({"package": NAME, "version": version, "source_sha": expected["source_sha"], "operation": args.operation, "files": len(files), "ok": True}))


if __name__ == "__main__":
    main()
