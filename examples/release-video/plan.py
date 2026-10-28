# SPDX-License-Identifier: AGPL-3.0-only
"""Load an explicitly reviewed script for exactly one published version."""
from pathlib import Path
import json


def make(version):
    path = Path(__file__).resolve().parent / "scripts" / (version + ".json")
    if not path.is_file():
        raise ValueError(
            "No reviewed chapter script for "
            + version
            + ". Create scripts/"
            + version
            + ".json and review every claim and command before recording."
        )
    plan = json.loads(path.read_text(encoding="utf-8"))
    assert plan["version"] == version and plan["chapters"]
    assert all(
        c["narration"] and c["commands"] and c["minimum"] > 0 for c in plan["chapters"]
    )
    return plan
