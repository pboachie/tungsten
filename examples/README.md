# Runnable example gallery

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
| [First typescript SDK](first-typescript/README.md) | Generate and build a small typescript package from a local OpenAPI 3.1 contract. |
| [First python SDK](first-python/README.md) | Generate and build a small python package from a local OpenAPI 3.1 contract. |
| [First rust SDK](first-rust/README.md) | Generate and build a small rust package from a local OpenAPI 3.1 contract. |
| [First mcp SDK](first-mcp/README.md) | Generate and build a small mcp package from a local OpenAPI 3.1 contract. |
| [First webmcp SDK](first-webmcp/README.md) | Generate and build a small webmcp package from a local OpenAPI 3.1 contract. |
| [First agent documentation](first-docs/README.md) | Generate agent-facing documentation from a local OpenAPI 3.1 contract. |
| [First go SDK](first-go/README.md) | Generate and build a small go package from a local OpenAPI 3.1 contract. |
| [First java SDK](first-java/README.md) | Generate and build a small java package from a local OpenAPI 3.1 contract. |
| [First csharp SDK](first-csharp/README.md) | Generate and build a small csharp package from a local OpenAPI 3.1 contract. |
| [First ruby SDK](first-ruby/README.md) | Generate and build a small ruby package from a local OpenAPI 3.1 contract. |
| [Start with OpenAPI 3.0](input-openapi30/README.md) | Initialize directly from a committed native OpenAPI document. |
| [Start with OpenAPI 3.1](input-openapi31/README.md) | Initialize directly from a committed native OpenAPI document. |
| [Start with Postman](input-postman/README.md) | Import a minimal Postman v2.1 collection offline; requests and scripts are not executed. |
| [Start with saved cURL](input-curl/README.md) | Parse a saved command as data with an explicitly reviewed route; never execute it. |
| [Start with an HTTP request file](input-http/README.md) | Import a saved HTTP request with an explicit route template. |
| [Start with a HAR capture](input-har/README.md) | Import a minimal HAR 1.2 observation with its origin and path explicitly selected. |
| [Start with Swagger 2.0](input-swagger/README.md) | Convert a local Swagger document without fetching or guessing unsupported semantics. |
| [Start with JSON Schema models](input-json-schema/README.md) | Declare JSON Schema models together with explicit HTTP routing in an endpoint manifest. |
| [Preview and confirm a destructive call](preview-confirmation/README.md) | Reject an unconfirmed deletion, preview its identical arguments, then execute with the returned token. |
| [Develop against a local mock](local-mock/README.md) | Run tungsten mock and call it through the generated Python client with strict response validation. |
| [Assert retries with saved mock scenarios](mock-scenarios/README.md) | Run an explicit integration against saved 429, interrupted-stream, malformed-response and lost-write scenarios, and fail deliberately broken retries. |
| [Detect stale generated files in CI](ci-drift/README.md) | Generate, change a copied contract, observe the no-diff gate fail, then regenerate and pass. |
| [Review semver and migration changes](surface-diff/README.md) | Compare a generated baseline with an additive operation using both diff review modes. |
| [Bulk sync within a quota](bulk-sync/README.md) | Bound concurrent calls, queue the rest and share 429 backoff with a request scheduler in TypeScript, Python and Rust. |
| [Trace calls with local telemetry](telemetry/README.md) | Collect correlated operation, attempt and phase telemetry in memory from the TypeScript, Python and Rust SDKs, with nothing sent anywhere. |
| [Receive signed webhooks](webhook-receiver/README.md) | Verify signed order.shipped deliveries and decode typed payloads with the TypeScript, Python and Rust SDKs, without a web framework. |
| [Embed a Rust SDK through JNI](android-jni/README.md) | Call a generated Rust SDK from Java through a small JNI bridge with a bounded deadline and response size, error envelopes, cancellation and cleanup, as an Android app would. |

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
- [GitHub Actions workflow](github-actions/README.md): a copyable workflow, inline or through the reusable [`actions/tungsten`](../actions/tungsten/README.md) action, that installs a pinned release, validates the contract and fails on stale generated output or a stale lock.
- [Reuse one configuration profile](profile-reuse/README.md): share reviewed defaults across an app and a multi-target library, with explicit per-project overrides.
- [Restricted agent integration](restricted-agent/README.md): an MCP server that reads freely, confirms writes and never offers deletion, with transport references from a shared profile.
