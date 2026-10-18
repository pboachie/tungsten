# SPDX-License-Identifier: AGPL-3.0-only
"""Manual entry point. No publishing, scheduling, credentials or account changes."""
from pathlib import Path
import argparse
import json
import os
import shutil
import subprocess
import sys

from paths import ROOT, WORK, ensure_work, run_path


def parser():
    p = argparse.ArgumentParser(
        description="Generate a version-audited chaptered terminal walkthrough for human review. No publishing step exists."
    )
    p.add_argument(
        "--version", required=True, help="Exact published release, e.g. v0.1.2"
    )
    p.add_argument(
        "--run-id",
        default="review-001",
        help="New isolated run directory name; prior captures are never overwritten",
    )
    p.add_argument(
        "--profile", help="Reviewed capability profile for this exact release"
    )
    p.add_argument(
        "--voice",
        default="am_fenrir",
        choices=["am_fenrir", "am_michael", "am_puck"],
        help="Generic local male Kokoro voice",
    )
    p.add_argument(
        "--stage",
        default="all",
        choices=[
            "audit",
            "prepare",
            "narrate",
            "record",
            "render",
            "qa",
            "compact",
            "package",
            "all",
        ],
    )
    p.add_argument(
        "--allow-registry",
        action="store_true",
        help="Allow reviewed public dependency resolution during preflight, never package install scripts",
    )
    return p


def main():
    args = parser().parse_args()
    run = run_path(args.version, args.run_id)
    if args.stage in ["all", "prepare"] and run.exists():
        raise FileExistsError(
            "Run already exists; choose a new run-id or an explicit later stage"
        )
    import audit

    settings, inventory = audit.prepare(args.version, args.profile)
    print("Audited", args.version, "at", inventory["source_commit"], flush=True)
    if args.stage == "audit":
        return
    if args.stage in ["all", "prepare"]:
        if run.exists():
            raise FileExistsError(
                "Run already exists; choose a new run-id or an explicit later stage"
            )
        run.mkdir(parents=True)
        for name in ["demo.py", "release_video.py", "paths.py"]:
            shutil.copy2(ROOT / name, run / name)
        shutil.copy2(ROOT / "list_tools.mjs", run / "list_tools.mjs")
        for file in [
            WORK / "cache" / args.version / "capabilities.json",
            WORK / "cache" / args.version / "settings.json",
        ]:
            shutil.copy2(file, run / file.name)
        shutil.copy2(settings["profile"], run / "reviewed-profile.json")
        import examples, preflight, plan

        # Acquire public examples only. No ZROtext service operations occur.
        reference = WORK / "cache/zrotext-reference"
        fixture = settings["public_fixture"]
        if not reference.is_dir():
            reference.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(
                ["git", "clone", "--depth", "1", fixture["repository"], str(reference)],
                check=True,
            )
        current = subprocess.check_output(
            ["git", "-C", str(reference), "rev-parse", "HEAD"], text=True
        ).strip()
        if current != fixture["commit"]:
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(reference),
                    "fetch",
                    "--depth",
                    "1",
                    "origin",
                    fixture["commit"],
                ],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(reference),
                    "checkout",
                    "--detach",
                    fixture["commit"],
                ],
                check=True,
            )
        if subprocess.check_output(
            ["git", "-C", str(reference), "status", "--porcelain"], text=True
        ).strip():
            raise ValueError("Public fixture checkout has local changes")
        import hashlib

        if (
            hashlib.sha256((reference / fixture["contract"]).read_bytes()).hexdigest()
            != fixture["sha256"]
        ):
            raise ValueError("Public fixture contract hash changed; review the profile")
        examples.prepare(run / "examples", reference)
        result = preflight.run(
            settings, run / "examples", args.allow_registry, reference
        )
        (run / "preflight.json").write_text(
            json.dumps(result, indent=2), encoding="utf-8"
        )
        script = plan.make(args.version)
        script["voice"] = args.voice
        (run / "plan.json").write_text(json.dumps(script, indent=2), encoding="utf-8")
        cache = WORK / "cache/npm"
        if not cache.is_dir():
            raise RuntimeError(
                "Reviewed npm cache missing; preflight must resolve the generated packages first"
            )
        shutil.copytree(cache, run / "npm-cache")
        if args.stage == "prepare":
            print("Prepared", run)
            return
    if not run.is_dir():
        raise FileNotFoundError("Prepare this run first")
    script = json.loads((run / "plan.json").read_text(encoding="utf-8"))
    if args.stage in ["all", "narrate"]:
        python = WORK / ".venv/Scripts/python.exe"
        if not python.is_file():
            raise RuntimeError("Local Kokoro environment missing; run bootstrap first")
        models = WORK / "tools/kokoro-model"
        subprocess.run(
            [
                str(python),
                str(ROOT / "narrate.py"),
                str(run),
                str(run / "plan.json"),
                str(models),
            ],
            check=True,
        )
    if args.stage in ["all", "record"]:
        if (run / "evidence/terminal-events.jsonl").exists():
            raise FileExistsError(
                "A recording already exists; preserve it and create a new run"
            )
        from record import record

        record(run, settings, script)
    if args.stage in ["all", "render"]:
        from render import render

        render(run, script)
    if args.stage in ["all", "qa"]:
        from qa import validate

        validate(run, settings)
    if args.stage in ["all", "compact"]:
        from compact import compact

        compact(run, settings)
    if args.stage in ["all", "package"]:
        from package import package

        package(run)
    print("Review-only deliverables:", run / "deliverables", flush=True)


if __name__ == "__main__":
    main()
