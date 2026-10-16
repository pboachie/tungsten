#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Extract one release section from CHANGELOG.md for the GitHub Release notes.

    release_notes.py --version 1.2.3
    release_notes.py --version 1.2.3 --output release-notes/release-notes.md
    release_notes.py --version Unreleased

Prints the body of the `## [<version>]` section of CHANGELOG.md (everything
between its heading and the next `## ` heading, without the heading itself and
without the trailing compare-link definitions), or writes it to --output.
The release workflow's prepare job writes the file for `gh release create
--notes-file`; a dry run prints the notes instead.

The script fails (exit 1) when the section is missing, empty, or - for a
version other than Unreleased - when its heading carries no date
(`## [1.2.3] - YYYY-MM-DD`), so a release can never publish unnamed or
undated notes. Standard library only.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

HEADING = re.compile(r"^## \[([^]]+)\]\s*(?:-\s*(\d{4}-\d{2}-\d{2}))?\s*$")
# A Markdown link-reference definition (`[0.1.2]: https://...`); the changelog
# keeps them below the last release section, out of the notes.
LINK_DEFINITION = re.compile(r"^\[[^]]+\]:\s+\S")


def extract(text: str, version: str) -> str:
    lines = text.splitlines()
    start, date = None, None
    for number, line in enumerate(lines):
        match = HEADING.match(line)
        if match and match[1] == version:
            start, date = number, match[2]
            break
    if start is None:
        raise SystemExit(f"CHANGELOG.md has no '## [{version}]' section")
    if version != "Unreleased" and not date:
        raise SystemExit(f"the '## [{version}]' heading in CHANGELOG.md has no date"
                         f" (write '## [{version}] - YYYY-MM-DD')")
    body: list[str] = []
    for line in lines[start + 1:]:
        if HEADING.match(line):
            break
        body.append(line)
    while body and not body[-1].strip():
        body.pop()
    while body and LINK_DEFINITION.match(body[-1]):
        body.pop()
        while body and not body[-1].strip():
            body.pop()
    while body and not body[0].strip():
        body.pop(0)
    return "\n".join(body) + "\n" if body else ""


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--changelog", type=Path, default=Path("CHANGELOG.md"),
                        help="changelog to read (default: CHANGELOG.md)")
    parser.add_argument("--version", required=True,
                        help="release version whose '## [version]' section is extracted")
    parser.add_argument("--output", type=Path,
                        help="write the notes to this file instead of standard output")
    args = parser.parse_args(argv)
    try:
        text = args.changelog.read_text(encoding="utf-8")
    except OSError as err:
        print(f"cannot read {args.changelog}: {err}", file=sys.stderr)
        return 2
    notes = extract(text, args.version)
    if not notes.strip():
        print(f"the '## [{args.version}]' section in CHANGELOG.md is empty", file=sys.stderr)
        return 1
    if args.output:
        try:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(notes, encoding="utf-8", newline="\n")
        except OSError as err:
            print(f"cannot write {args.output}: {err}", file=sys.stderr)
            return 2
    else:
        sys.stdout.write(notes)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
