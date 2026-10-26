#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Install the Python runtime archives and freshly generated SDKs into clean
environments on each given interpreter, then compile, import, type-check and
call a local mock. Nothing is published; no editable or checkout imports.

Usage: check-python-consumers.py --tungsten BIN --pyright BIN --work DIR
       [--minimum-deps PYTHON] PYTHON...

Two SDKs are generated per variant: the preview-confirmation example, called
through `tungsten mock`, and the behaviour scenario in
python_consumer_behaviour.py, called through its loopback server.

Each PYTHON is an interpreter path. Every interpreter checks the 3.10
compatibility output; interpreters at 3.12 or newer also check the default
output. `--minimum-deps` names the interpreter that additionally installs the
lowest declared dependency versions.
"""
from __future__ import annotations

import argparse
import contextlib
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import python_consumer_behaviour as behaviour

REPO = Path(__file__).resolve().parents[2]
EXAMPLE = REPO / "examples" / "preview-confirmation"
BOOTSTRAP = REPO / "crates" / "tungsten-cli" / "src" / "commands" / "bootstrap_python.py"
MODULE = "petstore_sdk"
# Lower bounds of the runtime and generated-SDK dependency ranges. Pydantic's
# own floor raises typing_extensions, so that pin applies to dataclasses only.
MINIMUM = {
    "certifi": "2026.10.19",
    "httpx": "0.27.0",
    "sniffio": "1.3.0",
    "typing_extensions": "4.6.0",
    "async-timeout": "4.0.3",
    "pydantic": "2.10.1",
}

CONSUMER = '''from __future__ import annotations

import asyncio
import sys

from petstore_sdk import AsyncPetstoreClient, PetstoreClient
from tungsten_runtime import Err


def sync_calls(base: str) -> None:
    with PetstoreClient(base_url=base, validate_responses="strict") as client:
        listed = client.pets.list()
        if isinstance(listed, Err):
            raise RuntimeError(str(listed.error))
        assert len(listed.value) > 0
        rejected = client.pets.delete(pet_id="pet-1")
        assert isinstance(rejected, Err) and rejected.error["category"] == "CONFIRMATION_REQUIRED"
        preview = client.pets.preview_delete(pet_id="pet-1")
        if isinstance(preview, Err):
            raise RuntimeError(str(preview.error))
        token = preview.value["confirmation_token"]
        assert isinstance(token, str) and token
        deleted = client.pets.delete(pet_id="pet-1", opts={"confirm": token})
        assert not isinstance(deleted, Err), deleted


async def async_calls(base: str) -> None:
    async with AsyncPetstoreClient(base_url=base, validate_responses="strict") as client:
        listed = await client.pets.list()
        if isinstance(listed, Err):
            raise RuntimeError(str(listed.error))
        assert len(listed.value) > 0


def main() -> None:
    base = sys.argv[1]
    sync_calls(base)
    asyncio.run(async_calls(base))
    print("sync and async calls returned typed results")


if __name__ == "__main__":
    main()
'''


def run(args: list[str], cwd: Path, **kwargs: object) -> subprocess.CompletedProcess[str]:
    print("+", " ".join(args), flush=True)
    return subprocess.run(args, cwd=cwd, check=True, text=True, **kwargs)  # type: ignore[call-overload]


def interpreter_version(python: str) -> tuple[int, int]:
    out = subprocess.run([python, "-I", "-c", "import sys; print(sys.version_info[0], sys.version_info[1])"],
                         check=True, capture_output=True, text=True).stdout.split()
    return int(out[0]), int(out[1])


def venv_python(venv: Path) -> Path:
    return venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")


def generate(binary: str, work: Path, scenario: str, models: str, python_version: str) -> Path:
    project = work / f"project-{scenario}-{models}-{python_version}"
    project.mkdir(parents=True)
    options = {"out": "sdk", "python_version": python_version}
    if models != "pydantic":
        options["models"] = models
    if scenario == "petstore":
        shutil.copyfile(EXAMPLE / "openapi.json", project / "openapi.json")
        shutil.copyfile(EXAMPLE / "agent.yml", project / "agent.yml")
        api, namespace = {"name": "petstore", "title": "Gallery Petstore"}, "pets"
    else:
        (project / "openapi.json").write_text(json.dumps(behaviour.SPEC, indent=2) + "\n")
        api, namespace = {"name": "qualification", "title": "Qualification API"}, "items"
    manifest = {"tungsten": 1, "api": api,
                "inputs": [{"namespace": namespace, "spec": "openapi.json"}],
                "servers": {"default": "http://127.0.0.1:4010", "allow_override": True},
                "targets": {"python": options}}
    (project / "tungsten.yml").write_text(json.dumps(manifest, indent=2) + "\n")
    run([binary, "check"], project)
    run([binary, "generate"], project)
    return project


def build_archives(python: str, source: Path, out: Path, sdist: bool) -> list[Path]:
    out.mkdir(parents=True)
    run([python, "-m", "build", "--wheel", *(["--sdist"] if sdist else []), "--outdir", str(out), str(source)],
        source)
    return sorted(out.iterdir())


@contextlib.contextmanager
def mock(binary: str, project: Path):
    with (project / "mock.log").open("w+") as log:
        process = subprocess.Popen([binary, "mock", "tungsten.yml", "--port", "0", "--json"],
                                   cwd=project, stdin=subprocess.PIPE, stdout=log, stderr=log, text=True)
        try:
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                log.seek(0)
                for line in log.read().splitlines():
                    if line.startswith('{"tool"'):
                        yield json.loads(line)["result"]["base_url"]
                        return
                if process.poll() is not None:
                    raise RuntimeError("mock exited before listening")
                time.sleep(0.05)
            raise RuntimeError("mock did not listen within 20 seconds")
        finally:
            if process.stdin:
                process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)


def consume(binary: str, pyright: str, python: str, version: tuple[int, int], runtime: Path,
            sdks: dict[str, tuple[Path, Path]], scratch: Path, models: str, minimum: bool) -> None:
    project = sdks["petstore"][0]
    venv = scratch / "venv"
    run([python, "-m", "venv", str(venv)], scratch)
    py = str(venv_python(venv))
    run([py, "-I", str(BOOTSTRAP)], scratch)
    install = [py, "-m", "pip", "install", "--disable-pip-version-check"]
    if minimum:
        constraints = scratch / "minimum.txt"
        skip = {"typing_extensions"} if models == "pydantic" else {"pydantic"}
        constraints.write_text("".join(f"{name}=={pin}\n" for name, pin in MINIMUM.items() if name not in skip))
        install += ["--constraint", str(constraints)]
    run([*install, str(runtime), *(str(wheel) for _, wheel in sdks.values())], scratch)
    run([py, "-m", "pip", "freeze", "--all"], scratch)
    # Outside the checkout and isolated: imports must resolve to site-packages.
    site = run([py, "-I", "-c", (
        f"import sys, pathlib, tungsten_runtime, petstore_sdk, {behaviour.MODULE}\n"
        f"for m in (tungsten_runtime, petstore_sdk, {behaviour.MODULE}):\n"
        "    assert 'site-packages' in m.__file__, m.__file__\n"
        "print(sys.version, file=sys.stderr)\n"
        "print(pathlib.Path(petstore_sdk.__file__).parents[1])"
    )], scratch, capture_output=True).stdout.strip()
    run([py, "-I", "-m", "compileall", "-q",
         *(str(Path(site) / name) for name in ("tungsten_runtime", MODULE, behaviour.MODULE))], scratch)
    consumer = scratch / "consumer.py"
    consumer.write_text(CONSUMER)
    behaviour_consumer = scratch / "behaviour_consumer.py"
    behaviour_consumer.write_text(behaviour.CONSUMER)
    (scratch / "pyrightconfig.json").write_text(json.dumps({
        "include": ["consumer.py", "behaviour_consumer.py"], "typeCheckingMode": "strict",
        "pythonVersion": f"{version[0]}.{version[1]}",
    }) + "\n")
    run([pyright, "--pythonpath", py, "-p", str(scratch / "pyrightconfig.json")], scratch)
    with mock(binary, project) as base:
        run([py, "-I", str(consumer), base], scratch)
        with urllib.request.urlopen(base + "/__tungsten/calls", timeout=10) as response:
            calls = json.load(response)
    deletes = [call for call in calls if call["method"] == "DELETE"]
    assert len(deletes) == 1, f"expected exactly the confirmed DELETE, saw {len(deletes)}"
    with behaviour.serve() as (base, recorder):
        run([py, "-I", str(behaviour_consumer), base], scratch)
    behaviour.check(recorder)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tungsten", required=True)
    parser.add_argument("--pyright", required=True)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--minimum-deps")
    parser.add_argument("pythons", nargs="+")
    args = parser.parse_args()
    work: Path = args.work.resolve()
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    builder = args.pythons[-1]
    runtime = build_archives(builder, REPO / "runtimes" / "python", work / "runtime-dist", sdist=True)
    projects: dict[tuple[str, str], dict[str, tuple[Path, Path]]] = {}
    for models in ("pydantic", "dataclasses"):
        for python_version in ("3.10", "3.12"):
            sdks = projects.setdefault((models, python_version), {})
            for scenario in ("petstore", "behaviour"):
                project = generate(args.tungsten, work, scenario, models, python_version)
                wheel = build_archives(builder, project / "sdk", project / "dist", sdist=False)[0]
                sdks[scenario] = project, wheel
    failures: list[str] = []
    for python in args.pythons:
        version = interpreter_version(python)
        for (models, python_version), sdks in projects.items():
            if python_version == "3.12" and version < (3, 12):
                continue
            # The wheel on every lane; the sdist and minimum dependencies on the floor lane.
            archives = [a for a in runtime if a.suffix == ".whl"]
            if python == args.minimum_deps:
                archives = runtime
            for archive in archives:
                for minimum in sorted({False, python == args.minimum_deps}):
                    label = (f"python {version[0]}.{version[1]} / {models} / output {python_version} / "
                             f"{archive.name}{' / minimum deps' if minimum else ''}")
                    print(f"::group::{label}", flush=True)
                    scratch = work / "consumers" / f"{len(failures)}-{time.monotonic_ns()}"
                    scratch.mkdir(parents=True)
                    try:
                        consume(args.tungsten, args.pyright, python, version, archive, sdks,
                                scratch, models, minimum)
                    except (subprocess.CalledProcessError, AssertionError, RuntimeError) as error:
                        print(f"FAILED {label}: {error}", flush=True)
                        failures.append(label)
                    print("::endgroup::", flush=True)
    if failures:
        print("failed consumer lanes:\n  " + "\n  ".join(failures), file=sys.stderr)
        sys.exit(1)
    print("all consumer lanes passed")


if __name__ == "__main__":
    main()
