# Start with Postman

Import a minimal Postman v2.1 collection offline; requests and scripts are not executed.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; no target-specific toolchain.

From the repository root:

```sh
python3 examples/run.py --only input-postman
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: a normalized OpenAPI project and agent documentation are generated from the collection.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

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
