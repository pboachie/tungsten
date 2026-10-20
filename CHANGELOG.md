# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project will adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
from its first stable release. Until 1.0, minor versions may contain breaking
changes.

## [Unreleased]

### Breaking

- Ruby browser OAuth helpers now create bounded S256/state authorization attempts.
  Qualify issuer defense, bind the owning session/listener and pass the returned
  handle plus full callback URL to `exchange_code`. Managed helpers require a
  fenced token store; application-owned protocol inputs use explicitly named
  low-level methods. Code/refresh response bodies default to 64 KiB through
  `max_oauth_token_response_bytes`. Generated examples select the actual
  authorization-code flow even when another authentication scheme appears first.

- Dart browser OAuth helpers now create managed S256/state authorization attempts.
  Supply the owning session/listener binding and qualified issuer defense, then
  exchange the returned handle with the complete callback URL. Managed helpers
  require `FencedTokenStore`; legacy primitives use explicitly named low-level
  methods. Code/refresh bodies default to 64 KiB through
  `maxOAuthTokenResponseBytes`. Cancellation fences token writes and retains
  admission until transport cleanup; refresh joins preserve the caller's
  original deadline.

- Rust compiler-library embedders now pass an invocation policy and raw optional
  external limits to `external::run` and `external::describe`. Select a context
  for each invocation, using `InvocationPolicy::absent()` only for deliberate
  legacy behavior. Preserve missing limits as `None` so protected ceilings can
  clamp defaults. Exhaustive `CliEnv` and `CompileOptions` literals must include
  their new policy fields.

- Java browser OAuth helpers now create managed S256/state authorization attempts.
  Qualify issuer defense, bind the owning session/listener and pass the returned
  handle plus full callback URL to code exchange. Kotlin delegates to the same
  core helper. Legacy primitives use explicitly named low-level methods. Managed
  begin and refresh require `TokenStore.Fenced`; the separate positive
  `maxOAuthTokenResponseBytes` option bounds token bodies (64 KiB by default).
  Attempts and refresh retain ownership until transport work settles and use
  one monotonic operation budget.

- Go browser OAuth helpers now manage bounded, one-use S256/state attempts.
  Qualify issuer defense, bind the owning session/listener, retain the attempt
  handle and pass the full callback URL to `ExchangeCode`. Managed helpers
  require a `FencedTokenStore`; application-owned protocol inputs move to
  `LowLevelAuthorizationURL` and `LowLevelExchangeCode`. Code/refresh responses
  default to 64 KiB through `MaxOAuthTokenResponseBytes`. Token jobs retain their
  monotonic deadline and physical ownership through cleanup, and diagnostics
  expose only recognized token-endpoint error labels.

- C# browser OAuth helpers now create managed S256/state authorization attempts.
  Qualify issuer defense and bind the owning session/listener, retain the returned
  handle, and pass the full callback URL to `ExchangeCodeAsync`. F# clients use
  the same core helper. Legacy primitives require explicitly named low-level
  methods/types. Managed begin and refresh require `IFencedTokenStore`; separate
  `MaxOAuthTokenResponseBytes` controls code/refresh bodies (64 KiB by default).

- Rust browser OAuth helpers now require managed S256/state attempts with an
  explicit qualified issuer defense and owning session/listener binding.
  `authorization_url` returns a handle and URL inside Tokio; `exchange_code`
  accepts that handle, the full callback URL and the same binding. Legacy
  primitives require `low_level_authorization_url`/`low_level_exchange_code`.
  Managed begin and refresh require atomic fenced custom stores. Code/refresh
  responses default to 64 KiB, configurable with the separate validated
  `max_oauth_token_response_bytes` option.

- TypeScript and Python browser OAuth authorization-code helpers now create
  managed S256 PKCE/state attempts. Supply an authenticated session/listener
  binding and an explicit qualified issuer defense, retain the returned handle,
  and pass the full callback URL to code exchange. TypeScript authorization URL
  creation is asynchronous. Legacy primitives require the separately named
  `lowLevelAuthorizationUrl`/`lowLevelExchangeCode` methods (Python snake case).
  Managed helpers and default refresh require atomic fenced custom-token-store
  writes and deletions. Code/refresh token bodies default to 64 KiB; use the
  separate positive `maxOAuthTokenResponseBytes` owner option (native casing
  varies) for larger provider responses.

- External emitters and descriptor probes use owned process groups or Job Objects
  and request cleanup when their leader exits. The process timeout includes a
  cleanup reserve (one quarter, at most one second). Incomplete cleanup or exhausted
  worker capacity refuses output with TG0803. Keep emitter helpers within their
  invocation and investigate remaining processes before retrying; these lifecycle
  controls do not provide a sandbox or execution approval.

- Ordinary buffered responses now default to 4 MiB per response across native
  runtimes. Omitted/null limits inherit that default; choose an explicit finite
  client/call limit for larger responses. Generated Rust CLIs expose
  `--max-response-bytes`. Invalid effective limits fail before authentication,
  storage or preview work. Overflow remains nonretryable and a write may have
  taken effect. OAuth and successful SSE budgets remain separate.

- Authenticated requests now require HTTPS by default across native runtimes,
  including OAuth token endpoints. Local development on `localhost`, IPv4
  `127/8` and IPv6 `::1` remains supported. Other HTTP targets return
  `AUTH_FAILED` before credentials are dispatched. Migrate to HTTPS, or enable
  the client's explicit `allowInsecureHttp` option (native casing varies) for
  intentional insecure transport. MCP and WebMCP inherit their client's policy.

- Caller-supplied confirmation-token keys now require at least 32 bytes across
  all runtime families. Empty or shorter keys fail client/options construction
  with a configuration error; omitting the key still generates a random key
  per client. Replace short keys with at least 32 cryptographically random bytes,
  or remove the override. Keys are copied at construction so later mutation of
  a caller's buffer cannot change the client's signing key. Rotating a key
  invalidates existing previews; request a new preview before confirming.

- Ordinary OpenAPI `x-agent-safety` annotations cannot lower the built-in HTTP-method
  tier without an explicit `agent.yml` tools safety override (TG0615). Spec-origin
  `x-agent-preview: none` on operations with side effects keeps a safe default
  preview unless a tools entry explicitly removes it (TG0616). Both warnings fail
  strict checks and generation. Move intentional policy reductions into the matching
  tools entry and regenerate affected SDKs and agent manifests together.

- Installers require official release provenance by default, including for
  mirrors. Install GitHub CLI 2.102.0 or newer before running them. Verification
  pins the repository, release workflow, issuer and requested tag; missing or
  rejected attestations refuse installation. `TUNGSTEN_NO_ATTESTATION=1` is the
  explicit, diagnosed checksum-only choice; `TUNGSTEN_REQUIRE_ATTESTATION=0`
  no longer permits an implicit fallback.

### Added

- Add explicitly pinned host policies for external emitters, with strict bounded
  capture, intersected deny/resource ceilings, pure policy digests and redacted
  inspection. Unsupported commands reject supplied policy before effects.

- Inspect supported effective configuration with `tungsten config`, including
  typed defaults/fallback policy and winning/shadowed authored origins. Plain
  text and JSON redact sensitive values and physical paths without compiling
  inputs, probing tools or executing emitters.

- Compose bounded local TypeScript profiles for package, version, runtime and
  dependency defaults. Retain authored provenance through the compiler driver,
  strict profile diagnostics and existing manifest fallback behavior.

- Declare the existing TypeScript target options and supported-value schema in
  the shared capability contract. Keep emitter defaults and TG0711 fallback
  behavior compatible while configuration and generation share validation.

- Validate saved mock scenarios portably and explicitly run integration argv on
  Linux with a run-phase deadline, cancellation, owned process-group cleanup,
  final exit/call-count assertions and a value-free JSON report. The run budget
  begins after compilation and validation; call counts do not prove effects.

- Load bounded, non-executable JSON scenarios with `tungsten mock --scenario`.
  Reuse existing response and fault controls, and read a value-free report of
  total calls, per-operation/status counts and unconsumed responses.

- Analyze unsaved JSON/YAML specs, overlays and local references through the
  shared compiler source provider. Keep buffer spans and navigation consistent,
  and cancel obsolete editor analysis while retaining debounce and stale-result
  protection.

- Add an offline `capabilities` query and JSON Schema for built-in target
  declarations. Share target aliases, imported-operation policy support and
  consumer prerequisites with configuration and doctor; include Kotlin, F#
  and Dart prerequisites without inferring qualification or published packages.

- Add a manual Windows release-video example with exact-release provenance,
  multiple input workflows, local narration, real terminal recording, chapters,
  compact review MP4s and a reusable source and evidence package.

- Add a gated JSR source-publication route for the core TypeScript runtime
  starting at 0.2.0, with artifact verification and OIDC provenance. Publication
  still requires the release checks and an explicit maintainer switch.

### Changed

- Locate primary and related LSP profile diagnostics in their captured local
  files, including invalid profiles, while preserving portable compiler and
  configuration-inspection source names.

- Reuse immutable SDK structure across support checks and emission for Java,
  Kotlin, C#, F#, Dart and Ruby. Generation stages one target's bytes at a time
  with bounded output metadata before applying files, preserving strict
  diagnostic withholding and custom-file ownership. Reports discard target
  bytes after collecting their results. Application errors and cooperative
  cancellation describe completed file writes/removals; output remains
  nontransactional across files and directories.

### Fixed

- Java generated usage now imports its resolved resource type and infers the
  response type. Dispatch request decoding also imports resolved resource types,
  so valid package prefixes such as `client.generated` cannot be shadowed by the
  dispatch client's field. Java and Kotlin authorization-code examples configure
  the managed OAuth flow explicitly.

- TypeScript, Python and Rust authorization-code and refresh diagnostics include
  only recognized OAuth token error labels. Arbitrary provider error strings
  are omitted even when they contain only lowercase letters and underscores.

- Windows Git input acquisition uses an exclusively created local temporary
  repository, avoiding Git working-directory and pack-file limits caused by
  deeply nested project caches. Verified project pins and offline reuse retain
  their existing locations; unsupported temporary roots fail safely.

- TypeScript, Python and Rust managed OAuth helpers cap endpoint text at 32 KiB
  and reject malformed Unicode before URL parsing. Protected or malformed refresh
  queries refuse before authorization begins and before default refresh dispatch,
  including a changed endpoint or the token-endpoint fallback.

- Check profile-derived TypeScript prerequisites in project `doctor`, and refuse
  invalid manifests or bounded profiles before running tool version probes.

- Correct the MCP runtime and generated server prerequisite to Node.js 22.13+
  (23.2+ on the Node.js 23 line), matching the module API used by its import
  graph. Keep doctor prerequisites for every configured target so a linked SDK
  cannot hide the stricter MCP floor.

- `tungsten emitters --no-external` and `TUNGSTEN_NO_EXTERNAL=1` now skip
  PATH-based descriptor probes as well as manifest commands. The inspection
  result reports that discovery is disabled instead of claiming no emitters exist.

## [0.1.3] - 2026-10-17

### Added

- Automate Maven Central, NuGet and RubyGems publication alongside npm, PyPI
  and crates.io after an approved release tag. Verify package identity, source
  and checksums before credentials, and compare public downloads with the tested
  archives. Add a package-only rehearsal and an embedded NuGet package icon.
  Rust publication also requires accepted first-publication ownership and both
  trusted publishers; source preparation does not complete registry acceptance.

- Enable registry packaging of the Apache-2.0 Rust runtime and CLI-kit with a
  versioned runtime dependency. Add opt-in crates.io trusted publishing to the
  existing release workflow; account ownership, first publication and publisher
  configuration remain maintainer release steps.

- Generated Go SDKs require the published `v0.1.3` runtime module through its
  `runtimes/go/v0.1.3` nested-module tag, replacing the interim pseudo-version.
  Runtime version overrides and local source replacements remain supported.
- Complete the architecture overview with explicit alternative input routes, separate
  optional configuration and all current SDK, CLI, MCP, WebMCP and docs outputs; link
  the reviewed response-contract recipe from the quickstart and gallery. Kotlin
  and F# installation guidance now links the inherited durable idempotency stores.
- Imported-input setup validates built-in target policy support before saving
  converted documents or project manifests; unsupported selections return TG0705
  with supported alternatives instead of leaving an unusable project behind.
- A bounded Rust/Axum source-declaration API in `tungsten_contract::source_axum`,
  with literal route/type evidence, file/line coverage and explicit unresolved
  constructs. It parses supplied source text without loading modules or running
  applications, macros or build scripts, and preserves separately reviewed overrides.
  Routing/handler binding shadows and conditional router contexts remain
  unresolved coverage rather than claimed operations.
- Ruby runtime gems declare their `base64` dependency so packaged SDKs load
  inside Bundler on Ruby 3.4+, with source and offline installation guidance.
- Durable Java, C# and Ruby idempotency stores share the version-1 owner lease
  format with TypeScript, Python, Go and Rust. Automatic keys use atomic
  reservations while existing get/put-only custom stores remain compatible.
- Python and Rust `max_response_bytes`, Go and C# `MaxResponseBytes`, Java
  `maxResponseBytes` and Ruby `max_response_bytes` now bound ordinary response
  bodies on opt-in, matching TypeScript. Client defaults and per-call overrides
  validate positive safe integers; header and counting reads close overflowed
  bodies and report `UNEXPECTED_RESPONSE` / `RESPONSE_TOO_LARGE` without retry,
  redirect following or OAuth replay. Mutation remediation preserves unknown
  outcomes. Python and TypeScript `collect` helpers accept optional byte/time
  budgets, retain admitted partials and preserve unbounded no-argument calls.

- Public installation examples distinguish published 0.1.2 runtimes from the
  0.1.3 source candidate, with Python compatibility prerequisites and HTTP/Git
  acquisition, cache verification and offline reuse explained in CLI help.
- Dedicated TG0808 diagnostics explain when an external emitter lacks imported-operation
  policy enforcement; protocol-1 emitters remain compatible with ordinary projects.
- Git input references (`git+https://...@ref#path`) with bounded shallow
  acquisition, deterministic document discovery, commit/content pins and verified
  offline reuse through the existing input and importer flows.
- A runnable example gallery for first SDKs, supported input routes, destructive
  confirmation, local mock development, generated-file drift and API surface
  review. The catalog runner checks recipe documentation and builds generated
  packages against matching runtime source in public CI.
- Prepare compiler/runtime source manifests and identity headers for 0.1.3 in
  lockstep. CI and release preparation check every runtime before building;
  generated SDK defaults require the 0.1.3 runtime capabilities, while explicit
  runtime overrides remain unchanged. Go uses a separately published nested-module tag.
- Runtime package installation and limits guidance, packaged README/LICENSE
  files, and consistent npm metadata checks.
- Opt-in `check --audit-contracts` response quality diagnostics for missing JSON
  schemas, unconstrained values and free-form objects, with scoped review notes
  and `--require-contracts` enforcement (or existing `--strict` enforcement).
  A runnable response-contract example
  enriches an untouched specification with reviewed run/job union schemas through
  standard OpenAPI overlays; generated response types and default checks are unchanged.
- CI builds the compiler and example emitter at their declared Rust minimum,
  checks the runtime at its own minimum, and rejects locked dependencies or
  README compiler prerequisites that exceed or disagree with those floors.
- The container accepts patched zlib updates above its minimum required Alpine
  version and verifies the installed version; Dependabot checks the root image.
- Installers detect whether `gh attestation` is available, explain checksum-only
  installation when it is absent, and show verification errors while refusing
  failed attestations. Both scripts honor required/disabled attestation settings
  and replace stale unpublished-release hints with the release listing.
- A runnable typed Python SSE recipe under `examples/typed-sse`, with an
  explicit FastAPI response declaration, a reviewed OpenAPI overlay, API-key
  authentication, sync/async consumers and explicit terminal-event cleanup.
- HTTP(S) input acquisition for `init --from`, compiler paths and manifest inputs,
  with bounded downloads, SHA256 source pins in `.tungsten-inputs`, explicit
  `--refresh-inputs`, verified `--offline` reuse and redacted `--input-header`
  authentication. Remote entries retain existing reference and import safety rules.
- Opt-in Python 3.10/3.11 SDK generation with `python_version`, matching
  package metadata and a Python 3.10-compatible runtime for both model modes.
- Link the importer guide from the quickstart, including per-format limits and imported-operation review.
- Imported specs (Postman, cURL, `.http`, HAR and endpoint manifests): a
  target that cannot enforce imported-operation policy (Java, C#, Kotlin, Ruby,
  Dart and the other SDK targets) now fails alone; without `--strict` the
  supported targets are still written, and the summary no longer blames
  `--strict`. `tungsten init` and the guided wizard warn up front, before
  anything is generated, when a chosen target will be refused for an imported
  input. The external emitter protocol gains an optional `import_policy` flag in
  the `--describe` response (and `specs/external-emitter.schema.json`), checked
  before dispatch, so an emitter such as `tungsten-emit-go` can opt in; an
  absent flag means no support, and existing protocol 1 emitters are unchanged.
- `--no-external` (global; also `TUNGSTEN_NO_EXTERNAL=1`) skips every
  `external` target instead of starting its command, with an informational
  TG0807 notice, so CI can run `check --ci`, `report`, `diff` and `generate`
  over a manifest it does not trust. Without it nothing changes. SECURITY.md,
  the README, the `targets` description in the `tungsten.yml` schema and the
  `check --ci` help now say that `external` commands run on every command that
  emits in memory (`generate --dry-run` and `--check`, `check --ci`, `diff`,
  `report`); the source comment that said otherwise is corrected.
- `report --all-targets` runs the emitter of every known target, as `report`
  used to.
- Postman import omits unsupported requests and folders per item instead of
  failing the whole collection: non-raw or non-JSON bodies, non-JSON saved
  responses, unsupported auth helpers, pre-request scripts and unresolved or
  dynamic variables. Each omission is listed under `omitted` (code, JSON
  pointer, reason) in `import-report.json`, and `init --from` prints the
  omitted count. A collection with no importable request fails with PM008;
  PM001 budgets and PM002 schema errors still fail the import.
- A Dart SDK target, `dart`: a pub package with final model classes that
  decode and encode themselves, sealed unions, `Future` methods returning
  `Result<T>`, `Stream` pages and `EventStream<T>` streams, typed macros and
  a `Dispatch` entry point, over the new `dart:io` runtime `tungsten_runtime`
  (`runtimes/dart`, Dart 3.6 or later, `package:crypto` as its only
  dependency). Options `package`, `version`, `runtime` and `runtime_path`;
  diagnostics TG1070 to TG1079. The Dart runtime passes the L4 contract
  suite in parity with TypeScript, with no skips.
- Generated Rust CLIs: `--max-pages <N>` (with `--all`) stops paging after N
  pages and notes on standard error that more pages are available. There is no
  default limit.
- Build provenance attestations for the container image, the Homebrew formula, the winget manifests and `DISTRIBUTION_SHA256SUMS`, with install notes on verifying them.
- MCP Streamable HTTP: optional `bearerToken` and `authorize(req)` options (and
  `--token-env <NAME>` on generated servers) to require authentication; a
  startup warning when binding a non-loopback host with no authentication and
  no `allowedHosts`.
- A Kotlin target (`kotlin`, short form `kt`, `tungsten-emit-kotlin`): a
  Kotlin/JVM SDK over the Java runtime and the new Kotlin layer
  `runtimes/kotlin` (`io.github.pboachie.tungsten:tungsten-runtime-kotlin`,
  JVM target 17, kotlin-stdlib and the Java runtime only). Data classes with
  named, defaulted parameters (`Patch<T>` where null is a value, `T?`
  otherwise), enum classes with wire values, sealed interfaces for unions,
  blocking typed functions returning the runtime's `Result`, previews, pages
  as a `Sequence` of page results, event streams, macros and `Dispatch`; a
  Gradle (Kotlin DSL) project per SDK that builds with `kotlinc -Werror`.
  The Kotlin layer reports `(kotlin)` in `User-Agent` and `tungsten-kt/0.1.0`
  in `X-Tungsten-Runtime` and adds `Result` extensions (`getOrThrow`, `fold`,
  `onError`). Options `group`, `artifact`, `package`, `version`, `runtime`,
  `runtime_path` and `java_runtime_path`; diagnostics TG1030 to TG1035.
  First version: blocking API only (no coroutines or `Flow`), JVM only, and
  the Java target's gaps.
- `ClientOptions.identity()` and `withIdentity(...)` in the Java runtime
  (additive), so a layer on top of it reports its own runtime identity.
- F# SDK target `fsharp` (short form `fs`), a built-in emitter over the .NET
  runtime. It generates records, discriminated unions for enums and unions
  with strict and lenient generated JSON codecs, `Task<Result<'T, Diagnostic>>`
  methods for operations, previews, pages and streams, typed macros and a
  client, with an explicit compile order. Options: `package_id`, `namespace`,
  `version`, `runtime`, `runtime_path` and `csharp_runtime_path`.
  Diagnostics TG1080 to TG1085, each with `tungsten explain` text.
- `runtimes/fsharp` (`Tungsten.Runtime.FSharp`, net10.0): the F# layer over
  `runtimes/csharp` with the F# identity, `Patch<'T>` as a union, Result and
  option interop, Task and async-sequence helpers and the JSON helpers of
  generated records. The public CI job `fsharp runtime build` builds it.
- `Tungsten.Runtime`: `ClientCore` accepts a `RuntimeIdentity`, so a layer
  over the runtime can name its own language in `User-Agent` and
  `X-Tungsten-Runtime`. The C# identity is unchanged.

Known gaps of the F# target: Task API only; net10.0 only; no Fantomas
formatting; Fable and JavaScript targets untested; plus the C# limits
(in-memory idempotency store, .NET regex dialect, by-status and intersection
values as `JsonNode`, OpenID Connect as a bearer token, no CLI or MCP).

- The release dependency audit supports reviewed, scoped and expiring advisory
  exceptions: entries in the `advisory_exceptions` list of
  `.github/dependency-policy.json` name an advisory id, the one locked package
  it applies to, the reason it was accepted and an `expires` date. The audit
  prints every exception in the release job log, suppresses only the matching
  advisory, and fails once an entry is past its expiry, so an exception is a
  time-limited review decision instead of a standing ignore. CONTRIBUTING and
  SECURITY.md now record that release tags are immutable — a failed release is
  fixed forward with a new version, never by moving a tag — and that a
  `workflow_dispatch` dry run (`dry_run: true`) of the release workflow, which
  runs the same audit, is required on the release commit before the tag is
  pushed.

### Changed

- Runtime performance. Successful responses are decoded once instead of
  several times: the Go runtime parses the body in one pass and unmarshals the
  original bytes into the typed result, and generated Rust SDKs call the new
  `Client::call_typed`, which deserializes the body bytes straight into the
  result type and builds the generic JSON value only when it is needed. The
  Python sync client runs on httpx's own transport with socket-level attempt
  deadlines instead of a background event-loop thread, and declares `sniffio`.
  The TypeScript runtime does less work preparing each request. Error
  envelopes, validation modes and timeout semantics are unchanged.
- Avoid unnecessary request argument copies in the Rust runtime and defer
  TypeScript middleware header lookup allocation until a header changes.

- Document released 0.1.2 compiler/runtime pins for npm, Python, Maven, NuGet
  and RubyGems consumers, plus matching-source Rust SDK/CLI and Go installation,
  independently from the unpublished 0.1.3 candidate.

- README, the architecture diagram and the `init` help present every
  supported input alternative instead of positioning tungsten as OpenAPI
  only: OpenAPI 3.0/3.1, Swagger 2.0, Postman Collection v2.1, saved cURL,
  `.http`/`.rest` files, HAR 1.2 and endpoint manifests, plus the manual
  method-and-endpoint path, shown as OR alternatives with each route's
  conversion path, limits and required review. The README gains a Supported
  inputs matrix; `assets/architecture.svg` and its accessible description
  show alternative inputs normalizing to OpenAPI before the frontend; the
  `init --from` help and the `tungsten-cli` crate description name the
  accepted inputs. `check`, `generate` and the other project commands still
  take an initialized project or a single OpenAPI document. (#173)
- `tungsten report` runs only the emitters of the configured targets. The
  other known targets stay in `coverage.targets` with the new status
  `not_configured` (and, for an `external` target skipped by `--no-external`,
  `skipped`) and their emitter is not run, so `report` on a large spec takes
  about as long as `generate --dry-run` (the Stripe showcase spec took 3 min
  18 s against 1.3 s for `check`). The token budgets of `docs` and `mcp` are
  empty unless that target is configured or `--all-targets` is given.
- `tungsten check` prints a warning or info code at most three times in the
  human output, then one line with the number left out, and its summary lists
  the count per code (`--all-warnings` prints everything; `--json` has always
  listed every diagnostic and gains an additive `by_code` array). TG0301 for
  an untagged union of exactly two candidates where one is a plain string,
  array or map is now info instead of a warning, so `--strict` no longer
  fails on those; other untagged unions still warn. (#111)
- Breaking (diagnostic code): `tungsten generate` now reports the refusal of a
  target that cannot apply an enforced imported-operation policy (for example
  `java` on a Postman-imported project) as the new error TG0705; it was
  reported as TG0702. TG0702 again means only "target has no emitter in this
  version; skipped". Tooling that matched TG0702 for this refusal must match
  TG0705; `tungsten explain TG0705` names the supported targets and the
  `tungsten init --resume --add-targets` path.
- The release workflow's PyPI publication job is enabled through trusted
  publishing (repository variable `PYPI_PUBLISH`, `pypi` environment, no
  stored token) and now refuses to upload a `tungsten-runtime` sdist or wheel
  whose version does not equal the release tag, mirroring the npm publication
  preflight.
- The published `ghcr.io/pboachie/tungsten` container image carries
  `org.opencontainers.image` title and description labels, so the GitHub
  package page shows the package description instead of "No description
  provided".
- Generated Rust CLIs: `--all` writes its array page by page instead of
  collecting every item first. The output is byte-for-byte the same on
  success; a failure after the first page leaves the array unterminated and is
  reported on standard error. `Dispatch` gains the provided method
  `pages_each`, which generated clients implement lazily.
- MCP `run_script`: the WASM engine now runs its isolate in a worker thread
  that is terminated at the deadline, so a long computation no longer occupies
  the server's event loop. Results, errors and limits are unchanged.
- MCP `run_script`: new `sandbox.maxConcurrent` option (`perSession`, default
  4; `global`, default 16). Calls over a limit are refused with a
  `VALIDATION_FAILED` "busy" envelope.
- MCP HTTP: a warning is logged when `sandbox.engine: "auto"` falls back to the
  WASM engine because deno was not found.
- Distribution verifies the build provenance attestation of every release asset it consumes before rebuilding the container image, Homebrew formula and winget manifests.
- Dependency audit: source-less `Cargo.lock` entries must be workspace members, and `[patch]`/`[replace]` tables in Cargo manifests are rejected.

### Fixed

- Python local setup installs the supplied runtime's declared dependencies using
  wheels and identifies the failing setup phase without changing its deadlines.

- Keep the Rust runtime's HMAC and SHA-256 dependencies on compatible digest
  APIs after dependency updates, preserving its declared minimum Rust version.
- Restore Python environment proxy and `NO_PROXY` routing while retaining
  socket-level attempt deadlines on direct and proxied connections.
- Isolate Git input commands from project configuration and inherited Git
  environment overrides; terminate the complete process tree on budget failure.
- Restrict input authentication headers to explicitly selected URLs or an
  explicitly scoped origin, so manifests cannot redirect credentials.
- Add Dart `maxResponseBytes` limits with bounded buffering and no automatic
  replay after an oversized response, including failed and redirected requests.
- Hash-lock release bundle validation dependencies and restrict installer
  attestation verification to the release workflow.
- Preserve Go response decoding semantics for duplicate JSON members, large
  integers and dynamic values modified before typed decoding.
- Close Python transport sockets when TLS setup fails or its deadline expires.
- The Axum source-declaration importer accepts long `Router::new().route(..)`
  builder chains; routers with 19 or more chained routes no longer fail the
  lexical expression budget. Chains are walked iteratively, and only chains over
  4096 links fail, with a dedicated diagnostic.
- Interrupting a remote HTTP(S) or Git input fetch no longer wedges the
  `.tungsten-inputs` cache. Pins, content and Git tree files are written to unique
  scratch names and renamed into place; the next online run holds a per-source
  `acquire.lock`, discards abandoned `*.pending` entries and restores truncated
  content-addressed files. Cache write, cleanup and lock-contention errors now
  name the affected path and how to recover.
- Remote inputs: `--input-header`/`TUNGSTEN_INPUT_HEADERS` values sent to a
  plain `http://` URL on a non-loopback host now print a warning (once per
  origin, never echoing header names or values); use `https://` for
  authenticated inputs. Redirects stay refused, so headers never cross origins.
- Git inputs read every selected file through one `git cat-file --batch`
  process and wait on Git without polling, so large subpaths acquire much
  faster. Timeouts stop Git through the standard library instead of an
  external `kill`/`taskkill` binary, and Git now shares tungsten's process
  group so an interrupt also ends an in-flight fetch.
- Release bundles include the Homebrew formula, winget archive and distribution
  checksums before publication, supporting GitHub immutable releases. Container
  distribution verifies the sealed assets and no longer uploads release assets.
- Python 3.11 SDKs import `Self` and `NotRequired` from the standard library,
  matching their generated lint target; Python 3.10 keeps the required backports.

- Dart pagination detects repeated cursors and page URLs, including the caller's
  starting cursor and cycles across earlier pages. Fetched pages are delivered
  before an `UNEXPECTED_RESPONSE` diagnostic, repeated requests are refused, and
  relative next-page links resolve against the effective URL after redirects.
- Release packaging: the aarch64 `gnu` Linux archive needed glibc 2.39 (it was
  built on `ubuntu-24.04-arm`) while the x86_64 archive needed 2.34, so the
  same `TUNGSTEN_LIBC=gnu` choice failed on Debian 12, Ubuntu 22.04, RHEL 9
  and Amazon Linux 2023 only on arm64. Both `gnu` targets now build on the
  Ubuntu 22.04 runners and share one documented floor, glibc 2.34. The release
  pipeline extracts the highest `GLIBC_` symbol version from every `gnu`
  binary and fails the job when it exceeds that floor, the release notes state
  the floor and that the `musl` archives are static, and `install.sh` checks
  `ldd` before installing a `gnu` archive and points older systems at the
  static musl default. The README install section documents all of this.
- Release workflow: the Woodpecker npm gate can no longer time out a real
  release run. The wait starts only after the npm artifacts it gates exist
  (the `Woodpecker npm release validation` job depends on `npm-artifacts`
  instead of racing the build jobs), its deadline is 45 minutes instead of 15
  (the v0.1.2 tag needed a manual re-run about 20 minutes after its first
  attempt died at 15; the value and its history are documented in
  `wait-woodpecker-release.mjs`, and the job timeout is 50 minutes),
  transient GitHub status or pipeline lookup failures are retried instead of
  failing the gate, and a timeout reports that only npm publication is blocked
  — the GitHub Release and attestations are unaffected — and to re-run the
  failed jobs. A `workflow_dispatch` dry run now exercises the same wait
  against the last released tag, so the gate is tested before a release
  relies on it, and the Woodpecker-side artifact validation step's own
  timeout is aligned with the GitHub-side 20-minute budget.
- MCP `run_script` on native Windows: the Deno child now receives `SystemRoot`,
  `SYSTEMDRIVE`, `TEMP`, `TMP`, `USERPROFILE`, `PATH` and `PATHEXT` (and nothing
  else from the parent), and Deno is located by file existence plus a `PATHEXT`
  extension instead of a POSIX `X_OK` check. Linux and macOS are unchanged.
- MCP: `--bearer-token-env <NAME>` is accepted as an alias of `--token-env`; the
  HTTP embeddings provider warns when `apiKeyEnv` is used with a plain `http://`
  non-loopback URL and reads at most 1 MiB of response (larger is an embedder
  failure, search falls back to BM25).
- Java runtime: a `StackOverflowError` raised by `java.util.regex` on a long
  subject (for example `^(a|a)+$` against 10,000 characters) is treated like a
  match timeout, so the pattern is reported as not checked and the error no
  longer escapes `accepts()`.
- Go emitter: the generated `go.mod` requires a runtime version that resolves
  for clean consumers. The default `runtime_version` is now the pseudo-version
  `v0.0.0-20261014151145-1a0ee4def654` of the v0.1.2 release commit, because
  the nested module has no `runtimes/go/v*` tag and `v0.1.0` did not resolve.
- Java, C# and Ruby runtimes: the server-sent events parser scans and counts
  each fragment once and joins a line only when it ends, so a long line
  delivered in small pieces costs linear instead of quadratic time (the
  TypeScript, Python, Rust and Go runtimes got this in 0.1.2).
- Java, C# and Ruby runtimes: pagination rejects cursor and page URL cycles
  as the other four runtimes do. A cursor or `Link` URL seen before (including
  the cursor the caller started with and an immediate repeat) delivers the
  pages fetched so far, then ends with `UNEXPECTED_RESPONSE`; before, a repeated
  cursor ended the traversal as a success and a longer loop ran on. A repeated
  request URL is refused before it is sent. Relative `Link` references now
  resolve against the URL that answered, after same-origin redirects, not the
  base URL. Callers whose server returned a repeated cursor now get this error
  instead of a silently truncated result.
- `doctor` now lists `java`, `mvn`, `dotnet`, `ruby`, `bundle` and `go`, and
  `doctor --strict` fails when a configured `java`, `csharp`, `ruby` or
  external `go` target lacks its toolchain (Java 17, .NET 10, Ruby 3.3,
  Go 1.22) or, for an external `go` target without a command, when
  `tungsten-emit-go` is not on PATH.
- A Path Item `$ref` with fields written beside it no longer drops them:
  sibling operations, `parameters`, `summary`, `description` and `servers`
  are merged over the referenced item for OpenAPI 3.0 and 3.1 alike, so a
  spec that lost an operation silently now compiles it (additive to the
  generated API). A field defined on both sides is the new warning TG0510
  (the sibling wins; `tungsten explain TG0510`), which fails a run only under
  `--strict`.
- `tungsten explain` now knows every code the importers and the Go emitter
  emit: PM001-PM007 (Postman), HR001-HR003 and HR006 (HAR), TC001-TC007
  (request-contract checks), GO001-GO006 (Go emitter) and TG1201 (saved cURL
  and `.http` input). `tungsten explain PM005` works outside a project, and
  `tungsten init` importer errors name the `tungsten explain <code>` command.
- Python and Java runtimes bound schema `pattern` matching: strings over 64 KiB
  skip the pattern check, and Java abandons a match after 1 s, treating it as
  not checked.
- C# emitter: documentation comments treat every C# line terminator (lone CR,
  U+0085, U+2028, U+2029) as a line break, so descriptions stay inside the
  comment.
- Go, C#, Java, Python, Ruby and Rust runtimes: validate the media type of a
  binary multipart part and refuse malformed values with `VALIDATION_FAILED`
  before any bytes are written.
- TypeScript runtime: header values are validated before the request body is
  built, and multipart part media types are checked as `type/subtype[;params]`.

## [0.1.2] - 2026-10-14

**Migrating to 0.1.2:** regenerate every generated SDK and its runtime
together; an SDK generated by 0.1.2 expects the 0.1.2 runtime of the same
language. Three generated sources changed shape, so hand-written code that
constructs them must be updated:

- Java: `ApiDescriptor` gained the `redirects` record component and lost its
  eleven-argument constructor; construct it with the canonical constructor.
- Ruby: `ClientOptions` gained the `redirects` member (first), which shifts
  positional construction and `deconstruct`; pass options by keyword.
- Rust: `CallOptions` gained the public `max_pages` field and `ApiDescriptor`
  the `redirects` field; build `CallOptions` with `..CallOptions::default()`
  and add `redirects` to handwritten `ApiDescriptor` literals.

### Breaking

- MCP `run_script`: the Deno sandbox no longer gets `--allow-net`. Scripts
  reach the API only through the session's client RPC. `denoArguments` and
  `sandboxCommand` ignore `host`, now optional, and `run_script` no longer
  needs or validates a `baseUrl`.
- MCP servers bound upstream response bodies to 4 MiB by default (new
  `maxResponseBytes` server option). Larger responses fail with
  `RESPONSE_TOO_LARGE` before they are materialized.
- WebMCP `maxResultBytes` now always bounds the upstream HTTP body before
  parsing, not only with `binaryResults` or `streamCollection`. An oversized
  response reports `RESPONSE_TOO_LARGE` instead of `RESULT_TOO_LARGE`.
- MCP sessions refuse a tool, sending nothing (`UNEXPECTED_RESPONSE`), when the
  manifest safety tier differs from the SDK operation's tier or from a macro's
  strictest declared or step tier. Regenerate the manifest and SDK together.
  Manifest annotations can make hints more cautious than the tier, never less.
- MCP HTTP servers answer 503 `Server closing` once `close()` starts, wait for
  sessions still initializing, and count those sessions against `maxSessions`.
- The Java transport buffers at most 64 response chunks per body and applies
  backpressure to the socket reader. Closing a body discards buffered chunks.
- CI and release preparation fail when a committed lockfile gains a package
  name missing from `.github/dependency-policy.json` or when OSV reports an
  advisory. Release npm installs run with `--ignore-scripts`, and Python
  packages build with hash-locked tools and `--no-isolation`.

- Postman import never copies variable values into contract paths. A
  `{{name}}` path segment is accepted only as a whole segment naming a variable
  declared in the request URL's `variable` list, and becomes the `{name}` path
  parameter; any other path variable is refused (PM005, use `:name`). Base URL
  variables must hold an origin only; a value with a path prefix is refused
  (PM005). Dynamic variables such as `{{$guid}}` are refused in every path
  segment (PM004), including after an unresolved base URL variable.
- `tungsten init` from a HAR capture requires a reviewed path for every
  imported path before saving: a `--route` template (literal paths too, for
  example `--route /health`) or a `--har-paths` JSON mapping of captured paths
  to templates, which the interactive setup also asks for; a
  captured path without one, a `--route` that matches no imported entry, or two
  equally specific matching templates are refused. `HarOptions` gains a
  `route_templates` field.
- `tungsten init` reports JSON setup input it cannot parse (invalid syntax or
  nesting deeper than 128 levels) instead of passing it on as OpenAPI.
- `install.sh --init` and `install.ps1 -Init` print the PATH note before setup
  and no longer report setup as an install failure (exit 1): a cancelled or
  refused setup exits 4 and a failed setup exits 5, with the binary installed.
- `tungsten init --resume --add-targets` keeps every backup: the first is
  `tungsten.yml.init-backup`, later ones `tungsten.yml.init-backup.N`; the JSON
  result names the one written. A flow-style `targets:` mapping is rewritten in
  block style, with a warning when comments inside it were dropped.
- Rust emitter API: `tungsten_emit_rs::Dep::Path` is now
  `Dep::Path { path, version }`; generated `runtime_path` and `cli.kit_path`
  dependencies carry the `runtime` or `cli.kit` requirement (default `0.1`).
- The `ghcr.io/pboachie/tungsten:latest` container tag moves only when the
  distributed tag is itself the newest published stable release (not a draft,
  not flagged prerelease).
- A 3xx status an operation declares (exactly or by class) is its success in all
  seven runtimes, also with `redirects: none`, so a mutation answered with a
  declared `303 See Other` succeeds. Redirects are still never followed on
  mutations, and with `redirects: none` a 3xx the operation does not declare (no
  match, or only `default`) fails as `UNEXPECTED_RESPONSE` without being
  followed. Java, Ruby and C# report that refusal with the redirect remediation the
  other runtimes use.
- Java: `ApiDescriptor` has the `redirects` record component; the eleven-argument
  constructor is removed, so construct it with the canonical constructor.
- Ruby: `ClientOptions` has the `redirects` member (first), which shifts positional
  construction and `deconstruct`; pass options by keyword.
- Rust: `CallOptions` has the public `max_pages` field (pages of one traversal;
  None or zero: unbounded), and `ApiDescriptor` the `redirects` field. Build
  `CallOptions` with `..CallOptions::default()` so later fields do not break
  handwritten literals.

### Fixed

- Wait for the MCP sandbox child to close before removing its temporary files,
  including failed starts and terminated scripts on Windows.
- Bound Postman variable expansion and parsed examples by one import-wide byte
  and node budget (PM001), so a large variable referenced many times cannot
  amplify memory use.
- Compare structured and raw Postman URL paths after the same normalization.
- Record imported schema provenance in time linear in the merged schema and
  samples.
- Filter HAR entries by origin and prefix before validating their URLs, and
  count non-HTTP(S) entries such as WebSocket or extension requests as filtered.
- Accept JSON `Content-Type` parameters such as `charset` in saved cURL and HTTP
  requests.
- Detect deeply nested JSON setup inputs with a JSON parser instead of
  misreading Postman or Swagger files as OpenAPI.

- Make generated Rust CLIs and SDKs packageable: the SDK dependency and local
  `runtime_path`/`cli.kit_path` dependencies carry a version requirement, and the
  generated workspace shares the Rust 1.88 minimum.
- `init --resume --add-targets` edits only the `targets` section of tungsten.yml
  (comments and the schema header stay), links new MCP/WebMCP outputs to the
  TypeScript SDK's local runtime, and can run repeatedly (numbered backups, no
  leftover pending file). Going back in the guided setup no longer keeps the
  abandoned project's answers.

- Warn when conditional JSON Schema constraints are ignored; strict check and
  generation reject them before writing output. Conditional validation is not
  implemented in generated clients.
- Enforce Python native HTTP attempt deadlines across response headers and body,
  joining cancellation and response cleanup; custom transports remain cooperative.
- Normalize embedded Rust support templates so LF and CRLF checkouts generate
  identical support files.

- Preserve explicit confirmation restrictions on imported operations even when
  an earlier scoped review classified the operation as read-only.
- Reject noncanonical confirmation tokens and prototype-derived MCP arguments.
  Confirmation previews bind immutable Blob/File contents and metadata.
- Reserve automatic idempotency keys atomically across clients sharing a store;
  bound Python/Rust asynchronous file-store work and stop dispatch after cancellation.
- Parse fragmented event streams with linear scanning, consistent trailing-CR
  limits and safe oversized retry hints across TypeScript, Python, Rust and Go.
- Apply positive polling deadlines across authentication, transport, retries and
  sleeps; isolate OAuth refresh by scheme with cancellable waiters in Go and Rust.
- Resolve pagination links against the final response URL, preserve nested page
  item types, reject cursor and URL cycles before repeated dispatch, and support
  explicit page limits across the four runtimes.
- Propagate Rust emitter graph facts with worklists and coalesce concurrent MCP
  query embeddings. Complete the README's generated Python SDK installation steps.
- Release a Python sync client's native transport thread, event loop and connection
  pool when the client is garbage collected without `close()`, run polling waits
  on the caller's thread so expiring polls no longer hold worker permits, and never
  block the transport's loop thread on itself when a stream is finalized there.
- File idempotency stores (TypeScript, Python, Go, Rust) record the owner of their
  `<path>.lock` lease (host, pid, token) and wait up to 2 s for another writer before
  refusing with the owner's process and host. Only its owner removes a lease, so a
  lease left by a process that died is removed by hand (the refusal says so); a
  writer re-checks ownership before committing, so it never overwrites a key once its
  lease is gone. Reservations are serialized per store file within a process, and a
  reserved key is kept when removing the lease fails.

- Bound aggregate Swagger reference and media-schema expansion during conversion.
- Require an explicit reviewed HAR path mapping before saving imported contracts.
- Pin the container's zlib package to its patched Alpine release.

### Security

- Reserve MCP session capacity during asynchronous initialization, validate
  manifest safety against executable descriptors, and keep Deno scripts on
  the client RPC channel without direct network permission.
- Bound upstream MCP response bodies to 4 MiB by default (configurable through
  `maxResponseBytes`), and apply WebMCP result byte limits before HTTP body
  materialization. Oversized responses return the SDK's `RESPONSE_TOO_LARGE`
  diagnostic; inconsistent MCP manifest/SDK safety tiers fail before dispatch.
- Require canonical decimal expiry in C#, Java and Ruby confirmation tokens.
- Bound Java transport queues with backpressure and cancellation cleanup.
- Check locked dependency names, sources, hashes and current advisories in CI
  and release preparation; disable npm install hooks in release builds.
- Hash-lock Python release tooling and disable build isolation during package
  creation to prevent unpinned build-tool resolution.
- Keep Postman path parameters explicit and restrict URL variables to origins.

### Added

- Add `defaults.redirects: none` and client redirect options across all seven SDK
  runtimes. Either setting disables every 3xx redirect without retries; the default
  remains same-origin reads only. Regenerate Rust SDKs; handwritten `ApiDescriptor`
  literals must add `redirects: RedirectPolicy::SameOriginRead` (or
  `redirects: RedirectPolicy::None`).

- Guided `init`: a bare `tungsten init` on a terminal starts it (`--interactive`
  asks for it; `--no-interactive`, `--json` or any other `init` option keeps the
  scripted path), with multi-output selection, consent before
  writes, explicit non-interactive generation, additive output edits with backups,
  no-schema inference and scoped TypeScript/Python/Rust client and MCP mock checks.
- Guided Rust local setup allows its first `cargo build` up to 20 minutes
  (previously 10) so slow machines and cold caches finish the one-time build
  instead of being cut off with the project files preserved for a retry.
- The MCP runtime's `tools/list` keeps marking keyed operations
  (`caller_owned`, `auto`, `content_hash` idempotency policies) with
  `idempotentHint` after its caution clamp, matching the generated manifest:
  retrying them with the same key is a no-op. A manifest can still set the
  hint to `false` for a specific tool.
- Guided advanced server/validation controls, restrictive per-operation settings,
  credential-free CLI environment profiles and synthetic JSON request examples.
- Bounded offline Swagger 2 conversion through the OpenAPI loader, preserving
  local definitions, parameter serialization, media types and authentication.
- Imported operations retain review provenance and require argument-bound
  confirmation until explicitly reviewed per operation and exact server origin.
  Scoped reviews require a runtime with import-policy support. Regenerate Rust
  SDKs when upgrading; handwritten `AgentMeta` literals must add
  `import_policy: None` or an explicit policy.
- Import supported Postman Collection v2.1 requests through the offline contract
  engine, preserving explicit authentication and reporting unsupported behavior.
- Parse a bounded saved cURL and HTTP-file subset as offline contract evidence,
  with explicit route templates and no shell execution or implicit file reads.
- Import bounded HAR captures offline with explicit origin selection, reviewed
  path templates, configurable structural redaction, and omission reports.

- Add a bounded offline HTTP evidence engine with deterministic OpenAPI inference,
  provenance, and persistent reviewed overrides.
- Versioned endpoint manifests can combine declared JSON Schema models with
  explicit HTTP routing and auth references using bounded offline model reads.

- TypeScript SDK targets can declare npm dependencies for preserved custom
  extensions; package archives include custom source and license notices.
- The `ascii_token_128` caller-owned idempotency format accepts 1–128 ASCII
  letters, digits, dots, underscores or hyphens in all four SDK runtimes.
- Rust runtime HMAC-SHA256 computation and constant-time verification
  primitives for custom SDK extensions.

- Project setup with `init --targets` and a project-aware `doctor --strict`;
  browser targets automatically include their sibling TypeScript SDK.
- Browser registration lifecycle controller for route and framework cleanup,
  cancellation of stale dynamic imports, and scoped loading.
- Opt-in bounded streaming and binary WebMCP results; response byte limits
  and stream reader cancellation in the TypeScript runtime.
- `diff --migration` upgrade actions, deprecation reporting and recorded
  runtime requirements. API surface snapshots use format 3 and read format 2.
- A transport-neutral, versioned tool catalogue with JSON Schema, scoped
  groups and external emitter conformance helpers (`schema tool-catalog`).
- Runtime tarball installation checks and gated npm trusted publishing for
  the TypeScript, MCP and WebMCP packages.

- Built-in `webmcp` target: a browser ESM package with a shared tool manifest
  and an explicit registration helper backed by the generated TypeScript SDK
  and the new Apache-2.0 `@elysra/tungsten-webmcp` runtime. Registration defaults to
  read-only operations, supports scoped opt-in writes and macros, confirmation
  previews, cancellation and disposal. The runtime adapts the experimental
  `document.modelContext` API and the legacy `navigator.modelContext` shape;
  generated-package browser smoke passed in Chromium 154.0.8037.57 with
  `WebMCPTesting` enabled. Full cross-browser conformance remains a follow-up.

- A Java target (`java`, `tungsten-emit-java`) and the Java runtime
  `runtimes/java` (`io.github.pboachie.tungsten:tungsten-runtime`, Java 17,
  standard library only): records for models, sealed interfaces for unions,
  typed resource methods returning `Result`, pages, event streams, previews,
  macros and `Dispatch`; a Maven project per SDK. Diagnostics TG1010 to
  TG1015.
- A C# SDK target, `csharp` (short form `cs`): a net10.0 project with records,
  enums and unions that decode and encode themselves (no reflection, trimming
  and AOT safe), async methods returning `Result<T>`, `IAsyncEnumerable` pages
  and `EventStream<T>` streams, typed macros and an `IDispatch` entry point,
  over the new dependency-free .NET runtime `Tungsten.Runtime`
  (`runtimes/csharp`). Options `package_id`, `namespace`, `version`,
  `runtime` and `runtime_path`; diagnostics TG1020 to TG1025.
- The language-neutral foundation of the coming Java, C#, Kotlin, Swift, PHP,
  Ruby and Dart SDKs in `tungsten-emit` (`sdk`): one SDK plan per API (types,
  operations, resource tree, macros), naming profiles for the seven languages
  in `tungsten-ir`, and the SDK descriptor document their runtimes read
  (`tungsten schema sdk-descriptors`). The repository guard knows the seven
  runtime directories. No target is added yet and existing output is
  unchanged.
- Resumable event streams in the TypeScript, Python and Rust runtimes and the
  MCP server: a dropped stream reconnects with `Last-Event-ID`, honouring the
  server's `retry` under a cap (`maxReconnects`, default 3), without repeating
  delivered events; an idle timeout ends or reconnects a silent stream
  (`STREAM_IDLE`); `on`, `collect`, `reduce`, `first` and `cancel` helpers.
  `tungsten mock` continues a stream after `Last-Event-ID` and can drop or
  stall one for testing.
- Optional embedding index for the MCP server's `search_tools`, off by
  default and with no model shipped. `targets.mcp.search.embeddings` names a
  provider (a command speaking a small JSON protocol, or a plain `http://`
  `/embeddings` endpoint); `tungsten generate` writes `index.embeddings.bin`
  and `index.embeddings.json` next to `manifest.json`, caching vectors by
  content hash. `@elysra/tungsten-mcp` ranks hybrid (BM25 and cosine, reciprocal
  rank fusion) when `search.embeddings` is set, and falls back to BM25 with
  the reason in the result when the query embedder fails. Diagnostics
  TG0570 to TG0574.

### Changed

- Rust runtime: `StreamEvent` has a new public field `reconnects`; code that
  builds or destructures `StreamEvent` with a struct literal must add it (or
  use `..`).

- Go SDKs through the external emitter interface: `emitters/go` builds
  `tungsten-emit-go` (enable it with `go: { out: ..., external: true,
  module: ..., package: ... }`), which generates a Go package with models,
  resources, a typed client, operation descriptors, pagination and stream
  helpers, macros and a README. The SDKs use the new Go runtime
  `github.com/pboachie/tungsten/runtimes/go` (standard library only, Go
  1.22 or later), with the same error envelope, retries, idempotency,
  previews, confirmation tokens, auth handling and resumable event streams
  as the other runtimes.

## [0.1.1] - 2026-10-11

Release engineering only: npm package READMEs for the published TypeScript
packages and resolvable Woodpecker workflow status URLs. No compiler, runtime
or generated-output changes, and nothing to migrate.

## [0.1.0] - 2026-10-11

First release. The compiler, the three SDK runtimes (`@elysra/tungsten-runtime`,
`tungsten-runtime` on PyPI and `tungsten-runtime` on crates.io), the MCP
runtime (`@elysra/tungsten-mcp`) and the installers ship together at 0.1.0.

### Added

- Compiler: OpenAPI 3.0 and 3.1 frontend, `tungsten.yml` and `agent.yml`
  manifests, a versioned intermediate representation (`tungsten ir dump`,
  `tungsten schema`) and diagnostics with stable codes (`tungsten check`,
  `tungsten explain`).
- Commands: `init`, `check`, `generate` (with `--check` for stale output),
  `diff`, `report` (text, JSON and a self-contained HTML report), `doctor`,
  `ir dump`, `schema`, `explain` and `mock`.
- TypeScript SDK emitter and the `@elysra/tungsten-runtime` client runtime
  (Apache-2.0): `Result` values, Zod validation, the diagnostic error
  envelope, safety tiers, previews with confirmation tokens, idempotency keys,
  unknown-outcome reporting and retries.
- Python SDK emitter (sync and async clients, strict Pydantic v2 models or
  plain dataclasses) and the `tungsten-runtime` Python package (Apache-2.0).
- Rust SDK emitter (async client, a blocking client and typed macro outputs)
  and the `tungsten-runtime` crate (Apache-2.0).
- Generated command-line interface for the Rust SDK, built on
  `tungsten-cli-kit`, with a stable output contract, exit codes,
  confirmation for destructive operations (`--yes`, `--i-understand`),
  configuration and pagination.
- MCP server emitter and the `@elysra/tungsten-mcp` runtime: discrete or
  token-budgeted progressive tool surfaces, an automatic mode chosen from the
  tool count, stdio and Streamable HTTP transports, a sandboxed script tool
  and confirmation of destructive operations.
- Streaming: server-sent events and JSON Lines responses in all three SDK
  runtimes (`stream()` and `<method>_stream` methods) and in the MCP server,
  which returns the collected events.
- Pagination: opaque string cursors and `after_id` / `has_more` styles are
  inferred from the OpenAPI description.
- Documentation emitter: `llms.txt`, `llms-full.txt`, `tools.json` and a
  README for each generated SDK.
- `tungsten mock`: a mock server generated from the OpenAPI description, for
  trying a generated SDK, CLI or MCP server without a live API.
- Behaviour that is identical across the TypeScript, Python and Rust
  runtimes, checked by a shared contract suite.
- Release pipeline: prebuilt archives for Linux (x86_64, aarch64), macOS
  (x86_64, aarch64) and Windows (x86_64) with a checksum file, a CycloneDX
  SBOM and build provenance attestations, plus `install.sh` and `install.ps1`
  installers that verify checksums.
- OAuth2 authorization-code helpers in the TypeScript, Python and Rust
  runtimes: PKCE (S256), the authorization URL, the code exchange, refresh with
  a single flight and a pluggable token store. A call sends the stored token,
  refreshes it before it expires and once after a 401. Generated clients expose
  the helpers only when the specification declares an `authorizationCode` flow.
- Repository guard and community files (contributing guide, security policy,
  code of conduct, issue forms and pull request template).
- `tungsten lsp`: a Language Server Protocol server over stdio for
  `tungsten.yml`, `agent.yml` and overlay files. It reports the same
  diagnostics as `tungsten check` on the YAML lines they belong to, completes
  keys, values and operation ids, documents fields and operations on hover,
  jumps from an operation id to its place in the OpenAPI document and
  outlines the manifests. See the README for editor setup.
- External emitters: a target of `tungsten.yml` with `external: true` (or a
  command) is generated by a separate `tungsten-emit-<name>` executable that
  speaks a small JSON protocol (the IR on standard input, files and
  diagnostics on standard output). The compiler writes the files, so
  `generate --check`, the output manifest and the API surface diff work
  unchanged. New: `tungsten emitters`, `tungsten schema external-emitter`,
  diagnostics TG0801 to TG0806 and the example `examples/external-emitter-markdown`.

### Changed

- `tungsten init` writes a default `targets` block.
- Documents that omit `info.version`, use query-style path keys, or leave out
  an API title now compile, with a warning where the result is inferred.
- Types no operation reaches are pruned from the generated models.
- Required constant headers and API-key environment variables are carried
  into the generated clients.
- Error codes are found through tagged unions, and unknown-outcome messages
  describe what the caller should verify.
- The compiler warns when a `gates` entry in `agent.yml` matches no
  operation.

[Unreleased]: https://github.com/pboachie/tungsten/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/pboachie/tungsten/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/pboachie/tungsten/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/pboachie/tungsten/releases/tag/v0.1.0
