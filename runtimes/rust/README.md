# Tungsten Rust runtime packages

## Registry status

`tungsten-runtime` and `tungsten-cli-kit` are published on crates.io at 0.1.2
and 0.1.3. Install the version that matches your compiler and generated SDK:

```sh
cargo add tungsten-runtime@0.1.3
cargo add tungsten-cli-kit@0.1.3   # only for a generated CLI
```

Generated Rust SDK and CLI crates can depend on the registry crates by setting
`runtime: "0.1.3"` (and `kit: "0.1.3"` for a CLI) without `runtime_path` or
`kit_path`. Source checkouts remain supported, and the source routes below still
apply when you need unreleased source or the unchanged `v0.1.2`
checkout.

## Stable 0.1.2 SDK and CLI from source

The published `tungsten 0.1.2` compiler works with the unchanged `v0.1.2`
source checkout. That tag's Rust crates retain their original **0.1.0** source
versions; the compiler release number and those older crate versions were
independent. Pin the source tag and both crate requirements together:

```sh
git clone --depth 1 --branch v0.1.2 https://github.com/pboachie/tungsten.git tungsten-v0.1.2
```

In your API project's `tungsten.yml`, merge this target with your existing
inputs. Replace the two absolute source paths with the checkout you just made:

```yaml
targets:
  rust:
    out: generated/rust
    crate: acme-sdk
    runtime: "=0.1.0"
    runtime_path: "/absolute/path/to/tungsten-v0.1.2/runtimes/rust/runtime"
    cli:
      crate: acme-cli
      bin: acme
      kit: "=0.1.0"
      kit_path: "/absolute/path/to/tungsten-v0.1.2/runtimes/rust/cli-kit"
```

```sh
tungsten --version                         # tungsten 0.1.2
tungsten generate .
cargo build --manifest-path generated/rust/Cargo.toml --workspace
cargo run --manifest-path generated/rust/Cargo.toml -p acme-cli -- --help
```

This builds the generated SDK and CLI with Rust 1.88 or newer, using the two
matching source crates. Relative `runtime_path` and `kit_path` values resolve
from the generated SDK and CLI crate directories respectively. Packaging a
generated crate for registry distribution needs registry runtime dependencies
instead of these source paths.
No 0.1.3 upgrade is needed for this stable source installation.

## Install from source

Requires Rust 1.88 or newer. Use the same source revision as your compiler and
generated SDK: `v0.1.2` for 0.1.2, or `v0.1.3` for 0.1.3.
Configure a local dependency in the consuming Cargo project (adjust the path):

```toml
[dependencies]
tungsten-runtime = { path = "../tungsten/runtimes/rust/runtime" }
```

For generated SDKs, `targets.rust.runtime_path` accepts the same runtime crate
path. The sibling `cli-kit` crate supplies the command engine for generated
Rust CLIs. The async runtime uses Tokio and reqwest with rustls.
For a local CLI-kit dependency, set `targets.rust.cli.kit_path` relative to the
generated CLI crate's directory. Keep it and the runtime on the same revision.
These runtime libraries do not install the Tungsten compiler.

## Shared contract

Generated clients use the runtime for request validation, authentication,
idempotency keys, confirmation previews, bounded retries, verification,
pagination, macros and server-sent events where the input API declares them.
Calls return a typed result or a diagnostic envelope describing recovery;
unknown outcomes never justify blindly repeating a mutation. The API remains
responsible for authorization and server-side idempotency.

## Limits

This section describes the 0.1.3 runtime. Its ordinary
response-byte cap is an addition after the `v0.1.2` runtime.

Set `ClientOptions::max_response_bytes` to opt into an ordinary-response byte cap;
a call option with the same name overrides it. The value must be a positive
safe integer (at most 9007199254740991); unset keeps the existing unbounded
behavior. Content-Length is checked before reading, and actual bytes are
counted even without that header. Overflow closes the body and returns
`UNEXPECTED_RESPONSE` with code `RESPONSE_TOO_LARGE` and `retryable: never`.
A mutation may have taken effect: inspect API state before retrying. The cap
does not limit a successful SSE stream; use its event and collection limits.

In unreleased 0.2.0, `ClientCore::download(op, args, &opts, DownloadOptions { max_bytes,
on_progress })` streams a 2xx body instead of buffering it: read the returned `Download`
with `read()` or `write_to(sink)`, which awaits the sink's future before the next read.
The body is bounded by `max_bytes` (default: no limit; a larger `Content-Length` fails
before any byte is read, `code` `RESPONSE_TOO_LARGE`), by `ClientOptions::idle_timeout`
(default the attempt `timeout`) between chunks (`STREAM_IDLE`) and by `deadline`
(`DEADLINE_EXCEEDED`). `max_response_bytes` applies only to error bodies of a download. A
body shorter than its `Content-Length`, a lost connection, `close()` or a sink error
(`SINK_FAILED`) ends it with an error; for a mutation the error says the call took
effect. The connection and its scheduler capacity are held until the body ends, fails,
is closed or the `Download` is dropped. A download is never retried once its answer
arrived and never resumed. `Download::filename` is the `Content-Disposition` name
reduced to a base name without directory parts or control characters; it never chooses a
destination.

File idempotency leases read their owner host from `/proc/sys/kernel/hostname`.
That host field is empty off Linux; it is diagnostic information, not a
cross-host exclusion guarantee. Applications own file-store permissions and
coordination across hosts.

No runtime ships a file-backed OAuth token store. Provide an application-owned
token-store implementation when tokens must survive process restarts; protect
stored credentials and avoid logging them. Registry packages and this source checkout are separate install routes; build
results for one do not establish the other.

## License

Apache-2.0; see the packaged LICENSE. The compiler is licensed separately
under AGPL-3.0-only.
