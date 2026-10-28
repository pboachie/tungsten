# Manual release-video walkthrough

Record real PowerShell output, add local male narration, and produce a chaptered
review MP4 with captions and execution evidence. This is an optional standalone
Windows example, run manually outside the gallery's automated recipes.

## Prerequisites and setup

Use Windows x64, Python 3.12, Git, and Node.js 22+ with npm. Run from this directory:

```powershell
python bootstrap.py
.\Run-ReleaseVideo.ps1 -Version v0.1.2 -RunId review-001 -Stage all -AllowRegistry
```

Bootstrap creates `.work/.venv`, installs the pinned dependencies in
`pyproject.toml`, and downloads a portable Go toolchain and Kokoro model assets.
Go and model downloads are SHA-256 checked. The model is about 340 MB and Go is
about 79 MB. Prerequisites, caches, source checkouts, generated SDKs, recordings,
and review outputs stay under ignored `.work/`. It changes no system PATH or
account setting. Do not run it against an active development checkout.

`-AllowRegistry` permits public npm dependency resolution during preflight with
install scripts disabled. Once the local cache is populated it can be omitted.
Recording uses the preflight lockfiles and offline npm cache. Explicit package
build commands run the generated TypeScript compiler script.

The default voice is generic male Kokoro `am_fenrir` at speed 0.96. Select
`-Voice am_michael` or `-Voice am_puck` during preparation for another local male
voice. There is no voice cloning, hosted TTS, or paid API.

## Manual stages

The stages are `audit`, `prepare`, `narrate`, `record`, `render`, `qa`, `compact`,
and `package`. `all` runs them in that order. Resume an existing prepared run by
selecting a later stage:

```powershell
.\Run-ReleaseVideo.ps1 -Version v0.1.2 -RunId review-001 -Stage record
.\Run-ReleaseVideo.ps1 -Version v0.1.2 -RunId review-001 -Stage render
.\Run-ReleaseVideo.ps1 -Version v0.1.2 -RunId review-001 -Stage qa
.\Run-ReleaseVideo.ps1 -Version v0.1.2 -RunId review-001 -Stage compact
.\Run-ReleaseVideo.ps1 -Version v0.1.2 -RunId review-001 -Stage package
```

New preparations require a new run name. Existing captures, audio, MP4s, and ZIPs
are refused rather than replaced. If a run fails, retain its command log and use
a new name after fixing the cause. For Python entry-point options:

```powershell
.work\.venv\Scripts\python.exe release_video.py --help
```

## Exact-version review

The included profile and chapter script describe the published **v0.1.2** tag,
commit `1a0ee4def65440010f1810ee447285e6a936192d`. They are historical release
data, not a claim about the current development branch or latest publication.

Audit verifies stable-release metadata, archive digest, executable version/hash,
the source tag and clean checkout, emitter inventory, input dispatch, imported
policy, and the Go emitter descriptor. It never builds the compiler from main.
The public ZROtext example is pinned independently by commit and contract hash.

For another exact published version, audit stops with a candidate
`.work/cache/<version>/profile-needs-review.json`. Review the source, runtime
versions, policy probes, and fixture; pass the approved profile with `-Profile`.
Create `scripts/<version>.json` with reviewed claims and commands. Update the
recipe/audit when a release changes those interfaces. A changed profile or tag is
not permission to reuse old footage or describe unpublished code as shipped.

## Inputs and output claims in the v0.1.2 recipe

Native OpenAPI 3.0/3.1 JSON/YAML produces TypeScript, Python, Rust, Java, C#, Ruby,
and external-emitter Go SDK source. Other outputs are Rust CLI source, MCP,
experimental browser WebMCP, and documentation.

The example prepares Swagger 2, Postman Collection 2.1, saved cURL, `.http` and
`.rest`, HAR 1.2, endpoint manifest v1 with JSON Schema 2020-12, and manual
method/endpoint inputs. Imported contracts in this release support
TypeScript/Python/Rust plus CLI/MCP/WebMCP/docs; Java/C#/Ruby/Go are refused with
TG0702. Preflight exercises the positive routes and those four policy refusals.
Standalone JSON Schema describes models; explicit HTTP routing is still needed.

Git retrieves a public source file before the compiler reads it locally. Native
Git/HTTP acquisition and remote references are outside this profile. The Postman
collection is derived from a declared read-only ZROtext route with no fabricated
saved response. Standards examples are unchanged OAI files. HAR and manual
response examples retain one actual public `httpbin.org/json` GET response with
no cookies, private headers, or user/IP data. Saved requests are parsed as data.
Public source checkouts are read without executing their repository scripts.

The walkthrough generates ten target directories, builds TypeScript/MCP/Go, and
lists the actual local MCP catalog. Python, Rust CLI, Java, C#, Ruby, and WebMCP
are source-only demonstrations here. No ZROtext API operation or SMS is invoked;
no mock service is started. Genuine compiler warnings remain visible.

## Review outputs

`.work/runs/<version>-<run-id>/deliverables` contains a 1080p MP4, opening contents,
visible chapter headings, exact chapter JSON/FFmetadata, companion timestamps,
SRT, QA report, compact MP4, and a small reusable source/evidence ZIP. Raw PTY
events, an asciicast, real command exits, input provenance, and model hashes are
retained in the run. The terminal is a genuine pseudoterminal at original
timestamps with scripted submissions; it is not a desktop capture or fake typing.
The render masks the local username. Portable evidence masks host home paths;
raw local evidence remains unchanged.

QA fully decodes the MP4, verifies chapters/subtitles and expected commands,
checks source cleanliness and generated outputs, measures audio loudness and
clipping, and compares waveform timing with narration. `compact` preserves the
resolution and frame timing with efficient H.264 and mono AAC, then repeats QA.
It stops above a conservative 16 MB budget so settings can be reviewed. Size
limits and chapter support depend on the destination; inspect final playback
and terminal legibility before delivery.

Human review is required for voice, pronunciation, pacing, release claims, and
publication. The runner has no publishing, subscription, scheduling, credential,
or persistent-permission step. The ZIP excludes models, dependencies, caches,
videos, source checkouts, and generated SDK artifacts. Broader video placement
for the project is tracked in [#258](https://github.com/pboachie/tungsten/issues/258).

## Sources and notices

- [Tungsten v0.1.2](https://github.com/pboachie/tungsten/releases/tag/v0.1.2) and
  the repository's existing icon.
- [Pinned ZROtext contract](https://github.com/pboachie/zrotext/blob/84bfeee59f57f7cbdac00f4fbe3f567d36c5e57c/protocol/v1/openapi/public-v1.json),
  [OAI examples](https://github.com/OAI/OpenAPI-Specification/tree/3.0.3/examples),
  and [httpbin JSON](https://httpbin.org/json).
- [kokoro-onnx](https://github.com/thewh1teagle/kokoro-onnx): MIT wrapper;
  [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M): Apache-2.0 model.
  Model weights are downloaded from upstream, not redistributed here.
- [Go](https://go.dev/LICENSE): BSD-style license; downloaded from the official
  distribution with its pinned digest.

The public contribution rules keep tests and captured test data outside this
repository. Run regression checks in a separate local harness; keep generated
review evidence in `.work/`, and describe new coverage scenarios for maintainers.
