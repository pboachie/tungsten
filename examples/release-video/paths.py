# SPDX-License-Identifier: AGPL-3.0-only
"""Portable locations; generated work stays in the example's ignored directory."""
from pathlib import Path
import os
import re

ROOT = Path(__file__).resolve().parent
WORK = ROOT / ".work"


def ensure_work():
    if WORK.is_symlink() or WORK.resolve() != ROOT / ".work":
        raise ValueError("Refusing a redirected .work directory")
    WORK.mkdir(exist_ok=True)
    return WORK


def run_path(version, name):
    if not re.fullmatch(r"v\d+\.\d+\.\d+", version):
        raise ValueError("Use an exact stable release tag vMAJOR.MINOR.PATCH")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}", name):
        raise ValueError("Use a simple run name without path separators")
    return ensure_work() / "runs" / (version + "-" + name)


def icon_path():
    for candidate in [
        ROOT / "assets/tungsten-icon.png",
        ROOT.parents[1] / "assets/tungsten-icon.png",
    ]:
        if candidate.is_file():
            return candidate
    raise FileNotFoundError(
        "Tungsten icon missing from repository assets or standalone bundle"
    )


def redact_host_paths(text):
    """Mask host home directories in exports, without rewriting original evidence."""
    for home in {str(Path.home()), str(Path.home()).replace("\\", "/")}:
        if len(home) > 3:
            text = text.replace(home.replace("\\", "\\\\"), "[home]")
            text = text.replace(home, "[home]")
    username = os.environ.get("USERNAME", "")
    if username:
        for token in [
            f"/Users/{username}/",
            f"\\Users\\{username}\\",
            f"\\\\Users\\\\{username}\\\\",
        ]:
            text = text.replace(token, token.replace(username, "[user]"))
    return text


def mask_terminal_usernames(events, username=None):
    """Preserve stream length and timestamps, including split output chunks."""
    username = os.environ.get("USERNAME", "") if username is None else username
    if not username:
        return events
    output = "".join(event["text"] for event in events if event["kind"] == "output")
    mask = "*" * len(username)
    for prefix, suffix in [("/Users/", "/"), ("\\Users\\", "\\")]:
        output = output.replace(prefix + username + suffix, prefix + mask + suffix)
    result = []
    offset = 0
    for event in events:
        item = dict(event)
        if event["kind"] == "output":
            end = offset + len(event["text"])
            item["text"] = output[offset:end]
            offset = end
        result.append(item)
    return result
