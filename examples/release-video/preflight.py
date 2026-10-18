# SPDX-License-Identifier: AGPL-3.0-only
"""Meaningful executable probes for input support and imported policy refusal."""
from pathlib import Path
from paths import ROOT, WORK, ensure_work, icon_path, redact_host_paths
import datetime
import json
import os
import shutil
import subprocess
import sys


def run(settings, examples, allow_registry=False, source_repo=None):
    root = ROOT
    scratch = (
        WORK
        / "cache"
        / ("preflight-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
    )
    scratch.mkdir(exist_ok=False)
    results = []
    env = dict(
        os.environ,
        GOCACHE=str(WORK / "cache/go-build"),
        GOPATH=str(WORK / "cache/go-work"),
        GIT_TERMINAL_PROMPT="0",
        GCM_INTERACTIVE="never",
    )

    def cmd(key, args, expect=0, contains=None):
        args = [str(a) for a in args]
        p = subprocess.run(
            args, text=True, encoding="utf-8", capture_output=True, env=env
        )
        entry = dict(
            key=key,
            argv=args,
            exit_code=p.returncode,
            expected_exit=expect,
            stdout=p.stdout,
            stderr=p.stderr,
        )
        results.append(entry)
        if p.returncode != expect or (contains and contains not in p.stdout + p.stderr):
            (scratch / "results.json").write_text(
                json.dumps(results, indent=2), encoding="utf-8"
            )
            raise RuntimeError("Preflight failed: " + key + "\n" + p.stdout + p.stderr)
        print("Preflight:", key, "exit", p.returncode, flush=True)

    binary = settings["binary"]
    examples = Path(examples).resolve()
    variants = [
        ("oas30", ["--from", str(examples / "openapi30.yaml")]),
        ("swagger", ["--from", str(examples / "swagger2.json")]),
        ("postman", ["--from", str(examples / "zrotext.postman_collection.json")]),
        ("curl", ["--from", str(examples / "public-get.curl"), "--route", "/json"]),
        ("http", ["--from", str(examples / "public-get.http"), "--route", "/json"]),
        ("rest", ["--from", str(examples / "public-get.rest"), "--route", "/json"]),
        (
            "har",
            [
                "--from",
                str(examples / "public-get.har"),
                "--har-origin",
                "https://httpbin.org",
                "--har-paths",
                str(examples / "har-paths.json"),
            ],
        ),
        ("endpoints", ["--from", str(examples / "endpoints.json")]),
        (
            "manual",
            [
                "--method",
                "get",
                "--endpoint",
                "/json",
                "--response-example",
                str(examples / "httpbin-response.json"),
            ],
        ),
    ]
    for key, flags in variants:
        project = scratch / key
        cmd(
            key + "-init",
            [
                binary,
                "init",
                *flags,
                "--dir",
                str(project),
                "--targets",
                "docs",
                "--generate",
                "--no-interactive",
            ],
        )
        cmd(key + "-freshness", [binary, "generate", str(project), "--check"])
    for target in ["java", "csharp", "ruby", "go"]:
        denied = scratch / ("denied-" + target + "-import")
        cmd(
            "policy-init-" + target,
            [
                binary,
                "init",
                "--from",
                examples / "zrotext.postman_collection.json",
                "--dir",
                denied,
                "--targets",
                "java",
                "--no-interactive",
            ],
        )
        config = {
            "tungsten": 1,
            "api": {"name": "imported_contract", "title": "Imported contract"},
            "inputs": [{"namespace": "imported", "spec": "imported.openapi.json"}],
            "targets": {target: {"out": "generated/" + target}},
        }
        if target == "go":
            config["targets"][target]["external"] = settings["go_emitter"].replace(
                "\\", "/"
            )
        (denied / "tungsten.yml").write_text(
            json.dumps(config, indent=2), encoding="utf-8"
        )
        cmd(
            "policy-refuses-" + target,
            [binary, "generate", denied, "--json"],
            expect=1,
            contains="TG0702",
        )
        if (denied / "generated" / target).is_dir():
            raise AssertionError("Rejected imported target produced output")
    # Fresh native generation and real dependency resolution in isolation make
    # the package independent of all previous recordings and preflight folders.
    if source_repo is None:
        source_repo = WORK / "cache/zrotext-reference"
    source_repo = Path(source_repo).resolve()
    native = scratch / "native"
    cmd(
        "native-init",
        [
            binary,
            "init",
            "--from",
            source_repo / "protocol/v1/openapi/public-v1.json",
            "--dir",
            native,
            "--targets",
            "typescript",
            "--no-interactive",
        ],
    )
    from demo import configure

    settings_file = scratch / "settings.json"
    settings_file.write_text(json.dumps(settings), encoding="utf-8")
    configure(source_repo, native, settings_file)
    cmd("native-generate-all", [binary, "generate", native])
    cmd("native-freshness", [binary, "generate", native, "--check"])
    cache = WORK / "cache/npm"
    cache.mkdir(exist_ok=True)
    locks = WORK / "cache/locks"
    locks.mkdir(exist_ok=True)
    for target in ["typescript", "mcp"]:
        folder = native / "generated" / target
        npm = [
            "npm.cmd",
            "--prefix",
            folder,
            "install",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--cache",
            cache,
        ]
        if not allow_registry:
            npm.append("--offline")
        cmd(target + "-install-public-generated-package", npm)
        cmd(target + "-build", ["npm.cmd", "--prefix", folder, "run", "build"])
        lock = folder / "package-lock.json"
        shutil.copy2(lock, locks / f"{target}.package-lock.json")
    cmd(
        "native-go-build",
        [settings["go"], "-C", native / "generated/go", "build", "./..."],
    )
    (scratch / "results.json").write_text(
        json.dumps(results, indent=2), encoding="utf-8"
    )
    summary = dict(
        probes=len(results),
        all_expected_results=True,
        positive_formats=[k for k, _ in variants],
        imported_policy_refusal=dict(
            code="TG0702",
            targets=["java", "csharp", "ruby", "go"],
            exit_code=1,
            output_withheld=True,
        ),
        native_generated_targets=[
            "typescript",
            "python",
            "rust",
            "java",
            "csharp",
            "ruby",
            "go",
            "mcp",
            "webmcp",
            "docs",
        ],
        native_build_checks=["typescript", "mcp", "go"],
        results_path=str(scratch / "results.json"),
    )
    (WORK / "cache/preflight-latest.json").write_text(
        json.dumps(summary, indent=2), encoding="utf-8"
    )
    return summary
