# SPDX-License-Identifier: AGPL-3.0-only
"""Check that two projects reuse one profile chain with per-project overrides.

Copies this example into a temporary directory, explains each project's
effective configuration, then generates and inspects the package metadata.
Uses only the compiler: no dependency installation and no network access.
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
        sys.exit(f"{project.name}: tungsten {' '.join(args)} failed:\n{done.stdout}{done.stderr}")
    return report["result"]


def winners(project: Path) -> dict[str, tuple[object, str]]:
    explained = tungsten(project, "config")
    return {
        field["path"]: (field["value"], field["winner"]["source"])
        for target in explained["targets"]
        for field in target["fields"]
        if field["winner"]
    }


def expect(label: str, actual: object, expected: object) -> None:
    if actual != expected:
        sys.exit(f"{label}: expected {expected!r}, got {actual!r}")
    print(f"ok  {label}: {actual!r}")


def main() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary) / "profile-reuse"
        shutil.copytree(HERE, root, ignore=shutil.ignore_patterns("check.py", "README.md"))
        storefront, catalog = root / "storefront", root / "catalog"

        # Ordinary app: its explicit version wins; undeclared targets stay absent.
        explained = winners(storefront)
        expect("storefront typescript version", explained["/targets/typescript/version"], ("2.3.0", "project"))
        generated = tungsten(storefront, "generate")
        expect("storefront targets", sorted(t["target"] for t in generated["targets"]), ["typescript"])

        # Multi-target library: the included organization profile and the library
        # profile both contribute; package identities come from the project.
        explained = winners(catalog)
        expect("catalog typescript version", explained["/targets/typescript/version"][0], "1.0.0")
        expect("catalog python models", explained["/targets/python/models"][0], "dataclasses")
        expect("catalog python package", explained["/targets/python/package"], ("catalog-client", "project"))
        tungsten(catalog, "generate")
        pyproject = (catalog / "generated" / "python" / "pyproject.toml").read_text()
        expect("catalog requires-python", 'requires-python = ">=3.11"' in pyproject, True)
        cargo = (catalog / "generated" / "rust" / "Cargo.toml").read_text()
        expect("catalog rust version", 'version = "1.0.0"' in cargo, True)


if __name__ == "__main__":
    main()
