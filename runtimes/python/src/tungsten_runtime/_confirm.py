# SPDX-License-Identifier: Apache-2.0
"""Confirmation tokens (planning/06 "Preview and confirmation",
``runtimes/ts/src/confirm.ts``):
``tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, "<op id>\\n<args digest>\\n<expiry>"))>``
where the args digest is the lowercase hex SHA-256 of the canonical JSON of
the arguments (``_json.canonical_json``). A token is valid only for the
operation (or ``macro:<name>``) and the exact arguments it was issued for,
until its expiry; tokens agree byte for byte with the TypeScript runtime's
for the same key, subject, arguments and clock."""

from __future__ import annotations

import hmac
import math
import re
from collections.abc import Mapping, Sequence
from typing import Final, Literal, cast

from ._json import canonical_json
from ._upload import UploadSource
from ._util import b64url, hmac_sha256, sha256_hex

#: Lifetime of a confirmation token.
CONFIRMATION_TTL_MS: Final = 5 * 60 * 1000

_TOKEN = re.compile(r"tgc1\.([0-9]{1,16})\.([A-Za-z0-9_-]{43})")

TokenCheck = Literal["valid", "expired", "mismatch", "malformed"]


def _without_uploads(value: object, path: list[str], uploads: list[object]) -> object:
    """``value`` with every ``UploadSource`` replaced by None, its path and
    declaration added to ``uploads`` (keys in sorted order, as the
    TypeScript runtime walks them)."""
    if isinstance(value, UploadSource):
        uploads.append({"path": path, "upload": {"length": value.length, "sha256": value.sha256}})
        return None
    if isinstance(value, Mapping):
        entries = cast(Mapping[object, object], value)
        keys = sorted(k for k in entries if isinstance(k, str))
        return {k: _without_uploads(entries[k], [*path, k], uploads) for k in keys}
    if isinstance(value, list | tuple):
        items = cast(Sequence[object], value)
        return [_without_uploads(v, [*path, str(i)], uploads) for i, v in enumerate(items)]
    return value


def args_digest(args: object) -> str:
    """SHA-256 of the canonical JSON of the arguments; upload sources are
    bound by their declared length and SHA-256 in a separate manifest, as in
    the TypeScript runtime."""
    uploads: list[object] = []
    value = _without_uploads(args, [], uploads)
    if not uploads:
        return sha256_hex(canonical_json(args))
    return sha256_hex(f"{canonical_json(value)}\nblob-content-v1\n{canonical_json(uploads)}")


def token_payload(subject: str, args: object, expiry: int) -> str:
    """The message a token signs."""
    return f"{subject}\n{args_digest(args)}\n{expiry}"


def _signature(key: bytes, subject: str, args: object, expiry: int) -> str:
    return b64url(hmac_sha256(key, token_payload(subject, args, expiry)))


def issue_token(key: bytes, subject: str, args: object, now: float) -> str:
    expiry = math.floor(now) + CONFIRMATION_TTL_MS
    return f"tgc1.{expiry}.{_signature(key, subject, args, expiry)}"


def check_token(key: bytes, token: str, subject: str, args: object, now: float) -> TokenCheck:
    match = _TOKEN.fullmatch(token)
    if match is None:
        return "malformed"
    expiry = int(match.group(1))
    if str(expiry) != match.group(1):
        return "malformed"
    expected = _signature(key, subject, args, expiry)
    if not hmac.compare_digest(expected, match.group(2)):
        return "mismatch"
    return "valid" if expiry > now else "expired"


def token_expiry(token: str) -> int | None:
    match = _TOKEN.fullmatch(token)
    if match is None or str(int(match.group(1))) != match.group(1):
        return None
    return int(match.group(1))
