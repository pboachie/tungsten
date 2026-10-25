# SPDX-License-Identifier: AGPL-3.0-only
"""Explicit manual Windows setup. All tools/models remain inside this folder."""
from pathlib import Path
import hashlib
import json
import subprocess
import sys
import urllib.request
import venv
import zipfile

from paths import ROOT, WORK, ensure_work

ASSETS = [
    (
        "go.zip",
        "https://go.dev/dl/go1.27.3.windows-amd64.zip",
        "f26081f438319757f8a1b46d7d0b2c5ec7583d4038e485e0aa725cfde7885fff",
    ),
    (
        "kokoro-v1.0.onnx",
        "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.1/kokoro-v1.0.onnx",
        "beb0d1848dee9a49da392cc3df26958d46cfa35d321edf434f52949153f0df3a",
    ),
    (
        "voices-v1.0.bin",
        "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.1/voices-v1.0.bin",
        "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d",
    ),
]


def pip_bootstrap():
    """Use the bundled helper in exports, or its canonical compiler source."""
    bundled = ROOT / "bootstrap_python.py"
    if bundled.is_file():
        return bundled
    return ROOT.parents[1] / "crates/tungsten-cli/src/commands/bootstrap_python.py"


def main():
    if sys.platform != "win32" or sys.version_info[:2] != (3, 12):
        raise RuntimeError(
            "Use Windows x64 and Python 3.12 for this reviewed environment"
        )
    ensure_work()
    cache = WORK / "cache/bootstrap"
    cache.mkdir(parents=True, exist_ok=True)
    tools = WORK / "tools"
    tools.mkdir(exist_ok=True)
    for name, url, digest in ASSETS:
        target = cache / name
        if not target.is_file():
            print("Downloading public tool/model:", name, flush=True)
            urllib.request.urlretrieve(url, target)
        if hashlib.sha256(target.read_bytes()).hexdigest() != digest:
            raise ValueError("Checksum mismatch: " + name)
        if name == "go.zip":
            destination = tools / "go-sdk"
            destination.mkdir(exist_ok=True)
            with zipfile.ZipFile(target) as archive:
                for member in archive.namelist():
                    resolved = (destination / member).resolve()
                    if not resolved.is_relative_to(destination.resolve()):
                        raise ValueError("Unsafe archive path")
                archive.extractall(destination)
        else:
            import shutil

            destination = tools / "kokoro-model"
            destination.mkdir(exist_ok=True)
            shutil.copy2(target, destination / name)
    environment = WORK / ".venv"
    if not environment.is_dir():
        venv.create(environment, with_pip=True)
    python = environment / "Scripts/python.exe"
    subprocess.run([str(python), "-I", str(pip_bootstrap())], check=True)
    subprocess.run(
        [str(python), "-m", "pip", "install", "--disable-pip-version-check", str(ROOT)],
        check=True,
    )
    (tools / "bin").mkdir(exist_ok=True)
    (cache / "verified-assets.json").write_text(
        json.dumps([dict(name=n, url=u, sha256=s) for n, u, s in ASSETS], indent=2),
        encoding="utf-8",
    )
    print(
        "Ready. Git and Node.js 22+ must be available. Run Run-ReleaseVideo.ps1 manually. No background task or system settings were installed."
    )


if __name__ == "__main__":
    main()
