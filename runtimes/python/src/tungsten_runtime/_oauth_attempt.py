# SPDX-License-Identifier: Apache-2.0
"""Bounded, in-memory browser authorization attempt values and strict parsing."""

from __future__ import annotations

import re
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from urllib.parse import unquote_plus, urlsplit

TTL_MS = 600_000
MAX_ATTEMPTS = 32
MAX_BYTES = 32_768
MAX_CALLBACK_BYTES = 16_384
MAX_ITEMS = 64
TOKEN_RESPONSE_BYTES = 65_536
RESERVED = frozenset(
    (
        "response_type",
        "client_id",
        "client_secret",
        "redirect_uri",
        "scope",
        "resource",
        "audience",
        "state",
        "code_challenge",
        "code_challenge_method",
        "code_verifier",
        "grant_type",
        "code",
        "iss",
        "error",
        "error_description",
        "error_uri",
        "request",
        "request_uri",
        "response_mode",
    )
)


@dataclass(frozen=True, slots=True)
class IssuerDefense:
    """Pin issuerResponse with an exact issuer, or explicitly qualify distinctRedirect."""

    kind: str
    value: str


@dataclass(frozen=True, slots=True)
class AuthorizationAttempt:
    attempt_handle: str = field(repr=False)
    url: str = field(repr=False)
    expires_in_ms: float


@dataclass(slots=True, repr=False)
class PendingAuthorization:
    scheme: str
    generation: int
    started: float
    binding: str
    redirect_uri: str
    client_id: str
    client_secret: str | None
    configured_redirect: str | None
    authorization_url: str
    token_url: str
    refresh_url: str | None
    configured_scopes: tuple[str, ...]
    requested_scopes: tuple[str, ...]
    resource: str | None
    audience: str | None
    account_context: str | None
    defense: IssuerDefense
    state: str
    verifier: str | None
    claimed: bool = False
    canceled: bool = False
    dispose: Callable[[], None] = lambda: None


def callback_fields(query: str) -> dict[str, str]:
    if len(query) > MAX_CALLBACK_BYTES or len(query.encode("utf-8", "strict")) > MAX_CALLBACK_BYTES:
        raise ValueError
    if query.count("&") + bool(query) > MAX_ITEMS:
        raise ValueError
    fields: dict[str, str] = {}
    size = 0
    for pair in query.split("&") if query else ():
        key, equals, value = pair.partition("=")
        if not key or not equals or re.search(r"%(?![0-9a-fA-F]{2})", pair):
            raise ValueError
        key = unquote_plus(key, encoding="utf-8", errors="strict")
        value = unquote_plus(value, encoding="utf-8", errors="strict")
        size += len(key.encode("utf-8")) + len(value.encode("utf-8"))
        if size > MAX_CALLBACK_BYTES or key in fields:
            raise ValueError
        fields[key] = value
    return fields


def qualified_endpoint(url: str, *, redirect: bool = False) -> None:
    if len(url) > MAX_BYTES or len(url.encode("utf-8", "strict")) > MAX_BYTES:
        raise ValueError
    parts = urlsplit(url)
    if not parts.scheme or parts.username or parts.password or "#" in url:
        raise ValueError
    if redirect:
        if "?" in url:
            raise ValueError
    elif (
        parts.scheme not in ("https", "http")
        or not parts.hostname
        or RESERVED.intersection(callback_fields(parts.query))
    ):
        raise ValueError


def capture(
    strings: Iterable[str],
    binding: str,
    defense: IssuerDefense,
    s256_supported: bool,
    scopes: Sequence[str] | None,
    extra: Mapping[str, str] | None,
    resource: str | None,
    audience: str | None,
    account_context: str | None,
) -> tuple[tuple[str, ...] | None, dict[str, str]]:
    size = 0

    def charge(value: object) -> None:
        nonlocal size
        if not isinstance(value, str) or len(value) > MAX_BYTES:
            raise ValueError
        size += len(value.encode("utf-8", "strict"))
        if size > MAX_BYTES:
            raise ValueError

    if (
        s256_supported is not True
        or not binding
        or defense.kind not in ("issuerResponse", "distinctRedirect")
        or not defense.value
    ):
        raise ValueError
    for value in strings:
        charge(value)
    for value in (binding, defense.value, resource or "", audience or "", account_context or ""):
        charge(value)
    owned_scopes: list[str] | None = None
    if scopes is not None:
        if len(scopes) > MAX_ITEMS:
            raise ValueError
        owned_scopes = []
        for scope in scopes:
            charge(scope)
            owned_scopes.append(scope)
    owned_extra: dict[str, str] = {}
    if extra is not None:
        if len(extra) > MAX_ITEMS:
            raise ValueError
        for i, (key, value) in enumerate(extra.items()):
            if i >= MAX_ITEMS or key in RESERVED:
                raise ValueError
            charge(key)
            charge(value)
            owned_extra[key] = value
    return None if owned_scopes is None else tuple(owned_scopes), owned_extra
