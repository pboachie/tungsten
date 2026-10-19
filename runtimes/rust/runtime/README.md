# tungsten-runtime (Rust)

## Registry status

Registry installation is currently unavailable. The 0.1.3 compiler is an
unreleased source candidate; use runtime source matching your compiler and
generated SDK. This library does not install the Tungsten compiler.

## Install from source

Requires Rust 1.88 or newer. Use `v0.1.2` source for stable 0.1.2, or the matching
0.1.3 candidate checkout. Configure a local dependency (adjust the path):

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

## Credential transport

Credentials require HTTPS by default, including OAuth client-credentials, code
exchange and refresh endpoints. Non-loopback HTTP returns `AUTH_FAILED` before
sending credentials. Migrate authenticated HTTP services to HTTPS, or explicitly
opt in with `ClientOptions { allow_insecure_http: true, ..Default::default() }` for intentional insecure transport.
This permits cleartext credentials; enable it only for a trusted environment.

The local-development exception covers `localhost` (also with a trailing dot),
literal IPv4 `127/8`, and IPv6 `::1`. Subdomains, unspecified addresses and
IPv4-mapped IPv6 addresses require the explicit override. DNS resolution does not
grant an exception. Anonymous HTTP requests remain supported.

## Limits

In unreleased 0.2.0, ordinary buffered responses default to **4 MiB (4,194,304 bytes) per response**.
Set `ClientOptions::max_response_bytes` to choose a finite client cap; the call option with the same
native name overrides it. Omitted/null values inherit the default or client cap,
not an unlimited mode. Values must be positive safe integers (at most
9007199254740991), validated before authentication, storage and preview effects.
This covers ordinary success/error bodies, server previews, each page and each
verification read. A page cap does not bound collecting many pages.

Content-Length can reject an oversized representation early; actual delivered
bytes are counted even without it. HEAD/204/205/304 metadata alone does not
consume the budget, but any body bytes a transport delivers are still counted.
Overflow closes the owned body and returns `UNEXPECTED_RESPONSE`, code
`RESPONSE_TOO_LARGE`, with `retryable: never`. A mutation may have taken effect:
inspect API state before retrying. The cap is separate from OAuth token-response
and successful SSE event/collection budgets.

The default transport does not decompress encoded bodies; use identity encoding. Encoded delivery does not establish a decoded-content limit.
The cap limits accepted body bytes; parsers, copies and HTTP decoders can allocate
more. Custom transports must stream nonempty bodies and honor cancellation;
their own buffering, decoding and settlement behavior need separate qualification.

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
