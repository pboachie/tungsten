# SPDX-License-Identifier: Apache-2.0
"""Idempotency key formats and the header an operation's key travels in
(planning/04 "Idempotency policies", ``runtimes/ts/src/idempotency.ts``)."""

from __future__ import annotations

import re
from typing import Literal

from ._util import field, is_record, items_of, str_field

_UUID_ANY = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", re.IGNORECASE
)
_UUID_V4 = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", re.IGNORECASE)
_PRINTABLE = re.compile(r"[\x21-\x7e]+")


def _format_kind(fmt: str | None) -> Literal["uuid_v4", "uuid", "token"]:
    f = re.sub(r"[^a-z0-9]", "", (fmt or "").lower())
    if f in ("uuidv4", "uuid4"):
        return "uuid_v4"
    return "uuid" if f == "uuid" else "token"


def key_format_description(fmt: str | None) -> str:
    """Human description of the key format a policy expects."""
    kind = _format_kind(fmt)
    if kind == "uuid_v4":
        return "UUIDv4 string"
    if kind == "uuid":
        return "UUID string"
    return "a non-empty printable ASCII string of at most 255 characters"


def check_key_format(key: str, fmt: str | None) -> str | None:
    """Checks a caller-owned key against the policy's ``format``; returns the
    description of the expected format when the key does not match."""
    kind = _format_kind(fmt)
    if kind == "uuid_v4":
        valid = _UUID_V4.fullmatch(key) is not None
    elif kind == "uuid":
        valid = _UUID_ANY.fullmatch(key) is not None
    else:
        valid = 0 < len(key) <= 255 and _PRINTABLE.fullmatch(key) is not None
    return None if valid else key_format_description(fmt)


def key_param_wire(op: object) -> str | None:
    params = field(op, "params")
    for p in items_of(params):
        if is_record(p) and p.get("role") == "idempotency_key":
            return str_field(p, "wire")
    return None


def has_key_param(op: object) -> bool:
    params = field(op, "params")
    return any(is_record(p) and p.get("role") == "idempotency_key" for p in items_of(params))


def key_header(op: object) -> str:
    """The wire header of the operation's key: the policy's header, else the
    parameter with role ``idempotency_key``, else ``Idempotency-Key``."""
    from_policy = str_field(field(field(op, "agent"), "idempotency"), "header")
    if from_policy:
        return from_policy
    return key_param_wire(op) or "Idempotency-Key"
