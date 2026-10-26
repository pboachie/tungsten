# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project will adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
from its first stable release. Until 1.0, minor versions may contain breaking
changes.

## [Unreleased]

### Security

- Fix verified Python packaging bootstrap on Windows and temporary paths with
  punctuation. Enter pip as a module and install the wheel through a local file
  URI, preserving hash verification and isolated, offline installation.

- Bootstrap Python package installation with the hash-verified pip 26.2.2 wheel,
  including fresh environments, and reject uv versions older than 0.13.4.
  Require certifi 2026.10.19 or newer in the Python runtime, without changing
  its Python compatibility floor. These packaging mitigations do not claim
  the still-unreleased CPython intermediate-certificate denylist fix.

- Refresh the locked MCP HTTP-body and URI parsers and schema validator to
  address request-body denial of service, URI normalization confusion and
  oversized-collection validation advisories.
- Pin the remaining example, Windows acceptance and release Node.js jobs to
  the October security releases (22.23.4 and 24.22.1), avoiding stale cached
  major-version selections without changing runtime compatibility floors.
- Validate native runtimes with patched Go 1.26.10, Temurin 21.0.13,
  .NET SDK 10.0.402, Ruby 3.3.13, Dart 3.13.7 and Deno 2.9.8 toolchains.
  Source-language and target framework declarations remain unchanged; EOL Go
  toolchains are not security-qualified runtimes.
- Qualify Java CI HTTPS trust with per-job copies excluding the four affected
  CA roots. Preserve installed stores and all other trust entries; certificate
  chains relying on those roots intentionally fail.
- Update the pinned PyPI publishing action to 1.14.3 for its bundled
  certificate-trust security fix; publication gates and permissions are unchanged.

- Refresh the locked TLS verification, CA bundles and MCP address parser, and use patched Node.js releases for package validation.
  The `rustls-webpki` and `webpki-roots` updates reject distrusted emSign
  certificate paths (RUSTSEC-2026-0382 and RUSTSEC-2026-0381).

### Documentation

- Add the `webhook-receiver` gallery recipe: TypeScript, Python and Rust
  receivers for a non-sensitive `order.shipped` webhook verify signed
  deliveries with example secrets, refuse a replay, a tampered body, a stale
  timestamp and an incomplete payload, accept a rotated-secret delivery, and
  print identical outcomes, without a web framework or network.

- Add a migration note contrasting the new opt-in total operation deadline
  with the existing per-attempt `timeoutMs` and its equivalents, across the
  runtimes, the generated Rust CLI and the MCP server.

- Expand the Postman import and generation-freshness guides with contract
  review, source-checkout prerequisites and links to the existing example
  workflows.

- Clarify TypeScript runtime API contracts, streaming budgets, OAuth attempts
  and replay-key storage, with separate core and Node adapter overviews.

- Document generated Rust CLI workflows for command discovery, deployment
  profiles, structured input, previews and confirmation, and script recovery
  from errors or partial pagination.

- Clarify the OAuth URL-as-data boundary and safe browser integration across
  SDKs and MCP hosts. HTTP(S) validation is not shell escaping; compatibility
  helpers retain application-owned custom-scheme policy. No browser launcher or
  URL-policy behavior is added or changed.

### Breaking

- C# and F# schema `min_length` and `max_length` now count Unicode code points (a
  surrogate pair is one character, a lone surrogate is one) instead of UTF-16 code
  units, as JSON Schema defines. Strings with characters above U+FFFF are no
  longer over-counted; a value that fit only because of the old count can now be
  rejected.


- Java and Ruby string `min_length`/`max_length` now count Unicode code points,
  as JSON Schema does, instead of UTF-16 code units: an emoji is one character
  (a surrogate pair is one, a lone surrogate is one). Strings with characters
  above U+FFFF that were rejected for length are now accepted, and the reverse
  never happens. Go already counted code points (an invalid UTF-8 byte counts
  as one). Kotlin delegates to Java and changes with it. Other runtimes align in
  their own entries.

- TypeScript, Python and Rust schema validation now counts string `minLength` and
  `maxLength` in Unicode code points, as JSON Schema does. TypeScript previously
  counted UTF-16 code units, so a string with characters outside the Basic
  Multilingual Plane (an emoji) counted twice and could be rejected at half the
  declared limit; regenerate TypeScript SDKs to get `withLength`. Python and Rust
  already counted code points and keep doing so; a lone surrogate counts as one in
  every runtime.

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

- TypeScript and Python stream `collect()` with no arguments is now bounded like the
  other runtimes: 4 MiB of canonical-JSON event data and 60 seconds, the defaults
  already applied when either budget was set. Pass explicit larger finite
  `maxBytes`/`maxMs` (`max_bytes`/`max_ms` in Python), or consume long or large
  streams incrementally with iteration, `reduce` or `on`.

- The Dart, C# and F# SDKs give three shapes real types that were the JSON value
  (`Object?` in Dart, `JsonNode` in C# and F#) until now. An `allOf` that cannot
  be merged into one record (a member is a union, an array or a record with typed
  extra members, or two members declare one property with different types) is a
  class/record with one typed part per member: each part decodes the whole value
  and `toJson`/`ToJson` merges the parts. An enum of booleans or fractional
  numbers (in C# and F# also of integers beyond 64 bits) is an enhanced enum
  (Dart), a readonly record struct (C#) or a union with `Unknown of JsonNode` (F#)
  of its JSON values, compared as JSON. An operation whose success bodies differ by
  exact status returns a union of its answers (Dart `<Resource><Operation>Response`
  with one `...Status<code>` subclass per status, C# a nested `<Operation>Response`
  record, F# `<Class><Member>Response`) holding the typed body of the response's
  HTTP status (none for an empty one) and its `status`. Code that read these values
  as `Object?`/`JsonNode` should use the typed parts or answers; the wire form and
  the runtime's validation are unchanged. A success status the operation does not
  declare fails as an unexpected response. Limits that remain: an `allOf` without
  members (TG1021, TG1071, TG1081) and, in Dart only, an enum with an integer
  beyond 64 bits (TG1071; a Dart `int` is 64 bits) keep the JSON value, and so do
  success bodies that differ but are not one JSON body per exact status (a range or
  `default` status, a text or binary body among them). TG1024, TG1074 and TG1084
  are no longer reported.

### Added

- Add the `file-transfer` gallery recipe (`python3 examples/run.py --only
  file-transfer`): TypeScript, Python and Rust SDKs upload a 16 MiB file from
  disk with a recreatable `UploadSource` and download it to a caller-chosen
  path with matching SHA-256, and show a refused second save, a byte limit and
  a missing file.
- Generate streamed transfer methods in TypeScript, Python and Rust SDKs. An
  operation whose 2xx response is a binary body gets a `<method>Download` /
  `<method>_download` method returning the runtime's `Download` (Rust: async
  client only, with `DownloadOptions`). A whole binary request body accepts an
  `UploadSource` (TypeScript `Uint8Array | UploadSource`, Python
  `BinaryInput`), and Rust SDKs add `<method>_upload(request, source, opts)`.
  Regenerated SDKs for such operations change; other operations are unchanged.
- Gallery recipe `android-jni`: call a generated Rust SDK from Java through a
  minimal JNI bridge (`examples/android-jni`), with one Tokio runtime per
  native handle, the total deadline and response-size bound, diagnostic
  envelopes as Java exceptions, cancellation on `close()` and freeing without
  use after close. It runs on the development host against a loopback mock;
  `TUNGSTEN_ANDROID_TARGETS` adds a `cargo check` per Android target. The
  Rust runtime README's Limits section states the Android scope: it
  type-checks for `aarch64-linux-android` and `x86_64-linux-android`, is not
  device-qualified, and uses bundled Mozilla roots rather than Android's
  trust store.

- Go, Java, Kotlin, C#, F#, Ruby and Dart SDKs honour multipart per-part
  `encoding.contentType` with the same rules as the TypeScript, Python and Rust
  SDKs. The SDK descriptor document's `BodyDescriptor` gains an optional
  `parts` list, and the targets list the `multipart_part_encoding` capability.

- Multipart request bodies honour the OpenAPI `encoding` map: a field whose
  Encoding Object declares a `contentType` is sent with that media type (the
  first entry of the list without wildcards) in TypeScript, Python and Rust
  SDKs. A file part without its own media type takes the declared one; an
  object part keeps JSON and uses the declared type when it is a JSON media
  type; a scalar part is sent as a typed part, JSON-encoded for a JSON media
  type. IR `BodyContent.parts` (`PartEncoding`) is additive and the feature is
  listed as `multipart_part_encoding`. Per-part `headers`, and positional or
  nested multipart encoding (TG0146/TG0147), are not supported.

- Save streamed downloads to a caller-chosen file: `saveDownload` in the
  TypeScript runtime's `/node` entry point, `Download.save_to` /
  `AsyncDownload.save_to` in Python and `Download::save_to` in Rust. Bytes go
  to an exclusively created `<name>.<random>.part` file next to the
  destination, which replaces it only after the whole body arrived; on any
  failure the partial file is removed and the destination is untouched. An
  existing destination is refused (`DESTINATION_EXISTS`) unless overwrite is
  set. Server-suggested file names never choose the destination.
- The runtime telemetry observer reports macro runs and previews in
  TypeScript, Python and Rust as one `macro` or `macro_preview` operation
  named after the macro, covering every step. Unreleased 0.2.0 only.

- The runtime telemetry observer reports `download()` (`entry` `download`,
  ending when the answer is handed over) and page exports (`export_pages`,
  like `pages`) in TypeScript, Python and Rust. Unreleased 0.2.0 only.

- Add the `examples/telemetry` gallery recipe: TypeScript, Python and Rust
  SDKs collect correlated operation, attempt and phase telemetry in memory for
  a read, a retried read and a missing record, print the same summary and
  check that no event holds a URL or payload.

- Add streamed upload sources to the TypeScript, Python and Rust runtimes
  (`UploadSource`; Rust `ClientCore::call_upload` / `preview_upload`) for a
  whole binary request body. A source is one-shot (never sent again once
  read: no retry, redirect or token-refresh resend) or recreatable (opened
  again for every attempt). A declared length (sent as `Content-Length`) and
  SHA-256 are checked against the bytes, holding back the last chunk so a
  mismatching source never completes its request (`UPLOAD_MISMATCH`); a
  failing source reports `OUTCOME_UNKNOWN` for a mutation
  (`UPLOAD_SOURCE_FAILED`). Operations whose idempotency key or confirmation
  depends on the body require a declared SHA-256, which is also the
  `content_hash` key. Registered as the `streaming_uploads` runtime
  capability.
- Add `tungsten update notice` to show whether update notices are on, where the
  setting comes from (default, environment, settings file, opt-out or an
  invalid value), the settings and cache paths and the cached answer.
  `--enable [stable|prerelease]`, `--disable` and `--interval-hours N` update
  the user settings file atomically and keep its other keys; a file that is not
  a JSON object is never overwritten. It never makes a network request.
  `--json` output adds the `update notice` command to the CLI output schema.

- The runtime telemetry observer reports `stream()` in TypeScript, Python and
  Rust as one `stream` operation covering its connection and every reconnect,
  ending `ok`, `error` (final failure item) or `cancelled` (cancelled or left
  early). Unreleased 0.2.0 only.

- `tungsten update check` links the upgrade rehearsal: when a newer release is
  available, `rehearse_command` (new `--json` field) holds the
  `tungsten upgrade rehearse --to <version> --workspace <dir>` invocation, and
  the guidance for migrations names `upgrade rehearse` and `upgrade apply`
  instead of the separate preview commands.

- Concurrent commands of a generated Rust CLI refresh a stored login session
  one process at a time: the token is refreshed before the call under a
  per-session lock file and read again first, so a refresh token is not
  presented twice to providers that rotate refresh tokens.

- The runtime telemetry observer now also reports `preview()`, `pages()` and
  `poll()` in TypeScript, Python and Rust as operations (`entry` `preview`,
  `pages` or `poll`), each covering every request it made with attempts
  numbered across it; a page iteration stopped early is `cancelled`. Only a
  `call` operation's end carries the final `http_status`. Unreleased 0.2.0
  only.


- OpenAPI 3.1+ `webhooks` are compiled. Each operation of a `webhooks` entry
  becomes an incoming webhook in the IR (`Namespace.webhooks`, omitted when
  empty: event key, method, operation id, docs and the JSON request body as a
  typed payload); webhooks are never callable. TypeScript, Python and Rust
  SDKs of an API that declares webhooks gain a `webhooks` module: payload types
  and schemas keyed by the `webhooks` key (`WebhookPayloads` /
  `webhookSchemas`, `WEBHOOKS`, the `WebhookName` and `WebhookEvent` enums)
  and `parseWebhook` / `parse_webhook`, which verifies a delivery with the
  runtime's Stripe-compatible verifier and validates the payload, constraints
  included, before the replay claim; a mismatch is `payload_invalid`. The
  runtimes gain the matching hook (`parse` in TypeScript and Python,
  `verify_webhook_with` in Rust). Registered as the `typed_webhooks` target
  feature. Unreleased 0.2.0 only.
- The TypeScript and Python runtimes include an optional OpenTelemetry
  adapter for the telemetry observer (`openTelemetryObserver(tracer)`,
  `open_telemetry_observer(tracer)`): one client span per operation named after
  its operation id, attempts and phases as span events, outcome attributes and
  error status, with at most 10000 spans open. It accepts any tracer of the
  OpenTelemetry API shape and adds no dependency. Unreleased 0.2.0 only.

- TypeScript, Python and Rust clients can report optional local telemetry to
  an observer (`ClientOptions.telemetry`), off by default and never sent
  anywhere by the runtime. `call()` emits correlated `operation`, `attempt`
  and `phase` events (authentication, scheduler queueing, retry backoff) with
  the same versioned fields in every runtime: durations, outcomes (including
  `cancelled`, `deadline_exceeded` and `outcome_unknown`), diagnostic
  categories, HTTP status codes and queue reasons, but never URLs, headers,
  bodies, credentials, tenant labels or error text. A bounded
  `MemoryTelemetryCollector` keeps recent events and counts dropped ones. The
  capability catalog declares the `telemetry_hooks` feature for these targets.
  Unreleased 0.2.0 only.



- `tungsten workspace --run check|generate|lock [-- ARGS]` runs the command on
  each selected project, one at a time in discovery order, and reports every
  project's exit code and diagnostics (`runs` in the JSON result); the
  workspace exits with the highest code. `--affected FILE` (repeatable)
  selects the projects that depend on a file: its manifest, a declared input,
  or a file the project's offline compilation read or looked for; a project
  that does not compile is always selected. A writing `generate` is refused
  before any project runs when selected projects share output directories
  (TG0642). The capability catalog lists the commands under
  `workspace.runs`.
- Add explicit streamed downloads to the TypeScript, Python and Rust
  runtimes: `download()` sends an operation by the rules of `call` and hands a
  2xx body over unread, to read chunk by chunk or write to a caller-owned sink
  with backpressure. A download has a total-byte limit (`maxBytes`, checked
  against `Content-Length` before the first byte and against the bytes read),
  progress reports, the idle timeout and the call deadline; a short body,
  a lost connection, cancellation and a failing sink end it with a diagnostic,
  and the connection and its scheduler capacity are released however it
  ends. The `Content-Disposition` file name is reduced to a plain base name.
  Buffered calls, `maxResponseBytes` and event streams are unchanged.
  Registered as the `streaming_downloads` runtime capability.
- Generated Rust CLIs keep login sessions in the Windows Credential Locker
  (through Windows PowerShell) by default on Windows. A session the locker
  refuses, for example one above its secret size limit, fails with advice to
  choose the owner-only file store explicitly.

- `tungsten lsp` serves every project in the editor's workspace folders, found
  as `tungsten workspace` finds them, plus any `tungsten.yml` that is opened.
  Edits, saves and watched changes re-check only the projects that read the
  changed file, so one project's edits never dirty another while a shared spec
  (including its unsaved buffer) re-checks every project using it. One check
  runs at a time across the session; diagnostics of a file read by several
  projects are merged; completion, hover, definitions and quick fixes come from
  the project owning the document. Adding or removing workspace folders
  (`workspace/didChangeWorkspaceFolders`) starts or cancels their projects'
  checks and clears stale diagnostics. Discovery findings (TG0640-TG0643) are
  shown on the files they name. The capability catalog's
  `workspace.editor_routing` is now `true`.
- Add durable checkpoints for resumable read-only page exports to the
  TypeScript, Python and Rust runtimes (`exportPages` / `export_pages`). Each
  page carries a caller-owned, HMAC-sealed `checkpoint` token to persist after
  the page is committed to the sink; passing it back resumes with the next page
  after a restart. Tokens bind the operation, pagination contract, base URL,
  tenant, scope and arguments without storing argument values or credentials,
  and malformed, altered, mismatched, expired (`maxAgeMs` / `max_age_ms`) or
  stopped tokens are refused before any request. A sink commit followed by a
  crash before the token is saved replays that page once, so sinks must
  deduplicate. `describeCheckpoint` / `describe_checkpoint` reports status
  without the cursor. Registered as the `pagination_checkpoints` capability for
  the `typescript`, `python` and `rust` targets. In Rust, `ExportPages::typed`
  decodes items and `blocking::Runtime::export_pages` iterates an export with
  blocking calls. Generated Rust CLIs accept `--checkpoint-file <PATH>` with
  `--all` on read-only paginated commands: with `<PREFIX>_CHECKPOINT_KEY` set
  to a secret of at least 32 bytes, the checkpoint is saved owner-only after
  each page is printed, and rerunning with the same file prints only the
  remaining pages; delete the file to start over. `checkpoint-file` is now a
  reserved flag name.

- Generated Rust CLIs sign in to OAuth2 `authorizationCode` schemes:
  `auth login` runs the runtime's managed S256 flow through a loopback
  redirect (bounded by the 10-minute attempt and `--deadline`), `auth switch`
  selects a stored account per profile, `auth logout` removes local sessions
  (tokens are not revoked at the provider) and `auth status` lists sessions
  without secrets. Sessions are partitioned by scheme, account, token
  endpoint, client id, base-URL origin and scopes. Tokens live in the macOS
  keychain or the Secret Service (`secret-tool`), or in an owner-only file
  only when `--store file` / `session_store = "file"` is chosen; a locked or
  missing keychain never falls back to plaintext. Login refuses to run in CI
  or without a terminal, and no other command starts one.

- `tungsten config` now inspects the C#, F#, Dart, Java, Kotlin, Ruby, MCP,
  WebMCP and docs target options declared in the capability catalog (status
  `declared_fields`): each declared field reports its authored value or fixed
  default, a value of the wrong JSON type is reported with the target's
  invalid-option warning code, and only package identity fields are exported
  unredacted. The emitter still validates values when it runs.
- The `init --targets` help lists the default and companion targets from the
  capability catalog.

- Add `tungsten refresh-import` to refresh a converted import (Postman, HAR,
  saved cURL, HTTP file, Swagger or endpoint manifest) in place instead of
  re-importing into a fresh project. It converts the new source revision
  (`--from`, or the recorded `source_url`) and previews the converted-document
  diff, added, removed and changed operations with the facts that changed, the
  approved imported operations that need renewed review, the regenerated
  output and API surface changes, and the generation lock outcome. `--apply`
  writes only after every changed approval is confirmed with `--accept-review`
  and an optional `--expect` digest still matches, then updates the
  conversion, import report, `source_sha256`, the remote pin and a current
  `tungsten.lock.json` together. Overlays, agent.yml and generated outputs are
  not modified. An interrupted apply is detected from its journal and restored
  with `--rollback`. The capability catalog lists the command for each
  converted input format as `refresh_command`.
- `init` records the reviewed importer settings of a saved cURL, HTTP file or
  HAR conversion (format, routes, shell dialect, HAR origin, path templates,
  prefixes and redactions) under `settings` in `import-report.json`, and
  `refresh-import` reuses them for flags that are not given again. `check` and
  `generate` refuse a project with an interrupted `refresh-import --apply`
  (TG0124) until `--rollback` restores it.
- `refresh-import --apply` stages the reviewed content beside its backups, so
  `refresh-import --resume` finishes an interrupted apply (writes, pin and
  lock) as an alternative to `--rollback`.
- `refresh-import` refreshes converted inputs acquired from Git sources
  (`git+https://…@ref#path`). The preview resolves the ref into a private
  scratch cache; `--apply` installs exactly that reviewed commit's pin and
  tree, so the ref is never resolved again between review and apply.
- The language server offers a disabled "Refresh the converted import" action
  on TG0122 for a converted input whose remote source changed, naming the
  `refresh-import` preview, `--apply` and `--offline` choices.

- Add `tungsten upgrade rehearse`, run with a candidate release's binary: it
  regenerates the project from its locked inputs, offline and without
  touching the project, and reports compiler, emitter, consumer toolchain,
  runtime family and option identity changes against `tungsten.lock.json`,
  input drift, file and API surface changes with `diff --migration` actions,
  and conflicts with edited generated files or files tungsten does not own.
  Generated-source compatibility is reported apart from package availability
  and runtime acceptance, which it does not check. With `--workspace` it
  stages the output, proposed lock and rehearsal record for review;
  `--from`/`--to` check the explicit identities. External emitters are never
  started.
- Add `support-bundle --rehearsal DIR` to include an allowlisted summary of an
  upgrade rehearsal workspace as the observed `upgrade.rehearsal` entry
  (bundle schema version 3): versions, verdict, check states, built-in
  target ids with change and conflict counts, finding kinds and the apply
  state, never paths, digests, file names or finding text. `--inspect`
  reads schema versions 1 to 3.
- Add `upgrade rehearse --consumer-target TARGET -- COMMAND` to run one
  explicit clean-consumer command (no shell) in a target's staged output
  after staging, with `--consumer-timeout-secs`. Its outcome, exit code and
  log are recorded, it sets `runtime_acceptance` to passed or failed, and a
  failure blocks `upgrade apply`. The command runs with your privileges and
  environment; it is not sandboxed.
- Add `upgrade rehearse --candidate FILE` to take the release selected by a
  saved `tungsten update check --json` result as the explicit candidate. It
  is read offline; the rehearsal is blocked unless the candidate is the
  running compiler and was discovered by the locked release, and notes when
  discovery did not confirm the platform archive. Discovery never upgrades.
- Add `tungsten upgrade apply --workspace DIR` to apply an accepted rehearsal
  as a separate step. It refuses when inputs, the lock, the staged files or
  any output file changed since the rehearsal, backs up what it replaces,
  keeps custom files, and records a journal; `--restore` undoes an
  interrupted or completed apply.

- `tungsten update check` classifies a newer release as `compatible` or
  `migration` (new `upgrade` field in `--json`). Before 1.0 a minor release, from
  1.0 a major release, any prerelease, or notes with a Breaking section count as
  migration; release notes can only make it stricter. Guidance names the
  non-writing preview commands (`diff --migration`, `lock --preview`,
  `generate --dry-run`) for migrations and `generate --check` otherwise, and the
  cached update notice says when a migration may be required.

- TypeScript, Python and Rust runtimes verify incoming webhooks without a web
  framework: `verifyWebhook` / `verify_webhook` check the Stripe-compatible
  `t=<unix>,v1=<hex HMAC-SHA256 of "t.body">` signature header against one or
  more secrets (rotation) in constant time, cap the body (default 1 MiB) and
  the header (8192 characters), enforce a timestamp tolerance (default 300
  seconds) and parse the JSON payload. Checks run in the same order in every
  runtime and a rejection reports one stable reason (`body_too_large`,
  `secret_missing`, `signature_missing`, `signature_malformed`,
  `signature_mismatch`, `timestamp_out_of_tolerance`, `payload_invalid`,
  `duplicate`) with a message that never holds the secret, signature or
  payload. An optional application-owned `claim` hook rejects repeated
  deliveries (keyed by the event `id`, else the timestamp and signature);
  storage, queues and acknowledgement stay with the application.
  `signWebhook` / `sign_webhook` build a header for local examples. Registered
  as the `webhook_verification` runtime feature. Unreleased 0.2.0 only.

- Add `tungsten_build::ProjectIdentity`, the shared in-process identity of
  one compilation: its manifest, the content digest of every source it read,
  the optional files it found absent, and a digest of the compiler version,
  load limits, acquisition and remote-reference policy and invocation policy.
  It revalidates against unsaved buffers and disk without modification times,
  reports which files a project depends on, and is never persisted. The
  language server uses it for result reuse, and a watched-file change to a
  file the last check neither read nor looked for no longer cancels or starts
  a check.

- `tungsten workspace [PATH]` lists the tungsten projects under a directory
  with deterministic ids (the project directory relative to PATH), their
  output directories and declared inputs. Discovery is bounded (8 levels,
  4,096 directories, 64 projects, 1 MiB manifests; TG0640), never follows
  symbolic links (TG0643), refuses directories with two manifests (TG0641)
  and reports output directories shared or nested across projects (TG0642).
  `--project ID` on `check`, `generate`, `lock`, `report`, `diff`, `mock`,
  `ir dump`, `overlay suggest` and `scenario` runs the command on one project
  of the workspace at PATH (unknown ids are TG0644), and `generate` refuses to
  write a project whose outputs collide with another's. The capability catalog
  declares the workspace contract under `workspace`.

- The `actions/tungsten` GitHub Action runs saved mock scenarios after the
  checks (`scenarios`, `scenario-input`) through `tungsten scenario run`, one
  line per scenario split on whitespace without a shell; a failed assertion
  fails the step, and scenarios are refused on `pull_request_target`.
- The `actions/tungsten` GitHub Action checks the projects of a workspace
  (`workspace`), narrowed by project id (`workspace-projects`) and by
  `tungsten workspace --affected` evidence for listed files (`affected-files`)
  or for the files changed since a base commit (`affected-base`), and reports
  the number of checked projects in a `projects` output.
- A reusable composite GitHub Action, `actions/tungsten`, installs a pinned
  tungsten release through `install.sh` from the action's own commit (checksum
  and build provenance verified, provenance cannot be disabled), then runs
  `tungsten check`, `tungsten generate --check` and `tungsten lock --check`
  for one or more projects. It writes one log per check, a run summary and a
  `failed` output, keeps external targets off by default and ignores
  `allow-external` for fork pull requests and `pull_request_target`. Linux
  and macOS runners; `examples/github-actions/tungsten-action.yml` shows it in
  a workflow.
- Add opt-in update notices. `TUNGSTEN_UPDATE_NOTICE=stable|prerelease` or the
  `update_notice` entry of the user settings file enables them; they are off by
  default. With notices on, `tungsten update check` caches its verified answer,
  and interactive human-mode commands print at most one stderr line per interval
  (default 24 hours) naming a newer release whose platform archive and
  `SHA256SUMS` were confirmed. Notices never make a network request and are
  suppressed with `--json`, `--offline`, `--policy`, in CI, without a terminal
  and for `lsp` and `mock`. Cached answers expire after 7 days; a corrupt or
  unwritable cache is ignored.

- Add the `examples/restricted-agent` customization example: an MCP server
  that reads freely, confirms writes and hides deletion, with transport
  references and SDK defaults from a shared profile and a check that verifies
  sources and the generated tool manifest offline.
- Declare proxy, `NO_PROXY`, private CA, mTLS and account/provider references
  under `transport` in profiles and `tungsten.yml`. Values are references
  (`{env: NAME}`, `{file: path}`, `{store: store:item}`) or non-secret names;
  inline material and settings that would disable certificate verification are
  refused. `tungsten config` explains each reference's source; tungsten never
  reads what they point to.
- `scenario run` now works on macOS and Windows as well as Linux. macOS owns
  the integration through a process group and non-reaping `waitid`, Windows
  through a Job Object; the binary cancels on Ctrl-C/SIGTERM on both.
- Add `tungsten init --preset app|library|agent`, also accepted at the guided
  setup's outputs prompt. `library` writes and selects a local profile
  (`profiles/library.yml`) with shared SDK versions; `agent` configures an MCP
  server and generates with `--strict`. Explicit `--targets` and `--strict`
  win; presets never select inputs or change agent safety defaults.

- The `fsharp` target now enforces imported-operation policy through the
  .NET runtime it wraps and may be generated for projects converted from
  Postman, cURL, `.http` or HAR input. Generated F# SDKs require a .NET runtime
  with `ClientCore.RequireImportPolicyV1()`.

- Declare every input format (OpenAPI, Swagger, Postman, saved cURL, HTTP
  request files, HAR, endpoint manifests and manual endpoints) in the
  capability catalog with its versions, route, detection, required review flags,
  acquisition and whether its operations are imported; `tungsten capabilities`
  and `--matrix` list them. Targets gain an `evidence` list: qualification,
  artifact and platform states become `recorded` only when a record names the
  exact combination and a public reference, and stay `unknown` otherwise.
  Editors now complete enumerated target option values, such as Python
  `python_version` and `models`, from the declared option schemas. Imported-operation
  policy support is declared once per target instead of also as a feature, so
  enabling it for a target (as for Dart) no longer makes `tungsten
  capabilities` report conflicting declarations.

- Serve local profile files in `tungsten lsp`: completion and hover for
  profile keys and target defaults from the shared profile schema, and file
  watchers for profile directories outside the workspace folder so editing a
  shared profile rechecks the project.

- `scenario run --timeout-ms` now bounds the whole command, including input
  acquisition and compilation, and SIGINT/SIGTERM cancel a run that is still
  preparing. Such a run reports `timed_out` or `cancelled` before any port is
  bound or integration started.

- The `csharp` target now enforces imported-operation policy and may be
  generated for projects converted from Postman, cURL, `.http` or HAR input.
  The C# runtime reads each operation's `import_policy` (`AgentMeta.ImportPolicy`),
  refuses a reviewed imported operation whose target origin is outside its saved
  review scope (`VALIDATION_FAILED` on `BaseUrl`, nothing sent), and applies the
  effective safety tier of unreviewed operations. Generated SDKs require a
  runtime with `ClientCore.RequireImportPolicyV1()`.

- Generated Python `pyproject.toml` files list a
  `Programming Language :: Python :: 3.x` classifier for every minor from the
  `python_version` floor up to 3.15, matching the runtime package.

- Add an opt-in request scheduler to the TypeScript, Python and Rust runtimes
  (`ClientOptions.scheduler`, `ClientOptions.tenant`). It bounds ordinary
  attempts and event streams in flight per tenant and origin, queues a bounded
  number of further requests first in, first out, rejects the rest without
  sending them (`RATE_LIMITED` codes `QUEUE_FULL`, `QUEUE_TIMEOUT`,
  `RATE_LIMIT_PAUSED`; `TRANSPORT_FAILED` code `SCHEDULER_CLOSED`), and shares
  `Retry-After` pauses between callers of one scope, resuming with a single
  probe. A scheduler can be shared by several clients, closed and drained.
  An optional retry budget (`retryRatio`/`retry_ratio`, with a reserve)
  limits the retries all callers of one scope make together; a retry it
  cannot pay for is not made. A runnable bulk-sync example shows a job staying
  within a local API's concurrency quota.
  Optional caps across every scope (`maxActiveTotal`/`max_active_total`,
  `maxStreamsTotal`/`max_streams_total`) bound requests in flight per
  scheduler; scopes waiting for them take turns so none is starved.
  The bulk-sync example is now a gallery recipe (`python3 examples/run.py
  --only bulk-sync`) that runs it in TypeScript, Python and Rust.
  An optional scheduler observer (`onEvent`/`on_event`) receives versioned
  `admitted`, `rejected`, `released`, `paused` and `retry_denied` events with
  queue wait, hold and pause durations. Events carry no scope, tenant, URL or
  payload, are not buffered, and an observer's exception or panic is ignored.

- Add `toolDeadlineMs` to the WebMCP adapter's registration options: each
  tool call gets that total deadline, passed to the TypeScript runtime as
  `deadlineMs`, as `ServerOptions.toolDeadlineMs` does for the MCP adapter.

- Add an opt-in total operation deadline to the TypeScript (`deadlineMs`),
  Python (`deadline_ms`) and Rust (`deadline`) runtimes, on client and call
  options. Unlike the per-attempt timeout, it bounds one logical call across
  authentication, idempotency-key reservation, retries and backoff, response
  bodies and verification, and a whole traversal, poll or macro. No attempt
  starts after it expires (`DEADLINE_EXCEEDED`); a mutation already sent still
  reports `OUTCOME_UNKNOWN`. Existing timeout semantics are unchanged.

- Add the same total operation deadline to the C# runtime
  (`ClientOptions.Deadline`, `CallOptions.Deadline`), measured on a monotonic
  clock. Authentication and a shared OAuth refresh are waited for only until
  the deadline, never cancelled for other callers. F# SDKs use the same
  options through the .NET runtime.

- Add the same total operation deadline to the Ruby runtime (`deadline_ms` on
  client and call options); generated Ruby SDKs document it.

- Add the same total operation deadline to the Dart runtime (`deadline` on
  `ClientOptions` and `CallOptions`); generated Dart SDKs document it.

- Add a per-tool-call total deadline to the MCP adapter
  (`ServerOptions.toolDeadlineMs`, `<API>_MCP_DEADLINE_MS` in generated
  servers). It maps to the TypeScript runtime's `deadlineMs`; tool results
  name `code: DEADLINE_EXCEEDED`, and a mutation already sent still reports
  `OUTCOME_UNKNOWN`.

- Add `--deadline <SECONDS>` to generated Rust CLIs, with the
  `<PREFIX>_DEADLINE` variable and the profile key `deadline`. It sets the Rust
  runtime's total deadline for the command (`--timeout` stays per attempt).
  The flag name is now reserved: an API argument named `deadline` gets the
  flag `--deadline-arg`.

- Add the same total operation deadline to the Go runtime
  (`ClientOptions.Deadline`, `CallOptions.Deadline`), applied as a
  `context.Context` deadline on top of the caller's context.

- Add the same total operation deadline to the Java runtime
  (`ClientOptions.Builder.deadline`, `CallOptions.Builder.deadline`, both a
  `Duration`). Blocking token or store work the calling thread performs is
  checked when it returns; a caller waiting on shared token work stops at its
  deadline without cancelling it.

- Kotlin: add `deadline(kotlin.time.Duration)` extensions on the client and
  call option builders, and describe the total deadline in the generated Java
  and Kotlin SDK READMEs.

- Add `tungsten lock`, a versioned single-project generation lock
  (`tungsten.lock.json`). It records the declared input closure (manifest,
  profiles, agent manifest, entry documents, local references and overlays in
  application order) by content digest, with the compiler, IR and built-in
  emitter identities, digests of the effective target options and each
  target's consumer toolchain requirements. Capture compiles offline and never
  fetches or refreshes inputs, runs emitters or probes tools; physical paths,
  option values and credentials are not recorded. `lock --check` fails on a
  missing, stale or corrupt lock and lists the drift; replacing a lock whose
  identities changed requires `--update`. External emitters, remote inputs
  and files outside the project are reported as reproduction gaps, and the
  lock states that generated consumer dependencies are not vendored. The lock
  grants no trust to external emitters. `tungsten schema generation-lock`
  prints its schema.

- Add `tungsten bundle export` and `tungsten bundle import` for portable
  single-project bundles. Export requires a current generation lock and copies
  only the locked input closure and the lock into a new plain directory with a
  `tungsten-bundle.json` inventory of paths, sizes and digests; it never
  includes credentials, input caches, generated output or unrelated files.
  `--preview` shows the inventory, excluded remote or out-of-project inputs and
  files named like credential material; exporting an incomplete bundle or such
  files needs `--allow-incomplete` or `--allow-sensitive`. Import validates the
  whole bundle before writing (file-count, size and depth limits, relative
  path confinement, no links, digests and lock consistency), keeps identical
  existing files, refuses to overwrite differing ones and runs nothing; the
  bundle grants no trust to external emitters or execution policy.
  `tungsten schema project-bundle` prints the inventory schema.

- Add `--locked` to `generate` and `check`. A locked run verifies the compiled
  project against `tungsten.lock.json` from the same compilation and fails with
  `TG0128`, listing the changed identities, when the lock is missing, stale or
  corrupt; nothing is generated. Locked runs acquire remote inputs offline from
  their pins and cannot be combined with `--refresh-inputs`.

- Add `locked: true` to `tungsten.yml`, making every `generate` and `check`
  of the project a locked run as with `--locked`. The JSON results of both
  commands report `locked`.

- Add `bundle export --include-consumer-cache DIR` (repeatable) to carry a
  prepared, project-relative directory of consumer dependencies, such as
  `npm pack` tarballs, `pip download` wheels or `cargo vendor` output, for
  offline builds of generated consumers. Files are copied as supplied, with
  role `consumer_cache`. The inventory's new `consumer_caches` records each
  directory with its file count and size, provenance, the exporting platform
  (`exported_on`) and a redistribution notice. Directories that overlap,
  lie outside the project, sit in `.tungsten-inputs` or VCS metadata, or
  overlap generated output are refused, as are links. Import accepts
  `consumer_cache` entries only below a declared directory. Offline consumer
  builds remain not guaranteed: tungsten does not check that a cache covers
  every dependency, and native packages may install only on the exporting
  platform.

- Add `bundle export --include-input-cache` to carry the verified
  `.tungsten-inputs` pin records and pinned content of remote HTTP(S) and Git
  inputs, and of the reviewed sources of converted imports, so such projects
  reproduce offline after import. Only the files of each verified pin are
  copied; import accepts them only below `.tungsten-inputs`. Without the flag,
  the reviewed source of a converted import is now listed as excluded too.
  Check that upstream terms allow redistributing bundled documents.

- Add `lock --outputs` to record, and with `--check` verify, the identity of
  the bytes each built-in target generates (file count and a digest over every
  generated path and content digest), by running the built-in emitters in
  memory; external emitters are never run and stay unrecorded. When the lock
  records outputs, `generate --locked` also fails with `TG0129` before writing
  if a selected built-in target generates different bytes from the same locked
  identities, which certifies byte-identical reproduction on another machine
  and surfaces platform-dependent differences explicitly. Plain `lock` and
  `lock --check` still never run emitters; they leave the recorded outputs of
  a current lock in place, and `lock --update` without `--outputs` drops them.

- Extend the versioned capability catalog with each built-in target's option
  keys, execution environments and implemented features, plus a registry of
  feature ids. `tungsten capabilities --matrix` prints the support matrix
  derived from those declarations, and the command refuses to answer from a
  catalog with conflicting declarations (duplicate names, unregistered
  features, empty version ranges, undeclared options). Editor completion of
  target option keys now comes from the catalog, adding the missing Python
  `module`, `python_version` and `models` keys. Declarations remain source
  statements: qualification and artifact availability stay `unknown`.

- Support-bundle collection, for preview and export, is limited to 30 seconds
  and writes nothing when the limit is reached; library cancellation of an
  export is now honoured while collection runs.

- Declare the `init --preset` starting points (`app`, `library`, `agent`)
  in the capability catalog: `init` and the wizard read each preset's
  targets and `--strict` default from it, `tungsten capabilities` reports
  them under `init.presets` and the support matrix lists them. The catalog
  check rejects a preset missing a companion target or a CLI preset without
  a declaration.

- Add the protocol design gate. An IR namespace may carry an optional
  `protocol` binding (a registered protocol id, its version and a
  protocol-specific `extension`); absent means HTTP, so existing IR is
  unchanged. The capability catalog registers protocols under `protocols`
  (`http` for every built-in target; `graphql`, `grpc` and
  `async_messaging` reserved with no generating target) and the support
  matrix lists them. Generation refuses a target, before writing it, for IR
  bound to a protocol the target does not declare (TG0706), so new
  frontends never reshape their operations into REST fields.

- Derive `tungsten init` and wizard target selection from the capability
  catalog, which now declares the default targets, the wizard defaults, the
  targets local setup (`--local-example`) covers and companion targets
  (`mcp` and `webmcp` add `typescript`); `tungsten capabilities` reports them
  under `init` and the support matrix lists them. `init --targets` and
  `--add-targets` refuse the reserved `mock` target before writing anything,
  naming the accepted outputs. The wizard's output list comes from the
  catalog and no longer says source-only targets refuse imported operations
  when they enforce the policy.

- Declare a typed `options_schema` for every built-in target in
  `tungsten capabilities`: C#, F#, Dart, Java, Kotlin, Ruby, MCP, WebMCP and
  docs now list each option's JSON type, fixed default, description and the
  warning code an invalid value produces. The emitters read their fixed
  defaults from the same declarations, the catalog check rejects an option
  key without a declared field, and editor hover shows the descriptions.

- On Linux, the `tungsten` binary turns SIGINT and SIGTERM during
  `support-bundle --output` into cooperative cancellation: the interrupted
  export removes its temporary file and exits with status 4 without
  publishing. A signal after publication does not change the outcome.

- Add opt-in, explicitly labelled user-supplied material to support bundles:
  `--attach-log <FILE>` and `--attach-config <FILE>` (at most four) add
  `user.attachment[N]` entries holding the redacted last 8 KiB of each file,
  with origin `user_supplied` and sensitivity `user_supplied_text`. Paths are
  not recorded. Environment values, credential-like values, URLs, private keys,
  home-path user names and long tokens are redacted, but redaction is not a
  guarantee. Such bundles use `bundle_schema_version: 2`. Bundles without
  attachments are unchanged version 1, and `support-bundle --inspect` reads
  both versions.

- Generate the README support matrix from the capability catalog.
  `tungsten capabilities --matrix --format markdown` prints the matrix
  between the markers that delimit README's generated block, and CI fails
  when the README differs (`.github/scripts/check-support-matrix.py --write`
  refreshes it). The README Targets table now reports the catalog's
  imported-operation policy, which had drifted for Java, Kotlin and Ruby.

- Add `tungsten support-bundle --inspect <FILE>` to review an existing support
  bundle locally on any platform. It reads one bounded regular file, refuses
  unsupported `bundle_schema_version` values by number, requires exactly the
  schema's keys, observed metadata-only entries and payload sizes that match,
  and lists the entries. Errors name schema locations only, never file content.

- Offer language-server quick fixes for unknown operation ids (`TG0603`) and
  unknown manifest fields or values (`TG0602`), replacing the value named by
  the diagnostic with the closest known name. Fixes are version-pinned edits
  of the current buffer and are withheld when the buffer no longer matches.

- Offer language-server overlay quick fixes for operations without an
  `operationId` (`TG0403`) and repeated operation ids (`TG0401`), using the
  actions `tungsten overlay suggest` derives. One previewed workspace edit adds
  the action to `<namespace>.suggested-overlay.yaml`, creating that file and
  listing it under the input's `overlays` in `tungsten.yml` when needed. No fix
  is offered from an analysis older than the latest edit.

- Link language-server diagnostics to the configuration that is responsible
  for them. A diagnostic in a profile points to the `profiles` entry of
  `tungsten.yml` that selected it, or to the `profiles` list for an included
  profile. A diagnostic at a composed target option names the declaration that
  sets the effective value and the declarations it shadows. Locations come
  from the compiled text, so unsaved buffers are reflected.

- Explain in the language server why an OpenAPI construct the capability
  catalog declares as not compiled (for example OpenAPI 3.2 `$self`, `TG0140`)
  has no fix. Editors that show disabled code actions get one per diagnostic
  code, with the construct, its refused or ignored policy and the
  `tungsten capabilities` and `tungsten explain` pointers.

- Reuse the language server's last check while its inputs are unchanged. A
  save, or a watched-file event such as output written by `tungsten generate`,
  no longer recompiles the project when every source the check read has the
  same content digest in the open buffers and on disk, and no optional file
  it found missing has appeared. Checks with errors or remote documents are
  never reused, and modification times are not consulted. On a 3,000-operation
  spec, a save without changes dropped from about 3 s to about 1 ms.

- Explain unavailable language-server overlay fixes: editors that show disabled
  code actions show why no fix is offered for a `TG0403`/`TG0401` diagnostic (a
  pending check, no derivable action, an editor without versioned edits or file
  creation, or a manifest shape the fix does not edit) and the equivalent
  `tungsten overlay suggest` command. Replacement fixes for `TG0603`/`TG0602`
  fall back to plain edits for editors without versioned workspace edits.

- The `java` and `kotlin` targets now enforce imported-operation policy and may
  be generated for projects converted from Postman, cURL, `.http` or HAR input.
  The Java runtime reads each operation's `import_policy`, refuses a reviewed
  imported operation whose target origin is outside its saved review scope
  (`VALIDATION_FAILED` on `baseUrl`, nothing sent), and applies the effective
  safety tier of unreviewed operations. Generated SDKs require a runtime with
  `ClientCore.requireImportPolicyV1()`.

- The `ruby` target now enforces imported-operation policy the same way: the
  Ruby runtime refuses a reviewed imported operation whose target origin is
  outside its saved review scope (`VALIDATION_FAILED` on `base_url`, nothing
  sent), and generated gems require
  `Tungsten::Runtime::ClientCore.require_import_policy_v1`.

- The `dart` target now enforces imported-operation policy the same way: the
  Dart runtime refuses a reviewed imported operation whose target origin is
  outside its saved review scope (`VALIDATION_FAILED` on `base_url`, nothing
  sent), and generated clients require `ClientCore.requireImportPolicyV1()`.

- The external Go emitter (`emitters/go`) now declares `import_policy: true` in
  its `--describe` response, so projects converted from Postman, cURL, `.http`
  or HAR input can generate Go SDKs. The Go runtime refuses a reviewed imported
  operation whose target origin is outside its saved review scope
  (`VALIDATION_FAILED` on `base_url`, nothing sent), and generated descriptors
  carry `tungsten.ImportPolicy`, so they build only against a runtime that
  enforces it.

- Add explicit Linux support-bundle export to a new file in an existing,
  trusted user-owned directory. Export preserves the metadata-only preview
  manifest, caps complete artifacts at 128 KiB, refuses replacement and
  reports publication and temporary-file cleanup separately. Other platforms
  retain preview and refuse export before collection. Cooperative library
  cancellation ends at publication; abrupt-termination cleanup is not promised.

- Extend support-bundle export to macOS with the same no-clobber writer as
  Linux. Every path component is opened without following symlinks, so the
  macOS `/tmp` and `/var` symlinks are refused; publication needs hard-link
  support in the destination filesystem.

- Extend support-bundle export to Windows with its own no-clobber writer: it
  holds every traversed directory open without delete sharing, refuses
  reparse points, network shares, device paths and reserved names, creates
  the temporary file with `CREATE_NEW` and publishes it with a hard link that
  never replaces an existing entry. The directory's owner and ACL are not
  checked.

- A copyable GitHub Actions workflow in `examples/github-actions/` installs a pinned,
  checksum- and provenance-verified release through `install.sh`, runs `tungsten check`
  and fails on stale generated output with `tungsten generate --check`. It uses
  `contents: read` permissions, SHA-pinned actions, no secrets and
  `TUNGSTEN_NO_EXTERNAL=1`, so a pull request cannot start `external` emitter commands.

- `tungsten support-bundle --preview` prints the exact, metadata-only contents
  of a diagnostic support bundle, as text or `--json`. Entries (tool and schema
  versions, the capability catalog, effective-configuration provenance,
  configuration diagnostic codes, declared tool prerequisites and a fixed
  reproduction recipe) are assembled from existing config-only sources: no
  IR build is needed, so a broken project still previews, and no program is
  started, no tool version is probed, nothing is fetched and no file is written.
  Option values, digests, project names, environment values, URLs, paths and
  diagnostic text are never included, and the output is size-bounded.
  `--exclude <id>` removes an entry. Writing the archive is not available yet.

- Add `tungsten update check`, an explicit, bounded release lookup. It makes one
  request (5 second limit, 1 MiB cap) for public pboachie/tungsten release
  metadata, compares the latest stable release with the running version
  (`--include-prerelease` also considers prereleases), confirms the release lists
  this platform's archive and `SHA256SUMS`, and prints upgrade guidance for the
  detected installation route (installer, cargo or npm; an unknown route gets
  guidance, not a command). Exit codes: 0 up to date, 5 newer release available,
  6 could not confirm (offline, opted out, network or metadata failure, or a
  partially published release). `--offline`, `--no-update-check` and
  `TUNGSTEN_NO_UPDATE_CHECK=1` make no request. It never installs, changes
  project pins or generated clients, or sends project data. `--json` output adds
  the `update check` command to the CLI output schema.

- An OpenAPI 3.2 `text/event-stream` response whose `itemSchema` gives each
  event a plain-text `data` string (no `contentMediaType`, or a `text/*` one,
  and no `contentSchema`) compiles to a stream of string events. Every runtime
  delivers such data as written instead of parsing it as JSON; the IR marks the
  stream `text_data` and SDK descriptors carry `text: true`. Generated Rust
  stream descriptors now always set the new `text` field.

- OpenAPI 3.2 response `itemSchema` compiles onto the existing streaming
  support. On a JSON Lines response it is the type of one line. On a
  `text/event-stream` response whose item is an event object with a JSON `data`
  string (`contentMediaType` JSON and `contentSchema`), the `contentSchema` is
  the event type. Other uses, such as request bodies, other media types,
  non-JSON event `data` or `schema` declared beside it, still report TG0145.
  `itemSchema` in OpenAPI 3.0 and 3.1 documents is still not read.

- A reusable OpenAPI 3.2 media type (`components.mediaTypes`) with
  `itemSchema` compiles where a response's `content` map refers to it as JSON
  Lines or an event stream. Its `itemSchema` is checked at each use, with the
  media type of the referring `content` entry, instead of being refused at the
  component. A use the compiler does not support, such as a request body,
  reports TG0145 at the `$ref`.

- The OpenAPI 3.2 `query` path-item method compiles to an operation sent with
  the HTTP `QUERY` method and its request body in generated clients.
  The IR gains the `QUERY` HTTP method, which is safe (read-only by default; set
  `defaults.safety.query` in `agent.yml` to override), and all runtimes accept
  it. `query` members of OpenAPI 3.0 and 3.1 documents are still not
  operations. TG0141 is retired and no longer reported; `tungsten capabilities`
  lists `query` as a compiled 3.2 construct.

- OpenAPI 3.2 `$self` is accepted when it cannot change what a reference
  resolves to: the document has no relative reference to another document,
  or `$self` is a bare file name in the document's directory. TG0140 now
  reports only a `$self` that would move the base of a relative `$ref`, one
  that is not a string or has a fragment, and a `$ref` that names the
  document by its absolute `$self` URI. `tungsten capabilities` lists `$self`
  as a compiled 3.2 construct.

- OpenAPI 3.2 `style: cookie` cookie parameters compile: values are sent with
  RFC 6265 syntax as written (only characters a cookie cannot carry are
  percent-encoded), and exploded arrays and objects as separate `n=v; n2=v2`
  pairs (explode defaults to true). The IR gains the `cookie` parameter style,
  and every runtime implements it. TG0144 now reports only `style: cookie` on a
  parameter that is not `in: cookie`. `tungsten capabilities` lists
  `style: cookie` as a compiled 3.2 construct.

- OpenAPI 3.2 `in: querystring` parameters compile when their `content` is one
  `application/x-www-form-urlencoded` media type with an object schema: the
  argument is an object sent as the whole query string, one form-encoded
  `key=value` pair per property (the IR records it as an exploded `form` query
  parameter, so runtimes need no change). TG0143 now reports only querystring
  parameters that cannot be sent this way: another media type, a schema that
  is not an object, or other query parameters beside it. `tungsten
  capabilities` lists `in: querystring` as a compiled 3.2 construct.

- OpenAPI 3.2 `additionalOperations` compile: each entry is an operation sent
  with its method token exactly as written (for example `COPY` or `LINK`),
  treated as unsafe and not idempotent. The IR gains the `CUSTOM` HTTP method
  and the operation's `custom_method` wire token; the TypeScript, Python and
  Rust runtimes accept a custom method token in descriptors (the Rust runtime's
  `HttpMethod` gains `Custom(&'static str)`), and other runtimes already send
  the method as given. TG0142 now reports only entries that cannot be sent: a
  key that is not an RFC 9110 token, a case variant of a fixed path item
  method, `CONNECT` or `TRACK`. `tungsten capabilities` lists
  `additionalOperations` as a compiled 3.2 construct.

- OpenAPI 3.2 reusable media types compile: a Media Type Object given by `$ref`
  (usually to `components.mediaTypes`) in a request body, response, parameter
  or header `content` map is read as if written in place, including its schema
  and event-stream extensions. TG0148 is retired and no longer reported;
  `tungsten capabilities` lists reusable media types as a compiled 3.2
  construct.

- OpenAPI 3.2 discriminator `defaultMapping` compiles: the IR `Discriminator`
  gains an optional `default_mapping` (additive), and TypeScript, Python and
  Rust SDKs and the mock decode a value whose tag is absent, not a string or
  mapped to no variant as that default variant instead of refusing it. Tool
  JSON schemas list such a union with `anyOf` and leave the default variant's
  tag open. A `defaultMapping` that is not a variant of the union is reported
  as TG0306 and ignored. Other SDK targets still refuse an unmapped tag.
  TG0149 is retired and no longer reported.

- Discriminator `defaultMapping` is honoured by more SDK targets: Java,
  Kotlin, Ruby, C#, F#, Dart and Go SDKs decode an object whose tag is absent,
  not a string or claimed by no variant as the default variant, as TypeScript,
  Python and Rust do. A non-string tag then fails only if the default
  variant's own field type refuses it.

- OpenAPI 3.2 security requirements that name a scheme by URI compile: a key
  such as `#/components/securitySchemes/apiKey`, or a URI into another input
  document's `components.securitySchemes`, selects that scheme. A component
  name wins over a URI that reads the same. TG0151 now reports only a URI that
  names no such scheme (for example a scheme in a file that is not an input),
  and the build reports it instead of the frontend; `tungsten capabilities`
  lists the construct as compiled.

- The OpenAPI 3.2 OAuth 2.0 `deviceAuthorization` flow compiles. It is kept in
  the IR like the `password` and `implicit` flows, and `OAuthFlow` gains an
  optional `device_authorization_url` (additive). Generated clients accept an
  access token obtained through the device flow as the scheme's credential;
  they do not run the device flow (RFC 8628) themselves. TG0150 is retired and
  no longer reported.

- OpenAPI 3.2.x documents are read: a document that uses only constructs OpenAPI
  3.1 has takes the 3.1 path and compiles to the same IR as its 3.1 twin. Each
  construct only 3.2 has is diagnosed at its member instead of being ignored, one
  code per feature. These are errors, because a client built without them would
  differ from the API: other `itemSchema` uses (TG0145), `prefixEncoding` and `itemEncoding` (TG0146),
  nested encodings (TG0147), `components.mediaTypes` and media types given by
  `$ref` (TG0148), discriminator `defaultMapping` (TG0149), the OAuth 2.0 device
  authorization flow (TG0150) and security requirements that name a scheme by URI
  (TG0151). These are warnings, because they add metadata with no effect on a
  client: `oauth2MetadataUrl` (TG0152), tag `parent` and `kind` (TG0153), example
  `dataValue` and `serializedValue` (TG0154), XML `nodeType` (TG0155), and tag
  `summary`, response `summary`, server `name` and security scheme `deprecated`
  (TG0156). `tungsten capabilities` and `tungsten doctor` list the supported
  OpenAPI versions and these constructs (additive `openapi_inputs` field). 3.0 and
  3.1 input and output are unchanged; `tungsten explain` describes each new code.

- Add a pub.dev release route for the Dart runtime (`tungsten_runtime`) from
  0.2.0: a frozen, checksummed archive with a zero-warning `dart pub publish
  --dry-run` on every release run, then a gated OIDC publish (`PUB_PUBLISH`,
  environment `pub`) whose public archive is compared with the frozen one.
  Generated Dart SDKs can depend on the published package instead of
  runtime source. The runtime's `crypto` dependency is now the range `^3.0.7`
  (pub.dev warns on exact pins) and the package declares pub.dev topics.

- Add opt-in coroutine support to the Kotlin target (`coroutines: true` in
  `targets.kotlin`). Each resource gains a `<op>Suspend` function next to the
  blocking one (same `Result`, run on `Dispatchers.IO`; cancelling the coroutine
  interrupts the call, which closes the request in flight and ends with a
  `CancellationException`) and cold `Flow`s, `<op>PagesFlow` and
  `<op>StreamFlow`, next to `Pages` and `Stream`. The flows are pull based with
  no buffer of their own (`.buffer(n)` bounds any prefetch); `maxResponseBytes`
  and pagination cycle detection still apply to every page and an error is the
  last element. The blocking API is unchanged and remains the default, and the
  new `tungsten-runtime-kotlin-coroutines` artifact (`runtimes/kotlin/coroutines`,
  `coroutines_runtime_path` for a source build) is the only one that depends on
  `kotlinx-coroutines-core` 1.10.2. Limits: `dispatch()`, macros, previews and
  the OAuth helpers stay blocking, the calls run on `Dispatchers.IO` (one
  platform thread per request in flight), no deadline spans a whole pagination,
  and a mutation cancelled after it was sent has an unknown outcome; reuse its
  idempotency key to repeat it.


- Go, Java and Ruby (and Kotlin, which delegates to Java) report a schema
  `pattern` they cannot check. The value is still accepted, as before, but the
  client now emits a non-fatal `PATTERN_UNCHECKED` diagnostic through
  `OnDiagnostic` (Go) or `onDiagnostic`/`on_diagnostic` naming the path, the
  pattern and the reason, never the value, once per path and pattern per call.
  This covers patterns the engine cannot compile and, in Java, subjects too long
  to match, matches that time out or overflow the stack. Direct schema validation
  without an observer stays silent. Unreleased 0.2.0 only.

- Go, Java and Python now check a schema `pattern` that uses the ECMAScript
  `u`-flag escape `\u{XXXX}` (one to six hex digits, at most `10FFFF`) instead
  of accepting it unchecked: it is translated to RE2 and Java's `\x{XXXX}` and to
  Python's `\UXXXXXXXX`, in and out of character classes. A malformed or
  out-of-range `\u{...}` is still accepted and reported as `PATTERN_UNCHECKED`.
  Python's `re` still has no `\p{...}` property classes, so those patterns stay
  unchecked and reported. Unreleased 0.2.0 only.

- TypeScript, Python and Rust report a schema `pattern` their regex engine cannot
  compile, or cannot match within a limit, as a non-fatal `PATTERN_UNCHECKED`
  diagnostic (`failed_parameter`, the pattern and the reason, never the value; once
  per call, path and pattern) through `onDiagnostic`/`on_diagnostic`. The value is
  still accepted, as before; these runtimes skipped such patterns silently.
  `SchemaLike.safeParse` may return `notes`, Rust gains `PatternNote` and
  `note_unchecked_pattern`, and Python gains `schema.collect_pattern_notes`.
- The Kotlin runtime layer now has a Maven Central route:
  `io.github.pboachie.tungsten:tungsten-runtime-kotlin` is built reproducibly
  with sources and documentation archives from `runtimes/kotlin/pom.xml`,
  depends on the exact same-version `tungsten-runtime` and `kotlin-stdlib`, and
  is checked, signed and verified after publication by the existing Maven
  Central release job and `MAVEN_PUBLISH` switch, together with the opt-in
  `io.github.pboachie.tungsten:tungsten-runtime-kotlin-coroutines` (from
  `runtimes/kotlin/coroutines/pom.xml`, adding only `kotlinx-coroutines-core`
  1.10.2, whose resolved jar the build checks against a pinned sha256).
  Generated Kotlin SDKs can then use the published artifacts instead of runtime
  source.
- Prepare the F# runtime for NuGet as `Tungsten.Runtime.FSharp`. The release
  packs and verifies it beside `Tungsten.Runtime` (Apache-2.0, icon, README,
  deterministic, exact `Tungsten.Runtime [version]` dependency and an
  `FSharp.Core` 10.1.401 floor), and publishes it behind the existing NuGet
  gate. Generated F# SDKs will restore it as a package instead of runtime source
  once a release publishes it; nothing is published yet.

- Add explicitly pinned host policies for external emitters, with strict bounded
  capture, intersected deny/resource ceilings, pure policy digests and redacted
  inspection. Unsupported commands reject supplied policy before effects.

- Inspect supported effective configuration with `tungsten config`, including
  typed defaults/fallback policy and winning/shadowed authored origins. Plain
  text and JSON redact sensitive values and physical paths without compiling
  inputs, probing tools or executing emitters.

- Check transport references in `tungsten doctor <project>`: report whether each
  referenced environment variable is set and each project file exists, without
  reading them. `--strict` fails on a missing referenced file.

- Keep the specific, value-free messages of profile and `transport` diagnostics
  in `tungsten config`, instead of a generic "configuration declaration is
  invalid". Other manifest diagnostics stay redacted.

- Select shared local configuration profiles from `tungsten init --profile PATH`
  (repeatable) and from the guided setup. Selections are composed with the
  compiler's limits before any file is written. Invalid, missing or remote
  profiles are refused, and no files are written.

- Apply profile defaults only to targets a project declares. A shared profile
  no longer adds a TypeScript, Python or Rust output to a project that did not
  ask for it.

- Add a profile-reuse example: an app and a multi-target library share one
  profile chain with per-project overrides, checked offline with the compiler.

- Compose Python (`package`, `version`, `runtime`, `models`, `python_version`)
  and Rust (`crate`, `version`, `runtime`) defaults from the same bounded local
  profiles, so one profile serves a multi-target library. Explicit project
  fields still win and each value keeps its winning/shadowed origin. Python and
  Rust options are declared in the shared capability schema and inspected by
  `tungsten config`. The emitters reuse the same validators, so TG0731/TG0740
  fallbacks are unchanged.

- Compose bounded local TypeScript profiles for package, version, runtime and
  dependency defaults. Retain authored provenance through the compiler driver,
  strict profile diagnostics and existing manifest fallback behavior.

- Declare the existing TypeScript target options and supported-value schema in
  the shared capability contract. Keep emitter defaults and TG0711 fallback
  behavior compatible while configuration and generation share validation.

- Add a runnable `mock-scenarios` gallery recipe: saved 429-then-success,
  interrupted-stream, malformed-response and lost-write scenarios run through
  `tungsten scenario run`, and deliberately broken retry, resume and replay
  variants must fail their assertions.

- Assert mock-side effects in saved scenarios with optional `expect.effects`
  counts of applied writes and idempotent replays, so a lost-write retry that
  changes its idempotency key fails even when call counts match. Failed runs
  name each failing assertion by file position in terminal output.

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

- Dart `FileIdempotencyStore` keeps automatic idempotency keys across restarts
  in the version-1 JSON file and `<path>.lock` owner lease shared with the other
  runtimes (mode 0600, atomic rename, closed on corruption, a lease is never
  reclaimed automatically). Stores gain `reserve(scope, logicalId, candidate)`
  through `AtomicIdempotencyStore`; get/put-only custom stores keep working.

### Changed

- The `mock-scenarios` gallery recipe (429 then success, stream interruption,
  malformed response and lost write) now runs on macOS and Windows as well as
  Linux, matching `scenario run`.

- The Python clean-consumer CI lane now also calls a second generated SDK sync and async,
  covering bearer auth, cursor pagination, nullable and union models, a safe
  retry, client-side validation, an operation deadline, async cancellation, an
  unknown mutation outcome that is not replayed, and server-sent events.

- Check the Python runtime archives on every CPython minor from 3.10 to 3.15
  (latest patches) in public CI, with macOS and Windows archive smoke, and add
  a clean-consumer check that installs the runtime wheel/sdist and freshly
  generated SDKs (Pydantic and dataclasses, 3.10 and default output) into
  isolated environments, then compiles, imports, strictly type-checks and
  calls a local mock synchronously and asynchronously. The floor interpreter
  also runs the lowest declared dependency versions. The runtime classifiers
  now list 3.14 and 3.15; `requires-python` keeps its `>=3.10` floor without
  an upper cap.

- Check the npm runtime packages on Node 26 (Current) as well as Node 22 and 24,
  and add an advisory Node 20 canary that does not imply support. The README
  now has one Node.js support table: supported lines, the declared floor for
  each artifact, the checked patch versions, and the build-only scope of the
  WebMCP floor. The TypeScript runtime now type-checks against Node 22 types,
  so it cannot rely on APIs newer than its floor.

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

- Check the MCP Node.js floor (22.13+, or 23.2+ on Node 23) in `init` local
  setup when the MCP target is selected, matching `doctor`. Previously setup
  accepted any Node 22 and failed later. `doctor` now also notes when Node is
  an odd-numbered line or newer than the newest line the package builds check.

- A TypeScript `deadlineMs` longer than about 24.8 days (2^31 - 1 ms) no
  longer fails authentication or idempotency-key reservation at once with
  `DEADLINE_EXCEEDED`; timers over that limit fire immediately in JavaScript.
  Retry waits and per-attempt timeouts above it are capped at it instead.

- The Rust runtime runs an OAuth2 token fetch or refresh as its own task: a
  caller whose `deadline` passes, or whose future is dropped, stops waiting
  (with `DEADLINE_EXCEEDED` for an expired deadline) instead of cutting short
  a fetch other callers share, which then failed them with `AUTH_FAILED` and
  sent a second token request. The TypeScript runtime also stops waiting for
  authentication, an OAuth rejection refresh or an idempotency key
  reservation when the caller's `signal` aborts. Python already behaved this
  way.

- Record profile files in `tungsten.lock.json` by their manifest-relative
  path below the selected profile root, instead of the logical
  `profile[N]/...` name. `bundle export` failed for any project that selected
  a profile. Locks written before this change report the profile file as
  drift; accept it with `tungsten lock --update`.

- Require Pydantic 2.10.1 or newer in generated Python packages and in
  guided `init` package setup: generated models failed to import with
  Pydantic 2.10.0, which the previous `>=2.10` range allowed.

- Discard speculative Python unchecked-pattern diagnostics from rejected
  untagged-union variants, retaining notes and nested paths only from the
  first accepting variant. Direct validation keeps its existing warning
  behavior when no diagnostic collector is active.

- MCP preserves special JSON member names in request identities and generated
  examples, so distinct requests do not accidentally share repeated-result
  redaction.
- `update check` now reports a timeout when release metadata stalls during the
  response body, instead of incorrectly reporting a network failure.

- Mock generation now repairs conflicting optional fields in `allOf` records by
  reusing a compatible generated value or omitting an optional field. Required
  fields remain intact, and the full intersection must validate before a repair
  is accepted.

- Generated TypeScript SDKs no longer lose a `PATTERN_UNCHECKED` diagnostic when
  the first untagged-union variant that accepts a value has a pattern the engine
  cannot compile and a later variant also accepts it. Diagnostics now come only
  from the first accepting variant, as in Dart and Python; rejected variants no
  longer report speculative patterns. Schema shapes, `.options` and encoding are
  unchanged.

- Dart schema validation now reports unchecked patterns from the first accepting
  untagged-union variant, preserving nested property and array paths. Rejected
  variants do not emit speculative warnings.

- Python runtime: offset and page-number pagination no longer fail with
  `UNEXPECTED_RESPONSE` on Python 3.10 and 3.11, where `int.is_integer` does not exist.

- Document that `tungsten-runtime` and `tungsten-cli-kit` are published on
  crates.io starting with 0.1.3 (0.1.2 was published earlier), and that the Rust
  installation docs now describe registry installation alongside the source routes. User docs now describe 0.1.3 as the latest
  release and list the verified 0.1.3 registry packages.

- C# and F# schema `pattern` values are now matched as the ECMAScript expressions
  the contract defines: `\d`, `\w`, `\s`, `\b`, `$` (no match before a trailing
  newline), `.`, `\u{...}`, `\p{...}` general categories, lookbehind and named groups
  behave as in TypeScript. A pattern that cannot be checked (invalid, a construct
  .NET cannot match faithfully, a value with characters above U+FFFF for a pattern
  that depends on code points, or matching that exceeds its time budget) still
  accepts the value but now reports a `PATTERN_UNCHECKED` diagnostic through
  `ClientOptions.OnDiagnostic` instead of passing silently. The C# and F# READMEs
  list the remaining differences.
- Correct the 0.1.3 "Known gaps of the F# target" note: the C# runtime's durable
  `FileIdempotencyStore` exists, so "in-memory idempotency store" no longer
  applies, and the ".NET regex dialect" limit is resolved above. The F# README now
  records that the `async` surface, targets below net10.0, Fantomas formatting and
  Fable/JavaScript remain out of scope for 0.2.0.
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

- Dart schema validation counts `min_length`/`max_length` in Unicode code points
  (an emoji was two) and runs `pattern` with Unicode semantics (`\p{...}`,
  `\u{...}`, whole astral characters), falling back to legacy syntax only for
  patterns the Unicode mode rejects. A pattern that cannot be compiled still
  accepts the value but now reports a `PATTERN_UNCHECKED` diagnostic through
  `ClientOptions.onDiagnostic` instead of passing silently.

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
  configuration were maintainer release steps, since completed at 0.1.3.

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
