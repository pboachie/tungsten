# SPDX-License-Identifier: AGPL-3.0-only
"""Stream a file up and down through the generated Python SDK.

The runner runs ``python transfer.py BASE_URL`` in a scratch directory."""

import hashlib
import os
import sys
from collections.abc import Iterator

from files_sdk import FilesClient
from tungsten_runtime import ClientOptions, UploadSource

SIZE = 16 * 1024 * 1024
base = sys.argv[1]

# A 16 MiB input file, written in pieces.
digest = hashlib.sha256()
with open("input.bin", "wb") as f:
    for i in range(SIZE // 65536):
        piece = bytes([i % 251]) * 65536
        digest.update(piece)
        f.write(piece)
sha256 = digest.hexdigest()


def chunks() -> Iterator[bytes]:
    """The file, read again from the start for every attempt."""
    with open("input.bin", "rb") as f:
        while piece := f.read(65536):
            yield piece


client = FilesClient(ClientOptions(), base_url=base)
source = UploadSource.recreatable(chunks, length=SIZE, sha256=sha256)
stored = client.files.update(name="report.bin", body=source)
if not stored.ok:
    raise SystemExit(f"upload failed: {stored.error}")
assert stored.value.sha256 == sha256, "the server stored different bytes"
print(f"python: uploaded {stored.value.bytes_} bytes, sha256 matches")

if os.path.exists("copy.bin"):
    os.remove("copy.bin")
download = client.files.get_download(name="report.bin")
if not download.ok:
    raise SystemExit(f"download failed: {download.error}")
# The server suggests a name; the caller still chooses the destination.
assert download.value.filename == "report.bin"
saved = download.value.save_to("copy.bin")
if not saved.ok:
    raise SystemExit(f"save failed: {saved.error}")
with open("copy.bin", "rb") as f:
    assert hashlib.file_digest(f, "sha256").hexdigest() == sha256, "the copy differs"
print(f"python: downloaded {saved.value} bytes to copy.bin, sha256 matches")

again = client.files.get_download(name="report.bin")
refused = again.value.save_to("copy.bin") if again.ok else again
print(f"python: second save refused: {refused.error['code']}")

capped = client.files.get_download(name="report.bin", opts={"max_bytes": 1024 * 1024})
print(f"python: capped download refused: {capped.error['code']}")
missing = client.files.get_download(name="missing.bin")
print(f"python: missing file: {missing.error['category']}")
