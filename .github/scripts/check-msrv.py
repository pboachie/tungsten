#!/usr/bin/env python3
"""Validate documented Rust floors and the locked dependency graphs."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path


def declared(root: Path) -> dict[str, str]:
    def floor(path: str, section: str) -> str:
        data = tomllib.loads((root / path).read_text(encoding="utf-8"))
        package = (
            data["workspace"]["package"] if section == "workspace" else data["package"]
        )
        value = package["rust-version"]
        if not isinstance(value, str) or not re.fullmatch(r"\d+\.\d+(?:\.\d+)?", value):
            raise ValueError(f"{path}: rust-version must be a numeric stable version")
        return value

    return {
        "compiler": floor("Cargo.toml", "workspace"),
        "runtime": floor("runtimes/rust/Cargo.toml", "workspace"),
        "example": floor("examples/external-emitter-markdown/Cargo.toml", "package"),
    }


def version(value: str) -> tuple[int, int, int]:
    pieces = tuple(int(part) for part in value.split("."))
    return (*pieces, *(0 for _ in range(3 - len(pieces))))


def check_docs(root: Path, floors: dict[str, str]) -> None:
    readme = (root / "README.md").read_text(encoding="utf-8")
    compiler = floors["compiler"]
    if (
        f"rustc-{compiler}%2B" not in readme
        or f"with Rust {compiler} or newer" not in readme
    ):
        raise ValueError(
            "README MSRV badge/build instructions disagree with Cargo.toml"
        )
    if floors["example"] != compiler:
        raise ValueError(
            "example emitter MSRV must match the compiler's declared floor"
        )


def check_graph(root: Path, manifest: str, floor: str) -> None:
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--locked",
            "--all-features",
            "--format-version",
            "1",
            "--manifest-path",
            manifest,
        ],
        cwd=root,
        text=True,
        stdout=subprocess.PIPE,
        check=True,
    )
    metadata = json.loads(result.stdout)
    resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
    violations = []
    for package in metadata["packages"]:
        requirement = package.get("rust_version")
        if (
            package["id"] in resolved
            and requirement
            and version(requirement) > version(floor)
        ):
            violations.append(
                f"{package['name']} {package['version']} requires Rust {requirement}"
            )
    if violations:
        raise ValueError(
            f"{manifest} declares Rust {floor}, but its locked graph exceeds it:\n"
            + "\n".join(sorted(violations))
        )
    print(f"MSRV dependency graph passed: {manifest} (Rust {floor})")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--toolchains",
        action="store_true",
        help="print manifest-derived toolchain names for CI",
    )
    parser.add_argument("--docs-only", action="store_true")
    args = parser.parse_args()
    floors = declared(args.repo)
    check_docs(args.repo, floors)
    if args.toolchains:
        for name, floor in floors.items():
            print(f"{name}={floor}")
    elif not args.docs_only:
        for name, manifest in (
            ("compiler", "Cargo.toml"),
            ("runtime", "runtimes/rust/Cargo.toml"),
            ("example", "examples/external-emitter-markdown/Cargo.toml"),
        ):
            check_graph(args.repo, manifest, floors[name])
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"MSRV check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
