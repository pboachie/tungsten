#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Build a local Ruby runtime gem and verify its packaged notices. Never publishes."""
from __future__ import annotations

import io
import subprocess
import tarfile
import tempfile
from pathlib import Path


def main() -> None:
    root = Path(__file__).resolve().parents[2]
    runtime = root / "runtimes" / "ruby"
    with tempfile.TemporaryDirectory(prefix="tungsten-gem-") as scratch:
        artifact = Path(scratch) / "tungsten-runtime.gem"
        subprocess.run(
            ["gem", "build", "tungsten-runtime.gemspec", "--output", str(artifact)],
            cwd=runtime, check=True,
        )
        with tarfile.open(artifact) as gem:
            compressed = gem.extractfile("data.tar.gz")
            if compressed is None:
                raise ValueError("gem has no data.tar.gz")
            payload = compressed.read()
        with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as data:
            for name in ("LICENSE", "README.md"):
                member = data.getmember(name)
                if not member.isfile():
                    raise ValueError(f"{name} must be a regular packaged file")
                packaged = data.extractfile(member)
                if packaged is None or packaged.read() != (runtime / name).read_bytes():
                    raise ValueError(f"gem must include {name} unchanged")
        if (runtime / "LICENSE").read_bytes() != (root / "runtimes" / "LICENSE").read_bytes():
            raise ValueError("Ruby runtime license differs from the shared Apache-2.0 license")
        print("Ruby gem includes the unchanged README and Apache-2.0 LICENSE")


if __name__ == "__main__":
    main()
