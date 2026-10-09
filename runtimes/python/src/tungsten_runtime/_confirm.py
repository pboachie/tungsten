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
from typing import Final, Literal

from ._json import canonical_json
from ._util import b64url, hmac_sha256, sha256_hex

#: Lifetime of a confirmation token.
CONFIRMATION_TTL_MS: Final = 5 * 60 * 1000

_TOKEN = re.compile(r"tgc1\.([0-9]{1,16})\.([A-Za-z0-9_-]{43})")

type TokenCheck = Literal["valid", "expired", "mismatch", "malformed"]


def args_digest(args: object) -> str:
    """SHA-256 of the canonical JSON of the arguments."""
    return sha256_hex(canonical_json(args))


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
    expected = _signature(key, subject, args, expiry)
    if not hmac.compare_digest(expected, match.group(2)):
        return "mismatch"
    return "valid" if expiry > now else "expired"


def token_expiry(token: str) -> int | None:
    match = re.match(r"tgc1\.([0-9]{1,16})\.", token)
    return int(match.group(1)) if match is not None else None
