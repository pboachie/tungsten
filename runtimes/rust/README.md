# Tungsten Rust runtime packages

## Install from source

Requires Rust 1.88 or newer. Until a matching crates.io release is available,
use a local dependency (adjust the checkout path):

```toml
[dependencies]
tungsten-runtime = { path = "../tungsten/runtimes/rust/runtime" }
```

For generated SDKs, `targets.rust.runtime_path` accepts the same runtime crate
path. The sibling `cli-kit` crate supplies the command engine for generated
Rust CLIs. The async runtime uses Tokio and reqwest with rustls.

## Shared contract

Generated clients use the runtime for request validation, authentication,
idempotency keys, confirmation previews, bounded retries, verification,
pagination, macros and server-sent events where the input API declares them.
Calls return a typed result or a diagnostic envelope describing recovery;
unknown outcomes never justify blindly repeating a mutation. The API remains
responsible for authorization and server-side idempotency.

## Limits

Set `ClientOptions::max_response_bytes` to opt into an ordinary-response byte cap;
a call option with the same name overrides it. The value must be a positive
safe integer (at most 9007199254740991); unset keeps the existing unbounded
behavior. Content-Length is checked before reading, and actual bytes are
counted even without that header. Overflow closes the body and returns
`UNEXPECTED_RESPONSE` with code `RESPONSE_TOO_LARGE` and `retryable: never`.
A mutation may have taken effect: inspect API state before retrying. The cap
does not limit a successful SSE stream; use its event and collection limits.

File idempotency leases read their owner host from `/proc/sys/kernel/hostname`.
That host field is empty off Linux; it is diagnostic information, not a
cross-host exclusion guarantee. Applications own file-store permissions and
coordination across hosts.

No runtime ships a file-backed OAuth token store. Provide an application-owned
token-store implementation when tokens must survive process restarts; protect
stored credentials and avoid logging them. Registry publication and package
availability are separate from building this source checkout.

## License

Apache-2.0; see the packaged LICENSE. The compiler is licensed separately
under AGPL-3.0-only.
