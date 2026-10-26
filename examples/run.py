#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Run the committed gallery recipes against matching local runtime source."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
CATALOG = json.loads((HERE / "gallery.json").read_text())


TUTORIALS = {
    "mock-scenarios": """
## Saved scenarios for retry behavior

This recipe compiles the committed `openapi.json` directly; it does not write a
project manifest or generate an SDK. Each file in `scenarios/` is version 1 JSON:
responses queued on the existing mock controls, plus the calls and effects the
integration should cause. Files are data only. The integration is the separate,
stdlib-only `integration.py`, selected explicitly after `--`:

```sh
tungsten scenario validate openapi.json --scenario scenarios/lost-write.json
tungsten scenario run openapi.json --scenario scenarios/lost-write.json \\
  --timeout-ms 30000 -- python3 -I integration.py lost-write
```

The integration reads its fresh loopback URL from `TUNGSTEN_MOCK_BASE_URL`. Each
run binds its own port and mock state, so runs never share responses or counts.

| Scenario | Correct integration | `--broken` variant that must fail |
|---|---|---|
| `rate-limit.json` | retries a 429 after `Retry-After` | gives up after the 429 |
| `stream-interrupted.json` | resumes with `Last-Event-ID` after a cut stream | stops at the cut |
| `malformed.json` | reports an invalid JSON body once (`--expect-exit 3`) | retries the same request |
| `lost-write.json` | retries a dropped write with the same `Idempotency-Key` | uses a new key for the retry |

The runner validates all four files, runs each correct integration, and
requires each broken variant to fail. The lost-write broken variant matches
every call count; only `expect.effects` (`applied` 2, `replayed` 0) rejects it.
`scenario run` is currently Linux-only, and this recipe fails explicitly on
other platforms after validation. Assertions describe the mock's observations
and its own processing, not a real provider's behavior.
""",
    "input-postman": """
## Problem: a collection is not yet a client contract

You have saved requests, but want a typed client without replaying those requests
or running collection scripts. Start with the small committed `collection.json`:
it records one `GET /pets` and a saved JSON response. Its `example.invalid` URL is
a placeholder, not a service to call. The sample response is evidence of one
shape, not a promise that every response has that shape.

### Select the matching compiler in this shell

The snippets below use POSIX `sh` syntax. Replace the placeholder with the
absolute path to the compiler built for this matching checkout, then export it:

```sh
export TUNGSTEN="/absolute/path/to/matching/tungsten"
```

Use that same shell for the following commands, including the canonical runner
command above. `TUNGSTEN` selects the runner's compiler; it does not change shell
`PATH`. The standalone commands below invoke `"$TUNGSTEN"` explicitly. For a
Windows compiler used from a POSIX shell, select its absolute executable path
including `.exe`. These snippets are not PowerShell syntax or a claim of native
platform qualification.

### 1. Import offline and inspect the result

Use the canonical command above from the repository root. The recipe initializes
the **docs target**, checks the project, generates twice, compares the generated
documentation and checks freshness. It does not build or invoke a Python SDK.
Expected results here describe those source-defined checks, not a captured run.

Open `examples/input-postman/.work/project/` and review:

| File | What to check |
|---|---|
| `imported.openapi.json` | Accepted operations, parameter and response shapes, and imported-operation annotations. |
| `import-report.json` | Omitted items, skipped test scripts and unknown contract facts. An omitted request is not a supported endpoint. |
| `tungsten.yml` | Input reference, selected targets and generated output paths. |
| `agent.yml` | Authentication-related policy, safety and allowed origins before any live use. Importing cannot approve its own safety or origins. |

The importer is structural: it never executes requests, pre-request scripts,
tests or dynamic variables. Unsupported items such as script-dependent requests,
non-JSON bodies/responses and unsupported authentication helpers can be omitted;
structural or limit errors can refuse the import. Read the report even when
initialization succeeds. See [supported inputs](../../README.md#supported-inputs)
for the conversion limits and supported targets.

### 2. Review the contract before choosing a client

Compare the normalized contract with the API owner's documentation. Supply the
missing success and error responses, required and optional fields, nullability,
pagination, authentication and authorization requirements. Confirm the origin
and operation safety policy explicitly. A saved `200` example cannot establish
these facts, and generation cannot supply server-side authorization.

Keep reviewed work outside the recipe's disposable `.work/` directory. For a
separate Python project, this is the source-supported initialization route from
the repository root; choose a fresh destination:

```sh
"$TUNGSTEN" init --from examples/input-postman/collection.json --dir postman-python --targets python --python-runtime runtimes/python --no-interactive
```

Review the new project's normalized input, report and manifests as above. Then:

```sh
"$TUNGSTEN" check postman-python
"$TUNGSTEN" generate postman-python
```

The generated package is under `postman-python/generated/python`. Follow its
installation guidance and the [Python runtime source guide](../../runtimes/python/README.md)
with the matching checkout. Inspect the generated names and methods; the
`PetstoreClient` used by another gallery fixture is not a promised name for an
arbitrary imported collection. These commands do not demonstrate a successful
Postman-derived client call.

### 3. Learn the call boundary with the existing local mock

For a separate, declared OpenAPI fixture and a strict typed loopback call, use:

```sh
python3 examples/run.py --only local-mock
```

That [recipe](../local-mock/README.md) installs its generated Python client and
matching runtime, starts a loopback mock, and checks for a nonempty typed Pet
list. Its expected client message is `mock returned typed pets`; this is the
message in the recipe source, not a transcript supplied here. It uses its own
contract, not the Postman conversion above. The [first Python package](../first-python/README.md)
recipe isolates installation/import; [preview and confirmation](../preview-confirmation/README.md)
adds a separate destructive-call example. A mock call does not validate a real
API, its credentials or production readiness.

### Prerequisites, recovery and limits

Use a compiler and runtime source from the same checkout, not an unrelated
binary found on PATH. The runner accepts an absolute compiler path through
`TUNGSTEN`; building that compiler uses the repository's `rust-toolchain.toml`.
Python 3.12+ is needed for this runner; the local-call step also needs venv/pip
and may download dependencies. See the [target matrix](../../README.md#targets)
for client-specific requirements and acceptance limits.

- **Missing compiler:** select the matching compiler through `TUNGSTEN`, or build
  it as described in the [gallery prerequisites](../README.md). Installing the
  Python runtime alone does not install the compiler.
- **Omitted or refused input:** inspect the diagnostic and import report; replace
  script-dependent behavior with an explicitly reviewed contract. Do not treat
  fewer imported operations as a complete conversion.
- **Existing destination or interrupted setup:** inspect partial files and keep
  reviewed changes; use a fresh directory rather than forcing an overwrite.
- **Unexpected client failure:** inspect the returned error and contract first.
  Do not retry a mutation merely because its outcome is unknown.

All commands are terminal/text-first and require no screenshot or recording.
`--no-interactive` avoids setup prompts in the separate-project command. Before
rerunning a recipe, save anything needed from `.work/`: the runner replaces it.
If interrupted, inspect the command's processes and partial artifacts before
starting another run; this guide does not promise cross-platform cancellation
or recovery qualification. No real API, proxy/CA setup or credential flow is
exercised by the offline import.
""",
    "ci-drift": """
## Problem: the contract changed but generated output did not

You want a review or CI gate to catch a forgotten regeneration, rather than
discovering stale methods in a consumer. This recipe uses a committed OpenAPI
contract and generated **docs output** to make that failure and repair small.
Expected results below describe the recipe's checks, not a recorded CI run.

### Select the matching compiler in this shell

The snippets below use POSIX `sh` syntax. Replace the placeholder with the
absolute path to the compiler built for this matching checkout, then export it:

```sh
export TUNGSTEN="/absolute/path/to/matching/tungsten"
```

Use that same shell for the following commands, including the canonical runner
command above. `TUNGSTEN` selects the runner's compiler; it does not change shell
`PATH`. The standalone commands below invoke `"$TUNGSTEN"` explicitly. For a
Windows compiler used from a POSIX shell, select its absolute executable path
including `.exe`. These snippets are not PowerShell syntax or a claim of native
platform qualification.

### 1. Follow the stale-output cycle

Run the canonical command above from the repository root. In
`examples/ci-drift/.work/`, the runner:

1. Copies `openapi.json`, writes a docs-target manifest and validates it.
2. Generates twice, compares file hashes and checks that output is fresh.
3. Adds `GET /status` with operation ID `getStatus` and a `204` response to the
   **copied** contract. The committed input remains unchanged.
4. Requires `generate --check` to exit `1` with diagnostic `TG0901`.
5. Regenerates and requires the same freshness check to succeed.

The runner handles the deliberately failing middle command. A nonzero exit of
the whole recipe is a failure to investigate, not the expected stale stage.
Generated `sdk/llms.txt` and `sdk/tools.json` are part of this docs example;
the directory name `sdk` does not make this an SDK build or an API call.

After the recipe completes, inspect the final copied contract and output. For
your own initialized project, the corresponding repair commands are:

```sh
"$TUNGSTEN" check path/to/project
"$TUNGSTEN" generate --check path/to/project
```

If the second command reports `TG0901`, review the contract change, then run:

```sh
"$TUNGSTEN" generate path/to/project
"$TUNGSTEN" generate --check path/to/project
```

Replace `path/to/project` with the directory containing your `tungsten.yml`.
Review and commit the intended generated diff with the contract change. Do not
remove generated files from the freshness check just to make it pass. See
[surface diff](../surface-diff/README.md) for the existing semver/migration
review recipe; a fresh tree alone does not establish backward compatibility.

### 2. Compare SDK, CLI and MCP generation honestly

The committed contracts in `first-python`, `first-rust`, `first-mcp` and this
directory have identical bytes. You can use the existing recipes to inspect
how those target configurations differ:

```sh
python3 examples/run.py --only first-python,first-rust,first-mcp
```

| Recipe | What its source checks |
|---|---|
| [Python SDK](../first-python/README.md) | Installs the generated package with matching Python runtime source and imports `PetstoreClient`. |
| [Rust SDK and CLI](../first-rust/README.md) | Builds the generated workspace and runs the generated CLI's `--help`. |
| [MCP server](../first-mcp/README.md) | Builds the matching TypeScript runtime, adapter, generated SDK and server package. |

These are separate projects and `.work/` directories, not one shared manifest
or one run of the changed contract through every consumer. CLI help is not an
API operation, and an MCP package build is not a tool invocation. The docs-only
staleness cycle above does not prove SDK/CLI/MCP behavioral parity. For an
actual loopback client step defined in source, use the separate
[local-mock recipe](../local-mock/README.md); it does not call a real API.

Use Python 3.12+ with venv/pip, the repository's pinned Rust toolchain with
Cargo, and Node with npm. For MCP, Node 22 requires **22.13 or newer**, and
Node 23 requires **23.2 or newer**; consult the
[MCP runtime guide](../../runtimes/mcp/README.md) and
[target matrix](../../README.md#targets) for the precise version ranges.
The runner uses matching runtime source and can download package dependencies.
`--generate-only` omits these package builds; it is not equivalent validation.

### 3. Put the same freshness rule in your repository

The [copyable GitHub Actions workflow](../github-actions/README.md) validates
`TUNGSTEN_PROJECT` and runs `tungsten generate --check` without regenerating
files in CI. Follow that guide's copy commands and upgrade instructions. It
pins the compiler release, installer and checkout action; upgrade them together
deliberately. Its current platform is Ubuntu 24.04 x86_64, not a promise for all
hosts. It disables external emitter commands for pull-request safety, so read
its trust and skipped-target limitations before adopting it. No hosted run or
successful installation is demonstrated by this page.

### Troubleshooting and safe reruns

- **`TG0901` in your project:** inspect missing, changed or extra generated files,
  regenerate with the matching compiler, review the diff and rerun the check.
- **Different results across machines:** compare compiler/runtime source and
  toolchain versions before accepting a new output baseline. See the
  [gallery prerequisites](../README.md) for compiler selection through `TUNGSTEN`.
- **Missing package toolchain:** install the prerequisites for the selected
  recipe. Choosing `--generate-only` changes the evidence; it does not turn a
  missing build or invocation into a pass.
- **Interrupted run or local edits:** preserve useful artifacts before rerunning.
  Each recipe replaces its own `.work/`; do not keep hand-maintained code there.
  Inspect any remaining child processes before another run. No universal
  cancellation or platform-recovery result is claimed here.

The steps are text-first and use normal terminal commands. Generation freshness,
package builds, installed-consumer behavior, live API acceptance and production
readiness are separate checks; keep the [target limits](../../README.md#targets)
visible when sharing a result.
""",
}


def command(argv: list[str], cwd: Path, expected: int = 0) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(argv, cwd=cwd, text=True, capture_output=True, timeout=900)
    if result.returncode != expected:
        raise RuntimeError(f"{argv!r} exited {result.returncode}, expected {expected}\n{result.stdout}\n{result.stderr}")
    return result


def compiler() -> str:
    value = os.environ.get("TUNGSTEN") or shutil.which("tungsten")
    if not value:
        local = REPO / "target/debug" / ("tungsten.exe" if os.name == "nt" else "tungsten")
        if local.is_file():
            value = str(local)
    if not value:
        raise RuntimeError("Set TUNGSTEN to the matching compiler, or run cargo build -p tungsten-cli")
    return str(Path(value).resolve())


def tungsten(binary: str, cwd: Path, *args: str, expected: int = 0) -> dict:
    return json.loads(command([binary, *args, "--json"], cwd, expected).stdout)


def relative(path: Path, output: Path) -> str:
    return Path(os.path.relpath(path, output)).as_posix()


def fresh_work(example: Path) -> Path:
    work = example / ".work"
    # Verify the absolute target before replacing generated artifacts.
    if work.is_symlink() or work.resolve() != example.resolve() / ".work":
        raise RuntimeError(f"refusing redirected work directory: {work}")
    if work.exists():
        shutil.rmtree(work)
    work.mkdir()
    return work


def target_options(target: str, work: Path) -> dict:
    out = work / "sdk"
    runtime = REPO / "runtimes"
    options: dict = {"out": "sdk"}
    if target == "typescript":
        options.update(package="@gallery/petstore", runtime_path=relative(runtime / "ts", out))
    elif target == "rust":
        options.update(runtime_path=relative(runtime / "rust/runtime", out / "petstore-sdk"),
                       cli={"kit_path": relative(runtime / "rust/cli-kit", out / "petstore-cli")})
    elif target in {"java", "csharp", "ruby"}:
        options["runtime_path"] = relative(runtime / target, out)
    elif target == "go":
        emitter = HERE / ".cache" / ("tungsten-emit-go.exe" if os.name == "nt" else "tungsten-emit-go")
        emitter.parent.mkdir(exist_ok=True)
        command(["go", "build", "-o", str(emitter), "."], REPO / "emitters/go")
        options.update(external=f'"{emitter}"', module="example.com/gallery/petstore",
                       package="petstore", runtime_path=relative(runtime / "go", out))
    elif target in {"mcp", "webmcp"}:
        options.update(package=f"@gallery/petstore-{target}", sdk="@gallery/petstore",
                       sdk_path="../typescript", runtime_path=relative(runtime / target, out),
                       sdk_runtime_path=relative(runtime / "ts", out))
    return options


def manifest(target: str, work: Path) -> dict:
    targets = {target: target_options(target, work)}
    if target in {"mcp", "webmcp"}:
        ts = target_options("typescript", work)
        ts.update(out="typescript", runtime_path=relative(REPO / "runtimes/ts", work / "typescript"))
        targets = {"typescript": ts, **targets}
    return {"tungsten": 1, "api": {"name": "petstore", "title": "Gallery Petstore"},
            "inputs": [{"namespace": "pets", "spec": "openapi.json"}],
            "servers": {"default": "http://127.0.0.1:4010", "allow_override": True},
            "targets": targets}


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n")


def python_environment(work: Path) -> Path:
    venv = work / "venv"
    python = venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    uv = shutil.which("uv")
    bootstrap = REPO / "crates/tungsten-cli/src/commands/bootstrap_python.py"
    if uv:
        command([sys.executable, "-I", str(bootstrap), "--check-uv", uv], work)
        command([uv, "venv", "--python", sys.executable, str(venv)], work)
        install = [uv, "pip", "install", "--python", str(python)]
    else:
        command([sys.executable, "-m", "venv", str(venv)], work)
        command([str(python), "-I", str(bootstrap)], work)
        install = [str(python), "-m", "pip", "install", "--disable-pip-version-check"]
    command([*install, "-e", str(REPO / "runtimes/python"), "-e", str(work / "sdk")], work)
    return python


def npm_build(path: Path) -> None:
    action = "ci" if (path / "package-lock.json").is_file() else "install"
    command(["npm", action, "--ignore-scripts", "--no-audit", "--no-fund"], path)
    command(["npm", "run", "build"], path)


def build(target: str, work: Path) -> None:
    sdk = work / "sdk"
    if target == "python":
        python = python_environment(work)
        command([str(python), "-c", "from petstore_sdk import PetstoreClient; print(PetstoreClient.__name__)"], work)
    elif target in {"typescript", "mcp", "webmcp"}:
        npm_build(REPO / "runtimes/ts")
        if target != "typescript":
            npm_build(REPO / "runtimes" / target)
            npm_build(work / "typescript")
        npm_build(sdk)
        if target == "typescript":
            command(["node", "--input-type=module", "-e", "import {PetstoreClient} from './dist/index.js'; console.log(PetstoreClient.name)"], sdk)
    elif target == "rust":
        command(["cargo", "build", "--workspace"], sdk)
        command(["cargo", "run", "--quiet", "-p", "petstore-cli", "--", "--help"], sdk)
    elif target == "go":
        command(["go", "build", "./..."], sdk)
    elif target == "java":
        command(["mvn", "-B", "-q", "package", "-DskipTests"], sdk)
    elif target == "csharp":
        command(["dotnet", "build", str(next(sdk.glob("*.csproj"))), "--nologo",
                 "--artifacts-path", str(work / "artifacts")], sdk)
    elif target == "ruby":
        command(["bundle", "install"], sdk)
        command(["bundle", "exec", "ruby", "-Ilib", "-e", "require 'petstore_sdk'"], sdk)


@contextlib.contextmanager
def mock(binary: str, work: Path):
    with (work / "mock.log").open("w+") as log:
        process = subprocess.Popen([binary, "mock", "tungsten.yml", "--port", "0", "--json"],
                                   cwd=work, stdin=subprocess.PIPE, stdout=log, stderr=log, text=True)
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


def local_client(binary: str, work: Path, preview: bool) -> None:
    python = python_environment(work)
    script = '''from petstore_sdk import PetstoreClient
from tungsten_runtime import Err
import sys
with PetstoreClient(base_url=sys.argv[1], validate_responses="strict") as client:
    result = client.pets.list()
    if isinstance(result, Err):
        raise RuntimeError(str(result.error))
    assert len(result.value) > 0
    print("mock returned typed pets")
'''
    if preview:
        script += '''    rejected = client.pets.delete(pet_id="pet-1")
    assert isinstance(rejected, Err) and rejected.error["category"] == "CONFIRMATION_REQUIRED"
    preview = client.pets.preview_delete(pet_id="pet-1")
    if isinstance(preview, Err):
        raise RuntimeError(str(preview.error))
    token = preview.value["confirmation_token"]
    assert isinstance(token, str) and token
    result = client.pets.delete(pet_id="pet-1", opts={"confirm": token})
    assert not isinstance(result, Err), result
    print("preview confirmed the same destructive arguments")
'''
    (work / "client.py").write_text(script)
    with mock(binary, work) as base:
        command([str(python), str(work / "client.py"), base], work)
        with urllib.request.urlopen(base + "/__tungsten/calls", timeout=10) as response:
            calls = json.load(response)
        deletes = [call for call in calls if call["method"] == "DELETE"]
        assert len(deletes) == (1 if preview else 0), "preview or missing confirmation executed a mutation"


SCENARIOS = {"rate-limit": 0, "stream-interrupted": 0, "malformed": 3, "lost-write": 0}


def scenario_run(binary: str, work: Path, name: str, broken: bool) -> dict:
    # --json precedes the subcommand: everything after `--` is the integration argv.
    argv = [binary, "--json", "scenario", "run", "openapi.json", "--scenario", f"scenarios/{name}.json",
            "--timeout-ms", "30000", "--expect-exit", str(SCENARIOS[name]),
            "--", sys.executable, "-I", str(work / "integration.py"), name]
    result = command(argv + (["--broken"] if broken else []), work, 1 if broken else 0)
    return json.loads(result.stdout)["result"]["run"]


def scenarios(binary: str, work: Path) -> None:
    for name in SCENARIOS:
        tungsten(binary, work, "scenario", "validate", "openapi.json", "--scenario", f"scenarios/{name}.json")
    if not sys.platform.startswith("linux"):
        raise RuntimeError("scenario files validated, but scenario run is currently available on Linux only")
    for name in SCENARIOS:
        run = scenario_run(binary, work, name, broken=False)
        assert run["passed"] and run["reason"] == "completed", (name, run)
        broken = scenario_run(binary, work, name, broken=True)
        assert not broken["passed"] and not broken["scenario"]["passed"], (name, broken)
    # A new idempotency key on the retry matches every call count; only the
    # effect assertion distinguishes it from a correct replay.
    lost = scenario_run(binary, work, "lost-write", broken=True)["scenario"]
    assert lost["total_calls"]["passed"] and all(c["passed"] for c in lost["calls"]), lost
    assert lost["effects"][0]["applied"]["actual"] == 2, lost


def digest_tree(root: Path) -> dict[str, str]:
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob("*") if p.is_file()}


def execute(entry: dict, binary: str, build_packages: bool) -> None:
    example = HERE / entry["name"]
    work = fresh_work(example)
    mode = entry["mode"]
    if mode == "input":
        args = ["init", "--from", str(example / entry["input"]), "--dir", str(work / "project"),
                "--targets", "docs", "--no-interactive", *entry.get("args", [])]
        tungsten(binary, work, *args)
        project = work / "project"
        assert (project / "tungsten.yml").is_file() and (project / "agent.yml").is_file()
        tungsten(binary, project, "check")
        tungsten(binary, project, "generate")
        docs = next(project.rglob("llms.txt")).parent
        first = digest_tree(docs)
        tungsten(binary, project, "generate")
        assert first == digest_tree(docs), "input regeneration was not deterministic"
        tungsten(binary, project, "generate", "--check")
        assert list(project.rglob("llms.txt")), "docs were not generated"
        return
    if mode == "scenario":
        for name in ("openapi.json", "integration.py"):
            shutil.copyfile(example / name, work / name)
        shutil.copytree(example / "scenarios", work / "scenarios")
        scenarios(binary, work)
        return
    target = entry.get("target", "python" if mode in {"mock", "preview"} else "docs")
    shutil.copyfile(example / "openapi.json", work / "openapi.json")
    write_json(work / "tungsten.yml", manifest(target, work))
    if mode == "preview":
        shutil.copyfile(example / "agent.yml", work / "agent.yml")
    tungsten(binary, work, "check")
    tungsten(binary, work, "generate")
    first = digest_tree(work / "sdk")
    assert first, "the selected target produced no files; inspect skipped/refused target diagnostics"
    tungsten(binary, work, "generate")
    assert first == digest_tree(work / "sdk"), "regeneration was not deterministic"
    tungsten(binary, work, "generate", "--check")
    if target == "docs":
        assert (work / "sdk/llms.txt").is_file() and (work / "sdk/tools.json").is_file()
    if mode in {"drift", "diff"}:
        changed = json.loads((work / "openapi.json").read_text())
        changed["paths"]["/status"] = {"get": {"operationId": "getStatus", "responses": {"204": {"description": "healthy"}}}}
        write_json(work / "openapi.json", changed)
        if mode == "drift":
            stale = tungsten(binary, work, "generate", "--check", expected=1)
            assert any(d["code"] == "TG0901" for d in stale["diagnostics"])
            tungsten(binary, work, "generate")
            tungsten(binary, work, "generate", "--check")
        else:
            result = tungsten(binary, work, "diff", "--semver")
            assert result["result"]["level"] == "minor", result
            migration = tungsten(binary, work, "diff", "--migration")
            assert any(c["semver"]["migration"]["actions"] for c in migration["result"]["changes"])
    elif mode in {"mock", "preview"}:
        local_client(binary, work, mode == "preview")
    elif build_packages:
        build(target, work)


def example_readme(entry: dict) -> str:
    return f'''# {entry["title"]}

{entry["description"]}

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; {entry["prerequisites"]}.

From the repository root:

```sh
python3 examples/run.py --only {entry["name"]}
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: {entry["outcome"]}.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.
''' + TUTORIALS.get(entry["name"], "")


def index_readme() -> str:
    rows = "\n".join(f'| [{e["title"]}]({e["name"]}/README.md) | {e["description"]} |' for e in CATALOG)
    return f'''# Runnable example gallery

Start with a matching compiler and source checkout (`cargo build -p tungsten-cli`
or `TUNGSTEN=/absolute/path/to/tungsten`). Each recipe has a committed small
Petstore-shaped input, complete prerequisites and a checked outcome. Run one
from its README, or run all recipes from the repository root:

```sh
python3 examples/run.py --check
```

The full run needs Python 3.12 with venv/pip, Node 22 with npm, the pinned Rust
toolchain, Go 1.22, Java 17 with Maven, .NET 10 and Ruby 3.3 with Bundler. It builds
generated packages against matching runtime source, including the Rust CLI.
It contacts package registries and loopback mocks, never an external API.
For compiler/input/workflow smoke coverage without first-SDK package builds,
use `--generate-only`. Use `--only NAME` (repeatable or comma-separated) to select
recipes. Missing tools fail explicitly; no entry is silently skipped.

The runner checks this index and every recipe README against the catalog,
then executes the described outcomes. CI runs all gallery recipes with package
builds enabled. Generated work stays below each recipe's ignored `.work/`;
canonical inputs are never modified.

| Recipe | What it demonstrates |
|---|---|
{rows}

JSON Schema needs explicit HTTP routing: its recipe uses a versioned endpoint
manifest with declared models, rather than guessing an API from a bare schema.
Imported observations need review before live use; see
[supported inputs](../README.md#supported-inputs).

The [target matrix](../README.md#targets) and [#61](https://github.com/pboachie/tungsten/issues/61)
own readiness claims. A sample does not establish registry publication, complete
validation parity or live acceptance.

Additional examples have their own runners and prerequisites:

- [Manual release-video walkthrough](release-video/README.md): version-audited Windows terminal recording, local narration, chapters and review-only delivery artifacts.
- [Six public APIs](showcase/README.md): pinned specifications and regenerated showcase.
- [External Markdown emitter](external-emitter-markdown/README.md): implement the emitter protocol.
- [Typed Python SSE](typed-sse/README.md): declare and consume named streams with explicit cleanup.
- [Response-contract quality](response-contract-audit/README.md): audit weak responses and apply reviewed schema overlays.
- [GitHub Actions workflow](github-actions/README.md): a copyable workflow that installs a pinned release, validates the contract and fails on stale generated output.
'''


def documentation(write: bool) -> None:
    outputs = {HERE / "README.md": index_readme()}
    outputs.update({HERE / e["name"] / "README.md": example_readme(e) for e in CATALOG})
    for path, expected in outputs.items():
        if write:
            path.write_text(expected)
        elif not path.is_file() or path.read_text() != expected:
            raise RuntimeError(f"gallery README drift: {path}; run python3 examples/run.py --write-index")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--only", action="append", default=[])
    parser.add_argument("--generate-only", action="store_true")
    parser.add_argument("--check", action="store_true", help="check README drift and run recipes (the default)")
    parser.add_argument("--write-index", action="store_true", help="rewrite gallery READMEs only")
    parser.add_argument("--list", action="store_true")
    args = parser.parse_args()
    if args.list:
        print("\n".join(e["name"] for e in CATALOG))
        return
    documentation(args.write_index)
    if args.write_index:
        return
    selected = {name for group in args.only for name in group.split(",")}
    known = {e["name"] for e in CATALOG}
    if selected - known:
        raise RuntimeError(f"unknown recipes: {', '.join(sorted(selected - known))}")
    binary = compiler()
    for entry in CATALOG:
        if selected and entry["name"] not in selected:
            continue
        print(f"Running {entry['name']}", flush=True)
        execute(entry, binary, not args.generate_only)
        if args.generate_only and entry["mode"] == "target" and entry["target"] != "docs":
            print(f"OK: {entry['target']} generation is deterministic and fresh; package build was not run", flush=True)
        else:
            print(f"OK: {entry['outcome']}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, RuntimeError, subprocess.SubprocessError, OSError) as error:
        print(f"gallery: {error}", file=sys.stderr)
        raise SystemExit(1)
