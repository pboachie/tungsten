<div align="center">

<img src="assets/logo.svg" alt="tungsten" width="360">

**Compile an OpenAPI description into SDKs, a CLI and an MCP server that AI agents can use safely.**

[![CI](https://github.com/pboachie/tungsten/actions/workflows/ci.yml/badge.svg)](https://github.com/pboachie/tungsten/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-pboachie.github.io-informational.svg)](https://pboachie.github.io/tungsten/)
[![License: AGPL-3.0](https://img.shields.io/badge/compiler-AGPL--3.0-blue.svg)](LICENSE)
[![Runtimes: Apache-2.0](https://img.shields.io/badge/runtimes-Apache--2.0-blue.svg)](runtimes/LICENSE)
[![MSRV](https://img.shields.io/badge/rustc-1.95%2B-orange.svg)](Cargo.toml)

</div>

tungsten is a deterministic compiler written in Rust. It reads OpenAPI 3.0 and 3.1 documents plus two small manifests, builds one typed intermediate representation (IR), and emits TypeScript, Python, Rust, Go, Java, C# and Ruby SDKs, a Rust command-line client, an MCP server, browser WebMCP tools and LLM-oriented docs. Every emitted client is built for an autonomous caller: failures are instructions, side effects are never duplicated, and irreversible calls need an explicit confirmation.

Read the documentation at [pboachie.github.io/tungsten](https://pboachie.github.io/tungsten/).

## Why tungsten

Most SDK generators optimize for a person reading autocomplete. An agent has a finite context window, no memory between retries, and no sense of what is irreversible. tungsten changes the design goal of each stage:

- **Diagnostic error envelopes.** Every failure, from argument validation to a transport error to an HTTP status, is one closed JSON shape with a category, the failing parameter, the received value, what was expected, a remediation sentence and an explicit `retryable` verdict. The same 14 members in every runtime, ready to paste into a model's context.
- **Safety tiers.** Each operation is `read_only`, `mutating`, `destructive` or `irreversible`, defaulted from the HTTP method and refined in `agent.yml`. Tiers drive retries, MCP tool annotations and confirmation rules.
- **Previews and confirmation tokens.** A destructive or irreversible call must be previewed first. The preview returns the request that would be sent and its effects, plus a confirmation token that is valid only for identical arguments. The generated CLI replaces tokens with `--yes` (and `--i-understand` for irreversible operations).
- **Idempotency and unknown outcomes.** Mutations are retried only with an idempotency key (or an identity body), always with the same key. A timeout or reset after a write is reported as `OUTCOME_UNKNOWN` with the call to verify it, not as a failure to retry blindly.
- **Verification.** An operation can name a read to run after success and the state to expect, attached to the result as evidence.
- **MCP progressive disclosure.** Past a tool-count threshold the server lists a few meta-tools (`search_tools`, `describe_tool`, `invoke`, `preview`, `list_clusters`) instead of every operation. In the maintainers' measurement on the ZROtext pilot (October 2026, BPE token counts), the progressive listing of its 41 tools costs 765 tokens, against 9,473 for the 14 tools of that project's hand-written MCP server.
- **Deterministic output.** The same inputs produce byte-identical files. Generated files record the generator version and the digest of their inputs, and `tungsten generate --check` fails when output is stale.

## Targets

| Target | Emits | Minimum toolchain | Runtime and installation |
|---|---|---|---|
| `typescript` | Typed SDK, `Result` values, Zod validation | Node 22 | `@elysra/tungsten-runtime`: matching npm package or built source |
| `python` | Sync and async SDK, Pydantic v2 models | Python 3.12 | Install `tungsten-runtime` from matching `runtimes/python` source |
| `rust` | Async SDK (serde, reqwest) and optional generated CLI | Rust 1.88 | Source paths to `runtimes/rust/runtime` and, for a CLI, `runtimes/rust/cli-kit` |
| `go` (external) | Models, resources, typed client, pagination and stream helpers | Go 1.22 | Build `tungsten-emit-go` from `emitters/go`; use `runtime_path` to matching `runtimes/go` source |
| `java` | Java SDK | Java 17 and Maven | Use `runtime_path` to matching `runtimes/java` source |
| `csharp` (`cs`) | C# SDK | .NET 10 | Use `runtime_path` to matching `runtimes/csharp`, referencing `Tungsten.Runtime.csproj` |
| `ruby` (`rb`) | Ruby SDK | Ruby 3.3 and Bundler | Use `runtime_path` to matching `runtimes/ruby` source |
| `mcp` | MCP server over stdio or Streamable HTTP, discrete or progressive | Node 22 | `@elysra/tungsten-mcp`: matching npm package or built source |
| `webmcp` | Browser ESM package with tool manifest and registration helper | Node 22 to build; compatible browser to run | `@elysra/tungsten-webmcp` and the generated TypeScript SDK |
| `docs` | `llms.txt`, `llms-full.txt`, `tools.json`, README | None beyond the compiler | No runtime dependency |

These targets are implemented in source. Compiler and runtime packages have independent versions: use a runtime compatible with the generated package's dependency declaration, or the matching compiler source checkout. npm publication is separate from compiler availability. Python, Rust, Java, C# and Ruby registry publication is not part of the 0.1.2 installation guarantee; the default Go module version also lacks a published nested-module tag. Use the source routes above rather than assuming a package name or version in generated metadata proves registry availability. Target `runtime_path` values are relative to the generated output directory.

Targets and the mock server (`tungsten mock`) share one IR. Generated-package and contract checks exercise each language, but source availability and passing shared scenarios do not establish complete validation, protocol or distribution parity. Unsupported features can vary by target; review compiler diagnostics and use `--strict` when ignored constraints must stop generation. Local checks do not establish live API acceptance or registry installation readiness.

## Install

Prebuilt binaries for Linux, macOS and Windows are attached to each [GitHub release](https://github.com/pboachie/tungsten/releases). The installer checks the archive against the release's `SHA256SUMS` before it installs anything:

```sh
curl -fsSL https://raw.githubusercontent.com/pboachie/tungsten/main/install.sh | sh   # Linux and macOS, into ~/.local/bin
```

```powershell
irm https://raw.githubusercontent.com/pboachie/tungsten/main/install.ps1 | iex        # Windows
```

Pass `--version v0.1.2` to `install.sh` (or `-Version v0.1.2` to `install.ps1`) to pin a release, and `--dir` (`-InstallDir`) to choose the target directory.

Or build from source, with Rust 1.95 or newer (the repository pins 1.97.0 in [`rust-toolchain.toml`](rust-toolchain.toml) for development):

```sh
git clone https://github.com/pboachie/tungsten
cd tungsten
cargo install --path crates/tungsten-cli   # installs the `tungsten` binary
tungsten doctor                            # reports optional tools (node, python3, uv, formatters)
```

Node, Python and the formatters are optional: a missing tool only disables the target or `--format` option that needs it.

## Quickstart

Install the compiler above first. This walkthrough uses Python 3.12 or newer and
[uv](https://docs.astral.sh/uv/getting-started/installation/). TypeScript targets
require Node 22 or newer. Installing the compiler does not install generated
SDKs or their language runtimes.

This walkthrough continues from the source installation above, using the same
checkout for the v0.1.2 compiler and runtime. From that `tungsten` directory, set
its absolute path and create a separate project. If you installed a release
binary, obtain its matching source archive from the release first:

```sh
export TUNGSTEN_SOURCE="$(pwd)"
mkdir ../petstore-demo
cd ../petstore-demo
```

Use your own `openapi.json`, or create this complete tiny example:

```json
{"openapi":"3.1.0","info":{"title":"Acme Petstore","version":"1"},"paths":{"/pets":{"get":{"operationId":"listPets","responses":{"200":{"description":"Pets","content":{"application/json":{"schema":{"type":"array","items":{"type":"string"}}}}}}}}}}
```

`init` writes `tungsten.yml` and `agent.yml` with **TypeScript, Python and docs**
targets by default. Edit the existing `targets` block rather than appending a
second one. For this example, replace its Python entry with:

```yaml
  python: { package: acme-petstore, module: acme_petstore, out: generated/python }
```

Review `agent.yml` and resolve the TODO entries for operations with side effects
before generating. The tiny example has only a read operation.

```sh
tungsten init --from openapi.json
# Edit tungsten.yml and agent.yml as described above.
tungsten check
tungsten generate
uv venv --python 3.12 .venv
# Install the language runtime and generated SDK into this environment.
# The local runtime satisfies the generated package's runtime dependency.
uv pip install --python .venv/bin/python "$TUNGSTEN_SOURCE/runtimes/python" ./generated/python
tungsten mock . --port 8099
```

Leave the mock running. In another terminal in `petstore-demo`, make the first
call using the environment where you installed the SDK:

```sh
.venv/bin/python - <<'PYTHON'
from acme_petstore import AcmePetstoreClient, Ok

with AcmePetstoreClient(base_url="http://127.0.0.1:8099") as client:
    result = client.pets.list()
    assert isinstance(result, Ok), result
    print(result.value)
PYTHON
```

The tiny example returns a successful list. For your own spec, the client class,
resource and method names follow its title and operation IDs. Calls return a
`Result`; API and transport errors carry a diagnostic envelope. When installing
a generated TypeScript SDK, build its matching source runtime first with
`npm ci && npm run build` in `runtimes/ts`, then install that directory and the
SDK's dependencies in the generated package and run `npm run build`. The Python
route above works without assuming runtime registry publication.

Destructive operations use the preview flow. From the TypeScript SDK generated for the ZROtext pilot:

```ts
const preview = await client.public.webhooks.disable.preview({ endpointId });
if (preview.ok && preview.value.confirmation_token) {
  console.log(preview.value.effects);
  await client.public.webhooks.disable({ endpointId }, { confirm: preview.value.confirmation_token });
}
```

More examples: [tungsten on six public APIs](examples/showcase/README.md) (the public OpenAPI descriptions of Stripe, GitHub, Twilio, OpenAI, Kubernetes and Discord): what `tungsten check` finds, MCP listing sizes, and generated SDK code in TypeScript, Python and Rust, reproducible with one script. [`examples/external-emitter-markdown`](examples/external-emitter-markdown) shows how to write an external emitter.

## Browser WebMCP

Add a browser target alongside your TypeScript SDK:

```yaml
targets:
  typescript: { package: "@acme/petstore", out: generated/typescript }
  webmcp:     { package: "@acme/petstore-webmcp", out: generated/webmcp }
```

Build the generated TypeScript SDK first, then the browser package (`npm install`
and `npm run build` in each). For source-local development, configure the SDK's
`runtime_path`, and the browser target's `sdk_path`, `runtime_path`
(`@elysra/tungsten-webmcp`) and `sdk_runtime_path` (`@elysra/tungsten-runtime`); see the guide.
Published defaults use version ranges, with `sdk_runtime` available to override
the browser package's direct SDK runtime range.

The generated ESM package exports a manifest and an explicit registration helper.
Your application supplies the SDK client configuration, API base URL and browser
session. Importing the package registers no tools. Registration defaults to
read-only operations; explicitly allowlist tools to expose writes or macros.
Operations that require confirmation also expose a `<tool-name>__preview` tool,
and execution still requires the SDK's argument-bound confirmation token.
Dispose the registration or abort its signal when the page or component leaves.
Scope registrations to the current page to keep large catalogs manageable.

For route-based loading, import a generated `groups/<moduleKey>` entry inside
the `createWebMcpRegistrationController` loader. Each group builds a scoped
descriptor/model graph and keeps read-only registration defaults. React and Vue
examples are in the [browser integration guide](https://pboachie.github.io/tungsten/browser-integration.html).
Opt-in bounded streams and binary outputs are described in
[tool results](https://pboachie.github.io/tungsten/tool-results.html).

WebMCP is experimental and browser availability varies. The runtime prefers the
current `document.modelContext` API with async registration and abort-based
cleanup, and supports a structural adapter for the older
`navigator.modelContext` API. Check capability before enabling the feature.
The generated-package browser smoke passed in Chromium 154.0.8037.57 with
`WebMCPTesting` enabled; full cross-browser conformance remains future work.
Browser tool schemas and annotations are hints: the generated SDK validates
arguments, while your API must enforce authorization. Use a secure context,
appropriate CORS and CSRF controls, and your application's browser session;
never bundle server credentials. See the [browser WebMCP guide](https://pboachie.github.io/tungsten/webmcp.html).

## Architecture

<p align="center"><img src="assets/architecture.svg" alt="OpenAPI 3.0/3.1 documents and overlays, with tungsten.yml and agent.yml, go through the frontend into a typed IR, the agent compiler, and the emitters, which depend on the runtimes" width="900"></p>

1. The **frontend** reads OpenAPI 3.0 and 3.1, resolves references and applies overlays.
2. The **IR** is a versioned JSON document (`tungsten ir dump`, schema via `tungsten schema ir`).
3. The **agent compiler** merges `agent.yml` into it: safety tiers, idempotency, confirmation, remediation text, verification hooks, macros and the disclosure mode for MCP.
4. **Emitters** (`crates/tungsten-emit-*`) write each target from the IR. They are pure functions of their inputs.
5. **Runtimes** (`runtimes/`) hold the behaviour shared by generated code: validation, auth, retries, idempotency stores, the envelope, previews, pagination and middleware.

Repository layout: `crates/` is the compiler (AGPL-3.0-only); `runtimes/ts`, `runtimes/mcp`, `runtimes/webmcp`, `runtimes/python`, `runtimes/rust`, `runtimes/go`, `runtimes/java`, `runtimes/csharp` and `runtimes/ruby` contain the libraries generated code depends on (Apache-2.0).

## CLI

```
tungsten [--json] <command>
```

| Command | Purpose |
|---|---|
| `init` | Write project manifests (`--from`, `--dir`, `--name`, `--targets`, `--force`); `--targets webmcp` includes a linked TypeScript SDK |
| `check` | Validate the manifests and specs and report diagnostics (`--ci`, `--strict`) |
| `generate` | Generate every configured target (`--target`, `--dry-run`, `--check`, `--force`, `--strict`) |
| `ir dump` | Write the IR as JSON (`--out`, `--compact`) |
| `explain` | Explain a diagnostic code (`TG0201`), an operation or a type |
| `schema` | Print a JSON Schema: `tungsten`, `agent`, `ir`, `cli-output`, `external-emitter`, `sdk-descriptors`, `tool-catalog` |
| `mock` | Serve a mock of the API from the IR (`--port`, `--seed`, `--gate`) |
| `report` | Coverage, safety matrix, token budgets and diagnostics, as text, JSON or one static HTML file (`--html`) |
| `diff` | Show regeneration changes; `--semver` classifies the API surface, `--migration` adds upgrade actions and runtime requirement review |
| `lsp` | Serve diagnostics, completion, hover, go-to-definition and symbols for `tungsten.yml`, `agent.yml` and overlays to an editor over stdio (`--config`) |
| `doctor` | Report external tools; pass a project path and `--strict` to fail when its required tools are missing or too old |

With `--json`, stdout carries exactly one JSON document described by `tungsten schema cli-output` (`lsp` speaks the language server protocol on stdio instead). Color is used only on a terminal and never when `NO_COLOR` is set.

### Editor support

`tungsten lsp` is a language server for `tungsten.yml`, `agent.yml` and the overlay files a project lists. It checks the project with the same compiler as `tungsten check` and shows the same `TG` diagnostics on the YAML lines they belong to, including unsaved changes to the two manifests (specs and overlays are re-read when saved). It completes keys, enumerated values and the operation ids, gates and clusters of the compiled project; shows field documentation, operation signatures and diagnostic explanations on hover; jumps from an operation id to its place in the OpenAPI document; and outlines the manifests. The workspace is the directory of `tungsten.yml` (found from the editor's workspace folder, or given with `--config <path>`); one project is served per process.

Visual Studio Code needs a generic language client extension, for example [Generic LSP Client](https://marketplace.visualstudio.com/items?itemName=llllvvuu.llllvvuu-glspc). In `settings.json`:

```json
{
  "glspc.languageId": "yaml",
  "glspc.serverCommand": "tungsten",
  "glspc.serverCommandArguments": ["lsp"]
}
```

Neovim with [nvim-lspconfig](https://github.com/neovim/nvim-lspconfig):

```lua
local configs = require('lspconfig.configs')
local util = require('lspconfig.util')

if not configs.tungsten then
  configs.tungsten = {
    default_config = {
      cmd = { 'tungsten', 'lsp' },
      filetypes = { 'yaml' },
      root_dir = util.root_pattern('tungsten.yml', 'tungsten.yaml'),
    },
  }
end

require('lspconfig').tungsten.setup({})
```

Any other client works the same way: start `tungsten lsp` as a stdio server for YAML files.

| Exit code | Meaning |
|---|---|
| 0 | Success (also `diff` when regeneration would change files, and `report` with warnings) |
| 1 | The input has errors (or warnings with `--strict`), generated output is stale (`generate --check`, `check --ci`), or the item asked for does not exist |
| 2 | Usage error: the command line could not be parsed |
| 3 | I/O or internal failure (for example `--out` is not writable) |
| 4 | Refused to act without confirmation (`init` over existing files, `generate` into a directory tungsten did not write: pass `--force`) |

The CLI that tungsten generates for a Rust target has its own exit codes (0 success, 2 usage or pre-flight validation, 3 API error, 4 retryable API error, 5 unknown outcome, 6 confirmation required, 7 missing credentials) so a calling agent can branch on them.

## Project status

tungsten is **early-stage**. Seven SDK targets, the generated Rust CLI, MCP, WebMCP, docs and the mock server are implemented. Validation is target-specific; do not infer that every target has the same live pilot coverage or published runtime packages. Check the [GitHub releases](https://github.com/pboachie/tungsten/releases) for available compiler binaries and matching sources. The quickstart installs a runtime from matching source and does not require registry publication; compiler and runtime packages have independent versions. Expect breaking changes to manifests, the IR and generated code between releases. Open an issue if a target limitation blocks you.

The test suite, conformance corpus and design documents live in a separate maintainers' repository, which runs against every pull request. This repository is source only.

## Licensing

- The compiler (`crates/`) is licensed under [AGPL-3.0-only](LICENSE).
- The runtime libraries that generated code depends on (`runtimes/`) and the external Go emitter (`emitters/go`) are licensed under [Apache-2.0](runtimes/LICENSE).
- Output generated by tungsten is not a work covered by the AGPL and carries no licence from tungsten. The owner of the API description chooses the licence of the generated code.

## Contributing and security

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request, and open an issue to discuss a large change first. Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md); please do not file them as public issues.
