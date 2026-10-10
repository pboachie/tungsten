# External emitter example: Markdown

A complete tungsten external emitter in about 200 lines of Rust. It writes one
Markdown page per resource (operations, methods, paths, parameters) and an
`index.md`. It depends only on `serde_json`, to show that an emitter needs no
tungsten code: the protocol is JSON on standard input and output, so the same
thing can be written in any language.

## Try it

```sh
cargo build --manifest-path examples/external-emitter-markdown/Cargo.toml
```

Point a target at the binary in `tungsten.yml` (a path is relative to that
file):

```yaml
tungsten: 1
api: { name: petstore }
inputs:
  - { spec: openapi.json, namespace: petstore }
targets:
  markdown:
    out: generated/markdown
    external: ../../examples/external-emitter-markdown/target/debug/tungsten-emit-markdown
    index: true                 # an option of this emitter
```

or put `tungsten-emit-markdown` on `PATH` and write `external: true`. Then:

```sh
tungsten emitters .             # lists it with its protocol and version
tungsten generate .             # writes generated/markdown/ and its manifest
tungsten generate . --check     # fails when the pages are stale
```

The pages are written by tungsten, not by the emitter, so the output
directory gets `.tungsten/manifest.json` and `.tungsten/surface.json` like any
built-in target, `generate --check` and `tungsten diff` work, and a page of a
resource that disappeared is removed.

## What it demonstrates

- `--describe`: prints `{ protocol, name, version, options }`.
- The request: `protocol`, `target`, `options` (here `index`) and `ir`, the
  document of `tungsten ir dump`.
- The response: `files` with `path` and `content`, and `diagnostics`. An
  operation without a summary produces a warning with the emitter's own code
  `MD001`, which tungsten shows as `TG0806` (`external emitter markdown:
  MD001: ...`) and which fails the run under `--strict`.

The protocol is specified in the rustdoc of `tungsten_emit::external`, and its
JSON Schema is printed by `tungsten schema external-emitter`.
