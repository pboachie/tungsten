<div align="center">

<img src="assets/logo.svg" alt="tungsten" width="360">

**Compile an API description — OpenAPI OR Swagger OR Postman OR saved cURL OR HTTP request files OR HAR OR an endpoint manifest — into SDKs, a CLI and an MCP server that AI agents can use safely.**

[![Woodpecker CI](https://badges.boachiefamily.net/api/badges/4/labeled.svg?label=Release%20CI)](https://ci.boachiefamily.net/repos/4) [![CI](https://github.com/pboachie/tungsten/actions/workflows/ci.yml/badge.svg)](https://github.com/pboachie/tungsten/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-pboachie.github.io-informational.svg)](https://pboachie.github.io/tungsten/)
[![License: AGPL-3.0](https://img.shields.io/badge/compiler-AGPL--3.0-blue.svg)](LICENSE)
[![Runtimes: Apache-2.0](https://img.shields.io/badge/runtimes-Apache--2.0-blue.svg)](runtimes/LICENSE)
[![MSRV](https://img.shields.io/badge/rustc-1.95%2B-orange.svg)](Cargo.toml)

</div>

tungsten is a deterministic compiler written in Rust. It reads one API description — an OpenAPI 3.0 or 3.1 document (JSON or YAML), a Swagger 2.0 document, a Postman Collection v2.1 file, a saved cURL command, an HTTP request file (`.http`/`.rest`), a HAR 1.2 capture or an endpoint manifest with JSON Schema 2020-12 models — plus two small manifests, builds one typed intermediate representation (IR), and emits TypeScript, Python, Rust, Go, Java, Kotlin, C#, F#, Dart and Ruby SDKs, a Rust command-line client, an MCP server, browser WebMCP tools and LLM-oriented docs. Documents are parsed locally; optional HTTP/Git acquisition pins the downloaded source for verified offline reuse. Every emitted client is built for an autonomous caller: failures are instructions, side effects are never duplicated, and irreversible calls need an explicit confirmation. See [Supported inputs](#supported-inputs) for the exact alternatives, their limits and the review each requires.

Read the documentation at [pboachie.github.io/tungsten](https://pboachie.github.io/tungsten/).

## Why tungsten

Most SDK generators optimize for a person reading autocomplete. An agent has a finite context window, no memory between retries, and no sense of what is irreversible. tungsten changes the design goal of each stage:

- **Diagnostic error envelopes.** Every failure, from argument validation to a transport error to an HTTP status, is one closed JSON shape with a category, the failing parameter, the received value, what was expected, a remediation sentence and an explicit `retryable` verdict. The same 14 members in every runtime, ready to paste into a model's context.
- **Safety tiers.** Each operation is `read_only`, `mutating`, `destructive` or `irreversible`, defaulted from the HTTP method and refined in `agent.yml`. Tiers drive retries, MCP tool annotations and confirmation rules. Ordinary spec annotations cannot lower the method tier or remove preview from an operation with side effects: those decisions require an explicit per-operation `tools` entry in `agent.yml` (`safety` and `preview: none`, respectively). The compiler keeps the safe policy and reports TG0615/TG0616; `--strict` rejects these warnings. Imported operations retain their separate reviewed-policy floor.
- **Previews and confirmation tokens.** A destructive or irreversible call must be previewed first. The preview returns the request that would be sent and its effects, plus a confirmation token that is valid only for identical arguments. The generated CLI replaces tokens with `--yes` (and `--i-understand` for irreversible operations).
- **Idempotency and unknown outcomes.** Mutations are retried only with an idempotency key (or an identity body), always with the same key. A timeout or reset after a write is reported as `OUTCOME_UNKNOWN` with the call to verify it, not as a failure to retry blindly.
- **Verification.** An operation can name a read to run after success and the state to expect, attached to the result as evidence.
- **MCP progressive disclosure.** Past a tool-count threshold the server lists a few meta-tools (`search_tools`, `describe_tool`, `invoke`, `preview`, `list_clusters`) instead of every operation. In the maintainers' measurement on the ZROtext pilot (October 2026, BPE token counts), the progressive listing of its 41 tools costs 765 tokens, against 9,473 for the 14 tools of that project's hand-written MCP server.
- **Deterministic output.** The same inputs produce byte-identical files. Generated files record the generator version and the digest of their inputs, and `tungsten generate --check` fails when output is stale.

## Supported inputs

`init --from` also accepts HTTP(S) document URLs for these formats. OpenAPI
URLs work directly as compiler paths or `inputs[].spec` in `tungsten.yml`.
Other formats go through the same local importer and its required review flags:

```sh
tungsten init --from https://example.com/contracts/openapi.json --dir my-api
tungsten generate my-api --offline
tungsten check my-api --refresh-inputs
```

The first fetch records the source URL and SHA256 under
`.tungsten-inputs/<url-sha256>/source.lock.json`, beside immutable downloaded
content. Retain the complete source cache directory for offline use. Online commands verify upstream
bytes against the pin and refuse changed content. `--refresh-inputs` explicitly
accepts a new OpenAPI pin; converted imports are instead refreshed with
`tungsten refresh-import`, which previews the converted diff, operation and
output changes and requires renewed review of changed approvals before
`--apply` writes the conversion, pin and lock. Their `source_url` and
`source_sha256` manifest fields preserve that provenance on every compilation.

For authentication, repeat `--input-header 'Authorization: Bearer ...'`, or set
`TUNGSTEN_INPUT_HEADERS` to newline-separated `Name: value` headers to keep
secrets out of command history. Unscoped headers apply only to the exact URL
typed on the command line. For URLs in a manifest, scope each header to an
origin: `--input-header '[https://api.example.com] Authorization: Bearer ...'`.
The scheme, host and effective port must match; redirects remain refused.
Header values are excluded from pins and fetch
diagnostics. URLs must have no credentials, query or fragment. Downloads are
limited to 8 MiB and 30 seconds; redirects are refused. Remote references are
still blocked, including relative references from a remote OpenAPI document.
The cache contains original input bytes, so retain it with the same access
controls as the source document and review it before sharing.

Git inputs use `git+https://host/owner/repository@ref#path` in `init --from`,
compiler paths or OpenAPI manifest inputs. A branch or tag resolves to exactly
one commit, which is fetched shallowly into a disposable bare repository. No
checkout, hooks, submodules or repository code runs. Git supplies private-source
credentials through trusted user or system configuration. Project Git config
and inherited `GIT_*` overrides are ignored. `--input-header`
applies only to HTTP document downloads. Local repositories for offline workflows
and hermetic validation use `git+file:///absolute/repository@ref#path`.

```sh
tungsten init --from 'git+https://github.com/example/api@main#contracts/openapi.yaml' --dir my-api
tungsten generate my-api --offline
```

The Git source lock under `.tungsten-inputs/git/` records the repository URL,
requested ref, resolved commit, selected document and SHA256 of every materialized
file. Ordinary runs retain that commit; `--refresh-inputs` is the explicit request
to resolve a moving ref again. Converted imports retain their review provenance
and change source commits through `tungsten refresh-import`, which installs
exactly the reviewed commit. Discovery
recognizes all supported input formats and requires an explicit `#path` when
multiple documents are found. Relative references retain the compiler's local
input-root confinement. Acquisition is limited to 60 seconds, 512 files, 8 MiB per
file and 64 MiB of materialized content and fetched Git object data. Narrow
`#path` to bound discovery; the shallow fetch itself must still fit the object
budget. Git redirects, symlinks, submodules and unsafe paths are refused.
Retain the complete `.tungsten-inputs/git/` cache for offline use, including its
locks and materialized files; offline reuse needs no Git executable. Every
materialized file is verified, and unexpected cached files or directories are
rejected. HTTP/Git pins verify source consistency, not the publisher's identity:
review and protect the cache and lock files together.

On Windows, Git's disposable bare repository uses a fresh private child of the
local temporary directory, so a deeply nested project cache does not become
Git's working directory or object-store path. Pins and verified input files
remain under the project's `.tungsten-inputs/git/` directory. `TMP`/`TEMP` must
select a writable local disk directory with room for Git's pack and lock
filenames; network, unusable or overly long temporary roots fail with TG0133.
Choose a shorter local temporary path when needed. Offline pin reuse needs no
temporary repository. Git's source repository paths and external credential
helpers retain their own platform limits.

`tungsten init` takes **one** input of any of the kinds below — OpenAPI OR Swagger OR Postman OR saved cURL OR HTTP request files OR HAR, with the endpoint-manifest and manual paths alongside them — not a bundle. Acquisition may download a document or fetch a Git commit; parsing never sends or replays the API requests described by that input, and no shell, script or capture is ever executed. Non-OpenAPI inputs are converted to a normalized OpenAPI document that `init` saves next to the manifests (`imported.openapi.json`, with an `import-report.json` where listed; the manual path writes `inferred.openapi.json` and `inference-report.json`), so the accepted input stays separate from the normalized artifact and the typed IR the compiler builds from it.

| Input | You provide | Entry point and conversion | Limits and required review |
|---|---|---|---|
| OpenAPI 3.0 / 3.1 | JSON or YAML document | `tungsten init --from openapi.json` (default when unset: `openapi.json`, `openapi.yaml` or `openapi.yml` in the target directory). The document is kept in place, so relative references retain meaning, and goes through the native frontend: `$ref` resolution, overlays | The full frontend surface; overlays remain optional. OpenAPI 3.2 documents are read when they use only constructs 3.1 has plus the `query` method (sent as HTTP `QUERY` with its body), `additionalOperations` (sent with their method token as written), form-encoded `in: querystring` objects, `style: cookie` cookie parameters, `$self` that leaves reference resolution unchanged and response `itemSchema` for JSON Lines and for server-sent events with JSON or plain-text data; each other 3.2-only construct gets its own diagnostic (TG0140 to TG0156), an error unless it only adds metadata, and `tungsten capabilities` lists them |
| Swagger 2.0 | JSON or YAML document | `tungsten init --from swagger.json` (also a default name) or any path. Bounded offline conversion to OpenAPI, saved as `imported.openapi.json` | Unsupported semantics fail explicitly instead of being guessed; conversion never resolves external references |
| Postman Collection v2.1 | JSON collection | `tungsten init --from collection.json`. Structural import; omissions are listed in `import-report.json` | Scripts and requests are never executed: script-dependent behavior, non-JSON bodies and saved responses and unsupported auth helpers are omitted per item, not interpreted — this is not a Postman runtime |
| Saved cURL command | A saved command as text | `tungsten init --from command.curl --route '/pets/{id}'` (auto-detected by a leading `curl`, a `.curl` extension, or `--input-format curl`). Parsed as data against POSIX, cmd or PowerShell quoting (`--shell-dialect`) | Exactly one explicit `--route` path template is required; the command is parsed, never executed |
| HTTP request files | `.http` or `.rest` file | `tungsten init --from requests.http --route '/pets/{id}'` (extension-detected, or `--input-format http`) | An explicit `--route` template per request |
| HAR 1.2 | A capture file | `tungsten init --from capture.har --har-origin https://api.example.com --har-paths paths.json`. Bounded offline observations of the recorded traffic | A multi-origin capture needs `--har-origin`; every captured path needs a reviewed template (`--route`, or a `--har-paths` JSON mapping) before anything is saved; `--include-prefix`/`--exclude-prefix` narrow the capture and `--redact-property` redacts property names. Observations are incomplete evidence about past traffic, not the live API |
| Endpoint manifest v1 | JSON manifest with declared models | `tungsten init --from endpoints.json`. Loaded by the endpoint-manifest frontend | Explicit HTTP routing metadata (base URL and endpoints) and JSON Schema 2020-12 models within the supported subset are required — not arbitrary standalone JSON-Schema-to-API inference |
| Manual endpoint | No document | `tungsten init --method get --endpoint /pets/{id}`, with optional `--request-example` and `--response-example` JSON files (only their structure is persisted; the response is recorded at status 200). Writes `inferred.openapi.json` and `inference-report.json` | Explicit offline evidence only; not schema-free API discovery |

All conversion routes share these limits: setup inputs are bounded (8 MiB, with larger OpenAPI documents served by the explicit compiler flow below); every converted operation is marked as imported, and targets that cannot enforce imported-operation policy refuse it (TG0705 for built-in targets, TG0808 for external emitters that do not declare support) — the [support matrix](#targets) lists which built-in targets enforce it, non-opted-in external emitters do not, and `init` refuses unsupported built-in selections before saving the converted input or project manifests. Imported observations are incomplete contracts: review the inferred response types, authentication, safety tiers and origins in `agent.yml` and mark reviewed operations before live use.

`init` is the only entry point for the alternative inputs. The other commands (`check`, `generate`, `mock`, `diff`, `report`) take an initialized project (a directory with `tungsten.yml`, or the manifest), or — for the native format only — a single OpenAPI document passed directly.

## Targets

The support matrix below is generated from the capability catalog (`tungsten capabilities --matrix`; `--json` gives the same declarations as data), and CI checks that it matches. A declared feature or execution environment describes source behavior, not qualification.

<!-- tungsten-capabilities-matrix:start -->
<!-- generated by `tungsten capabilities --matrix` (contract 1) -->

| Target | Aliases | Emits | Consumer toolchain | Runs on | Features | Imported-operation policy | Qualification | Artifacts |
|---|---|---|---|---|---|---|---|---|
| `csharp` | `cs` | C# SDK | dotnet >=10.0 | dotnet | `typed_sdk`, `pagination_helpers` | enforced | unknown | unknown |
| `dart` |  | Dart SDK (sealed unions, Future methods returning Result, Stream pages) | dart >=3.6 <4.0 | dart_io | `typed_sdk`, `pagination_helpers` | enforced | unknown | unknown |
| `docs` |  | llms.txt, llms-full.txt, tools.json and README | none | static | `llms_txt` | enforced | unknown | unknown |
| `fsharp` | `fs` | F# SDK (records, discriminated unions, Task of Result) | dotnet >=10.0 | dotnet | `typed_sdk`, `pagination_helpers` | enforced | unknown | unknown |
| `java` |  | Java SDK | java >=17.0; mvn installed | jvm | `typed_sdk`, `pagination_helpers` | enforced | unknown | unknown |
| `kotlin` | `kt` | Kotlin/JVM SDK (blocking functions returning Result; opt-in coroutines) | java >=17.0; gradle installed | jvm | `typed_sdk`, `pagination_helpers`, `coroutines` | enforced | unknown | unknown |
| `mcp` |  | MCP server over stdio or Streamable HTTP, discrete or progressive | node >=22.13 excluding 23.0.x, 23.1.x; npm installed | node | `mcp_stdio`, `mcp_streamable_http`, `mcp_embedding_search`, `mcp_script_sandbox` | enforced | unknown | unknown |
| `mock` |  | reserved; no emitter in this version | none |  |  | n/a | unknown | unknown |
| `python` | `py` | Sync and async Python SDK, Pydantic v2 or dataclass models | python3 >=3.12 (`python_version` selects the minimum) | cpython | `typed_sdk`, `pagination_helpers`, `pagination_checkpoints`, `dataclass_models`, `webhook_verification`, `typed_webhooks`, `streaming_downloads`, `telemetry_hooks` | enforced | unknown | unknown |
| `ruby` | `rb` | Ruby SDK | ruby >=3.3; bundle installed | ruby | `typed_sdk`, `pagination_helpers` | enforced | unknown | unknown |
| `rust` | `rs` | Async Rust SDK (serde, reqwest) and optional generated CLI | cargo >=1.88; rustc >=1.88 | native | `typed_sdk`, `pagination_helpers`, `pagination_checkpoints`, `generated_cli`, `webhook_verification`, `typed_webhooks`, `cli_login_sessions`, `streaming_downloads`, `telemetry_hooks` | enforced | unknown | unknown |
| `typescript` | `ts` | Typed TypeScript SDK, Result values, Zod validation | node >=22.0; npm installed | node | `typed_sdk`, `pagination_helpers`, `pagination_checkpoints`, `webhook_verification`, `typed_webhooks`, `streaming_downloads`, `telemetry_hooks` | enforced | unknown | unknown |
| `webmcp` |  | Browser ESM package with tool manifest and registration helper | node >=22.0; npm installed | browser | `webmcp_registration` | enforced | unknown | unknown |

| Feature | Domain | Opt-in option | Description |
|---|---|---|---|
| `typed_sdk` | target |  | typed models and operation methods for every compiled operation |
| `pagination_helpers` | target |  | page iteration helpers for operations with declared pagination |
| `pagination_checkpoints` | runtime |  | sealed, caller-owned checkpoints that resume read-only page exports after a restart |
| `generated_cli` | target | `cli` | a command-line client crate next to the SDK |
| `cli_login_sessions` | target | `cli` | generated-CLI browser login, account switching and logout for OAuth2 authorizationCode schemes, with sessions in the macOS keychain, Secret Service or Windows Credential Locker, or an explicit owner-only file |
| `dataclass_models` | target | `models` | stdlib dataclass models without a Pydantic dependency |
| `coroutines` | target | `coroutines` | suspend functions and cold Flows next to the blocking surface |
| `mcp_stdio` | protocol |  | Model Context Protocol server over stdio |
| `mcp_streamable_http` | protocol |  | Model Context Protocol server over Streamable HTTP |
| `mcp_embedding_search` | target | `search` | optional embedding index for tool search |
| `mcp_script_sandbox` | runtime | `sandbox` | opt-in run_script sandbox |
| `webmcp_registration` | protocol |  | browser tool manifest and WebMCP registration helper |
| `telemetry_hooks` | runtime |  | opt-in local telemetry observer with bounded, redacted operation, attempt and phase events |
| `streaming_downloads` | runtime |  | explicit streamed response bodies with byte limits, progress, idle timeout and deadline |
| `webhook_verification` | runtime |  | framework-free incoming webhook verification: Stripe-compatible `t=,v1=` HMAC-SHA256 signatures compared in constant time, secret rotation, bounded body, timestamp tolerance and an application-owned replay hook |
| `typed_webhooks` | target |  | typed payloads of OpenAPI `webhooks` and a parse helper that verifies a delivery and validates its payload before the replay claim |
| `llms_txt` | target |  | llms.txt, llms-full.txt and tools.json documentation |

| OpenAPI line | Support | Diagnosed constructs |
|---|---|---|
| 3.0 | supported | 0 |
| 3.1 | supported | 0 |
| 3.2 | subset | 8 |

| Input format | Versions | Route | Detection | Operations | Review flags | Acquisition |
|---|---|---|---|---|---|---|
| `openapi` | 3.0, 3.1, 3.2 | compiler | `openapi` member | native |  | local, https, git |
| `swagger` | 2.0 | init_conversion | `swagger` member | imported |  | local, https, git |
| `postman` | 2.1 | init_conversion | `info` and `item` members | imported |  | local, https, git |
| `curl` |  | init_conversion | a leading `curl`, a `.curl` extension or `--input-format curl` | imported | `--route` | local, https, git |
| `http` |  | init_conversion | a `.http` or `.rest` extension or `--input-format http` | imported | `--route` | local, https, git |
| `har` | 1.2 | init_conversion | `log` member | imported | `--har-paths` | local, https, git |
| `endpoint_manifest` | 1 | init_conversion | `endpoint_manifest` member | imported |  | local, https, git |
| `manual_endpoint` |  | init_inference | `--method` and `--endpoint`, without `--from` | imported |  |  |

| IR protocol | Status | Targets | Protocol-specific extension |
|---|---|---|---|
| `http` | implemented | every built-in target | none; method, path, parameters and bodies are the common contract |
| `graphql` | reserved | none (refused, TG0706) | selection sets, fragments and subscription transport |
| `grpc` | reserved | none (refused, TG0706) | service and method names, streaming mode and status codes |
| `async_messaging` | reserved | none (refused, TG0706) | channel bindings, delivery guarantees and acknowledgment |

| `init` selection | Targets |
|---|---|
| Default (`--targets` omitted) | `typescript`, `python`, `docs` |
| Wizard default | `typescript`, `docs` |
| Local setup (`--local-example`, wizard) | `typescript`, `python`, `rust`, `mcp`, `webmcp`, `docs`, and `cli` (Rust SDK and CLI) |
| `--preset app` | `typescript`, `docs` |
| `--preset library` | `typescript`, `python`, `rust`, `docs` |
| `--preset agent` (`--strict`) | `typescript`, `mcp` |
| Selecting `mcp` adds | `typescript` |
| Selecting `webmcp` adds | `typescript` |

Source declarations only: a declared feature or environment is not qualification, and `unknown` evidence never becomes a compatibility claim.
<!-- tungsten-capabilities-matrix:end -->

Runtime installation and toolchain notes per target:

| Target | Toolchain notes | Runtime and installation |
|---|---|---|
| `typescript` | Node 22 | `@elysra/tungsten-runtime`: matching npm package or built source |
| `python` | Python 3.12 by default; [opt-in 3.10/3.11](#python-interpreter-compatibility) | PyPI runtime for a matching published release, or matching `runtimes/python` source |
| `rust` | Rust 1.88 | Matching source paths to `runtimes/rust/runtime` and, for a CLI, `runtimes/rust/cli-kit`; see [installation](runtimes/rust/README.md) |
| `go` (external) | Go 1.22 | External emitter (Models, resources, typed client, pagination and stream helpers). Build `tungsten-emit-go` from `emitters/go`; use `runtime_path` to matching `runtimes/go` source; see [module status](runtimes/go/README.md) |
| `java` | Java 17 and Maven | Use `runtime_path` to matching `runtimes/java` source |
| `kotlin` (`kt`) | Java 17 and Gradle | Use `runtime_path` to matching `runtimes/kotlin` ([limits](runtimes/kotlin/README.md#limits)); it builds on the Java runtime in `runtimes/java` (`java_runtime_path`). The coroutine surface adds the `tungsten-runtime-kotlin-coroutines` artifact (`runtimes/kotlin/coroutines`, `coroutines_runtime_path`) and `kotlinx-coroutines-core` |
| `csharp` (`cs`) | .NET 10 | Use `runtime_path` to matching `runtimes/csharp`, referencing `Tungsten.Runtime.csproj` ([limits](runtimes/csharp/README.md#limits)) |
| `fsharp` (`fs`) | .NET 10 | Use `runtime_path` to matching `runtimes/fsharp` ([limits](runtimes/fsharp/README.md#limits)); it references the .NET runtime in `runtimes/csharp` (`csharp_runtime_path`) |
| `dart` | Dart 3.6 | Use `runtime_path` to matching `runtimes/dart` (`tungsten_runtime`, a `dart:io` package; [limits](runtimes/dart/README.md#limits)) |
| `ruby` (`rb`) | Ruby 3.3 and Bundler | Use `runtime_path` to matching `runtimes/ruby` source |
| `mcp` | Node 22.13+; Node 23 needs 23.2+ | `@elysra/tungsten-mcp`: matching npm package or built source |
| `webmcp` | Node 22 to build; compatible browser to run | `@elysra/tungsten-webmcp` and the generated TypeScript SDK |
| `docs` | None beyond the compiler | No runtime dependency |

These targets are implemented in source. Starting with 0.1.3,
compiler and runtime package versions advance together with the release tag.
Generated packages default to compatible 0.1.4 runtime floors; an explicit
`runtime` setting remains yours to qualify. The earlier 0.1.2 release has
independently versioned non-npm runtime sources. Use the matching source checkout
when a registry artifact is unavailable: source version metadata is not evidence
of publication. Published 0.1.3 packages are available for npm, Python, Rust, Java,
C# and Ruby, and 0.1.2 packages remain available; use the explicit
[released-consumer overrides](#released-012-consumers) below to stay on 0.1.2. Go also has a
[matching-source route](runtimes/go/README.md) for stable consumers. Its `v0.1.3`
module has a verified Apache-2.0 archive; the older stable-commit
pseudo-version's archive notice differs, as explained in that guide.
Target `runtime_path` values resolve from the generated package or crate
directory; see the target's installation guidance.

Kotlin, F# and Dart are 0.1.3 targets. They require the 0.1.3 or newer
compiler and matching wrapper/core runtime sources; the 0.1.2 compiler
does not generate these targets.

Targets and the mock server (`tungsten mock`) share one IR. Generated-package and contract checks exercise each language, but source availability and passing shared scenarios do not establish complete validation, protocol or distribution parity. Unsupported features can vary by target. Review all compiler diagnostics: `--strict` rejects warnings, including unsupported `if`/`then`/`else` constraints, but informational diagnostics can still describe other unsupported features. Local checks do not establish live API acceptance or registry installation readiness.

## Install

Install the compiler first. The [runtime packages below](#runtime-packages)
support generated output; they do not install the `tungsten` compiler command.
The latest published compiler release is [v0.1.3](https://github.com/pboachie/tungsten/releases/tag/v0.1.3),
published October 19, 2026, and includes HTTP/Git inputs and Python 3.10/3.11
compatibility. This branch can contain changes after that release; see the
Unreleased section of the [changelog](CHANGELOG.md).

Prebuilt binaries for Linux, macOS and Windows are attached to each [GitHub release](https://github.com/pboachie/tungsten/releases). The installer checks the archive against the release's `SHA256SUMS` before it installs anything:

```sh
curl -fsSL https://raw.githubusercontent.com/pboachie/tungsten/main/install.sh | sh   # Linux and macOS, into ~/.local/bin
```

```powershell
irm https://raw.githubusercontent.com/pboachie/tungsten/main/install.ps1 | iex        # Windows
```

Pass `--version v0.1.2` to `install.sh` (or `-Version v0.1.2` to `install.ps1`) to pin a release, and `--dir` (`-InstallDir`) to choose the target directory.

See [`install.sh --help`](install.sh) or `Get-Help ./install.ps1 -Full`
([PowerShell script](install.ps1)) for environment variables and exit codes.
The installers in this source require build provenance by default. Install
[GitHub CLI](https://cli.github.com/) 2.102.0 or newer through its official
installation instructions first; older versions do not safely enforce the exact
workflow identity. The verifier must accept the archive for `pboachie/tungsten`,
`.github/workflows/release.yml`, issuer
`https://token.actions.githubusercontent.com` and the requested release tag.
Attestations are obtained from `github.com`; verification needs access to GitHub
and its trust services. Missing/unusable verifiers, unavailable or invalid
attestations, and identity mismatches refuse installation. The installer does
not bootstrap a verifier or silently choose a weaker mode.

To intentionally install using only `SHA256SUMS`, set
`TUNGSTEN_NO_ATTESTATION=1`. The installer prints a checksum-only warning and
still rejects checksum mismatches. `TUNGSTEN_REQUIRE_ATTESTATION=1` remains an
explicit assertion of the default policy and conflicts with that opt-out;
setting it to `0` does not disable default verification. Both settings accept
only `0` or `1`.

`TUNGSTEN_REPO`, `TUNGSTEN_RELEASE_BASE_URL` and PowerShell `-BaseUrl` change
download locations, not trusted signer identities. Mirrors can supply the same
official release bytes for verification. Custom builds or restricted/offline
mirrors without verifiable official provenance need the explicit checksum-only
choice; their origin is not authenticated by a matching checksum. Older tagged
installers retain their released behavior.

On Linux the `musl` archives (the installer default) are statically linked and
avoid a glibc dependency. Prefer them on older distributions. The published
**v0.1.2** `gnu` archives (`TUNGSTEN_LIBC=gnu`) require glibc **2.34 on x86_64**
and **2.39 on aarch64**, as verified from their required symbol versions. Check
your architecture and glibc version before selecting a stable GNU archive:
`install.sh` currently checks only a 2.34 floor and does not enforce the stable
aarch64 archive's higher requirement. The 0.1.3 release targets
a common glibc 2.34 floor for both architectures; that target does not describe
the published v0.1.2 binaries.

Release archives, the Homebrew formula, the winget manifests and the `ghcr.io/pboachie/tungsten` container image carry build provenance attestations. To verify them with the GitHub CLI:

```sh
gh attestation verify oci://ghcr.io/pboachie/tungsten:0.1.2 --repo pboachie/tungsten
gh attestation verify tungsten.rb --repo pboachie/tungsten
```

To run the guided `tungsten init` setup right after installing, pass `--init` to `install.sh` (`-Init` to `install.ps1`). It needs a terminal, and the installer prints the PATH note first. A cancelled setup leaves the installed binary in place and exits with status 4 (5 when setup fails); run `tungsten init` again when ready:

```sh
curl -fsSL https://raw.githubusercontent.com/pboachie/tungsten/main/install.sh | sh -s -- --init
```

The `irm … | iex` form cannot pass `-Init`. Run the downloaded script as a script block instead, or save it and run `.\install.ps1 -Init`:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/pboachie/tungsten/main/install.ps1))) -Init
```

Or build from source, with Rust 1.95 or newer (the repository pins 1.97.0 in [`rust-toolchain.toml`](rust-toolchain.toml) for development):

```sh
git clone https://github.com/pboachie/tungsten
cd tungsten
git checkout v0.1.3                        # latest release
cargo install --path crates/tungsten-cli   # installs the `tungsten` binary
tungsten doctor                            # reports optional tools (node, python3, uv, formatters)
```

Node, Python and the formatters are optional: a missing tool only disables the target or `--format` option that needs it.

### Check for a newer release

`tungsten update check` asks GitHub once (5 second limit, 1 MiB response cap) for the public
release metadata of this repository and compares the latest stable release with the version
you run. It sends no project data, installs nothing and does not change project pins or
generated clients. Add `--include-prerelease` to consider prereleases and `--json` for the
machine-readable result. A newer release is recommended only when it lists the archive for
your platform and `SHA256SUMS`. Upgrade guidance depends on how the CLI was installed: a
pinned installer command for the release installer, `cargo install` for cargo, and
package-manager guidance for npm. When the installation route cannot be determined,
the command says so and prints no replacement command.

A newer release is also classified in `upgrade`: `compatible` stays on your compatibility
line (a patch release before 1.0, a minor or patch release from 1.0) and is followed by
`tungsten generate --check`; `migration` starts a new line (a minor or major release before
1.0, a major release from 1.0), is a prerelease, or has a Breaking section in its notes, and
the guidance asks you to rehearse before replacing generated clients. Release notes can only
make the classification stricter. When a newer release is available, `rehearse_command` holds
the `tungsten upgrade rehearse --to <version> --workspace <dir>` invocation to run with the new
binary in each project; `tungsten upgrade apply` is the separate step that applies a reviewed
rehearsal.

| Exit code | Meaning |
| --- | --- |
| 0 | Up to date, or this build is newer than the newest release considered |
| 5 | A newer release with your platform's archive is available |
| 6 | Could not confirm: offline, opted out, timeout, network or rate-limit failure, invalid or oversized metadata, no matching release, or a newer release without your platform's archive |

`--offline` (also a global flag), `--no-update-check` and `TUNGSTEN_NO_UPDATE_CHECK=1`
make no request and exit 6 with a reason in the `--json` result.

Update notices are off by default. Enable them with `tungsten update notice --enable`
(`--enable prerelease`, `--disable`, `--interval-hours N`), which writes the user settings
file and keeps its other keys, or with `TUNGSTEN_UPDATE_NOTICE=stable` (or `prerelease`).
`tungsten update notice` alone shows the effective setting, where it came from, the file
paths and the cached answer. The user settings file is `$TUNGSTEN_CONFIG_DIR/settings.json`
if set, else `$XDG_CONFIG_HOME/tungsten/settings.json` (`~/.config/tungsten/settings.json`),
`~/Library/Application Support/tungsten/settings.json` on macOS or
`%APPDATA%\tungsten\settings.json` on Windows:

```json
{ "update_notice": { "channel": "stable", "interval_hours": 24 } }
```

With notices on, `tungsten update check` saves its verified answer to `update-check.json`
in the user cache directory (`TUNGSTEN_CACHE_DIR` overrides it). Other commands never make
a request for notices: after an interactive command, tungsten reads that file and, at most
once per interval (`TUNGSTEN_UPDATE_NOTICE_INTERVAL_HOURS`, 1 to 720, default 24), prints one
line on stderr naming a newer release whose archive and `SHA256SUMS` the check confirmed.
Answers older than 7 days are ignored. No notice is printed with `--json`, `--offline`,
`--policy`, in CI, when stderr is not a terminal, or for `lsp` and `mock`.
`TUNGSTEN_NO_UPDATE_CHECK=1` or `TUNGSTEN_UPDATE_NOTICE=off` turns notices off; an invalid
setting also turns them off. A missing, corrupt or unwritable cache never changes a command's
output or exit code.

### Runtime packages

For a published compiler release and generated client, choose the runtime for the
output you use. These package versions were verified on October 20, 2026;
Node 22+ is required for TypeScript/MCP, Python needs 3.10+, Java needs 17+,
C# needs .NET 10, and Ruby needs 3.3+. Run Python installation commands in an
activated virtual environment.

For current source builds, MCP servers require Node 22.13+ (Node 23 requires
23.2+); TypeScript SDKs and WebMCP build tooling retain their Node 22 prerequisite.

#### Node.js support

Upstream lifecycle as of October 2026: 22 and 24 are LTS, 26 is Current, and
20 is end-of-life. Odd-numbered branches, Node 18 and older, and unreleased
majors are not supported. A new major is promoted only after it passes the
package build lanes; a line is retired when upstream support ends.

| Artifact | Supported lines | Declared floor | Lanes checked in public CI |
|---|---|---|---|
| `@elysra/tungsten-runtime` and generated TypeScript SDKs (run) | 22, 24, 26 | `>=22` | 22.23.4, 24.22.1, 26.12.1 |
| `@elysra/tungsten-mcp` and generated MCP servers (run) | 22.13+, 24, 26 | `^22.13.0 \|\| >=23.2.0` | 22.23.4, 24.22.1, 26.12.1 |
| `@elysra/tungsten-webmcp` and generated WebMCP packages (build) | 22, 24, 26 | `>=22` | 22.23.4, 24.22.1, 26.12.1 |
| Node 20 (legacy) | Not supported | — | 20.20.2, advisory only |

The WebMCP floor covers its npm build tooling only. Generated WebMCP bundles run
in a browser that provides the WebMCP API. Passing an npm build does not make a
browser compatible. The core runtime entry point uses Web APIs only; its `/node`
export and the MCP runtime need Node. The Node 20 canary builds the packages and
imports the SDK and WebMCP runtimes for information only. The MCP runtime cannot
load on Node 20 because it needs `module.stripTypeScriptTypes` (Node 22.13 or
23.2). The tungsten compiler and generated Rust CLIs never need Node.

| Output | Runtime package | Install the published runtime |
|---|---|---|
| TypeScript SDK | [`@elysra/tungsten-runtime`](https://www.npmjs.com/package/@elysra/tungsten-runtime) | `npm install --save-exact @elysra/tungsten-runtime@0.1.3` |
| MCP server | [`@elysra/tungsten-mcp`](https://www.npmjs.com/package/@elysra/tungsten-mcp) | `npm install --save-exact @elysra/tungsten-mcp@0.1.3` |
| Browser WebMCP | [`@elysra/tungsten-webmcp`](https://www.npmjs.com/package/@elysra/tungsten-webmcp) | `npm install --save-exact @elysra/tungsten-webmcp@0.1.3` |
| Python SDK | [`tungsten-runtime`](https://pypi.org/project/tungsten-runtime/) | `python -m pip install tungsten-runtime==0.1.3` |
| Rust SDK | [`tungsten-runtime`](https://crates.io/crates/tungsten-runtime) | `cargo add tungsten-runtime@0.1.3` |
| Rust CLI | [`tungsten-cli-kit`](https://crates.io/crates/tungsten-cli-kit) | `cargo add tungsten-cli-kit@0.1.3` |
| Java SDK | [`io.github.pboachie.tungsten:tungsten-runtime`](https://central.sonatype.com/artifact/io.github.pboachie.tungsten/tungsten-runtime/0.1.3) | `mvn dependency:get -Dartifact=io.github.pboachie.tungsten:tungsten-runtime:0.1.3` |
| C# SDK | [`Tungsten.Runtime`](https://www.nuget.org/packages/Tungsten.Runtime/0.1.3) | `dotnet add package Tungsten.Runtime --version 0.1.3` |
| Ruby SDK | [`tungsten-runtime`](https://rubygems.org/gems/tungsten-runtime/versions/0.1.3) | `gem install tungsten-runtime --version 0.1.3` |

Verify a TypeScript runtime installation with
`node --input-type=module -e 'import("@elysra/tungsten-runtime").then(() => console.log("runtime ready"))'`,
or Python with `python -c 'import tungsten_runtime; print("runtime ready")'`.
Then install and build your generated package as its README describes. These
runtime imports do not generate a client or call an API.

Published 0.1.2 packages do not satisfy dependencies on 0.1.3 runtime
capabilities; use the 0.1.3 packages above, or matching runtime source as in the
quickstart below. Go supports matching stable source and the `v0.1.3` module. See
the [Rust installation](runtimes/rust/README.md) and
[Go installation options](runtimes/go/README.md). Runtime libraries do not install
the Tungsten compiler. The [runnable gallery](examples/README.md)
shows complete installation and first-use flows for each output.

### Released 0.1.2 consumers

Keep the compiler and generated clients on the released route when deploying
on 0.1.2 instead of the latest release. Pin the installer to `v0.1.2` as above and confirm
`tungsten --version` prints `tungsten 0.1.2`. Merge these exact runtime settings
into the targets you use in `tungsten.yml`; your output paths, API package names
and generated package versions remain your own:

```yaml
targets:
  typescript: {out: generated/typescript, runtime: "0.1.2"}
  python: {out: generated/python, runtime: "==0.1.2"}
  java: {out: generated/java, runtime: "0.1.2"}
  csharp: {out: generated/csharp, runtime: "0.1.2"}
  ruby: {out: generated/ruby, runtime: "= 0.1.2"}
  mcp: {out: generated/mcp, runtime: "0.1.2", sdk_runtime: "0.1.2", sdk_path: "../typescript"}
  webmcp: {out: generated/webmcp, runtime: "0.1.2", sdk_runtime: "0.1.2", sdk_path: "../typescript"}
```

Run `tungsten generate .`, then follow each generated README to install and
build it. Build the TypeScript SDK before the adapters that reference it.
The 0.1.2 compiler's native runtime defaults predate the 0.1.2
packages, so keep the Java, C# and Ruby overrides explicit. Registry
consumers use those packages without a runtime source checkout.

The [stable Rust source recipe](runtimes/rust/README.md#stable-012-sdk-and-cli-from-source)
uses the unchanged `v0.1.2` checkout for both Rust crates. This source route
remains valid and does not require a 0.1.3 upgrade; the 0.1.3 crates are also
available from crates.io. Newer compiler features require matching runtime source or
the 0.1.3 packages; Python 3.10/3.11 output belongs to the 0.1.3 route. Generated SDK compatibility still needs qualification against
your own API and deployment.

For Go, build the external emitter from `v0.1.2` and set `runtime_path` to that
checkout's `runtimes/go`; the [stable Go recipe](runtimes/go/README.md#released-012-compiler-and-emitter)
retains its Apache source/license context and avoids the old pseudo-version
ZIP's inherited repository-root notice.

## Quickstart

Install the compiler above first. This walkthrough uses Python 3.12 or newer and
[uv](https://docs.astral.sh/uv/getting-started/installation/). TypeScript targets
require Node 22 or newer. Installing the compiler does not install generated
SDKs or their language runtimes.

This walkthrough continues from the 0.1.3 source installation above, using the same
checkout for the compiler and runtime. From that `tungsten` directory, set
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

The walkthrough uses an OpenAPI document, the native input. `init --from` also accepts Swagger 2.0, Postman collections, saved cURL commands, `.http`/`.rest` files, HAR captures and endpoint manifests — one of them, not a bundle — each with its own flags and review steps; see [Supported inputs](#supported-inputs).

The [importing guide](https://pboachie.github.io/tungsten/importing.html) gives
per-format commands, diagnostic families, saved-command shell dialects, HAR path
review and the supported JSON Schema subset. Imported operations stay unreviewed
until you explicitly review their safety and allowed origins in `agent.yml`.

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

Redirects default to bounded same-origin reads; mutations never follow redirects
under any policy. Set `defaults.redirects: none` in `agent.yml`, or the SDK client's
`redirects` option to `none`, to follow no redirect at all. A 3xx the operation
declares (exactly or by class) is still its success response; a 3xx it does not
declare fails as `UNEXPECTED_RESPONSE` without a second request or retry.
Either `none` setting wins: client options cannot relax a manifest restriction.
Rust and C# require the runtime-owned HTTP client for this policy; omit a custom
`http_client`/`HttpClient`. Custom fetch/transport callbacks must honor the runtime's
no-follow contract. Upgrade runtimes together with generated clients; older
runtimes fail explicitly when a generated client requires this capability.

Python's `ClientOptions.timeout_ms` covers each native HTTP attempt through headers
and the complete ordinary body, with cancellation cleanup joined before capacity
is released. It is not an overall deadline across retries. Streaming bodies use
the idle timeout after successful headers. Custom transports must cooperate;
uninterruptible callbacks retain capacity until completion and late results are
rejected, without a hard settlement deadline. OS resolver cancellation depends on
the platform.

Migrating to the total deadline (unreleased 0.2.0): `timeoutMs` and its
equivalents keep their meaning. They bound one attempt, so retries, backoff,
authentication, pages and verification can together take several times as long.
To bound what a caller waits for one logical call, set the new opt-in total
deadline as well: `deadlineMs` (TypeScript), `deadline_ms` (Python), `deadline`
(Rust), `Deadline` (Go), `--deadline <SECONDS>` with `<PREFIX>_DEADLINE` or the
profile key `deadline` (generated Rust CLI), and `toolDeadlineMs` with
`<API>_MCP_DEADLINE_MS` (MCP server, per tool call). Other runtimes describe
their support in their README's `Limits` section. With both set, each attempt
gets at most the time the deadline has left, and queue admission waits no longer
than the deadline. No attempt starts after the deadline expires (`code`
`DEADLINE_EXCEEDED`). A mutation already sent at that point reports
`OUTCOME_UNKNOWN` and is never replayed blindly, so retry it only with the same
idempotency key. An open event stream is not cut by the deadline; its idle and
collection limits apply. Shared token refreshes and custom transports or stores
that ignore cancellation are no longer waited for, but they are not stopped.
Without a deadline, behaviour is unchanged.

For a saved collection, start with [Postman import and contract review](examples/input-postman/README.md).
For an API change, use the [stale-output and regeneration walkthrough](examples/ci-drift/README.md).
Both build on the existing gallery recipes.

More examples: the [runnable example gallery](examples/README.md) covers first
SDKs, supported input routes, local mocks, destructive confirmation and CI/surface
review workflows. [Typed Python SSE](examples/typed-sse/README.md) shows how to
declare or overlay an incomplete stream response and consume named events with
sync/async cleanup. [Response-contract quality](examples/response-contract-audit/README.md)
shows the opt-in audit and reviewed schema overlays without inferring guaranteed
types. [Tungsten on six public APIs](examples/showcase/README.md)
uses the public OpenAPI descriptions of Stripe, GitHub, Twilio, OpenAI, Kubernetes
and Discord to show diagnostics, MCP listing sizes and generated SDK code in
TypeScript, Python and Rust, reproducible with one script. The
[external Markdown emitter](examples/external-emitter-markdown) shows how to
write an external emitter.

An `external:` target in `tungsten.yml` names a command that tungsten starts as a process, unsandboxed and with your privileges. It runs on every command that produces output in memory, not only on a writing `generate`: `generate --dry-run` and `--check`, `check --ci`, `diff` and `report` run it too (plain `check` and `ir dump` do not). A CI job that runs `tungsten check --ci` on pull requests therefore executes whatever the pull request puts in `tungsten.yml`. Pass `--no-external` or set `TUNGSTEN_NO_EXTERNAL=1` to skip external targets (reported as TG0807) when the manifest is not trusted.

Hosts can explicitly select stricter external-emitter constraints with both
`--policy PATH` and `--policy-digest blake3:<64 lowercase hex digits>`. First review
a local JSON document, for example:

```json
{"tungsten_policy":1,"external":{"deny":true,"timeout_ms":10000,"max_output_bytes":1048576}}
```

Run `tungsten policy digest PATH` separately to validate and hash those exact
bytes; it prints the core BLAKE3 digest and writes no files. Execution never
creates or updates a pin. A relative policy path resolves against invocation cwd.
There is no automatic discovery, environment selection, include, interpolation,
URL fetch or replay from exported configuration. Selected symlink/reparse entries,
nonregular files and unsupported local namespaces are refused. Capture reads one
regular handle once, with a 64 KiB byte limit and nesting limit eight; strict JSON
rejects duplicate/unknown members, null, trailing data and empty constraints.
`deny` is a boolean; timeout is 1..=3,600,000 ms and output is 1..=2^40 bytes,
subject to platform representation. Missing constraints add no restriction.

| Command | Supplied execution policy |
| --- | --- |
| `generate` (write, check, dry-run), `check` (including CI), `report`, `diff`, `emitters` | Capture and enforce before external run/descriptor probes |
| `config` | Capture and inspect, without acquiring API inputs or running tools |
| `policy digest` | Reject execution-policy flags; validate/hash only its named file |
| `doctor`, `init`/wizard, `scenario`, `lsp`, `mock`, `ir`, `overlay`, `explain`, `capabilities`, `schema`, any unlisted command | Reject supplied policy before reads or command effects |
| `--help`, `--version` | Parser-owned; no policy read |

Embedding hosts select immutable `CapturedPolicy` documents with expected digests,
then a fresh `InvocationPolicy` for each invocation. `CliEnv.policy` intersects
the CLI selection instead of replacing it; `CompileOptions.policy` stays outside
editor overrides. At most 16 submitted documents and 1 MiB of captured bytes are
accepted, counting duplicates. Denials combine by OR and ceilings by minimum;
`false` or a weaker later document cannot undo a restriction. Config inspection
reports opaque `policy[N]` sources, exact digests, known pointers and all contributing
constraints, including ties, without physical paths or replayable authority.
Absent policy retains existing behavior and is reported as `absent`; a permissive
selection is never approval. Prepared IR does not retain a fallback context.
Long-lived hosts must explicitly recapture/revalidate pins for new invocations.

Library callers must migrate `external::run` and `external::describe` to supply
`&InvocationPolicy` and raw `RequestedExternalLimits`; a precomputed `Limits`
cannot substitute for context. Select `InvocationPolicy::absent()` explicitly
only when legacy behavior is intended. Keep missing/default requested values as
`None`, rather than converting them to explicit defaults that could exceed a
protected ceiling. Exhaustive `CliEnv`/`CompileOptions` literals also need their
new optional `policy` field (or `..Default::default()`).

Explicit project timeout/output values above a protected ceiling fail (TG0810)
before input acquisition or external descriptor negotiation. Missing/null project
limits use existing defaults, then clamp to the ceiling. Descriptor probes use
their own 5 s/1 MiB defaults, also clamped. Generation requests advertise the
effective output cap. Denied execution is TG0811 and fails the requested operation;
`--no-external` remains an independent unconditional TG0807 skip. Invalid captures
or mismatched pins are TG0809. `tungsten schema execution-policy` publishes the
strict document shape.

These are per-external-operation limits, not whole-command time or heap limits.
The exact pinned capture is authoritative for that invocation; canonical paths
are local provenance, not replacement-safe executable identity. Restrictions do
not verify parent symlinks/junctions, mounts or mapped-volume locality; a selected
regular file can reside on network-backed storage, and OS file operations are not
hard real-time bounded. They do
not approve executables, minimize inherited environment, revoke running work,
protect acquisition Git/providers, doctor tools, setup/package managers or
scenario subprocesses, or isolate filesystem/network access. Policies do not
establish a sandbox.

External emission and `--describe` own a Unix process group or Windows Job Object.
Leader exit, timeout and output overflow trigger group/job termination and leader
reaping. `timeout_ms` includes process launch, pipe work and cleanup; one quarter,
up to one second, is reserved for cleanup. OS calls and scheduling can exceed that
budget. Zero or unrepresentable timeouts refuse execution. Each launch reserves
four of 16 process-wide worker slots. Unfinished workers retain
their handles and slots until they finish and a later invocation joins them;
capacity exhaustion refuses before execution. TG0803 reports incomplete cleanup
and accepts no files. Investigate remaining emitter processes before retrying.
Unix descendants can escape their group or pass pipes elsewhere; these lifecycle
controls do not establish that every descendant stopped, approve an executable,
filter inherited credentials or isolate filesystem/network access. Library
embeddings must exclusively reap their external children and must not auto-reap
children or change SIGCHLD during execution.
On Linux, an unreadable `/proc/self/status` or an ignored/caught SIGCHLD disposition
refuses external execution before launch. That check cannot detect every signal
flag or competing reaper; the embedding ownership requirement still applies.

The same opt-out disables `tungsten emitters` discovery, including `--describe`
probes of executables on PATH. Its empty result means discovery was disabled;
it does not establish that no emitters are installed. Without the opt-out,
discovery executes those programs and requires the same trust as generation.

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

<p align="center"><img src="assets/architecture.svg" alt="Choose one API input: native OpenAPI, imported descriptions or an explicit manual endpoint. Project manifests and optional reviewed overlays are separate configuration. A shared frontend, typed IR and agent compiler feed ten SDK languages, a Rust CLI, MCP, browser WebMCP and agent documentation. Generated packages use matching runtimes; restrictions and availability are listed in the input and target matrices." width="1000"></p>

1. The **frontend** reads OpenAPI 3.0 and 3.1, resolves references and applies overlays. The other supported inputs (Swagger 2.0, Postman, saved cURL, `.http`/`.rest`, HAR, endpoint manifests) are converted offline to a normalized OpenAPI document first; `init` writes it as `imported.openapi.json` and the frontend reads it like any OpenAPI document.
2. The **IR** is a versioned JSON document (`tungsten ir dump`, schema via `tungsten schema ir`).
3. The **agent compiler** merges `agent.yml` into it: safety tiers, idempotency, confirmation, remediation text, verification hooks, macros and the disclosure mode for MCP.
4. **Emitters** (`crates/tungsten-emit-*`) write each target from the IR. They are pure functions of their inputs.
5. **Runtimes** (`runtimes/`) hold the behaviour shared by generated code: validation, auth, retries, idempotency stores, the envelope, previews, pagination and middleware.

Repository layout: `crates/` is the compiler (AGPL-3.0-only); `runtimes/ts`, `runtimes/mcp`, `runtimes/webmcp`, `runtimes/python`, `runtimes/rust`, `runtimes/go`, `runtimes/java`, `runtimes/kotlin`, `runtimes/csharp`, `runtimes/fsharp`, `runtimes/dart` and `runtimes/ruby` contain the libraries generated code depends on (Apache-2.0).

## CLI

```
tungsten [--json] <command>
```

| Command | Purpose |
|---|---|
| `init` | Write project manifests from any supported input ([Supported inputs](#supported-inputs)): `--from`, `--dir`, `--name`, `--targets`, `--force`; `--targets webmcp` includes a linked TypeScript SDK |
| `workspace` | List the projects under a directory (bounded discovery, never following links) with their ids, outputs and inputs, and report outputs two projects share (TG0642); `--project ID` and `--affected FILE` select projects, `--run check\|generate\|lock -- ARGS` runs a command on each, one at a time. `--project ID` on `check`, `generate`, `lock` and the other project commands runs on one project of the workspace at PATH |
| `check` | Validate the manifests and specs and report diagnostics (`--ci`, `--strict`); opt into response quality with `--audit-contracts` or audit-only enforcement with `--require-contracts`; `--ci` runs the emitters in memory, external ones included (`--no-external` skips them) |
| `generate` | Generate every configured target (`--target`, `--dry-run`, `--check`, `--force`, `--strict`); `--locked` refuses (TG0128) unless the project matches `tungsten.lock.json` and acquires inputs offline |
| `ir dump` | Write the IR as JSON (`--out`, `--compact`) |
| `explain` | Explain a diagnostic code (`TG0201`), an operation or a type |
| `schema` | Print a JSON Schema: `tungsten`, `agent`, `ir`, `cli-output`, `external-emitter`, `sdk-descriptors`, `tool-catalog`, `generation-lock`, `project-bundle` |
| `mock` | Serve a mock of the API from the IR (`--port`, `--seed`, `--gate`, `--scenario`) |
| `scenario validate` / `scenario run` | Validate a saved scenario, or run explicit integration argv with exit and call-count assertions (Linux run) |
| `report` | Coverage, safety matrix, token budgets and diagnostics, as text, JSON or one static HTML file (`--html`); only configured targets run, `--all-targets` runs every known emitter |
| `diff` | Show regeneration changes; `--semver` classifies the API surface, `--migration` adds upgrade actions and runtime requirement review |
| `lsp` | Serve diagnostics, quick fixes, completion, hover, go-to-definition and symbols for `tungsten.yml`, `agent.yml` and overlays to an editor over stdio (`--config`) |
| `doctor` | Report external tools; pass a project path and `--strict` to fail when its required tools are missing or too old |
| `capabilities` | Query built-in target declarations without running tools or emitters; optionally select a target id or alias, such as `capabilities py --json`; `--matrix` prints the Markdown support matrix derived from the same declarations, and `--matrix --format markdown` wraps it in the markers of the README block CI checks |
| `lock` | Capture the project generation lock `tungsten.lock.json` offline: content identities of the manifest, profiles, agent manifest, inputs, references and overlays (in order), compiler and built-in emitter identities, effective target option digests and consumer toolchains. `--check` fails when the lock is missing, stale or corrupt, `--preview` writes nothing, `--update` accepts changed identities; `--outputs` also records or verifies the identity of the bytes each built-in target generates |
| `bundle export` / `bundle import` | Export the locked input closure and `tungsten.lock.json` into a new plain bundle directory with a digest inventory (`--preview` lists it; `--include-input-cache` carries the verified pins of remote inputs; `--include-consumer-cache DIR` carries a prepared consumer dependency directory with provenance and platform notes; incomplete or credential-named content needs `--allow-incomplete` / `--allow-sensitive`), or validate a bundle completely and copy it into `--dest` without overwriting differing files. Neither runs emitters or grants trust |
| `support-bundle --preview` | Inspect the exact metadata-only support manifest without writing files; `--exclude <id>` removes an entry. On Linux, macOS and Windows, `--output <PATH>` instead publishes it to a new file in an existing, trusted directory; `--inspect <FILE>` checks and shows an existing bundle |

With `--json`, stdout carries exactly one JSON document described by `tungsten schema cli-output` (`lsp` speaks the language server protocol on stdio instead). Color is used only on a terminal and never when `NO_COLOR` is set.

### Inspect and export support metadata

`tungsten support-bundle --preview --json` shows a versioned manifest containing
allowlisted tool, capability and configuration provenance, diagnostic codes and
declared prerequisites. Collection works with a broken project and does not
compile it, read API documents, probe tool versions, run extensions or contact a
network. It excludes configuration values, private paths, URLs, credentials,
source text, logs, hashes and project identifiers. Use `--exclude <id>` to omit
an entry.

Logs and configuration files are never collected unless you name them.
`--attach-log <FILE>` and `--attach-config <FILE>` (repeatable, at most four in
total) add `user.attachment[N]` entries, numbered from 0 with log files first.
Each entry has origin `user_supplied` and sensitivity `user_supplied_text`.
Such a bundle uses `bundle_schema_version: 2` with privacy level
`metadata_and_user_supplied`; without attachments the bundle stays version 1
and unchanged. Each file must be a regular, nonsymlink file, and its path is
never recorded. Only its last 8 KiB are kept, starting at a line boundary.
Automatic redaction then replaces environment variable values, values of
credential-like keys (`password`, `token`, `secret`, `api_key`, `authorization`
and similar), `Bearer`/`Basic` credentials, private-key blocks, URLs, user names
in home directory paths and long token-like strings, and counts its
replacements. Redaction is not a guarantee that arbitrary text is safe to
share: review the preview, and use `--exclude user.attachment[N]` to drop an
attachment. An unreadable attachment fails the command before anything is
written.

On Linux, macOS and Windows, `tungsten support-bundle --output <PATH>` creates
a new local file.
Its bytes are exactly the same manifest serialized as compact JSON with one
trailing newline, capped at 128 KiB including metadata; entry payloads retain
their 16 KiB individual and 64 KiB combined limits. The JSON command result is
a separate receipt containing the manifest, complete artifact size, publication
state and temporary-file cleanup state. The manifest's `mode: "preview"` and
`wrote_files: false` describe inspection; the receipt records publication.

The destination directory must already exist, belong to the effective user
and have no group or other write permission. Traversed directories must be
owned by that user or root and protected from other-user replacement;
root-owned sticky ancestors such as `/tmp` are allowed. Symlink directories,
`..` traversal and existing destination entries are refused. Publication uses
an exclusive file with mode 0600 and never replaces an existing entry. These
checks do not protect against root, another process with the same user ID,
permission changes by those actors or a filesystem that violates local file
operation semantics. Every path component is opened without following
symlinks, so macOS paths through the `/tmp`, `/var` and `/etc` symlinks are
refused; use the `/private/...` path or a directory in your home folder.
Publication needs hard-link support in the destination filesystem, which some
FAT, exFAT and network volumes lack. On Windows, every directory from the
drive root to the destination is held open without delete sharing, so none
can be renamed or replaced during export; reparse points (symlinks and
junctions), network shares, device paths, reserved device names and
alternate data streams are refused. Windows does not check the directory's
owner or ACL: export into a directory only you can write, such as one in your
user profile. Other platforms refuse export before collecting data; preview
remains available.

Collection, for preview and export alike, stops after 30 seconds with nothing
written. It only reads, so an abandoned collection has no side effects; in an
embedding process its worker thread may keep running until it finishes.

Library callers can cooperatively cancel an export before publication with
`run_with_cancellation`. Successful publication is the commit point: a later
cancellation does not remove the file. A cleanup failure after publication
returns a nonzero status while retaining `published: true` in the receipt.
On Linux, the `tungsten` binary blocks SIGINT and SIGTERM for the duration of
an export and turns them into the same cooperative cancellation: an
interrupted export stops collection or writing within about 50 ms, removes its
temporary file and exits with status 4 without publishing. A signal after
publication leaves the published file and the success status in place. On
macOS and Windows the default signal and console-control behavior still
applies. Abrupt termination (SIGKILL, a crash or power loss), interrupts on
macOS and Windows, or unconfirmed cleanup can leave one owned
`.tungsten-support-*.pending` entry per invocation; later commands do not sweep
existing temporary entries. File syncing before publication does not establish
a power-loss durability guarantee. There is no automatic upload or issue creation.

`tungsten support-bundle --inspect <FILE>` reviews an existing bundle on any
platform before you share it. It reads one regular, nonsymlink file of at most
128 KiB, writes nothing and starts no program. A bundle whose
`bundle_schema_version` this tungsten does not read is refused with that
number; versions 1 and 2 are read. Otherwise every object must have exactly
the keys of that schema version, and the six observed entries must be
metadata only. Version 2 may follow them with up to four entries labelled
user supplied. Recorded sizes, statuses and totals must match the payloads.
Free text in observed entries is limited to printable ASCII, and attachment
text to 12 KiB without control characters other than newline and tab. The result lists each entry with its exact payload. Errors name
only schema locations such as `/entries/0/payload/os`, never file content or
unexpected keys. These checks confirm the bundle's shape, not that its text is
safe to publish.

Generation prepares compatible Java, Kotlin, C#, F#, Dart and Ruby SDK structure
once per command, preserving each target's naming. Writing runs stage one
target's generated bytes at a time in an owned system temporary directory and
verify them before applying that target. `--strict` finishes every emitter's
diagnostic preflight before any destination writes. Temporary staging also
applies to `--dry-run`; destination files remain unchanged. Staging metadata is
limited to 262,144 files and 64 MiB of accounted path/digest/index metadata across
the selected targets; exceeding a limit fails with TG0701. This bounds retained
output metadata, not total compiler memory or temporary disk use. `--check`,
`check --ci`, `diff` and `report` process each target's output in memory.

Custom files remain user-owned and obsolete generated files are removed using
the previous output manifest. Output writes are not a transaction across files
or target directories. Cancellation or an I/O failure during application can
leave earlier files written or removed; the failing file may also be partly
changed. TG0701 reports completed write/removal counts.
Correct the failure and run generation again. Cooperative cancellation checks
run between file operations; a single emitter pass or file operation may finish
first. Cleanup of owned temporary files is attempted when the command unwinds;
forced process termination or a cleanup error can leave a temporary directory.

### Saved mock scenarios

`tungsten mock . --scenario retry.json --port 0` validates a saved scenario
against the project's callable operations, then starts a fresh loopback server
with its responses queued. The listening URL is printed in the normal report
(`result.base_url` with `--json`). Each process owns its port and mock state.
Files never run commands or expand environment variables. You can launch your
integration separately and point it at that URL, or use the explicit runner
below to manage its lifetime and assert the final result.

For an operation named `openapi.listPets`, this version 1 JSON file expects a
rate limit followed by one successful retry (replace the operation with one in
your project):

```json
{
  "version": 1,
  "programs": [
    {"operation": "openapi.listPets", "status": 429, "headers": {"Retry-After": "0"}},
    {"operation": "openapi.listPets", "status": 200, "body": []}
  ],
  "expect": {
    "total_calls": 2,
    "calls": [
      {"operation": "openapi.listPets", "status": 429, "count": 1},
      {"operation": "openapi.listPets", "status": 200, "count": 1}
    ]
  }
}
```

After the integration has finished and no calls are in flight, fetch
`GET /__tungsten/scenario`. HTTP 200 means the report was read: **assert its
`passed` field**, which is true only when the total and every listed count match
and all queued responses were consumed. `calls` in the report follows the
file's assertion order; omitting an assertion's `status` counts every call of
that operation. Status `0` counts dropped connections. Counts remain accurate
even when `--max-recorded-calls` evicts request payloads. Reports contain counts
only, without operation names, credentials or bodies. A snapshot can change
when another request arrives; it does not stop the server or prove provider
effects. In particular, a recorded write is not proof of an applied write.

Optional `expect.effects` entries assert what the mock did with an operation's
requests: `applied` counts fresh requests it processed to a 2xx answer, even when
that answer was then dropped (`drop-after-write`) or replaced (`status=...;apply`);
`replayed` counts requests answered from a stored idempotency key. Programmed
responses are neither. State at least one of the two counts. This recipe checks
that a client retries a lost write with the same `Idempotency-Key`, so the write
is applied once and replayed once:

```json
{
  "version": 1,
  "programs": [
    {"operation": "openapi.createPet", "inject": "drop-after-write"}
  ],
  "expect": {
    "total_calls": 2,
    "calls": [
      {"operation": "openapi.createPet", "status": 0, "count": 1},
      {"operation": "openapi.createPet", "status": 201, "count": 1}
    ],
    "effects": [
      {"operation": "openapi.createPet", "applied": 1, "replayed": 1}
    ]
  }
}
```

A client that sends a new key on the retry still matches both call counts, but
reports `applied` 2 and `replayed` 0, so the scenario fails. The report lists
`effects` in file order only when the file declares any. Effect counts describe
the mock's own processing; they do not prove a real provider's behavior or that
the client received a response.

The [mock-scenarios gallery recipe](examples/mock-scenarios/README.md) runs
429-then-success, interrupted-stream, malformed-response and lost-write
scenarios against a small integration, and shows each deliberately broken
retry failing.

Programs use the existing `/__tungsten/program` controls: `status` (200–599),
optional JSON `body`, string-valued `headers`, `times`, error `code`, and
`cut_after` bytes; or `inject` with optional `times`. For malformed JSON, use a
string body and an `application/json` header. For stream interruption, use
`inject: "drop-mid-stream"` or a stream string body with `cut_after`. For a lost
write response, use `inject: "drop-after-write"`; the mock's existing validation
and idempotency behavior still applies. `inject: "reset"` is unsupported in a
program because it acts before routing. Unknown fields and unsupported controls
are rejected before the port is bound.

Version 1 limits each file to 1 MiB, 256 programs, 256 call assertions, 256
effect assertions, 10,000 programmed calls and at most 10,000 expected total calls. Timeout/stall injection
holds cannot exceed 30 seconds. These are input limits, not an overall run
deadline. Keep scenarios synthetic: bodies and headers are served as written,
and the separate `/__tungsten/calls` endpoint still exposes recorded data.
Stop the server with Ctrl-C or close its stdin when done. Existing
`POST /__tungsten/reset` clears responses, recordings and scenario assertions;
restart with the same file to rerun it. Without a loaded scenario the assertion
endpoint returns 404. These manual-server controls remain available for local
exploration; the runner keeps its loaded responses and assertions immutable.

Validate without binding a server or launching a process on any platform:

```sh
tungsten scenario validate . --scenario retry.json
```

On Linux (including WSL), macOS or Windows, run your integration explicitly:

```sh
tungsten --json scenario run . --scenario retry.json --timeout-ms 30000 --expect-exit 0 -- python3 integration.py
```

The integration reads its URL from `TUNGSTEN_MOCK_BASE_URL`. Arguments after
`--` are passed directly to the named executable. There is no implicit shell;
commands cannot be supplied by the saved file. An interpreter such as `python3`
runs only when explicitly named by the caller. On Windows, a bare name resolves
only to an `.exe` (for example `python`); name a script shim in full, such as
`npm.cmd`. Other platforms reject `run` before input acquisition, port
binding or process creation; `validate` remains portable.

`--timeout-ms` bounds the whole command. It starts before input acquisition
and compilation, and covers scenario validation, server startup, process
execution, cleanup and final assertions; a small part of the time left for the
run is reserved for cleanup. Cancellation is also observed during acquisition
and compilation. A run stopped there reports `timed_out` or `cancelled` with
`validated: false` and never binds a port or starts the integration. Inputs
still being acquired are abandoned, not interrupted: a remote fetch continues
in the background only until the process exits or its own acquisition limit
expires. Library callers' `RunOptions.timeout` remains the run-phase budget.
The runner owns a fresh loopback port and a process group (a Job Object on
Windows), stops
its server before reading final counts, and fails on cancellation, timeout,
cleanup failure, unfinished admitted requests, unexpected exit or unmet counts.
Request completion tracks server-side handler and response-body generation,
independently of when a call was counted; it does not prove transport delivery.
The binary handles SIGINT/SIGTERM for this run (Ctrl-C, Ctrl-Break and console
close on Windows); library callers supply explicit cancellation instead of
installing signal handlers. Linux uses a scoped signal mask; macOS and Windows
install one Ctrl-C handler in the binary on first use. The Linux binary refuses an
unreadable or malformed `/proc/self/status` and observable ignored/caught SIGCHLD
dispositions before acquiring inputs or creating a server or child. Embeddings
must exclusively reap their runner children, avoid auto-reap dispositions and
keep SIGCHLD disposition stable; the binary preflight does not establish these
conditions for a concurrent embedding or inspect every `SA_NOCLDWAIT` setting.
macOS has no equivalent preflight; the same embedding requirements apply.
Windows terminates the Job Object on cleanup. Process groups and Job Objects cover ordinary
descendants, not processes that deliberately leave the group. Signal delivery
and leader reaping do not prove that every descendant has settled, and this is
not a sandbox.

Runner connections are capped at 32 before request bodies are read, bodies at
1 MiB, and admitted API calls at 10,000 (`--max-calls` can lower that limit).
Excess admission or attempts to mutate control state fail the run. Recorded-call
payloads are not retained; the bounded 64-entry idempotency cache can retain
request fingerprints (including body bytes) and responses. Child stdin, stdout
and stderr are disconnected. The machine report includes stable status and
counts without child output, argv, PID, port or timing. `result.run.passed` requires the expected exit code (default
0), matching counts and consumed responses. The command exits 0 only on success;
run assertion/cancellation failures exit 1. Count assertions still do not prove
provider effects, including applied writes or exactly-once behavior.

`capabilities` returns contract version 1: target ids/aliases, source implementation,
runtime family, imported-operation policy support and local consumer prerequisites.
The same declarations drive target aliases and `doctor --strict`, including Kotlin's
Java/Gradle tools, F#'s .NET SDK and Dart's `>=3.6 <4.0` SDK. Python's default
minimum is 3.12; `doctor` resolves the declared `python_version` option for 3.10
or 3.11 projects. Prerequisites describe local builds, not compiler execution or
browser runtime support. Qualification, platform qualification and artifact
availability are explicitly `unknown`: these source records do not prove a
tested combination or published package. External emitters are not inspected by
this command. Fields may be added within version 1; consumers should reject
unknown major contract versions before interpreting them.

The TypeScript declaration also includes `options_schema`, a JSON Schema for
supported raw `package`, `version`, `runtime`, `runtime_path` and `dependencies`
values. The `ts` alias returns the same declaration. Python and Rust declare
their typed options the same way. Every other built-in target declares a field
schema with the JSON type, fixed default and description of each option it
reads (besides `out`), plus the warning code an invalid value produces; the
emitter keeps the semantic validators, and `config` inspection covers only the
TypeScript, Python and Rust schemas. The reserved `mock` target reports null.
This does not change generation policy:
invalid TypeScript scalars still produce TG0711 warnings and use defaults;
invalid custom dependencies produce errors. Unknown keys remain ignored, and
defaults that depend on the compiled API remain in the emitter. This is an
option declaration; existing project target validation remains compatible.

The catalog also registers the IR's protocols under `protocols`. Every IR
namespace without a `protocol` binding is HTTP, which every built-in target
generates; `graphql`, `grpc` and `async_messaging` are reserved for future
frontends, which keep the common operation contract and put
protocol-specific contract in the binding's `extension` rather than in REST
fields. A target is refused with TG0706, before anything is written for it,
when the IR binds a protocol the target does not declare.

### Local TypeScript profiles

Select ordered local defaults in `tungsten.yml`:

```yaml
profiles:
  - path: profiles/sdk.yml
    root: profiles
```

Both selection paths are relative to the project manifest; `root` is an explicit
existing include directory. A profile outside the project may be deliberately
selected with relative `../` paths. Absolute, drive-relative and rooted Windows
paths are refused. Profile v1 is a YAML or JSON mapping:

```yaml
tungsten_profile: 1
includes: [base.yml]
targets:
  typescript:
    package: '@example/sdk'
    version: '1.0.0'
    runtime: '^0.1.3'
    dependencies:
      yaml: '^2'
```

Includes resolve relative to their declaring profile and remain inside the
selected root after symlink resolution. Traverse selections/includes left to
right, depth first, applying parents before their child and each canonical file
once at its first encounter. Every reused graph path still checks confinement
and depth. Limits per compilation are 1 MiB per file, 16 MiB of unique captured
bytes, 64 identities/selections, 256 include edges and depth eight (roots at zero).
Profile reads use the shared source provider, including unsaved local files in
existing directories, and capture each identity once for that compilation.

Scalars override individually; dependency maps merge by package name. Explicit
project fields take precedence. Missing fields and empty dependency maps add
nothing; profile nulls and unknown fields are errors, with no deletion syntax.
The `ts` alias is accepted inside profiles, but declaring both aliases is an
error. Profiles cannot set project identity, inputs, agent/tools, executables,
output directories or `runtime_path`. Existing project invalid/null target
values keep their TG0711 warning/fallback or dependency-error behavior.

Project `doctor` checks the same composed target configuration, including a
TypeScript target introduced only by a profile. It validates the manifest and
bounded profiles before probing tool versions; it does not compile or acquire
API inputs, load agent content, or execute emitters.

The compiler driver returns typed declarations and winning/shadowed authored
origins in memory; emitter defaults still depend on the compiled API. Profile
source/digest names are portable (`profile[0]/base.yml`), while physical identity
stays in provenance for confinement. A name collision with another compilation
input is an error. This result is not a replayable approval document. Hosts can
select the external-emitter restrictions described above; executable approval
remains separate.

#### Transport and account references

Profiles and `tungsten.yml` can declare where a runtime finds enterprise
transport material, without holding it:

```yaml
transport:
  proxy: { env: CORP_HTTPS_PROXY }        # env only: proxy URLs can carry credentials
  no_proxy: { env: CORP_NO_PROXY }
  ca_bundle: { file: certs/corp-ca.pem }  # env or a relative file, relative to the project
  client_certificate: { env: CORP_MTLS_CERT }
  client_key: { store: "keychain:corp-mtls" }  # env or a secure-store item, never a file
  credential_provider: secure_store       # environment, secure_store or hook
  account: build-bot                      # non-secret names
  environment: staging
```

Fields compose like scalar target defaults: later profiles win, explicit project
fields win over profiles, and `tungsten config` shows each reference with its
winning and shadowed source. Inline values (a proxy URL, a PEM block, a token) are
refused, as is any key that would disable certificate verification; trust a
private CA with `ca_bundle` instead. References are names only: tungsten never
reads the variables, files or store items they point to, and generated SDKs do not
consume them yet. `tungsten doctor <project>` reports whether each referenced
environment variable is set and each referenced project file exists, without reading
either; `--strict` fails on a missing file, while variables may be supplied where
the client runs and store items are left to the runtime's adapter. Wire them into a native or custom transport that keeps TLS
verification, redirect/auth rules, deadlines and admission in place.

`tungsten lsp` serves profile files too: completion and hover from the profile
schema, diagnostics in the profile that caused them, and watchers for profile
directories outside the workspace folder.

#### Init presets

`tungsten init --preset NAME` starts from a named set of outputs; the same names
are accepted at the guided setup's outputs prompt. Explicit `--targets` and
`--strict` win, and a preset never selects inputs, reviews operations or changes
`agent.yml` safety defaults.

| Preset | Outputs | Also |
| --- | --- | --- |
| `app` | TypeScript SDK, docs | |
| `library` | TypeScript, Python and Rust SDKs, docs | writes `profiles/library.yml` (shared package versions) and selects it, so other projects can reuse it |
| `agent` | MCP server on the TypeScript SDK | generates with `--strict` |

```sh
tungsten init --no-interactive --preset library --from openapi.json --dir my-sdk
tungsten init --no-interactive --preset app --from openapi.json --generate --local-example
```

Exit codes are those of `init`: 0 on success, 1 when the input has errors, 2 for
an unknown preset, 4 when an existing manifest or profile would be overwritten
without `--force` (nothing is written). `--no-interactive` never prompts.

Inspect the supported effective configuration without compiling API inputs:

```sh
tungsten config .
tungsten config path/to/tungsten.yml --json
tungsten config . --strict --json
```

The command validates the manifest and bounded local profiles through the same
config-only loader as project doctor. It shows the winning and shadowed authored
origins for each supported TypeScript option and custom dependency, its default
and invalid-value policy, and `runtime_path` precedence over `runtime`. Package
names derived from the compiled API remain unresolved. Static package/runtime
defaults come from the shared option declaration. Targets without a typed option
declaration are explicitly `unsupported_options`; external targets are
`external_unverified`, with opaque ordinals in manifest order.

Runtime specifiers, local runtime paths and dependency specifiers are always
redacted by the option schema. Unknown settings, transport/auth content, physical
paths, source excerpts and invalid authored values are omitted. Sources use
`project` and `profile[n]` capture ordinals with profile content digests, so even
profile filenames are not exported. These digests identify captured bytes, not
publisher trust. Plain text and JSON carry the same explanation, without prompts,
color, login, tool probes, input acquisition or emitter execution. Global input
acquisition flags do not cause acquisition during inspection.

Exit 0 means the inspected configuration has no errors; exit 1 means manifest,
profile or supported-option errors, or warnings with `--strict`. Unsupported
target markers are informational. Invalid scalar project options retain their
warning/default policy; invalid custom dependencies fail. This inspection is
neither a replayable manifest nor evidence that generation or a consumer works.

### Editor support

Diagnostics for selected local profiles point to the captured physical file,
including invalid or unsaved included profiles outside the manifest directory.
Compiler source names and exported configuration provenance remain portable.
Dedicated profile completion and navigation remain follow-up work.

`tungsten lsp` is a language server for `tungsten.yml`, `agent.yml` and the overlay files a project lists. It checks the project with the same compiler as `tungsten check` and shows the same `TG` diagnostics on their source lines. Unsaved JSON/YAML manifests, specs, overlays and local references participate in the same source snapshot, including new files in an existing input directory. Closing a buffer restores its disk contents; file watchers pick up saved changes and deletions. File-size limits and local-reference confinement still apply. The selected project's `.tungsten-inputs` acquisition cache is reserved for verified disk content: buffers cannot replace or extend a pinned Git tree, while explicitly selected local overlays outside the cache remain editable. It completes keys, enumerated values and the operation ids, gates and clusters of the compiled project; shows field documentation, operation signatures and diagnostic explanations on hover; jumps from an operation id to its place in the OpenAPI document; and outlines the manifests. Quick fixes replace an unknown operation id (`TG0603`) or an unknown manifest field or value (`TG0602`) with the closest name the compiler knows; each fix is a single edit pinned to the buffer version it was computed for, so the editor previews it, refuses it after a later edit and can undo it. An operation without an `operationId` (`TG0403`) or with a repeated one (`TG0401`) gets the overlay action `tungsten overlay suggest` derives, added to the input's suggested overlay in one previewed edit. When a fix cannot be offered, editors that show disabled code actions show why and the equivalent command. Editors without versioned workspace edits get plain edits for the replacement fixes, still checked against the current buffer. A fix never runs emitters, installs packages or refreshes inputs. Every project in the editor's workspace folders is served at once: projects are found the way `tungsten workspace` finds them (bounded, never following links), a folder inside a project serves that project, and opening a `tungsten.yml` elsewhere serves it too; `--config <path>` serves exactly one project. Each project is checked on its own, one check at a time for the whole session: editing a file re-checks only the projects that read it, so a shared spec updates every project using it and no other, diagnostics of a file read by several projects are merged, and removing a workspace folder cancels its projects' checks and clears their diagnostics. Requests about a file are answered from the project that owns it (its manifest, else the first project reading it, else the innermost project directory holding it).

Edits retain the existing debounce and one active analysis worker. A newer edit
cancels obsolete work cooperatively between compiler phases and during reference,
operation and component traversals; obsolete results do not replace diagnostics
or the last accepted navigation index. Diagnostics for open files carry their
document version, and older change notifications are ignored. An in-progress read,
parse or indivisible compiler pass finishes before the next cancellation check;
this is not a hard execution deadline. No persistent analysis cache is introduced.


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
      filetypes = { 'yaml', 'json' },
      root_dir = util.root_pattern('tungsten.yml', 'tungsten.yaml'),
    },
  }
end

require('lspconfig').tungsten.setup({})
```

Any other client works the same way: start `tungsten lsp` as a stdio server for YAML files, and for JSON files too when the project has JSON specs or overlays, so their unsaved edits are checked (the Neovim example attaches to both).

Quick fixes are ordinary code actions: the light bulb or `Ctrl+.` in Visual Studio Code, `vim.lsp.buf.code_action()` in Neovim. Overlay fixes create a file and edit `tungsten.yml` in one workspace edit, so the client must support `documentChanges` with the `create` resource operation; clients with change-annotation support ask for confirmation and preview every file first (Visual Studio Code opens its refactor preview). Neovim applies the edit directly; undo it per buffer with `u`, and delete the created overlay to revert fully. A client that shows disabled code actions explains why a fix is unavailable, with the equivalent `tungsten overlay suggest` command.

| Exit code | Meaning |
|---|---|
| 0 | Success (also `diff` when regeneration would change files, and `report` with warnings) |
| 1 | The input has errors (or warnings with `--strict`), generated output is stale (`generate --check`, `check --ci`), or the item asked for does not exist |
| 2 | Usage error: the command line could not be parsed |
| 3 | I/O or internal failure (for example `--out` is not writable) |
| 4 | Refused to act without confirmation (`init` over existing files, `generate` into a directory tungsten did not write: pass `--force`) |

The CLI that tungsten generates for a Rust target has its own exit codes (0 success, 2 usage or pre-flight validation, 3 API error, 4 retryable API error, 5 unknown outcome, 6 confirmation required, 7 missing credentials) so a calling agent can branch on them.

## Project status

tungsten is **early-stage**. Ten SDK targets, the generated Rust CLI, MCP, WebMCP, docs and the mock server are implemented. Validation is target-specific; do not infer that every target has the same live pilot coverage or published runtime packages. Check the [GitHub releases](https://github.com/pboachie/tungsten/releases) for available compiler binaries and matching sources. The quickstart installs a runtime from matching source and does not require registry publication; from 0.1.3, compiler and runtime source versions follow the release tag together. Expect breaking changes to manifests, the IR and generated code between releases. Open an issue if a target limitation blocks you.

The test suite, conformance corpus and design documents live in a separate maintainers' repository, which runs against every pull request. This repository is source only.

## Licensing

- The compiler (`crates/`) is licensed under [AGPL-3.0-only](LICENSE).
- The runtime libraries that generated code depends on (`runtimes/`) and the external Go emitter (`emitters/go`) are licensed under [Apache-2.0](runtimes/LICENSE).
- Output generated by tungsten is not a work covered by the AGPL and carries no licence from tungsten. The owner of the API description chooses the licence of the generated code.

## Contributing and security

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request, and open an issue to discuss a large change first. Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md); please do not file them as public issues.

### Python interpreter compatibility

Python targets default to Python 3.12+. To generate a client for Python 3.10
or 3.11, set a quoted `python_version` in `targets.python`:

```yaml
targets:
  python:
    python_version: "3.10"
```

Supported values are `"3.10"`, `"3.11"` and `"3.12"` (the default). Both
Pydantic and `models: dataclasses` support these modes. Compatibility output
uses `typing_extensions` aliases and retains forward references, validation
and transport behavior. The matching runtime supports Python 3.10 and newer;
compatibility clients require `tungsten-runtime>=0.1.3,<0.2.0`, including when
an explicit runtime specifier is supplied. A `runtime_path` checkout must
contain this compatibility support. Publishing the matching runtime remains
part of the coordinated release.

Public CI builds the runtime sdist and wheel on the latest patch of CPython
3.10, 3.11, 3.12, 3.13, 3.14 and 3.15 (macOS and Windows on 3.14), and
installs them with freshly generated SDKs into clean environments: both model
modes, `"3.10"` output on every minor and default output on 3.12 and newer,
with compile, import, strict Pyright and sync/async requests covering
confirmation, auth, pagination, safe retries, validation, deadlines,
cancellation, unknown outcomes and server-sent events. The 3.10 lane also runs
the lowest declared dependency versions. Generated packages list classifiers
for these minors from their `python_version` upward. Python 3.10 is
end-of-life upstream: it remains a compatibility lane, not a
security-maintained runtime. Prerelease interpreters are not claimed, and
`requires-python` sets only a floor.
