#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Fill the data blocks of examples/showcase/README.md from what run.sh collected.

    render.py --write   rewrite the blocks between the marker comments
    render.py --check   exit 1 when the committed README differs from a fresh render

Everything between `<!-- showcase:begin NAME -->` and `<!-- showcase:end NAME -->` is
generated: NAME is `summary`, `sources` or `api-<id>`. The text outside the markers is
written by hand. The output depends only on the pinned specs, the manifests in apis/,
and the tungsten binary, never on the time or the machine. Standard library only.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
LINE_CAP = 220
EXCERPT_LINES = 40
TIERS = ("read_only", "mutating", "destructive", "irreversible")
ORIGINS = {
    "tool": "this operation's entry in agent.yml",
    "defaults": "the defaults in agent.yml",
    "built_in": "tungsten's built-in default",
    "inferred": "inferred from the other settings",
}
RAW_URL = re.compile(r"^https://raw\.githubusercontent\.com/([^/]+)/([^/]+)/([^/]+)/(.+)$")


class RenderError(Exception):
    pass


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError as err:
        raise RenderError(f"cannot read {path}: {err}") from err


def load_json(path: Path):
    try:
        return json.loads(read(path))
    except json.JSONDecodeError as err:
        raise RenderError(f"{path} is not JSON: {err}") from err


def load_lock(path: Path) -> dict[str, tuple[str, str]]:
    lock = {}
    for line in read(path).splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) != 3:
            raise RenderError(f"{path}: expected '<sha256>  <file>  <url>', got: {line}")
        lock[parts[1]] = (parts[0], parts[2])
    return lock


def spec_file(api_id: str) -> str:
    """The cache file name an API's tungsten.yml points at."""
    match = re.search(r"\.cache/specs/([^\"'\s]+)", read(HERE / "apis" / api_id / "tungsten.yml"))
    if not match:
        raise RenderError(f"apis/{api_id}/tungsten.yml names no spec under .cache/specs")
    return match[1]


# ── small formatting helpers ─────────────────────────────────────────────────

def n(value: int) -> str:
    return f"{value:,}"


def cap(line: str) -> str:
    return line if len(line) <= LINE_CAP else line[: LINE_CAP - 4].rstrip() + " ..."


def fence(text: str, lang: str = "") -> str:
    text = text.rstrip("\n")
    ticks = "```"
    while ticks in text:
        ticks += "`"
    return f"{ticks}{lang}\n{text}\n{ticks}"


def dedent(lines: list[str], width: int) -> list[str]:
    return [line[width:] if line[:width].strip() == "" else line for line in lines]


def cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


# ── excerpts of generated code ───────────────────────────────────────────────

def find_marker(files: list[Path], op: str) -> tuple[list[str], int, Path] | None:
    marker = f"(`{op}`)"
    for path in sorted(files):
        lines = read(path).splitlines()
        for index, line in enumerate(lines):
            if marker in line:
                return lines, index, path
    return None


def trim_doc(doc: list[str], blank: str, ellipsis: str, marker: str, plain) -> list[str]:
    """Keep the summary, the paragraph with the route and safety line, and the
    confirmation instructions of a doc comment; mark the rest with one `...`."""
    paragraphs: list[list[str]] = [[]]
    for line in doc:
        if plain(line) == "":
            if paragraphs[-1]:
                paragraphs.append([])
        else:
            paragraphs[-1].append(line)
    paragraphs = [p for p in paragraphs if p]
    out: list[str] = []
    skipping = False
    for number, paragraph in enumerate(paragraphs):
        text = plain(paragraph[0])
        if number == 0 or any(marker in line for line in paragraph) or text.startswith("Requires confirmation"):
            if out:
                out.append(blank)
            out += paragraph[:3] + ([ellipsis] if len(paragraph) > 3 else [])
            skipping = False
        elif not skipping:
            out += [blank, ellipsis]
            skipping = True
    return out


def ts_excerpt(lines: list[str], index: int, marker: str) -> list[str]:
    start = index
    while start > 0 and lines[start].strip() != "/**":
        start -= 1
    end = index
    while end < len(lines) and lines[end].strip() != "*/":
        end += 1
    decl = end + 1
    stop = decl
    if not lines[decl].rstrip().endswith(";"):
        while stop < len(lines) and lines[stop] != "  };":
            stop += 1
    doc = trim_doc(
        lines[start + 1 : end],
        "   *",
        "   * ...",
        marker,
        lambda line: line.strip().removeprefix("*").strip(),
    )
    return dedent([lines[start]] + doc + lines[end : stop + 1], 2)


def py_excerpt(lines: list[str], index: int, marker: str) -> list[str]:
    start = index
    while start > 0 and not re.match(r"^    (async )?def ", lines[start]):
        start -= 1
    sig_end = start
    while not (lines[sig_end].rstrip().endswith(":") and "->" in lines[sig_end]):
        sig_end += 1
    first = sig_end + 1
    if lines[first].strip().endswith('"""') and len(lines[first].strip()) > 6:
        return dedent(lines[start : first + 1], 4)
    close = first + 1
    while lines[close].strip() != '"""':
        close += 1
    doc = trim_doc(lines[first:close], "", "        ...", marker, lambda line: line.strip().removeprefix('"""'))
    return dedent(lines[start : sig_end + 1] + doc + [lines[close]], 4)


def rs_excerpt(lines: list[str], index: int, marker: str) -> list[str]:
    start = index
    while start > 0 and lines[start - 1].lstrip().startswith("///"):
        start -= 1
    sig = index
    while not lines[sig].rstrip().endswith("{"):
        sig += 1
    first_code = index
    while lines[first_code].lstrip().startswith("///"):
        first_code += 1
    doc = trim_doc(
        lines[start:first_code],
        "    ///",
        "    /// ...",
        marker,
        lambda line: line.strip().removeprefix("///").strip(),
    )
    return dedent(doc + lines[first_code : sig + 1] + ["        // ...", "    }"], 4)


LANGS = (
    ("typescript", "TypeScript", "ts", "typescript/src/resources", "*.ts", ts_excerpt),
    ("python", "Python", "python", "python", "resources/*.py", py_excerpt),
    ("rust", "Rust", "rust", "rust", "*/src/resources/*.rs", rs_excerpt),
)


def sdk_excerpts(out: Path, api_id: str, op: str) -> str:
    parts = []
    for _key, label, lang, subdir, pattern, extract in LANGS:
        root = out / api_id / subdir
        files = [p for p in root.rglob(pattern)] if root.is_dir() else []
        found = find_marker(files, op)
        if not found:
            raise RenderError(f"{api_id}: no generated {label} code for {op}")
        lines, index, path = found
        body = [cap(line) for line in extract(lines, index, f"(`{op}`)")][:EXCERPT_LINES]
        rel = path.relative_to(out / api_id).as_posix()
        parts.append(
            f"<details>\n<summary>{label}</summary>\n\n"
            f"`{rel}`\n\n{fence(chr(10).join(body), lang)}\n\n</details>"
        )
    return "\n\n".join(parts)


def llms_block(out: Path, api_id: str, op: str) -> str:
    lines = read(out / api_id / "docs" / "llms-full.txt").splitlines()
    marker = f"- Operation: {op} "
    at = next((i for i, line in enumerate(lines) if line.startswith(marker)), None)
    if at is None:
        raise RenderError(f"{api_id}: {op} is not in llms-full.txt")
    start = at
    while not lines[start].startswith("### "):
        start -= 1
    end = at
    while end + 1 < len(lines) and not lines[end + 1].startswith("### "):
        end += 1
    block = [cap(line) for line in lines[start : end + 1]]
    while block and not block[-1].strip():
        block.pop()
    return "\n".join(block)


# ── per-API data ─────────────────────────────────────────────────────────────

def counts_by_code(diagnostics: list[dict]) -> list[tuple[str, str, int]]:
    tally: dict[tuple[str, str], int] = {}
    for d in diagnostics:
        key = (d["code"], d["severity"])
        tally[key] = tally.get(key, 0) + 1
    return [(code, sev, count) for (code, sev), count in sorted(tally.items())]


def api_block(api: dict, data: Path, out: Path) -> str:
    api_id = api["id"]
    check_txt = read(data / api_id / "check.txt").rstrip()
    check_doc = load_json(data / api_id / "check.json")
    check = check_doc["result"]
    report = load_json(data / api_id / "report.json")["result"]
    meanings = {g["code"]: g["summary"] for g in report["diagnostic_groups"]}
    mcp = report["budgets"]["mcp"]
    safety = report["safety"]
    row = next((r for r in safety if r["operation"] == api["danger"]), None)
    if row is None:
        raise RenderError(f"{api_id}: {api['danger']} is not an operation of the API")
    tiers = {t: sum(1 for r in safety if r["tier"]["value"] == t) for t in TIERS}
    files = {t["target"]: t["files"] for t in report["coverage"]["targets"]}
    ops = check["stats"]["operations"]["total"]
    counts = check["counts"]

    diag_rows = "\n".join(
        f"| `{code}` | {sev} | {n(count)} | {cell(meanings.get(code, ''))} |"
        for code, sev, count in counts_by_code(check_doc["diagnostics"])
    )
    if not diag_rows:
        diag_rows = "| none | | | |"

    over = mcp["over_budget"]
    tier_line = " · ".join(f"{t.replace('_', '-')} {n(tiers[t])}" for t in TIERS)

    def origin(key: str) -> str:
        return ORIGINS.get(row[key]["origin"], row[key]["origin"])

    safety_rows = "\n".join(
        f"| {label} | `{row[key]['value']}` | {origin(key)} |"
        for key, label in (
            ("tier", "Safety tier"),
            ("confirmation", "Confirmation"),
            ("preview", "Preview"),
            ("idempotency", "Idempotency"),
            ("verify", "Verification"),
        )
    )

    summary = (
        f"<b>{api['name']}</b> · {n(ops)} operations · {n(counts['errors'])} errors, "
        f"{n(counts['warnings'])} warnings · MCP listing {n(mcp['discrete_tokens'])} "
        f"to {n(mcp['progressive_tokens'])} tokens"
    )
    return f"""<details>
<summary>{summary}</summary>

{api['description']} {api['note']}

#### What tungsten found

`tungsten check apis/{api_id}`

{fence(check_txt, "text")}

Diagnostics of that run, by code:

| Code | Severity | Count | Meaning |
|---|---|---:|---|
{diag_rows}

Safety tiers assigned to the {n(ops)} operations: {tier_line}. Generated files: TypeScript {n(files['typescript'])}, Python {n(files['python'])}, Rust {n(files['rust'])}, docs {n(files['docs'])}.

#### MCP surface

| | |
|---|---|
| Tools if every operation were listed (discrete) | {n(mcp['discrete_tokens'])} tokens |
| Listing in progressive mode | {n(mcp['progressive_tokens'])} tokens |
| Mode `auto` chose | `{mcp['mode']}` (threshold {mcp['threshold']} tools, {n(len(mcp['tools']))} here) |
| Tools above the 600-token schema budget | {n(over)} |

#### A generated SDK call

`{api['call']}` in the three generated SDKs (excerpts; `...` marks what is left out).

{sdk_excerpts(out, api_id, api['call'])}

#### Agent safety: `{api['danger']}`

The compiled view of one destructive operation, as the docs target writes it for an agent:

{fence(llms_block(out, api_id, api['danger']), "text")}

| Setting | Value | Comes from |
|---|---|---|
{safety_rows}

The same operation in the three SDKs. Each SDK documents how to confirm: pass a confirmation, or the token that `preview` returns together with the request and its effects.

{sdk_excerpts(out, api_id, api['danger'])}

#### Reproduce

```sh
./run.sh --only {api_id}
tungsten mock apis/{api_id} --port 8099
```

</details>"""


def split_url(url: str) -> tuple[str, str, str, str]:
    match = RAW_URL.match(url)
    if not match:
        raise RenderError(f"not a raw.githubusercontent.com URL: {url}")
    return match.groups()  # type: ignore[return-value]


def summary_block(apis: list[dict], data: Path, version: str) -> str:
    rows = []
    for api in apis:
        check = load_json(data / api["id"] / "check.json")["result"]
        report = load_json(data / api["id"] / "report.json")["result"]
        mcp = report["budgets"]["mcp"]
        c = check["counts"]
        rows.append(
            f"| {api['name']} | {check['stats']['operations']['total']:,} "
            f"| {c['errors']} / {c['warnings']:,} / {c['infos']:,} "
            f"| {mcp['discrete_tokens']:,} | {mcp['progressive_tokens']:,} |"
        )
    return (
        f"Numbers from tungsten {version}.\n\n"
        "| API | Operations | Errors / warnings / infos | MCP listing, discrete (tokens) "
        "| MCP listing, progressive (tokens) |\n|---|---:|---:|---:|---:|\n" + "\n".join(rows)
    )


def sources_block(apis: list[dict], lock: dict[str, tuple[str, str]]) -> str:
    rows = []
    for api in apis:
        sha, url = lock[spec_file(api["id"])]
        owner, repo, ref, path = split_url(url)
        label = api.get("ref_label")
        shown = f"`{label}` (`{ref[:12]}`)" if label else f"`{ref[:12]}`"
        rows.append(
            f"| {api['name']} | [{owner}/{repo}](https://github.com/{owner}/{repo}) | {shown} "
            f"| `{path}` | `{sha[:12]}` | {api['licence']} ({api['copyright']}) |"
        )
    return (
        "| API | Source repository | Pinned at | File | SHA-256 | Licence of the description |\n"
        "|---|---|---|---|---|---|\n" + "\n".join(rows)
    )


# ── README rewriting ─────────────────────────────────────────────────────────

def block_re(name: str) -> re.Pattern[str]:
    return re.compile(
        rf"(<!-- showcase:begin {re.escape(name)} -->)(.*?)(<!-- showcase:end {re.escape(name)} -->)",
        re.DOTALL,
    )


def render(readme: str, only: str | None, data: Path, out: Path) -> str:
    apis = load_json(HERE / "apis.json")["apis"]
    lock = load_lock(HERE / "specs.lock")
    version = load_json(data / apis[0]["id"] / "check.json")["version"]
    blocks = {
        "summary": lambda: summary_block(apis, data, version),
        "sources": lambda: sources_block(apis, lock),
    }
    for api in apis:
        blocks[f"api-{api['id']}"] = lambda api=api: api_block(api, data, out)
    for name, make in blocks.items():
        if only and name not in (f"api-{only}",):
            continue
        pattern = block_re(name)
        if not pattern.search(readme):
            raise RenderError(f"README.md has no block '{name}' (<!-- showcase:begin {name} -->)")
        text = make()
        readme = pattern.sub(lambda m: f"{m[1]}\n{text}\n{m[3]}", readme, count=1)
    return readme


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    parser.add_argument("--only", metavar="ID", help="only the block of this API")
    parser.add_argument("--data", type=Path, default=HERE / ".cache" / "data")
    parser.add_argument("--out", type=Path, default=HERE / ".cache" / "out")
    args = parser.parse_args(argv)
    readme_path = HERE / "README.md"
    try:
        current = read(readme_path)
        fresh = render(current, args.only, args.data, args.out)
    except RenderError as err:
        print(f"render.py: {err}", file=sys.stderr)
        return 3
    if args.check:
        if fresh != current:
            print("examples/showcase/README.md differs from a fresh render; run ./run.sh and commit", file=sys.stderr)
            return 1
        print("examples/showcase/README.md matches a fresh render")
        return 0
    if fresh != current:
        readme_path.write_text(fresh, encoding="utf-8")
        print("README.md updated")
    else:
        print("README.md unchanged")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
