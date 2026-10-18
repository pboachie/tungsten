# SPDX-License-Identifier: AGPL-3.0-only
"""Pin a published release, inspect its exact source, and fail closed on drift."""
from pathlib import Path
import datetime
import hashlib
import json
import re
import shutil
import subprocess
import urllib.request
import zipfile

from paths import ROOT, WORK, ensure_work

SDK_NAMES = {
    "typescript": "TypeScript",
    "python": "Python",
    "rust": "Rust",
    "java": "Java",
    "csharp": "C#",
    "ruby": "Ruby",
    "kotlin": "Kotlin",
    "fsharp": "F#",
    "dart": "Dart",
}


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def get_json(url):
    request = urllib.request.Request(
        url,
        headers={
            "User-Agent": "Tungsten-Release-Video/1.0",
            "Accept": "application/vnd.github+json",
        },
    )
    with urllib.request.urlopen(request, timeout=45) as response:
        return json.load(response)


def shell(args, **kwargs):
    return subprocess.check_output(args, text=True, encoding="utf-8", **kwargs).strip()


def prepare(version, profile_path=None):
    ensure_work()
    if not re.fullmatch(r"v\d+\.\d+\.\d+", version):
        raise ValueError("Use an exact stable release tag vMAJOR.MINOR.PATCH")
    cache = WORK / "cache" / version
    cache.mkdir(parents=True, exist_ok=True)
    release_file = WORK / "cache" / f"release-{version}.json"
    if release_file.is_file():
        release = json.loads(release_file.read_text(encoding="utf-8"))
    else:
        release = get_json(
            f"https://api.github.com/repos/pboachie/tungsten/releases/tags/{version}"
        )
        release_file.write_text(json.dumps(release, indent=2), encoding="utf-8")
    if (
        release.get("draft")
        or release.get("prerelease")
        or not release.get("published_at")
    ):
        raise ValueError("A published stable release is required")
    if release["tag_name"] != version:
        raise ValueError("Release identity mismatch")
    asset = next(
        a
        for a in release["assets"]
        if a["name"] == f"tungsten-{version[1:]}-x86_64-pc-windows-msvc.zip"
    )
    archive = cache / "compiler.zip"
    if not archive.is_file():
        urllib.request.urlretrieve(asset["browser_download_url"], archive)
    expected = asset.get("digest", "").removeprefix("sha256:")
    if not expected:
        raise ValueError(
            "Release asset has no digest; obtain/review SHA256SUMS before continuing"
        )
    if sha(archive) != expected:
        raise ValueError("Published archive checksum mismatch")
    binary = cache / "tungsten.exe"
    with zipfile.ZipFile(archive) as z:
        names = [
            n
            for n in z.namelist()
            if n.endswith("/tungsten.exe") or n == "tungsten.exe"
        ]
        if len(names) != 1:
            raise ValueError("Ambiguous binary in release archive")
        binary.write_bytes(z.read(names[0]))
    binary_version = shell([str(binary), "--version"])
    if binary_version != "tungsten " + version[1:]:
        raise ValueError("Binary version disagrees with release tag")
    source = cache / "source"
    if not source.is_dir():
        subprocess.run(
            [
                "git",
                "clone",
                "--depth",
                "1",
                "--branch",
                version,
                "https://github.com/pboachie/tungsten.git",
                str(source),
            ],
            check=True,
        )
    commit = shell(["git", "-C", str(source), "rev-parse", "HEAD"])
    tag = shell(["git", "-C", str(source), "rev-parse", version + "^{}"])
    if commit != tag:
        raise ValueError("Source checkout is not the exact requested tag")
    if shell(["git", "-C", str(source), "status", "--porcelain"]):
        raise ValueError("Audited source checkout has local changes")
    registry = (source / "crates/tungsten-cli/src/targets.rs").read_text(
        encoding="utf-8"
    )
    builtins = re.findall(r'^\s*"([a-z]+)"\s*=>\s*Some\(&', registry, re.M)
    native = [v for v in builtins if v in SDK_NAMES]
    profile = Path(profile_path) if profile_path else ROOT / "profile.json"
    settings = json.loads(profile.read_text(encoding="utf-8"))
    if settings["release"] != version:
        template = {
            **settings,
            "release": version,
            "source_commit": commit,
            "archive_sha256": sha(archive),
            "binary_sha256": sha(binary),
            "observed_builtin_targets": builtins,
            "review_required": "Review source dispatch, target policy, runtime versions and probes. Do not reuse the old profile blindly.",
        }
        (cache / "profile-needs-review.json").write_text(
            json.dumps(template, indent=2), encoding="utf-8"
        )
        raise ValueError(
            "New release requires a reviewed version profile; candidate template written under cache"
        )
    for field, value in [
        ("source_commit", commit),
        ("archive_sha256", sha(archive)),
        ("binary_sha256", sha(binary)),
    ]:
        if settings[field] != value:
            raise ValueError("Profile provenance mismatch: " + field)
    expected_native = set(settings["sdk_targets"]) - {"go"}
    if set(native) != expected_native:
        raise ValueError("SDK emitter inventory changed; review profile")
    dispatch = (source / "crates/tungsten-cli/src/commands/init_input.rs").read_text(
        encoding="utf-8"
    )
    for token in [
        "is_curl",
        "is_http",
        "import_har",
        "import_collection",
        "endpoint_manifest",
        "swagger::convert",
    ]:
        if token not in dispatch:
            raise ValueError("Missing audited input route: " + token)
    policy = (source / "crates/tungsten-cli/src/commands/generate.rs").read_text(
        encoding="utf-8"
    )
    if "TG0702" not in policy or "unsupported_import_policy" not in policy:
        raise ValueError("Imported target policy changed")
    evidence = []
    for file in [
        "crates/tungsten-cli/src/targets.rs",
        "crates/tungsten-cli/src/commands/init_input.rs",
        "crates/tungsten-cli/src/commands/generate.rs",
        "crates/tungsten-cli/src/args.rs",
        "emitters/go/main.go",
        "README.md",
    ]:
        path = source / file
        evidence.append(
            {
                "file": file,
                "sha256": sha(path),
                "url": f"https://github.com/pboachie/tungsten/blob/{version}/{file}",
            }
        )
    go_emitter = WORK / "tools/bin" / f"tungsten-emit-go-{version}.exe"
    go_emitter.parent.mkdir(parents=True, exist_ok=True)
    go = WORK / "tools/go-sdk/go/bin/go.exe"
    if not go.is_file():
        raise ValueError(
            "Portable Go toolchain missing; run bootstrap or supply the reviewed official toolchain"
        )
    receipt = go_emitter.with_suffix(".build.json")
    if go_emitter.is_file():
        if not receipt.is_file():
            raise ValueError(
                "External Go emitter has no build receipt; review the cache before reuse"
            )
        recorded = json.loads(receipt.read_text(encoding="utf-8"))
        if recorded["source_commit"] != commit or recorded["sha256"] != sha(go_emitter):
            raise ValueError(
                "External Go emitter differs from its audited build receipt"
            )
    if not go_emitter.is_file():
        import os

        env = dict(
            os.environ,
            GOCACHE=str(WORK / "cache/go-build"),
            GOPATH=str(WORK / "cache/go-work"),
        )
        subprocess.run(
            [
                str(go),
                "-C",
                str(source / "emitters/go"),
                "build",
                "-trimpath",
                "-o",
                str(go_emitter),
                ".",
            ],
            env=env,
            check=True,
        )
        receipt.write_text(
            json.dumps(
                dict(
                    source_commit=commit,
                    sha256=sha(go_emitter),
                    go_version=shell([str(go), "version"]),
                ),
                indent=2,
            ),
            encoding="utf-8",
        )
    descriptor = json.loads(shell([str(go_emitter), "--describe"]))
    if descriptor.get("name") != "go" or descriptor.get("protocol") != 1:
        raise ValueError("Unexpected external Go emitter protocol")
    inventory = {
        "version": version,
        "published_at": release["published_at"],
        "source_commit": commit,
        "binary_version": binary_version,
        "archive_sha256": sha(archive),
        "binary_sha256": sha(binary),
        "sdk_languages": settings["sdk_targets"],
        "other_outputs": settings["other_targets"],
        "inputs": settings["inputs"],
        "imported_targets": settings["imported_targets"],
        "native_git_input": settings["native_git_input"],
        "native_http_input": settings["native_http_input"],
        "remote_references": settings["remote_references"],
        "unreleased_in_this_version": settings["unreleased_in_this_version"],
        "go_emitter": descriptor,
        "go_emitter_sha256": sha(go_emitter),
        "evidence": evidence,
        "profile_sha256": sha(profile),
        "audited_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "publication": "HUMAN_REVIEW_REQUIRED",
    }
    inventory["public_fixture"] = settings["public_fixture"]
    (cache / "capabilities.json").write_text(
        json.dumps(inventory, indent=2), encoding="utf-8"
    )
    paths = {
        "version": version,
        "binary": str(binary.resolve()),
        "source": str(source.resolve()),
        "go": str(go.resolve()),
        "go_emitter": str(go_emitter.resolve()),
        "runtime": settings["npm_runtime"],
        "inventory": str((cache / "capabilities.json").resolve()),
        "profile": str(profile.resolve()),
        "public_fixture": settings["public_fixture"],
    }
    (cache / "settings.json").write_text(json.dumps(paths, indent=2), encoding="utf-8")
    return paths, inventory


if __name__ == "__main__":
    import sys

    paths, inventory = prepare(sys.argv[1])
    print(json.dumps(inventory, indent=2))
