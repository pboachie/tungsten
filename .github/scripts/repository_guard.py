#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Repository guard for the public tungsten repository (CI job "repository-guard").

The public repository is code plus the standard community files: tests, test
data, documentation and plans live in the private operations repository. This
guard enforces that rule and the licence headers on a commit's tree, and
rejects AI attribution in commit messages.

    repository_guard.py                     check the tree of HEAD
    repository_guard.py --ref REF           check the tree of REF
    repository_guard.py --range BASE HEAD   also check every commit in BASE..HEAD
                                            (an empty BASE means all of HEAD's history)
    repository_guard.py --repo PATH ...     run against another checkout

Tree rules:
  - no test directories, test files, Rust test attributes (#[cfg(test)],
    #[test], ...), [dev-dependencies], [[test]] or [[bench]] targets, and
    every crate library keeps `doctest = false`;
  - no documentation files and no documentation directories (doc/, docs/,
    documentation/). The only documentation files allowed are the community
    files: README.md, LICENSE, CONTRIBUTING.md, SECURITY.md,
    CODE_OF_CONDUCT.md and CHANGELOG.md at the root; the Apache-2.0 LICENSE
    in runtimes/ (and a copy of it in a runtime package directory, for npm
    packaging); Markdown directly in .github/ (pull request template and the
    like); .github/ISSUE_TEMPLATE/*.md and *.yml; and examples/<name>/README.md.
    Everything else with a documentation extension or name stays forbidden,
    wherever it is (docs/CONTRIBUTING.md, CHANGELOG.txt, assets/notes.md);
  - the root assets/ directory holds images only (.svg and .png); examples/ holds code (and the
    per-example README.md above), and test directories are refused there as
    anywhere else;
  - every .rs file outside runtimes/ starts with
    `// SPDX-License-Identifier: AGPL-3.0-only`, and every source file under
    runtimes/ with `// SPDX-License-Identifier: Apache-2.0` (`#` for Python);
  - Go sources (.go, no _test.go) and go.mod/go.sum only in the Go runtime
    (runtimes/go/) and the external Go emitter (emitters/go/), every .go file
    there with `// SPDX-License-Identifier: Apache-2.0`;
  - the SDK runtimes of LANGUAGE_ROOTS: Java (runtimes/java/: .java,
    pom.xml), C# (runtimes/csharp/: .cs, .csproj, Directory.Build.props),
    Kotlin (runtimes/kotlin/: .kt, .kts), Swift (runtimes/swift/: .swift,
    Package.swift, Package.resolved), PHP (runtimes/php/: .php,
    composer.json, phpstan.neon), Ruby (runtimes/ruby/: .rb, .gemspec,
    Gemfile) and Dart (runtimes/dart/: .dart, pubspec.yaml,
    analysis_options.yaml). Their sources and manifests are refused
    anywhere else (generated SDKs never live here). Headers: `//
    SPDX-License-Identifier: Apache-2.0` in .java, .cs, .kt, .kts, .swift
    and .dart (Package.swift may put `// swift-tools-version:` first); `#
    SPDX-...` in .rb, .gemspec, Gemfile, pubspec.yaml,
    analysis_options.yaml and phpstan.neon; in .php the `//` header follows
    the `<?php` line; pom.xml, .csproj and .props carry `<!--
    SPDX-License-Identifier: Apache-2.0 -->` (after an optional XML
    declaration); composer.json has `"license": "Apache-2.0"`. Their test
    files (*Test.java, *Tests.java, *Test.kt, *Tests.cs, *Tests.swift,
    *Test.php, *_test.rb, *_spec.rb, *_test.dart) and test directories (any
    case, `Tests/` included, and spec/ under runtimes/ruby/) are refused.
Commit rules: no Co-Authored-By trailer naming an AI assistant or its vendor,
no session-link trailers, no "generated with" footers naming an AI tool, and
no author or committer identity of an AI assistant.

The gh-pages branch is the one exception: it holds the generated documentation
site (HTML written by the docs workflow of the private repository, never edited
by hand) and is not checked. The CI workflow does not run for pushes to it, and
`--ref gh-pages` is skipped explicitly (exit 0).

Exit status: 0 clean, 1 violations, 2 the check could not run (fails closed).
Standard library only; reads Git objects, never the working tree. Its tests
live in the private operations repository.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path, PurePosixPath

# The branch that carries the generated documentation site; never checked.
PAGES_BRANCH = "gh-pages"

# ── tree rules ───────────────────────────────────────────────────────────────

TEST_DIRS = {
    "test", "tests", "__tests__", "testdata", "test-data", "test_data",
    "fixtures", "golden", "goldens", "snapshots", "benches", "e2e",
}
TEST_FILE = re.compile(
    r"(?:^test_.*\.(?:rs|py)$|_tests?\.(?:rs|py)$|_test\.go$"
    r"|\.(?:test|spec)\.(?:[cm]?[jt]sx?)$|\.snap$"
    r"|_test\.(?:rb|dart)$|_spec\.rb$)",
    re.IGNORECASE,
)
# Test classes of the SDK languages, matched case-sensitively so that
# `Latest.java` or `Contest.cs` stay ordinary sources.
TEST_CLASS_FILE = re.compile(r"(?:Tests?\.(?:java|kt)|Tests?\.cs|Tests\.swift|Test\.php)$")
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
# Community files, matched against the whole path: nothing else may be
# documentation, and a documentation directory is refused even around these.
ALLOWED_DOCS = re.compile(
    r"^(?:README\.md|LICENSE|CONTRIBUTING\.md|SECURITY\.md|CODE_OF_CONDUCT\.md|CHANGELOG\.md"
    r"|runtimes/LICENSE|runtimes/[^/]+/LICENSE"
    r"|\.github/[^/]+\.md"
    r"|\.github/ISSUE_TEMPLATE/[^/]+\.(?:md|ya?ml)"
    r"|examples/[^/]+/README\.md)$"
)
ASSET_EXTENSIONS = {".svg", ".png"}

AGPL_HEADER = "// SPDX-License-Identifier: AGPL-3.0-only"
APACHE_HEADER = "// SPDX-License-Identifier: Apache-2.0"
APACHE_HEADER_PY = "# SPDX-License-Identifier: Apache-2.0"
RUNTIME_SOURCE = {".rs", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".py"}
# The Go modules: the runtime and the external emitter (tungsten-emit-go),
# both Apache-2.0. Go sources, go.mod and go.sum live only there.
GO_ROOTS = ("runtimes/go/", "emitters/go/")
GO_MODULE_FILES = {"go.mod", "go.sum"}
HEADER_LINES = 5  # the header may follow a shebang or a blank line

# The SDK runtimes of the further languages: (root, language, source
# extensions, manifest names, manifest extensions). Their sources and
# manifests live only under their root.
LANGUAGE_ROOTS = (
    ("runtimes/java/", "Java", {".java"}, {"pom.xml"}, set()),
    ("runtimes/csharp/", "C#", {".cs"}, {"Directory.Build.props"}, {".csproj"}),
    ("runtimes/kotlin/", "Kotlin", {".kt", ".kts"}, set(), set()),
    ("runtimes/swift/", "Swift", {".swift"}, {"Package.swift", "Package.resolved"}, set()),
    ("runtimes/php/", "PHP", {".php"}, {"composer.json", "phpstan.neon"}, set()),
    ("runtimes/ruby/", "Ruby", {".rb"}, {"Gemfile"}, {".gemspec"}),
    ("runtimes/dart/", "Dart", {".dart"}, {"pubspec.yaml", "analysis_options.yaml"}, set()),
)
SLASH_HEADER_SOURCES = {".java", ".cs", ".kt", ".kts", ".swift", ".dart"}
HASH_HEADER_FILES = {"Gemfile", "pubspec.yaml", "analysis_options.yaml", "phpstan.neon"}
HASH_HEADER_SOURCES = {".rb", ".gemspec"}
XML_MANIFESTS = {"pom.xml", "Directory.Build.props"}
XML_HEADER = "<!-- SPDX-License-Identifier: Apache-2.0 -->"


def language_of(path: str) -> tuple[str, str] | None:
    """(root, language) of an SDK runtime source or manifest, by its name."""
    name = PurePosixPath(path).name
    suffix = PurePosixPath(name).suffix.lower()
    for root, language, sources, names, manifest_suffixes in LANGUAGE_ROOTS:
        if suffix in sources or name in names or suffix in manifest_suffixes:
            return root, language
    return None


def language_violations(path: str) -> list[str]:
    found = []
    owner = language_of(path)
    if owner and not path.startswith(owner[0]):
        found.append(f"{owner[1]} source or manifest outside {owner[0]}")
    parts = PurePosixPath(path).parts
    if path.startswith("runtimes/ruby/") and any(p.lower() == "spec" for p in parts[:-1]):
        found.append("test directory 'spec/' (tests live in the private repository)")
    return found


def language_header_violations(path: str, text: str) -> list[str] | None:
    """Licence header problems of an SDK runtime file; None for other files."""
    name = PurePosixPath(path).name
    suffix = PurePosixPath(name).suffix.lower()
    if language_of(path) is None or not path.startswith("runtimes/"):
        return None
    lines = text.splitlines()
    if name == "Package.resolved":
        return []
    if name == "composer.json":
        try:
            licence = json.loads(text).get("license")
        except (ValueError, AttributeError):
            return ["composer.json does not parse"]
        return [] if licence == "Apache-2.0" else ['composer.json without "license": "Apache-2.0"']
    if name in XML_MANIFESTS or suffix in {".csproj", ".props"}:
        return [] if has_header(text, XML_HEADER) else [f"manifest without the licence header '{XML_HEADER}'"]
    if suffix == ".php":
        ok = bool(lines) and lines[0].strip() == "<?php" and any(
            line.strip() == APACHE_HEADER for line in lines[1:1 + HEADER_LINES]
        )
        return [] if ok else [f"PHP source without '<?php' followed by the licence header '{APACHE_HEADER}'"]
    if suffix in SLASH_HEADER_SOURCES:
        return [] if has_header(text, APACHE_HEADER) else [f"runtime source without the licence header '{APACHE_HEADER}'"]
    if suffix in HASH_HEADER_SOURCES or name in HASH_HEADER_FILES:
        return [] if has_header(text, APACHE_HEADER_PY) else [f"runtime file without the licence header '{APACHE_HEADER_PY}'"]
    return []


def path_violations(path: str) -> list[str]:
    parts = PurePosixPath(path).parts
    name = parts[-1]
    found = []
    test_dir = next((p for p in parts[:-1] if p.lower() in TEST_DIRS), None)
    if test_dir:
        found.append(f"test directory '{test_dir}/' (tests live in the private repository)")
    elif TEST_FILE.search(name) or TEST_CLASS_FILE.search(name):
        found.append("test file (tests live in the private repository)")
    suffix = PurePosixPath(name).suffix.lower()
    doc_dir = next((p for p in parts[:-1] if p.lower() in DOC_DIRS), None)
    if doc_dir:
        found.append(f"documentation directory '{doc_dir}/' (docs live in the private repository)")
    elif not ALLOWED_DOCS.match(path) and (suffix in DOC_EXTENSIONS or (not suffix and DOC_NAME.match(name))):
        found.append(
            "documentation file (only the community files are allowed: README.md, LICENSE, CONTRIBUTING.md,"
            " SECURITY.md, CODE_OF_CONDUCT.md and CHANGELOG.md at the root, Markdown in .github/ and"
            " .github/ISSUE_TEMPLATE/, examples/<name>/README.md; docs live in the private repository)"
        )
    if (suffix == ".go" or name in GO_MODULE_FILES) and not path.startswith(GO_ROOTS):
        found.append("Go source or module file outside runtimes/go/ and emitters/go/")
    if parts[0] == "assets" and len(parts) > 1 and suffix not in ASSET_EXTENSIONS:
        found.append("assets/ holds .svg and .png images only")
    if not test_dir:
        found += language_violations(path)
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
    language = language_header_violations(path, text)
    if language is not None:
        found += language
    elif suffix == ".go" and path.startswith(GO_ROOTS):
        if not has_header(text, APACHE_HEADER):
            found.append(f"Go source without the licence header '{APACHE_HEADER}'")
    elif in_runtimes and suffix in RUNTIME_SOURCE:
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
    if args.ref.removeprefix("refs/heads/").removeprefix("origin/") == PAGES_BRANCH:
        print(f"repository guard skipped: {PAGES_BRANCH} holds the generated documentation site, not code")
        return 0
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
        print("This repository is source code and the standard community files (see CONTRIBUTING.md): tests,"
              " test data and documentation are maintained in the private repository. Commits carry no AI"
              " attribution: reword them (git rebase -i) and push again.", file=sys.stderr)
        return 1
    print(f"repository guard passed ({summary})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
