# Detect stale generated files in CI

Generate, change a copied contract, observe the no-diff gate fail, then regenerate and pass.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; no target-specific toolchain.

From the repository root:

```sh
python3 examples/run.py --only ci-drift
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: generate --check fails with TG0901 after a contract change and succeeds after regeneration.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

## Problem: the contract changed but generated output did not

You want a review or CI gate to catch a forgotten regeneration, rather than
discovering stale methods in a consumer. This recipe uses a committed OpenAPI
contract and generated **docs output** to make that failure and repair small.
Expected results below describe the recipe's checks, not a recorded CI run.

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
tungsten check path/to/project
tungsten generate --check path/to/project
```

If the second command reports `TG0901`, review the contract change, then run:

```sh
tungsten generate path/to/project
tungsten generate --check path/to/project
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
