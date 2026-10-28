# Python-backed browser WebMCP tools (experimental)

Register a page's WebMCP tools with registerRemoteWebMcp and run every call on a Python web app through the generated Python SDK, with the app's own sign-in.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; Python venv and pip, and Node 22 or later with npm.

From the repository root:

```sh
python3 examples/run.py --only webmcp-python
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: the page registers the server-chosen tools, reads typed pets, is refused an unconfirmed delete and a cross-user token, deletes with its own preview token, is refused when signed out and removes its tools on dispose, while the loopback mock sees exactly one DELETE.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

## Problem: browser tools for a Python web app

A page's WebMCP tools run in the browser, but a Python application wants the API
called from its server, with its own sessions and without API credentials in
the page. The recipe generates the Python SDK and the experimental
`mcp-python` server with `webmcp: true`, which adds `petstore_mcp/webmcp.py`.
It then runs [app.py](app.py), a small ASGI app that mounts that dispatch under
`/webmcp` with the app's own sign-in. [page.mjs](page.mjs) plays the page: it
calls `registerRemoteWebMcp({ endpoint: "/webmcp" })` from
`@elysra/tungsten-webmcp` against a stub `document.modelContext`, and calls the
tools the way an agent in the browser would. The API is a loopback
`tungsten mock`. Expected output:

```text
registered for alice: pets_pets_list, pets_pets_delete, pets_pets_delete__preview
pets_pets_list: typed pets from the Python SDK
pets_pets_delete without a token: CONFIRMATION_REQUIRED, nothing sent
bob cannot use alice's confirmation token
alice's confirmed delete: done
signed out: the tool listing is refused (401)
dispose: every owned tool removed
```

The server decides which tools a user sees and authorizes every call: reads
by default, plus the delete that `app.py` allowlists. Each user has their own
API client and confirmation tokens. Every request needs the
`X-Tungsten-WebMCP: 1` header and a same-origin `Origin`, and no CORS headers
are sent. This is experimental. It runs in Node with a stub WebMCP backend, and
a real WebMCP browser has not been qualified. See the `tungsten-mcp` and
`@elysra/tungsten-webmcp` READMEs and
[#155](https://github.com/pboachie/tungsten/issues/155).
