# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project will adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
from its first stable release. Until 1.0, minor versions may contain breaking
changes.

## [Unreleased]

### Added

- Compiler: OpenAPI and `tungsten.yml` configuration frontend with diagnostics
  (`tungsten check`, `tungsten explain`) and a versioned intermediate
  representation (`tungsten ir dump`).
- TypeScript SDK emitter and the `@tungsten/runtime` client runtime
  (Apache-2.0).
- Python SDK emitter (sync and async clients) and the `tungsten-runtime`
  Python package (Apache-2.0).
- Rust SDK emitter and the `tungsten-runtime` Rust crate (Apache-2.0).
- Generated command-line interface for the Rust SDK, with a stable output
  contract, exit codes, confirmation for destructive operations, configuration
  and pagination.
- MCP server emitter and runtime: token-budgeted tool surfaces for AI agents,
  sandboxed execution and confirmation of destructive operations.
- Documentation emitter for generated SDKs.
- `tungsten mock`: a mock server generated from the OpenAPI description, for
  trying a generated SDK or CLI without a live API.
- OAuth2 authorization-code helpers in the TypeScript, Python and Rust
  runtimes: PKCE (S256), the authorization URL, the code exchange, refresh with
  a single flight and a pluggable token store. A call sends the stored token,
  refreshes it before it expires and once after a 401. Generated clients expose
  the helpers only when the specification declares an `authorizationCode` flow.
- Repository guard and community files (contributing guide, security policy,
  code of conduct, issue forms and pull request template).

[Unreleased]: https://github.com/pboachie/tungsten/commits/main
