# SPDX-License-Identifier: AGPL-3.0-only
"""Package explicit sources and small, privacy-masked execution evidence."""
from pathlib import Path
from bootstrap import pip_bootstrap
import hashlib
import json
import zipfile
from paths import ROOT, icon_path, redact_host_paths

SOURCES = [
    "audit.py",
    "bootstrap.py",
    "compact.py",
    "demo.py",
    "examples.py",
    "list_tools.mjs",
    "narrate.py",
    "package.py",
    "paths.py",
    "plan.py",
    "preflight.py",
    "qa.py",
    "record.py",
    "release_video.py",
    "render.py",
    "Run-ReleaseVideo.ps1",
    "pyproject.toml",
    "README.md",
]


def package(run):
    run = Path(run)
    out = run / "deliverables"
    report = json.loads((out / "qa.json").read_text(encoding="utf-8"))
    if report["publication"] != "HUMAN_REVIEW_REQUIRED":
        raise ValueError("Unexpected review gate")
    video = out / report["file"]
    if hashlib.sha256(video.read_bytes()).hexdigest() != report["sha256"]:
        raise ValueError("Video differs from the verified artifact")
    version = report["compiler"]
    destination = out / f"tungsten-{version}-manual-release-video-system.zip"
    if destination.exists():
        raise FileExistsError("Preserve this package and use a new run")
    selected = [(ROOT / name, name, False) for name in SOURCES]
    selected += [
        (pip_bootstrap(), "bootstrap_python.py", False),
        (run / "reviewed-profile.json", "profile.json", False),
        (run / "plan.json", f"scripts/{version}.json", False),
        (icon_path(), "assets/tungsten-icon.png", False),
    ]
    for folder in ["examples", "evidence"]:
        selected.extend(
            (p, "review-evidence/" + p.relative_to(run).as_posix(), True)
            for p in (run / folder).rglob("*")
            if p.is_file()
        )
    for name in ["plan.json", "capabilities.json", "preflight.json"]:
        selected.append((run / name, "review-evidence/" + name, True))
    for name in ["segments.json", "voice-provenance.json"]:
        selected.append((run / "audio" / name, "review-evidence/audio/" + name, True))
    for name in [
        "qa.json",
        "chapters.txt",
        "chapters.json",
        "chapters.ffmetadata",
        "captions.srt",
        "timeline.json",
        "render.json",
    ]:
        selected.append((out / name, "review-evidence/" + name, True))
    if (out / "compact-qa.json").is_file():
        selected.append(
            (out / "compact-qa.json", "review-evidence/compact-qa.json", True)
        )
    manifest = []
    with zipfile.ZipFile(destination, "x", zipfile.ZIP_DEFLATED) as archive:
        for path, name, mask in selected:
            data = path.read_bytes()
            if mask:
                data = redact_host_paths(data.decode("utf-8")).encode("utf-8")
            archive.writestr(name, data)
            manifest.append(
                dict(
                    file=name, sha256=hashlib.sha256(data).hexdigest(), bytes=len(data)
                )
            )
        archive.writestr(
            "REVIEW_REQUIRED.json",
            json.dumps(
                dict(
                    publication="HUMAN_REVIEW_REQUIRED",
                    version=version,
                    review=[
                        "Listen to narration and review pronunciation/pacing",
                        "Verify claims against the exact published release",
                        "Inspect playback and chapter navigation",
                    ],
                    privacy="Host home paths masked in exported evidence; raw local capture preserved",
                    automated_publication=False,
                    persistent_permissions=False,
                    paid_api=False,
                ),
                indent=2,
            ),
        )
        archive.writestr("MANIFEST.json", json.dumps(manifest, indent=2))
    with zipfile.ZipFile(destination) as archive:
        assert archive.testzip() is None
        assert not any(
            "/node_modules/" in n
            or n.startswith((".work/", "cache/", "tools/", ".venv/", "generated/"))
            for n in archive.namelist()
        )
    summary = dict(
        file=destination.name,
        bytes=destination.stat().st_size,
        sha256=hashlib.sha256(destination.read_bytes()).hexdigest(),
        entries=len(manifest) + 2,
        publication="HUMAN_REVIEW_REQUIRED",
    )
    (out / "package.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return summary
