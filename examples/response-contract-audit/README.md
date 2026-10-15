# Review and enrich response contracts

This fictional Runs API demonstrates a FastAPI-shaped exported contract with
`dict[str, Any]` responses. `openapi.yaml` has a free-form POST response and a
GET response with no schema. `demo.py` is the local server whose two payload
shapes are explicitly described in `contracts.overlay.yaml`. These schemas are
illustrative assertions about that server, not inferred AOBench contracts.

Prefer fixing upstream response models: for example, declare a FastAPI response
model of `Run | Job` for POST, and `Run` for GET. If that is unavailable, keep a
reviewed standard OpenAPI overlay alongside the original specification.
Tungsten applies it before type construction. No generated files need edits.

## Run the recipe

Build tungsten from the repository root with `cargo build -p tungsten-cli`, then
run the following from this directory (POSIX shell, Python 3.12 or later):

```sh
TUNGSTEN=../../target/debug/tungsten

# The original document stays unchanged. Expect TG0930 (missing JSON schema)
# and TG0931 (free-form object); this audit alone still exits successfully.
"$TUNGSTEN" check openapi.yaml --audit-contracts

# The manifest lists our reviewed overlay. Expect no audit findings.
"$TUNGSTEN" check tungsten.yml --require-contracts
"$TUNGSTEN" generate tungsten.yml
"$TUNGSTEN" generate tungsten.yml --check

python3 -m venv .venv
.venv/bin/python -m pip install -e ../../runtimes/python -e sdk/python mypy
.venv/bin/python -m mypy --strict demo.py
.venv/bin/python demo.py
```

The last command starts and stops a loopback HTTP server and calls both
`wait=True` and `wait=False`. The generated return type remains `Run | Job` for
both calls. A boolean request parameter does not narrow the response. The
example uses `isinstance` after strict response validation; no discriminator or
tag field is invented. Completed/failed runs require `result`; queued/running
jobs require `run_id`. Required fields and distinct status enums distinguish
the variants.

This recipe uses Python 3.12. Python 3.10 compatibility is tracked separately in
issue #157; this recipe does not change runtime requirements.

## What the audit reports

`check --audit-contracts` audits JSON success responses, including references,
array items, record properties, map values and union members. Each finding names
an operation, status, media type, weak node pointer and resulting type limitation.
Shared source nodes appear once, at their first reaching operation. TG0305 is
reused for unconstrained schemas, TG0930 covers absent schemas, and TG0931 covers
free-form objects. TG0932 means the audit exceeded its traversal budget and is
incomplete. Use `--json` for the full machine-readable findings.

Typed maps, closed objects, no-content responses and non-JSON bodies are not
quality failures. Nested weak fields still count even when optional or nullable.
An unconstrained `allOf` conjunct or constrained `$ref` base is not itself a gap:
the other conjuncts may supply its contract, though nested fields are audited.
This is a focused quality audit, not a complete JSON Schema soundness proof.
Existing TG0733 (Python inline records) and TG0530 (untyped SSE events) retain
their existing behavior.

For an intentional arbitrary JSON value or free-form map, add a scoped review
note through an overlay:

```yaml
- target: $.paths['/v1/runs'].post.responses['200'].content['application/json'].schema
  update:
    x-tungsten-reviewed-contract: 'Reviewed: this endpoint intentionally returns arbitrary metadata'
```

The note must be a nonempty string. On a schema it acknowledges that subtree;
on a media-type entry it acknowledges the whole body, including a missing
schema. A note next to a `$ref` acknowledges that use without muting unrelated
uses of the shared schema. It changes no generated type or runtime validation.
Review the scope before adding it. Default checks and severity stay unchanged.
`check --require-contracts` implies the audit and fails on audit findings alone;
other warnings remain warnings. `check --audit-contracts --strict` fails on audit
warnings and all other compiler warnings. This example intentionally retains
TG0301 for its untagged union, so broad `--strict` fails even after enrichment;
`--require-contracts` enforces its response quality without inventing a tag.

## Review, provenance and drift

The overlay contains hand-written named `Run` and `Job` definitions and their
union. There is no inference scaffold: for a real API, the reviewer must decide
the actual fields, requiredness, nullability, status values, variant overlap and
error responses from authoritative server models and payload validation. Names,
samples and prose alone cannot establish a guaranteed contract.

Per-input overlays run in listed order, then project-level overlays run in
listed order. Actions run in file order. Object updates merge and array updates
append. The first action here removes the old POST schema before adding its
replacement. Inserted-value diagnostic spans identify the overlay; generated
headers record input/overlay digests. Preserve the spec and overlay in source
control with a review record tying definitions to their evidence.

An unmatched action target reports TG0206 and an invalid action reports TG0207.
Use `check --strict` to fail on unmatched targets along with all other warnings,
or inspect TG0206 in the JSON diagnostics under your project's warning policy.
`--require-contracts` promotes only audit findings, so it does not replace that
overlay-target check. These diagnostics detect a
changed location; an overlay that still matches can hide changed upstream
semantics, so review upstream diffs and verify payloads separately. Once upstream
publishes equivalent response schemas, remove the overlay entry, regenerate,
audit again and rerun those payload checks.

Running `generate` twice must produce identical SDK files; `generate --check`
compares generated output without writing. Changing a reviewed schema/overlay
after generation makes that check fail until regeneration. Commit generated
SDKs in your consumer project and run the same command in CI. A no-diff CI gate
detects generated-file drift; it cannot prove server truth. Validate captured
valid and invalid payloads against the reviewed models, and validate actual
responses in strict mode as separate safeguards. Regression fixtures belong in
the maintainer suite, per CONTRIBUTING.
