#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Keep the README support matrix identical to `tungsten capabilities --matrix`.

The block between the start and end markers in README.md is generated from the
capability catalog. This check fails when the two differ; `--write` replaces
the block with the current output instead.
"""

from __future__ import annotations

import argparse
import difflib
import subprocess
import sys
from pathlib import Path

START = "<!-- tungsten-capabilities-matrix:start -->"
END = "<!-- tungsten-capabilities-matrix:end -->"


def generated(binary: str) -> str:
    result = subprocess.run(
        [binary, "capabilities", "--matrix", "--format", "markdown"],
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    block = result.stdout
    if not (block.startswith(START + "\n") and block.endswith(END + "\n")):
        sys.exit(f"{binary} capabilities --matrix --format markdown printed no marked block")
    return block


def split(readme: str) -> tuple[str, str, str]:
    if readme.count(START) != 1 or readme.count(END) != 1:
        sys.exit(f"README.md must contain exactly one `{START}` and one `{END}` line")
    head, rest = readme.split(START, 1)
    block, tail = rest.split(END, 1)
    return head, START + block + END + "\n", tail.removeprefix("\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=".", help="repository root")
    parser.add_argument(
        "--tungsten", default="target/debug/tungsten", help="compiler binary to query"
    )
    parser.add_argument("--write", action="store_true", help="rewrite the README block")
    args = parser.parse_args()

    path = Path(args.repo) / "README.md"
    readme = path.read_text(encoding="utf-8")
    head, current, tail = split(readme)
    expected = generated(args.tungsten)
    if current == expected:
        print("README support matrix matches `tungsten capabilities --matrix`")
        return 0
    if args.write:
        path.write_text(head + expected + tail, encoding="utf-8")
        print("README support matrix updated")
        return 0
    print(
        "README support matrix differs from `tungsten capabilities --matrix`; run\n"
        "  python3 .github/scripts/check-support-matrix.py --write\n"
        "after `cargo build`, and commit README.md",
        file=sys.stderr,
    )
    sys.stderr.writelines(
        difflib.unified_diff(
            current.splitlines(keepends=True),
            expected.splitlines(keepends=True),
            "README.md",
            "tungsten capabilities --matrix",
        )
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
