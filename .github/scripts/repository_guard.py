#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Repository guard for the public tungsten repository (CI job "repository-guard").

The public repository is code only: tests, test data, documentation and plans
live in the private operations repository. This guard enforces that rule and
the licence headers on a commit's tree, and rejects AI attribution in commit
messages.

    repository_guard.py                     check the tree of HEAD
    repository_guard.py --ref REF           check the tree of REF
    repository_guard.py --range BASE HEAD   also check every commit in BASE..HEAD
                                            (an empty BASE means all of HEAD's history)
    repository_guard.py --repo PATH ...     run against another checkout

Tree rules:
  - no test directories, test files, Rust test attributes (#[cfg(test)],
    #[test], ...), [dev-dependencies], [[test]] or [[bench]] targets, and
    every crate library keeps `doctest = false`;
  - no documentation files: the only allowed ones are README.md at the root,
    LICENSE at the root and the Apache-2.0 LICENSE in runtimes/ (and a copy
    of it in a runtime package directory, for npm packaging);
  - every .rs file outside runtimes/ starts with
    `// SPDX-License-Identifier: AGPL-3.0-only`, and every source file under
    runtimes/ with `// SPDX-License-Identifier: Apache-2.0` (`#` for Python).
Commit rules: no Co-Authored-By trailer naming an AI assistant or its vendor,
no session-link trailers, no "generated with" footers naming an AI tool, and
no author or committer identity of an AI assistant.

Exit status: 0 clean, 1 violations, 2 the check could not run (fails closed).
Standard library only; reads Git objects, never the working tree. Its tests
live in the private operations repository.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path, PurePosixPath

# ── tree rules ───────────────────────────────────────────────────────────────

TEST_DIRS = {
    "test", "tests", "__tests__", "testdata", "test-data", "test_data",
    "fixtures", "golden", "goldens", "snapshots", "benches", "e2e",
}
TEST_FILE = re.compile(
    r"(?:^test_.*\.(?:rs|py)$|_tests?\.(?:rs|py)$"
    r"|\.(?:test|spec)\.(?:[cm]?[jt]sx?)$|\.snap$)",
    re.IGNORECASE,
)
# An attribute at the start of a line: #[...] or #![...].
RUST_ATTRIBUTE = re.compile(r"^\s*#!?\[\s*([A-Za-z_][\w:]*)\s*(.*)$")
STRING_LITERAL = re.compile(r'"(?:[^"\\]|\\.)*"')

DOC_DIRS = {"doc", "docs", "documentation"}
DOC_EXTENSIONS = {
    ".md", ".markdown", ".mdx", ".rst", ".adoc", ".asciidoc", ".txt", ".org",
    ".pdf", ".doc", ".docx", ".odt", ".rtf", ".tex",
}
# Extensionless documentation files (CHANGELOG, NOTICE, LICENSE-MIT, ...).
DOC_NAME = re.compile(
    r"^(?:readme|licen[cs]e|copying|notice|changelog|changes|history|news|contributing"
    r"|authors|security|code_of_conduct|support|governance|maintainers)(?:[-_].*)?$",
    re.IGNORECASE,
)
ALLOWED_DOCS = re.compile(r"^(?:README\.md|LICENSE|runtimes/LICENSE|runtimes/[^/]+/LICENSE)$")

AGPL_HEADER = "// SPDX-License-Identifier: AGPL-3.0-only"
APACHE_HEADER = "// SPDX-License-Identifier: Apache-2.0"
APACHE_HEADER_PY = "# SPDX-License-Identifier: Apache-2.0"
RUNTIME_SOURCE = {".rs", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".py"}
HEADER_LINES = 5  # the header may follow a shebang or a blank line


def path_violations(path: str) -> list[str]:
    parts = PurePosixPath(path).parts
    name = parts[-1]
    found = []
    test_dir = next((p for p in parts[:-1] if p.lower() in TEST_DIRS), None)
    if test_dir:
        found.append(f"test directory '{test_dir}/' (tests live in the private repository)")
    elif TEST_FILE.search(name):
        found.append("test file (tests live in the private repository)")
    if not ALLOWED_DOCS.match(path):
        doc_dir = next((p for p in parts[:-1] if p.lower() in DOC_DIRS), None)
        suffix = PurePosixPath(name).suffix.lower()
        if doc_dir:
            found.append(f"documentation directory '{doc_dir}/' (docs live in the private repository)")
        elif suffix in DOC_EXTENSIONS or (not suffix and DOC_NAME.match(name)):
            found.append(
                "documentation file (only README.md, LICENSE and runtimes/LICENSE are allowed;"
                " docs live in the private repository)"
            )
    return found


def has_header(text: str, *headers: str) -> bool:
    return any(line.strip() in headers for line in text.splitlines()[:HEADER_LINES])


def rust_test_lines(text: str) -> list[int]:
    lines = []
    for number, line in enumerate(text.splitlines(), 1):
        match = RUST_ATTRIBUTE.match(line)
        if not match:
            continue
        name, rest = match[1], STRING_LITERAL.sub('""', match[2])
        if name.split("::")[-1] in {"test", "bench"} or (
            name in {"cfg", "cfg_attr"} and re.search(r"\btest\b", rest)
        ):
            lines.append(number)
    return lines


def manifest_violations(path: str, text: str, paths: set[str]) -> list[str]:
    try:
        manifest = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return ["Cargo.toml does not parse"]
    found = []
    sections = [manifest] + list(manifest.get("target", {}).values())
    if any(key in section for section in sections for key in ("dev-dependencies", "dev_dependencies")):
        found.append("[dev-dependencies] (test-only dependencies belong to the private harness)")
    for kind in ("test", "bench"):
        if manifest.get(kind):
            found.append(f"[[{kind}]] target (tests live in the private repository)")
    if "package" in manifest:
        root = str(PurePosixPath(path).parent)
        default_lib = f"{root}/src/lib.rs" if root != "." else "src/lib.rs"
        lib = manifest.get("lib")
        if (lib is not None or default_lib in paths) and (lib or {}).get("doctest") is not False:
            found.append("library target without `doctest = false` (doctests are tests)")
    return found


def content_violations(path: str, text: str, paths: set[str]) -> list[str]:
    found = []
    suffix = PurePosixPath(path).suffix.lower()
    in_runtimes = path.startswith("runtimes/")
    if suffix == ".rs":
        for number in rust_test_lines(text):
            found.append(f"line {number}: Rust test attribute (tests live in the private repository)")
    if in_runtimes and suffix in RUNTIME_SOURCE:
        headers = (APACHE_HEADER_PY,) if suffix == ".py" else (APACHE_HEADER,)
        if not has_header(text, *headers):
            found.append(f"runtime source without the licence header '{headers[0]}'")
    elif suffix == ".rs" and not has_header(text, AGPL_HEADER):
        found.append(f"Rust source without the licence header '{AGPL_HEADER}'")
    if PurePosixPath(path).name == "Cargo.toml":
        found += manifest_violations(path, text, paths)
    return found


# ── commit rules ─────────────────────────────────────────────────────────────
# A detection list, not attribution: names and domains of AI coding assistants
# whose tools add co-author trailers or footers to commits.
AI_NAME = re.compile(
    r"\b(?:claude|anthropic|openai|chatgpt|gpt-?\d[\w.-]*|copilot|gemini|codex|devin"
    r"|aider|codeium|windsurf|tabnine|codewhisperer|cursor\s*agent)\b"
    r"|@(?:[\w-]+\.)*(?:anthropic\.com|openai\.com|cursor\.(?:com|sh)|devin\.ai|aider\.chat|codeium\.com|windsurf\.com)\b",
    re.IGNORECASE,
)
CO_AUTHOR = re.compile(r"^\s*co-authored-by\s*:(.*)$", re.IGNORECASE | re.MULTILINE)
SESSION_TRAILER = re.compile(r"^\s*[A-Za-z][\w-]*-session(?:-id|-url)?\s*:", re.IGNORECASE | re.MULTILINE)
GENERATED_FOOTER = re.compile(r"^.*\b(?:generated|written|created)\s+(?:with|by|using)\b(.*)$", re.IGNORECASE | re.MULTILINE)


def message_violations(message: str) -> list[str]:
    found = []
    if any(AI_NAME.search(value) for value in CO_AUTHOR.findall(message)):
        found.append("Co-Authored-By trailer naming an AI assistant")
    if SESSION_TRAILER.search(message):
        found.append("session-link trailer")
    if any(AI_NAME.search(rest) for rest in GENERATED_FOOTER.findall(message)):
        found.append("footer crediting an AI tool")
    return found


def identity_violations(author: str, committer: str) -> list[str]:
    return [f"{role} identifies an AI assistant" for role, who in (("author", author), ("committer", committer))
            if AI_NAME.search(who)]


# ── Git access ───────────────────────────────────────────────────────────────

class GitError(Exception):
    pass


def git(repo: Path, *args: str, stdin: bytes | None = None) -> bytes:
    result = subprocess.run(["git", "-C", str(repo), *args], input=stdin, capture_output=True, check=False)
    if result.returncode:
        detail = result.stderr.decode("utf-8", "replace").strip().splitlines()
        raise GitError(f"git {args[0]} failed: {detail[-1] if detail else 'no output'}")
    return result.stdout


def tree_files(repo: Path, ref: str) -> list[tuple[str, str, str]]:
    raw = git(repo, "ls-tree", "-r", "-z", "--full-tree", ref)
    files = []
    for record in filter(None, raw.split(b"\0")):
        info, path = record.split(b"\t", 1)
        mode, _kind, oid = info.decode().split()
        files.append((mode, oid, path.decode("utf-8", "surrogateescape")))
    return files


def read_blobs(repo: Path, oids: list[str]) -> dict[str, bytes]:
    if not oids:
        return {}
    raw = git(repo, "cat-file", "--batch", stdin=("\n".join(oids) + "\n").encode())
    offset, blobs = 0, {}
    for oid in oids:
        end = raw.index(b"\n", offset)
        header = raw[offset:end].split()
        if len(header) != 3 or header[1] != b"blob":
            raise GitError("a tree entry is not a readable blob")
        size = int(header[2])
        blobs[oid] = raw[end + 1:end + 1 + size]
        offset = end + 1 + size + 1
    return blobs


def check_tree(repo: Path, ref: str) -> list[str]:
    files = tree_files(repo, ref)
    paths = {path for _, _, path in files}
    blobs = read_blobs(repo, sorted({oid for mode, oid, _ in files if mode not in {"160000", "120000"}}))
    findings = []
    for mode, oid, path in files:
        issues = path_violations(path)
        if mode == "160000":
            issues.append("submodule (the public repository vendors nothing)")
        elif mode != "120000":
            data = blobs[oid]
            if b"\0" not in data:
                issues += content_violations(path, data.decode("utf-8", "replace"), paths)
        findings += [f"{path}: {issue}" for issue in issues]
    return findings


def check_commits(repo: Path, base: str, head: str) -> tuple[int, list[str]]:
    revs = [head] if not base else [head, "--not", base]
    commits = git(repo, "rev-list", *revs).decode().split()
    findings = []
    for sha in commits:
        raw = git(repo, "show", "-s", "--format=%an <%ae>%x00%cn <%ce>%x00%B", sha).decode("utf-8", "replace")
        author, committer, message = raw.split("\0", 2)
        issues = identity_violations(author, committer) + message_violations(message)
        findings += [f"commit {sha[:12]}: {issue}" for issue in issues]
    return len(commits), findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", type=Path, default=Path("."), help="repository checkout (default: .)")
    parser.add_argument("--ref", default="HEAD", help="commit whose tree is checked (default: HEAD)")
    parser.add_argument("--range", nargs=2, metavar=("BASE", "HEAD"), help="also check the commits in BASE..HEAD")
    args = parser.parse_args(argv)
    try:
        findings = check_tree(args.repo, args.ref)
        summary = f"tree of {args.ref}"
        if args.range:
            count, commit_findings = check_commits(args.repo, *args.range)
            findings += commit_findings
            summary += f" and {count} commit{'s' if count != 1 else ''}"
    except (GitError, OSError, ValueError) as err:
        print(f"repository guard failed closed: {err}", file=sys.stderr)
        return 2
    if findings:
        print(f"repository guard: {len(findings)} violation{'s' if len(findings) != 1 else ''} in the {summary}:",
              file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        print("This repository is source code only (README.md, Contributing): tests, test data and documentation"
              " are maintained in the private repository. Commits carry no AI attribution: reword them"
              " (git rebase -i) and push again.", file=sys.stderr)
        return 1
    print(f"repository guard passed ({summary})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
