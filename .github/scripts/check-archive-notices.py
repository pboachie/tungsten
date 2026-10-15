#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Verify README/LICENSE bytes in a built JAR or NuGet archive. Never publishes."""
from __future__ import annotations

import argparse
import zipfile
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("runtime", type=Path)
    parser.add_argument("--prefix", default="")
    args = parser.parse_args()
    with zipfile.ZipFile(args.archive) as artifact:
        for name in ("README.md", "LICENSE"):
            if artifact.read(args.prefix + name) != (args.runtime / name).read_bytes():
                raise ValueError(f"{args.archive}: {name} missing or changed")
    print(f"{args.archive.name}: packaged README and LICENSE verified")


if __name__ == "__main__":
    main()
