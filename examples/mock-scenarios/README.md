# Assert retries with saved mock scenarios

Run an explicit integration against saved 429, interrupted-stream, malformed-response and lost-write scenarios, and fail deliberately broken retries.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; Linux for scenario run; no target-specific toolchain.

From the repository root:

```sh
python3 examples/run.py --only mock-scenarios
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: four saved scenarios pass, and broken retry, resume and replay behavior fails its assertions.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

## Saved scenarios for retry behavior

This recipe compiles the committed `openapi.json` directly; it does not write a
project manifest or generate an SDK. Each file in `scenarios/` is version 1 JSON:
responses queued on the existing mock controls, plus the calls and effects the
integration should cause. Files are data only. The integration is the separate,
stdlib-only `integration.py`, selected explicitly after `--`:

```sh
tungsten scenario validate openapi.json --scenario scenarios/lost-write.json
tungsten scenario run openapi.json --scenario scenarios/lost-write.json \
  --timeout-ms 30000 -- python3 -I integration.py lost-write
```

The integration reads its fresh loopback URL from `TUNGSTEN_MOCK_BASE_URL`. Each
run binds its own port and mock state, so runs never share responses or counts.

| Scenario | Correct integration | `--broken` variant that must fail |
|---|---|---|
| `rate-limit.json` | retries a 429 after `Retry-After` | gives up after the 429 |
| `stream-interrupted.json` | resumes with `Last-Event-ID` after a cut stream | stops at the cut |
| `malformed.json` | reports an invalid JSON body once (`--expect-exit 3`) | retries the same request |
| `lost-write.json` | retries a dropped write with the same `Idempotency-Key` | uses a new key for the retry |

The runner validates all four files, runs each correct integration, and
requires each broken variant to fail. The lost-write broken variant matches
every call count; only `expect.effects` (`applied` 2, `replayed` 0) rejects it.
`scenario run` is currently Linux-only, and this recipe fails explicitly on
other platforms after validation. Assertions describe the mock's observations
and its own processing, not a real provider's behavior.
