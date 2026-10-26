# SPDX-License-Identifier: Apache-2.0
"""Durable checkpoints for resumable read-only page exports.

A checkpoint is a caller-owned token,
``tgp1.<base64url(canonical JSON payload)>.<base64url(HMAC-SHA-256(key, …))>``,
recording how far a traversal got after a page the application committed.
The payload binds the traversal to the API, operation, contract shape, base
URL, tenant, caller scope and every argument but the position, as an HMAC
under the same key, so neither arguments nor credentials are stored. The
next-page value (a provider cursor or URL) is stored as is: treat the token as
sensitive and keep it out of logs.
"""

from __future__ import annotations

import base64
import binascii
import hmac
import json
import re
from typing import Any, Final, cast

from ._json import canonical_json
from ._util import b64url, hmac_sha256
from .types import CheckpointStatus

#: Version prefix of checkpoint tokens.
CHECKPOINT_PREFIX: Final = "tgp1."
#: Longest accepted checkpoint token, in characters.
MAX_CHECKPOINT_CHARS: Final = 16384
_MAX_SAFE_INTEGER: Final = 2**53 - 1
_SEAL: Final = "tungsten-page-checkpoint\n"
_B64URL = re.compile(r"[A-Za-z0-9_-]*")


def _safe_int(value: object) -> bool:
    return type(value) is int and 0 <= value <= _MAX_SAFE_INTEGER


def checkpoint_binding(key: bytes, identity: dict[str, object]) -> str:
    """The binding of a traversal's identity under ``key``."""
    return b64url(hmac_sha256(key, "tungsten-page-binding\n" + canonical_json(identity)))


def seal_checkpoint(key: bytes, payload: dict[str, object]) -> str:
    """Seal ``payload`` into a token."""
    body = b64url(canonical_json(payload).encode("utf-8"))
    return f"{CHECKPOINT_PREFIX}{body}.{b64url(hmac_sha256(key, _SEAL + body))}"


def read_checkpoint(token: object) -> dict[str, Any] | None:
    """The payload of a well-formed token, without checking its seal."""
    if (
        not isinstance(token, str)
        or len(token) > MAX_CHECKPOINT_CHARS
        or not token.startswith(CHECKPOINT_PREFIX)
    ):
        return None
    parts = token[len(CHECKPOINT_PREFIX) :].split(".")
    if len(parts) != 2 or _B64URL.fullmatch(parts[0]) is None:
        return None
    try:
        raw = base64.urlsafe_b64decode(parts[0] + "=" * (-len(parts[0]) % 4))
        payload: object = json.loads(raw.decode("utf-8"))
    except (ValueError, binascii.Error, UnicodeDecodeError, RecursionError):
        return None
    if not isinstance(payload, dict):
        return None
    found = cast(dict[str, Any], payload)
    if (
        type(found.get("v")) is not int
        or found.get("v") != 1
        or not isinstance(found.get("operation"), str)
        or not isinstance(found.get("style"), str)
        or not isinstance(found.get("binding"), str)
        or found.get("state") not in ("partial", "complete", "stopped")
        or not _safe_int(found.get("pages"))
        or not _safe_int(found.get("issued"))
        or "next" not in found
    ):
        return None
    return found


def open_checkpoint(key: bytes, token: str) -> dict[str, Any] | None:
    """The payload of ``token`` when its seal verifies under ``key``."""
    payload = read_checkpoint(token)
    if payload is None:
        return None
    body, seal = token[len(CHECKPOINT_PREFIX) :].split(".")
    expected = b64url(hmac_sha256(key, _SEAL + body))
    return payload if hmac.compare_digest(expected, seal) else None


def describe_checkpoint(token: object) -> CheckpointStatus | None:
    """The secret-free status of a well-formed checkpoint token (operation,
    style, state, pages, issue time) for logs and support, or None when it is
    not one. Its seal is not checked; the cursor is omitted."""
    payload = read_checkpoint(token)
    if payload is None:
        return None
    return CheckpointStatus(
        version=1,
        operation=payload["operation"],
        style=payload["style"],
        state=payload["state"],
        pages=payload["pages"],
        issued_at=payload["issued"],
    )
