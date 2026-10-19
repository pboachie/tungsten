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

## Browser OAuth authorization

In unreleased 0.2.0, `OAuthFlow::authorization_url` begins a managed attempt and
returns an `AuthorizationAttempt` with a URL, opaque handle and lifetime. Call it
inside a Tokio runtime with time enabled so the expiry task can run; the
presence of a current runtime handle alone does not prove that capability.
Supply a nonempty
`AuthorizationUrlParams::session_binding` owned by your callback listener/session,
qualify provider S256 support with `s256_supported: true`, and select an explicit
`IssuerDefense`. `IssuerResponse` requires the provider's exact decoded `iss` in
every callback, including denial. `DistinctRedirect` requires an application
qualification that a separate redirect isolates the issuer; missing `iss` never
selects this fallback. The application owns provider qualification and listener
ownership; the runtime cannot infer them from a URL.

The runtime generates independent state and S256 PKCE for every attempt and
retains its verifier privately. `exchange_code` takes `ExchangeCodeParams` with
the attempt handle, full callback URL and the same session binding. It validates
the captured issuer/client/endpoints/redirect/context and claims the attempt once
before sending. Duplicate or malformed callback fields, mismatched callbacks,
replay and expired/canceled attempts refuse without another token exchange. A
denial or exchange failure consumes its attempt; begin a new attempt instead of
retrying a potentially consumed code. Never log the authorization URL, callback,
attempt handle, session binding, tokens or verifier.

Each client admits at most 32 pending/exchanging attempts. Attempts expire after
10 monotonic minutes; captured inputs are limited to 32 KiB, 64 scopes and 64
extra fields, while raw/decoded callbacks are limited to 16 KiB and 64 fields.
`cancel_authorization` drops a pending verifier; cancellation/expiry retains a
claimed work slot until that work settles. `logout` invalidates attempts before
store work and fences late token writes and refresh results. Concurrent completed
logins select credentials at their successful store commits. A separate mutation
epoch prevents older refresh success or rejection from replacing/deleting a newer
managed login, including one returning the same token strings.

Managed begin and refresh require `TokenStore::supports_fencing` and both
conditional store operations. Invoke their predicate exactly once against the
current token at the final serialized mutation, with no speculative check, retry
or gap before write/removal. The predicate changes a session epoch; report true
only for a committed mutation and preserve the old token if the write fails. A
predicate followed by a failed write may conservatively invalidate old refresh
work. The default `MemoryTokenStore` supplies this per-client contract. Shared
multi-account persistence needs separate partitioning and adapter qualification.
Direct/external store writes and `low_level_authorization_url` /
`low_level_exchange_code` bypass managed attempt/epoch protections and need
application-owned coordination; do not mix external credential replacement with
an active managed session. Use logout and a new session first. `pkce` remains a
low-level utility; managed begin creates its own pair.

Code/refresh token responses default to 64 KiB. A separately validated positive
safe-integer `ClientOptions::max_oauth_token_response_bytes` overrides that cap.
It bounds accepted response bytes, not decoder allocations, total invocation
memory or client-credentials responses. Custom stores/transports own their
settlement behavior; logical invalidation does not promise immediate physical
zeroization or completion of externally owned work.

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
