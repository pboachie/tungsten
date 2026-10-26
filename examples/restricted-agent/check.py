# SPDX-License-Identifier: AGPL-3.0-only
"""Check a restricted agent integration built from a shared profile.

Copies this example into a temporary directory, explains its effective
configuration (target defaults and transport references with their sources),
generates it with --strict and inspects the MCP tool manifest. Uses only the
compiler: no dependency installation and no network access.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
TUNGSTEN = os.environ.get("TUNGSTEN") or str(HERE.parents[1] / "target" / "debug" / "tungsten")


def tungsten(project: Path, *args: str) -> dict:
    done = subprocess.run(
        [TUNGSTEN, "--json", *args], cwd=project, capture_output=True, text=True, check=False
    )
    report = json.loads(done.stdout)
    if done.returncode != 0:
        sys.exit(f"tungsten {' '.join(args)} failed:\n{done.stdout}{done.stderr}")
    return report["result"]


def expect(label: str, actual: object, expected: object) -> None:
    if actual != expected:
        sys.exit(f"{label}: expected {expected!r}, got {actual!r}")
    print(f"ok  {label}: {actual!r}")


def main() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        project = Path(temporary) / "restricted-agent"
        shutil.copytree(HERE, project, ignore=shutil.ignore_patterns("check.py", "README.md"))

        explained = tungsten(project, "config")
        sources = {
            field["path"]: (field["value"], field["winner"]["source"])
            for field in explained["transport"]
            if field["winner"]
        }
        expect("proxy reference", sources["/transport/proxy"], ({"env": "CORP_HTTPS_PROXY"}, "profile[0]"))
        expect("CA bundle reference", sources["/transport/ca_bundle"], ({"env": "CORP_CA_BUNDLE"}, "profile[0]"))
        expect("account", sources["/transport/account"], ("helpdesk-agent", "project"))
        version = next(
            field
            for target in explained["targets"]
            for field in target["fields"]
            if field["path"] == "/targets/typescript/version"
        )
        expect("SDK version", (version["value"], version["winner"]["source"]), ("0.4.0", "profile[0]"))

        tungsten(project, "generate", "--strict")
        manifest = json.loads((project / "generated" / "mcp" / "manifest.json").read_text())
        tools = {tool["target"]: tool for tool in manifest["tools"]}
        expect(
            "agent tools",
            sorted(tools),
            ["tickets.createTicket", "tickets.getTicket", "tickets.listTickets"],
        )
        create = tools["tickets.createTicket"]
        expect("create safety", create["safety"], "irreversible")
        expect("create needs confirmation", "confirmation_token" in create["inputSchema"]["properties"], True)
        expect("list is read-only", tools["tickets.listTickets"]["annotations"]["readOnlyHint"], True)
        descriptors = (project / "generated" / "typescript" / "src" / "descriptors.ts").read_text()
        expect("SDK keeps delete for humans", "tickets.deleteTicket" in descriptors, True)
        package = json.loads((project / "generated" / "typescript" / "package.json").read_text())
        expect("SDK package version", package["version"], "0.4.0")


if __name__ == "__main__":
    main()
