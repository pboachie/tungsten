# Stream files up and down

Upload a file from disk and download it to a caller-chosen path without holding it in memory, with declared length and SHA-256, byte limits and safe partial-file handling, in TypeScript, Python and Rust.

Prerequisites: matching tungsten compiler and source checkout, Python 3.12 or
later for the runner; Python venv and pip, Node 22 or later with npm, and the pinned Rust toolchain and Cargo.

From the repository root:

```sh
python3 examples/run.py --only file-transfer
```

The runner copies this directory's committed input into `.work/`, writes the
complete project manifest there, then validates and generates it twice. Package
dependencies use matching runtime source. Generated files remain in `.work/`
for inspection; rerunning replaces only that generated directory. Package-manager
downloads may need internet access. No external API is called.

Expected outcome: the TypeScript, Python and Rust clients each upload and download a 16 MiB file with matching SHA-256, refuse a second save without overwrite and a body over the byte limit, and report a missing file as NOT_FOUND.

Add `--generate-only` to inspect compiler output without building a first-SDK
package. Mock and confirmation scenarios always install and exercise the Python
SDK. Recipes verify deterministic generation and `generate --check`; input
recipes check their initialized project's generated output.

Target availability, minimum toolchains and acceptance limits belong to the
[canonical target matrix](../../README.md#targets), maintained through
[#61](https://github.com/pboachie/tungsten/issues/61). A small example does not
establish full target parity, registry availability or live API readiness.

## Problem: files too large to hold in memory

Media, backups and exports should move between disk and an API without the
whole file in memory, and without leaving a half-written file behind when the
transfer fails. The recipe starts a loopback file store
([server.py](server.py)), generates TypeScript, Python and Rust SDKs for it
([openapi.json](openapi.json)) and, in each language
([transfer.py](transfer.py), [transfer.mjs](transfer.mjs),
[transfer.rs](transfer.rs)):

- writes a 16 MiB input file and uploads it from disk with a recreatable
  `UploadSource` that declares its length and SHA-256;
- downloads it with the generated download method and saves it to
  `copy.bin`, checking the SHA-256;
- shows that saving again without overwrite is refused, that a byte limit
  refuses the body from its `Content-Length`, and that a missing file is
  `NOT_FOUND`.

Expected output, the same for each language:

```text
python: uploaded 16777216 bytes, sha256 matches
python: downloaded 16777216 bytes to copy.bin, sha256 matches
python: second save refused: DESTINATION_EXISTS
python: capped download refused: RESPONSE_TOO_LARGE
python: missing file: NOT_FOUND
```

## The calls

```python
source = UploadSource.recreatable(chunks, length=SIZE, sha256=sha256)
client.files.update(name="report.bin", body=source)
client.files.get_download(name="report.bin").value.save_to("copy.bin")
```

```js
const source = UploadSource.recreatable(() => Readable.toWeb(createReadStream("input.bin")), { length: SIZE, sha256 });
await client.files.update({ name: "report.bin", body: source });
await saveDownload((await client.files.getDownload({ name: "report.bin" })).value, "copy.bin");
```

```rust
client.files().update_upload(request, source, &opts).await?;
client.files().get_download(get(), &opts, DownloadOptions::default()).await?.save_to("copy.bin", false).await?;
```

- An upload source is **one-shot** (a stream read once: a request whose body
  was read is never sent again) or **recreatable** (opened again for every
  attempt, as here). A declared length and SHA-256 are checked against the
  bytes; the last chunk is held back until they match, so a file that changed
  under the upload never completes its request.
- A download is read in chunks with backpressure, bounded by `max_bytes`
  (`maxBytes`), the idle timeout and the call deadline. Saving writes to a
  temporary `.part` file next to the destination and renames it only after
  the whole body arrived; a failure removes it and leaves the destination as
  it was. The server's suggested `filename` never chooses the destination.
- Range requests and resumable uploads are not supported. See each runtime
  README's Limits section.
