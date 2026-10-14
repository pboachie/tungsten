# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project will adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
from its first stable release. Until 1.0, minor versions may contain breaking
changes.

## [Unreleased]

### Added

- Generated Rust CLIs: `--max-pages <N>` (with `--all`) stops paging after N
  pages and notes on standard error that more pages are available. There is no
  default limit.

### Changed

- Generated Rust CLIs: `--all` writes its array page by page instead of
  collecting every item first. The output is byte-for-byte the same on
  success; a failure after the first page leaves the array unterminated and is
  reported on standard error. `Dispatch` gains the provided method
  `pages_each`, which generated clients implement lazily.

## [0.1.2]

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

### Fixed

- Wait for the MCP sandbox child to close before removing its temporary files,
  including failed starts and terminated scripts on Windows.

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

### Security

- Reserve MCP session capacity during asynchronous initialization, validate
  manifest safety against executable descriptors, and keep Deno scripts on
  the client RPC channel without direct network permission.
- Bound upstream MCP response bodies to 4 MiB by default (configurable through
  `maxResponseBytes`), and apply WebMCP result byte limits before HTTP body
  materialization. Oversized responses return the SDK's `RESPONSE_TOO_LARGE`
  diagnostic; inconsistent MCP manifest/SDK safety tiers fail before dispatch.
- Bound Java transport queues with backpressure and cancellation cleanup.
- Check locked dependency names, sources, hashes and current advisories in CI
  and release preparation; disable npm install hooks in release builds.
- Hash-lock Python release tooling and disable build isolation during package
  creation to prevent unpinned build-tool resolution.

### Fixed

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

[Unreleased]: https://github.com/pboachie/tungsten/compare/v0.1.1...HEAD
[0.1.2]: https://github.com/pboachie/tungsten/compare/v0.1.1...HEAD
[0.1.0]: https://github.com/pboachie/tungsten/releases/tag/v0.1.0
